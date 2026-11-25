//! DisplayAnything tray app: runs the agent in the background, shows pairing
//! codes, manages saved peers, and starts at login.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod remote;

use da_core::da_input::{self, Combo, Edge, Hotkeys, InjectorOptions, Recording, Target};
use da_core::{
    discovery, AutoConnector, Config, Event, Fingerprint, Identity, InputShare, Node, Permissions,
    PlatformInputTarget, ShareStatus, TrustStore, DEFAULT_PORT,
};
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tokio::sync::oneshot;

const LAUNCHED_AT_LOGIN: &str = "--minimized";
const AUTOSTART_SET_MARKER: &str = "autostart_configured";
const SETTINGS_FILE: &str = "tray_settings.json";

/// Settings the tray app remembers between runs.
#[derive(Serialize, Deserialize, Clone)]
struct Settings {
    /// Fingerprint of the device we share our keyboard and mouse with.
    #[serde(default)]
    input_peer: Option<String>,
    /// Custom switching hotkeys (None = the defaults).
    #[serde(default)]
    hotkeys: Option<Hotkeys>,
    /// Write da.log in the data folder.
    #[serde(default = "default_true")]
    logging: bool,
    /// Side of this computer where the shared-with device sits, for moving
    /// over by pushing the pointer past that edge (None = off).
    #[serde(default)]
    edge: Option<Edge>,
    /// When another computer controls this one, swap its Ctrl and
    /// Command/Windows keys (so a PC's Ctrl+C copies on a Mac).
    #[serde(default = "default_swap")]
    swap_ctrl_meta: bool,
    /// Pointer speed for the other computer's mouse, in percent.
    #[serde(default = "default_speed")]
    pointer_speed: u16,
}

fn default_speed() -> u16 {
    100
}

impl Settings {
    fn injector_options(&self) -> InjectorOptions {
        InjectorOptions {
            swap_ctrl_meta: self.swap_ctrl_meta,
            pointer_speed: f64::from(self.pointer_speed.clamp(25, 400)) / 100.0,
        }
    }
}

fn default_swap() -> bool {
    InjectorOptions::default().swap_ctrl_meta
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            input_peer: None,
            hotkeys: None,
            logging: true,
            edge: None,
            swap_ctrl_meta: default_swap(),
            pointer_speed: default_speed(),
        }
    }
}

impl Settings {
    fn load(dir: &std::path::Path) -> Self {
        std::fs::read_to_string(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, dir: &std::path::Path) {
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join(SETTINGS_FILE), t);
        }
    }
}

struct AppState {
    node: Arc<Node>,
    data_dir: PathBuf,
    listen_error: Mutex<Option<String>>,
    online: Mutex<HashSet<String>>,
    pending_code: Mutex<Option<oneshot::Sender<String>>>,
    settings: Mutex<Settings>,
    share: Mutex<Option<Arc<InputShare>>>,
    input_error: Mutex<Option<String>>,
    receiving_from: Mutex<Option<String>>,
    /// Name of a peer actively controlling this computer right now.
    controlled_by: Mutex<Option<String>>,
    /// mDNS advertisement; replaced when this device is renamed.
    advertiser: Mutex<Option<discovery::Advertiser>>,
    /// Which hotkey action is being recorded right now.
    recording_action: Mutex<Option<String>>,
    /// Replays input from other computers here.
    input_target: Option<Arc<PlatformInputTarget>>,
}

#[derive(Serialize, Clone)]
struct InputStatus {
    supported: bool,
    accessibility_ok: bool,
    /// Fingerprint of the chosen peer.
    peer_fingerprint: Option<String>,
    share: Option<ShareStatus>,
    error: Option<String>,
    /// Name of a peer connected to this computer's keyboard and mouse.
    receiving_from: Option<String>,
    /// Name of a peer actively controlling this computer right now.
    controlled_by: Option<String>,
}

#[derive(Serialize, Clone)]
struct Status {
    name: String,
    fingerprint: String,
    port: u16,
    listen_error: Option<String>,
    data_dir: String,
    /// Version plus the commit CI built from, so testers can tell builds apart.
    build: String,
}

#[derive(Serialize, Clone)]
struct PeerView {
    name: String,
    fingerprint: String,
    short: String,
    last_addr: Option<String>,
    auto_connect: bool,
    online: bool,
    permissions: Permissions,
    custom_name: bool,
}

/// Events forwarded to the window.
#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum UiEvent {
    PairCode {
        peer_name: String,
        peer_ip: String,
        code: String,
        expires_in_secs: u64,
    },
    Paired {
        name: String,
    },
    PairFailed {
        reason: String,
    },
    PeersChanged,
    InputChanged,
    EdgeChanged,
    /// Live view while recording a hotkey.
    HotkeyHeld {
        combo: Combo,
        held_ms: u64,
        needed_ms: u64,
    },
    /// Recording finished: saved, or refused with a reason.
    HotkeyDone {
        action: String,
        error: Option<String>,
    },
    Notice {
        text: String,
    },
}

fn fp_from(s: &str) -> Result<Fingerprint, String> {
    Fingerprint::from_hex(s).map_err(|e| e.to_string())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn build_id() -> String {
    let sha = option_env!("DA_BUILD").map(|s| &s[..s.len().min(7)]);
    format!(
        "{} ({})",
        env!("CARGO_PKG_VERSION"),
        sha.unwrap_or("local build")
    )
}

#[tauri::command]
fn status(state: State<'_, AppState>) -> Status {
    let id = state.node.identity();
    Status {
        name: state.node.name(),
        fingerprint: id.fingerprint().short(),
        port: DEFAULT_PORT,
        listen_error: state.listen_error.lock().unwrap().clone(),
        data_dir: state.data_dir.display().to_string(),
        build: build_id(),
    }
}

#[tauri::command]
fn peers(state: State<'_, AppState>) -> Vec<PeerView> {
    let online = state.online.lock().unwrap().clone();
    let mut v: Vec<PeerView> = state
        .node
        .trust()
        .peers()
        .iter()
        .map(|p| PeerView {
            name: p.name.clone(),
            fingerprint: p.fingerprint.clone(),
            short: p.fingerprint().map(|f| f.short()).unwrap_or_default(),
            last_addr: p.last_addr.clone(),
            auto_connect: p.auto_connect,
            online: online.contains(&p.fingerprint),
            permissions: p.permissions,
            custom_name: p.custom_name,
        })
        .collect();
    v.sort_by_key(|p| p.name.to_lowercase());
    v
}

const MAX_NAME_LEN: usize = 63;

fn clean_name(name: &str) -> Result<String, String> {
    let n: String = name.trim().chars().filter(|c| !c.is_control()).collect();
    if n.chars().count() > MAX_NAME_LEN {
        return Err(format!(
            "Keep the name to {MAX_NAME_LEN} characters or fewer."
        ));
    }
    Ok(n)
}

fn advertise(node: &Node) -> Option<discovery::Advertiser> {
    match discovery::advertise(&node.name(), DEFAULT_PORT, &node.identity().fingerprint()) {
        Ok(adv) => Some(adv),
        Err(e) => {
            tracing::warn!("mDNS unavailable: {e:#}");
            None
        }
    }
}

/// Rename this device. Saved for next time, re-announced on the network,
/// and picked up by connected devices within a few seconds.
#[tauri::command]
fn rename_device(state: State<'_, AppState>, name: String) -> Result<Status, String> {
    let name = clean_name(&name)?;
    if name.is_empty() {
        return Err("The name can't be empty.".into());
    }
    da_core::device_name(&state.data_dir, Some(&name)).map_err(|e| format!("{e:#}"))?;
    state.node.set_name(&name);
    {
        let mut adv = state.advertiser.lock().unwrap();
        adv.take(); // withdraw the old name before announcing the new one
        *adv = advertise(&state.node);
    }
    Ok(status(state))
}

/// Give a saved device a name on this computer. An empty name goes back to
/// the name that device gives itself.
#[tauri::command]
fn rename_peer(
    app: AppHandle,
    state: State<'_, AppState>,
    fingerprint: String,
    name: String,
) -> Result<(), String> {
    let fp = fp_from(&fingerprint)?;
    let name = clean_name(&name)?;
    let name = (!name.is_empty()).then_some(name.as_str());
    state
        .node
        .rename_peer(&fp, name)
        .map_err(|e| format!("{e:#}"))?;
    let _ = app.emit("da-event", UiEvent::PeersChanged);
    Ok(())
}

/// Pair with a device by IP. Resolves when pairing finishes; meanwhile the
/// window asks for the code and sends it with `submit_code`.
#[tauri::command]
async fn pair(app: AppHandle, state: State<'_, AppState>, addr: String) -> Result<String, String> {
    let addr = da_core::parse_addr(&addr).map_err(|e| e.to_string())?;
    let (tx, rx) = oneshot::channel();
    *state.pending_code.lock().unwrap() = Some(tx);
    let node = state.node.clone();
    let app2 = app.clone();
    let result = node
        .pair(addr, || async move {
            let _ = app2.emit("da-need-code", ());
            rx.await.map_err(|_| anyhow::anyhow!("pairing cancelled"))
        })
        .await;
    state.pending_code.lock().unwrap().take();
    let _ = app.emit("da-event", UiEvent::PeersChanged);
    result.map(|p| p.name).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn submit_code(state: State<'_, AppState>, code: String) -> Result<(), String> {
    let tx = state
        .pending_code
        .lock()
        .unwrap()
        .take()
        .ok_or("no pairing is waiting for a code")?;
    tx.send(code)
        .map_err(|_| "pairing already ended".to_string())
}

#[tauri::command]
fn cancel_pair(state: State<'_, AppState>) {
    state.pending_code.lock().unwrap().take();
}

#[tauri::command]
fn unpair(state: State<'_, AppState>, fingerprint: String) -> Result<(), String> {
    state
        .node
        .trust()
        .remove(&fingerprint)
        .map_err(|e| e.to_string())?;
    state.online.lock().unwrap().remove(&fingerprint);
    Ok(())
}

#[tauri::command]
fn set_auto_connect(
    state: State<'_, AppState>,
    fingerprint: String,
    on: bool,
) -> Result<(), String> {
    let fp = fp_from(&fingerprint)?;
    state
        .node
        .trust()
        .set_auto_connect(&fp, on)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn set_permissions(
    state: State<'_, AppState>,
    fingerprint: String,
    permissions: Permissions,
) -> Result<(), String> {
    let fp = fp_from(&fingerprint)?;
    state
        .node
        .trust()
        .set_permissions(&fp, permissions)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(app: AppHandle, on: bool) -> Result<(), String> {
    let al = app.autolaunch();
    if on { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
}

fn input_status_of(state: &AppState) -> InputStatus {
    InputStatus {
        supported: da_input::platform_supported(),
        accessibility_ok: da_input::permissions::input_permissions_ok(false),
        peer_fingerprint: state.settings.lock().unwrap().input_peer.clone(),
        share: state.share.lock().unwrap().as_ref().map(|s| s.status()),
        error: state.input_error.lock().unwrap().clone(),
        receiving_from: state.receiving_from.lock().unwrap().clone(),
        controlled_by: state.controlled_by.lock().unwrap().clone(),
    }
}

#[tauri::command]
fn input_status(state: State<'_, AppState>) -> InputStatus {
    input_status_of(&state)
}

/// Start capturing local input if it is not running yet.
fn ensure_share(state: &AppState) -> Result<Arc<InputShare>, String> {
    if let Some(s) = state.share.lock().unwrap().clone() {
        return Ok(s);
    }
    // Enter the app's Tokio runtime so starting the share (which spawns
    // tasks) works whichever thread calls this, including the main thread.
    let rt = tauri::async_runtime::handle();
    let _guard = rt.inner().enter();
    let hotkeys = state
        .settings
        .lock()
        .unwrap()
        .hotkeys
        .clone()
        .unwrap_or_default();
    let edge = state.settings.lock().unwrap().edge;
    match InputShare::start(state.node.clone(), hotkeys) {
        Ok(s) => {
            s.set_edge(edge);
            *state.share.lock().unwrap() = Some(s.clone());
            *state.input_error.lock().unwrap() = None;
            Ok(s)
        }
        Err(e) => {
            let msg = format!("{e:#}");
            *state.input_error.lock().unwrap() = Some(msg.clone());
            Err(msg)
        }
    }
}

/// Choose which paired device our keyboard and mouse can switch to
/// (`None` turns sharing off).
// Async so it runs on the Tokio runtime: starting the share spawns tasks,
// and plain (sync) commands run on the main thread, outside the runtime.
#[tauri::command]
async fn set_input_peer(
    state: State<'_, AppState>,
    fingerprint: Option<String>,
) -> Result<InputStatus, String> {
    let fp = fingerprint.as_deref().map(fp_from).transpose()?;
    {
        let mut settings = state.settings.lock().unwrap();
        settings.input_peer = fingerprint.clone();
        settings.save(&state.data_dir);
    }
    match fp {
        Some(fp) => ensure_share(&state)?.set_peer(Some(fp)),
        None => {
            if let Some(s) = state.share.lock().unwrap().as_ref() {
                s.switch(Target::Local);
                s.set_peer(None);
            }
        }
    }
    Ok(input_status_of(&state))
}

#[tauri::command]
fn switch_input(state: State<'_, AppState>, remote: bool) -> Result<(), String> {
    let share = state
        .share
        .lock()
        .unwrap()
        .clone()
        .ok_or("keyboard and mouse sharing is off")?;
    share.switch(if remote {
        Target::Remote
    } else {
        Target::Local
    });
    Ok(())
}

/// macOS: show the system prompt that leads to the Accessibility setting.
#[tauri::command]
fn request_accessibility() -> bool {
    da_input::permissions::input_permissions_ok(true)
}

#[tauri::command]
async fn discover() -> Result<Vec<(String, String)>, String> {
    let found = discovery::browse(std::time::Duration::from_secs(3))
        .await
        .map_err(|e| e.to_string())?;
    Ok(found
        .into_iter()
        .filter_map(|f| {
            let ip = f.addrs.iter().find(|a| a.is_ipv4()).or(f.addrs.first())?;
            let addr = if f.port == DEFAULT_PORT {
                ip.to_string()
            } else {
                SocketAddr::new(*ip, f.port).to_string()
            };
            Some((f.name, addr))
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Background work
// ---------------------------------------------------------------------------

fn start_background(app: &AppHandle, node: Arc<Node>) {
    // Listener.
    let app_l = app.clone();
    let node_l = node.clone();
    tauri::async_runtime::spawn(async move {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                if let Err(e) = node_l.serve(listener).await {
                    tracing::error!("listener stopped: {e:#}");
                }
            }
            Err(e) => {
                let msg = format!(
                    "Could not listen on port {DEFAULT_PORT}: {e}. Is another copy running?"
                );
                *app_l.state::<AppState>().listen_error.lock().unwrap() = Some(msg.clone());
                let _ = app_l.emit("da-event", UiEvent::Notice { text: msg });
            }
        }
    });

    // mDNS advertisement lives as long as the app (replaced on rename).
    *app.state::<AppState>().advertiser.lock().unwrap() = advertise(&node);

    // Reconnect to saved peers.
    tauri::async_runtime::spawn(AutoConnector::new(node.clone(), true).run());

    // Forward core events to the window and the tray tooltip.
    let app_e = app.clone();
    let mut rx = node.subscribe();
    tauri::async_runtime::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            let state = app_e.state::<AppState>();
            let ui = match ev {
                Event::PairCodeShown {
                    peer_name,
                    peer_addr,
                    code,
                    expires_in,
                } => {
                    show_main(&app_e);
                    Some(UiEvent::PairCode {
                        peer_name,
                        peer_ip: peer_addr.ip().to_string(),
                        code,
                        expires_in_secs: expires_in.as_secs(),
                    })
                }
                Event::Paired { name, .. } => Some(UiEvent::Paired { name }),
                Event::PairFailed { reason, .. } => Some(UiEvent::PairFailed { reason }),
                Event::PeerOnline { fingerprint, .. }
                | Event::SessionOpened { fingerprint, .. } => {
                    state.online.lock().unwrap().insert(fingerprint.to_hex());
                    Some(UiEvent::PeersChanged)
                }
                Event::PeerOffline { fingerprint, .. } => {
                    state.online.lock().unwrap().remove(&fingerprint.to_hex());
                    Some(UiEvent::PeersChanged)
                }
                Event::InputTargetChanged { .. } | Event::InputLink { .. } => {
                    Some(UiEvent::InputChanged)
                }
                Event::InputReceiving { peer, active } => {
                    if active
                        && !(da_input::permissions::accessibility_trusted(false)
                            && da_input::permissions::post_events_granted(false))
                    {
                        // macOS lets injected mouse moves through without the
                        // permission but silently drops keystrokes.
                        show_main(&app_e);
                        let _ = app_e.emit(
                            "da-event",
                            UiEvent::Notice {
                                text: format!(
                                    "{peer} is connected, but macOS is blocking its keystrokes. In System Settings, \
                                     search for Accessibility (under Privacy & Security), remove DisplayAnything with \
                                     the minus button, add it again, then restart the app. Or run scripts/DisplayAnythingInstall.sh."
                                ),
                            },
                        );
                    }
                    if !active {
                        *state.controlled_by.lock().unwrap() = None;
                    }
                    *state.receiving_from.lock().unwrap() = active.then_some(peer);
                    Some(UiEvent::InputChanged)
                }
                Event::InputControlled { peer, active } => {
                    *state.controlled_by.lock().unwrap() = active.then_some(peer);
                    Some(UiEvent::InputChanged)
                }
                Event::PeerRenamed { .. } => Some(UiEvent::PeersChanged),
                Event::HotkeyRecording(Recording::Held(combo, held)) => Some(UiEvent::HotkeyHeld {
                    combo,
                    held_ms: held.as_millis() as u64,
                    needed_ms: da_input::switcher::RECORD_HOLD.as_millis() as u64,
                }),
                Event::HotkeyRecording(Recording::Done(combo)) => finish_recording(&state, combo),
                Event::EdgeLocked(_) => Some(UiEvent::EdgeChanged),
                Event::RemoteViewed { peer, active } => Some(UiEvent::Notice {
                    text: if active {
                        format!("{peer} is viewing this screen. Close its window there to stop.")
                    } else {
                        format!("{peer} stopped viewing this screen.")
                    },
                }),
                Event::SessionClosed { .. } | Event::Listening { .. } | Event::Rejected { .. } => {
                    None
                }
            };
            let n = state.online.lock().unwrap().len();
            let share_status = state.share.lock().unwrap().as_ref().map(|s| s.status());
            // Green only while control is actually shared: this computer is
            // driving the other one, or the other one is driving this one.
            let sharing = share_status.as_ref().is_some_and(|s| s.remote_active)
                || state.controlled_by.lock().unwrap().is_some();
            update_tray_icon(&app_e, sharing);
            let remote_peer = state
                .share
                .lock()
                .unwrap()
                .as_ref()
                .map(|s| s.status())
                .filter(|s| s.remote_active)
                .and_then(|s| s.peer);
            if let Some(tray) = app_e.tray_by_id("main") {
                let tip = match (remote_peer, n) {
                    (Some(p), _) => format!("DisplayAnything: keyboard and mouse on {p}"),
                    (None, 0) => "DisplayAnything: no devices connected".to_string(),
                    (None, 1) => "DisplayAnything: 1 device connected".to_string(),
                    (None, n) => format!("DisplayAnything: {n} devices connected"),
                };
                let _ = tray.set_tooltip(Some(tip));
            }
            if let Some(ui) = ui {
                let _ = app_e.emit("da-event", ui);
            }
        }
    });
}

/// The tray / menu bar icon: green with a check mark while keyboard and
/// mouse control is being shared in either direction.
fn tray_icon(connected: bool) -> Option<tauri::image::Image<'static>> {
    let bytes: &[u8] = if connected {
        include_bytes!("../icons/tray-connected.png")
    } else {
        include_bytes!("../icons/tray.png")
    };
    tauri::image::Image::from_bytes(bytes).ok()
}

static TRAY_CONNECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn update_tray_icon(app: &AppHandle, connected: bool) {
    use std::sync::atomic::Ordering;
    if TRAY_CONNECTED.swap(connected, Ordering::Relaxed) == connected {
        return;
    }
    if let (Some(tray), Some(icon)) = (app.tray_by_id("main"), tray_icon(connected)) {
        let _ = tray.set_icon(Some(icon));
    }
}

/// The tray's "switch" items, greyed out while edge crossing is the mode.
static TRAY_SWITCH_ITEMS: std::sync::OnceLock<(MenuItem<tauri::Wry>, MenuItem<tauri::Wry>)> =
    std::sync::OnceLock::new();

fn update_tray_switch_items(edge_mode: bool) {
    if let Some((switch, home)) = TRAY_SWITCH_ITEMS.get() {
        let _ = switch.set_enabled(!edge_mode);
        let _ = home.set_enabled(!edge_mode);
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open DisplayAnything", true, None::<&str>)?;
    let switch = MenuItem::with_id(
        app,
        "switch",
        "Switch keyboard and mouse",
        true,
        None::<&str>,
    )?;
    let home = MenuItem::with_id(
        app,
        "home",
        "Keyboard and mouse to this computer",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &switch, &home, &quit])?;
    let _ = TRAY_SWITCH_ITEMS.set((switch, home));
    let edge_mode = app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap()
        .edge
        .is_some();
    update_tray_switch_items(edge_mode);
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("DisplayAnything")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "open" => show_main(app),
            "switch" | "home" => {
                let share = app.state::<AppState>().share.lock().unwrap().clone();
                if let Some(share) = share {
                    let to_remote = e.id.as_ref() == "switch" && !share.status().remote_active;
                    share.switch(if to_remote {
                        Target::Remote
                    } else {
                        Target::Local
                    });
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_main(tray.app_handle());
            }
        });
    match tray_icon(false) {
        Some(icon) => tray = tray.icon(icon),
        None => {
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
        }
    }
    tray.build(app)?;
    Ok(())
}

/// Write panics to `crash.log` in the data folder so a crash leaves a trace
/// the user can send us.
fn install_crash_log() {
    let Ok(dir) = da_core::default_data_dir() else {
        return;
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::fs::create_dir_all(&dir);
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let text = format!(
            "[{when}] DisplayAnything {} crashed: {info}\n{}\n\n",
            env!("CARGO_PKG_VERSION"),
            std::backtrace::Backtrace::force_capture()
        );
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("crash.log"))
        {
            let _ = f.write_all(text.as_bytes());
        }
        default_hook(info);
    }));
}

const ACTIONS: [&str; 4] = ["to_remote", "to_local", "panic", "edge_lock"];

fn current_hotkeys(state: &AppState) -> Hotkeys {
    state
        .settings
        .lock()
        .unwrap()
        .hotkeys
        .clone()
        .unwrap_or_default()
}

fn save_hotkeys(state: &AppState, hk: Option<Hotkeys>) {
    let applied = hk.clone().unwrap_or_default();
    {
        let mut s = state.settings.lock().unwrap();
        s.hotkeys = hk;
        s.save(&state.data_dir);
    }
    if let Some(share) = state.share.lock().unwrap().clone() {
        share.set_hotkeys(applied);
    }
}

#[derive(Serialize)]
struct HotkeysView {
    hotkeys: Hotkeys,
    is_default: bool,
}

#[tauri::command]
fn get_hotkeys(state: State<'_, AppState>) -> HotkeysView {
    let hotkeys = current_hotkeys(&state);
    HotkeysView {
        is_default: hotkeys == Hotkeys::default(),
        hotkeys,
    }
}

/// Start recording a new combination for `action` (to_remote, to_local or
/// panic). The window gets `hotkey_held` updates, then `hotkey_done`.
// Async: may start input capture, which needs the Tokio runtime.
#[tauri::command]
async fn record_hotkey(state: State<'_, AppState>, action: String) -> Result<(), String> {
    if !ACTIONS.contains(&action.as_str()) {
        return Err(format!("unknown action {action}"));
    }
    let share = ensure_share(&state)?;
    *state.recording_action.lock().unwrap() = Some(action);
    share.start_recording();
    Ok(())
}

#[tauri::command]
fn cancel_hotkey_record(state: State<'_, AppState>) {
    state.recording_action.lock().unwrap().take();
    if let Some(share) = state.share.lock().unwrap().clone() {
        share.cancel_recording();
    }
}

#[tauri::command]
fn reset_hotkeys(state: State<'_, AppState>) -> HotkeysView {
    save_hotkeys(&state, None);
    get_hotkeys(state)
}

/// A recording finished: check the combination and save it for the action.
fn finish_recording(state: &AppState, combo: Combo) -> Option<UiEvent> {
    let action = state.recording_action.lock().unwrap().take()?;
    let result = (|| {
        if let Some(p) = combo.problem() {
            return Err(p.to_string());
        }
        let mut hk = current_hotkeys(state);
        let clash = |name: &str, used: &Combo| -> Result<(), String> {
            if *used == combo {
                Err(format!("That combination is already used for \"{name}\"."))
            } else {
                Ok(())
            }
        };
        let names = [
            ("to_remote", "Switch to the other computer"),
            ("to_local", "Come back"),
            ("panic", "Always come back"),
            ("edge_lock", "Pause edge crossing"),
        ];
        let current = |a: &str| match a {
            "to_remote" => &hk.to_remote,
            "to_local" => &hk.to_local,
            "panic" => &hk.panic,
            _ => &hk.edge_lock,
        };
        for (other, name) in names {
            // Switching over and back may share a combination (it toggles).
            let toggle_pair = matches!(
                (action.as_str(), other),
                ("to_remote", "to_local") | ("to_local", "to_remote")
            );
            if other != action && !toggle_pair {
                clash(name, current(other))?;
            }
        }
        match action.as_str() {
            "to_remote" => hk.to_remote = combo.clone(),
            "to_local" => hk.to_local = combo.clone(),
            "panic" => hk.panic = combo.clone(),
            _ => hk.edge_lock = combo.clone(),
        }
        save_hotkeys(state, (hk != Hotkeys::default()).then_some(hk));
        Ok(())
    })();
    Some(UiEvent::HotkeyDone {
        action,
        error: result.err(),
    })
}

#[derive(Serialize)]
struct EdgeView {
    edge: Option<Edge>,
    locked: bool,
}

#[tauri::command]
fn get_edge(state: State<'_, AppState>) -> EdgeView {
    EdgeView {
        edge: state.settings.lock().unwrap().edge,
        locked: state
            .share
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.edge_locked()),
    }
}

/// Set which side of this computer the other device is on (None = off).
#[tauri::command]
fn set_edge(state: State<'_, AppState>, edge: Option<Edge>) {
    {
        let mut s = state.settings.lock().unwrap();
        s.edge = edge;
        s.save(&state.data_dir);
    }
    if let Some(share) = state.share.lock().unwrap().clone() {
        share.set_edge(edge);
    }
    update_tray_switch_items(edge.is_some());
}

#[tauri::command]
fn set_edge_locked(state: State<'_, AppState>, locked: bool) {
    if let Some(share) = state.share.lock().unwrap().clone() {
        share.set_edge_locked(locked);
    }
}

static LOG_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static LOG_FILE: std::sync::OnceLock<Mutex<Option<std::fs::File>>> = std::sync::OnceLock::new();

/// Writes to da.log only while logging is switched on.
struct LogWriter;

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if LOG_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(f) = LOG_FILE.get() {
                if let Some(f) = f.lock().unwrap().as_mut() {
                    let _ = f.write_all(buf);
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn open_log_file() -> Option<std::fs::File> {
    let dir = da_core::default_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("da.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

fn set_logging_enabled(on: bool) {
    let cell = LOG_FILE.get_or_init(|| Mutex::new(None));
    let mut f = cell.lock().unwrap();
    if on && f.is_none() {
        *f = open_log_file();
    }
    LOG_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
fn get_swap(state: State<'_, AppState>) -> bool {
    state.settings.lock().unwrap().swap_ctrl_meta
}

#[tauri::command]
fn set_swap(state: State<'_, AppState>, on: bool) {
    tracing::info!(on, "Ctrl/Command swap changed");
    let mut s = state.settings.lock().unwrap();
    s.swap_ctrl_meta = on;
    s.save(&state.data_dir);
    if let Some(t) = &state.input_target {
        t.set_options(s.injector_options());
    }
}

#[tauri::command]
fn get_pointer_speed(state: State<'_, AppState>) -> u16 {
    state.settings.lock().unwrap().pointer_speed
}

#[tauri::command]
fn set_pointer_speed(state: State<'_, AppState>, percent: u16) {
    let percent = percent.clamp(25, 400);
    tracing::info!(percent, "pointer speed changed");
    let mut s = state.settings.lock().unwrap();
    s.pointer_speed = percent;
    s.save(&state.data_dir);
    if let Some(t) = &state.input_target {
        t.set_options(s.injector_options());
    }
}

#[tauri::command]
fn get_logging(state: State<'_, AppState>) -> bool {
    state.settings.lock().unwrap().logging
}

#[tauri::command]
fn set_logging(state: State<'_, AppState>, on: bool) {
    if on {
        set_logging_enabled(true);
        tracing::info!("logging switched on ({})", build_id());
    } else {
        tracing::info!("logging switched off");
        set_logging_enabled(false);
    }
    let mut s = state.settings.lock().unwrap();
    s.logging = on;
    s.save(&state.data_dir);
}

/// Log to `da.log` in the data folder (and stderr), so testers can send it.
/// Can be switched off in Settings. Started fresh once it passes 5 MB.
fn init_logging() {
    use tracing_subscriber::prelude::*;
    let on = da_core::default_data_dir()
        .map(|d| Settings::load(&d).logging)
        .unwrap_or(true);
    set_logging_enabled(on);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,da_input=debug,da_core=debug".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(|| LogWriter),
        )
        .init();
    tracing::info!("DisplayAnything {} starting", build_id());
}

/// Show the log file in Explorer / Finder.
#[tauri::command]
fn show_log(state: State<'_, AppState>) -> Result<(), String> {
    let path = state.data_dir.join("da.log");
    #[cfg(windows)]
    let r = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open")
        .arg("-R")
        .arg(&path)
        .spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let r = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(&state.data_dir))
        .spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

fn main() {
    install_crash_log();
    init_logging();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app)
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![LAUNCHED_AT_LOGIN]),
        ))
        .setup(|app| {
            let data_dir = da_core::default_data_dir()?;
            let name = da_core::device_name(&data_dir, None)?;
            let identity = Identity::load_or_create(&data_dir, &name)?;
            let trust = TrustStore::load(&data_dir)?;
            let node = Node::new(identity, trust, Config::default())?;

            let settings = Settings::load(&data_dir);

            // Let paired devices with permission drive this computer.
            let input_target = match PlatformInputTarget::new(settings.injector_options()) {
                Ok(t) => {
                    node.set_input_target(Some(t.clone()));
                    // On macOS, ask for Accessibility up front: without it the
                    // keyboard does not work when another computer drives this one.
                    da_input::permissions::input_permissions_ok(true);
                    Some(t)
                }
                Err(e) => {
                    tracing::warn!("receiving input is unavailable: {e:#}");
                    None
                }
            };

            // Start at login by default, once; after that the user's choice stands.
            let marker = data_dir.join(AUTOSTART_SET_MARKER);
            if !marker.exists() {
                if let Err(e) = app.autolaunch().enable() {
                    tracing::warn!("could not enable start at login: {e}");
                }
                let _ = std::fs::write(&marker, b"1");
            }

            app.manage(remote::Viewers::default());
            app.manage(AppState {
                node: node.clone(),
                data_dir,
                listen_error: Mutex::new(None),
                online: Mutex::new(HashSet::new()),
                pending_code: Mutex::new(None),
                settings: Mutex::new(settings.clone()),
                share: Mutex::new(None),
                input_error: Mutex::new(None),
                receiving_from: Mutex::new(None),
                controlled_by: Mutex::new(None),
                advertiser: Mutex::new(None),
                recording_action: Mutex::new(None),
                input_target,
            });
            build_tray(app)?;
            start_background(app.handle(), node);

            // Resume keyboard and mouse sharing with the saved device.
            if let Some(fp) = settings.input_peer.as_deref().and_then(|f| fp_from(f).ok()) {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<AppState>();
                    if let Ok(share) = ensure_share(&state) {
                        share.set_peer(Some(fp));
                    }
                });
            }

            if !std::env::args().any(|a| a == LAUNCHED_AT_LOGIN) {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label().starts_with(remote::WINDOW_PREFIX) {
                // Closing a remote screen window ends that session.
                if let WindowEvent::Destroyed = event {
                    window
                        .state::<remote::Viewers>()
                        .window_closed(window.label());
                }
                return;
            }
            // Closing the window keeps the app running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            status,
            peers,
            pair,
            submit_code,
            cancel_pair,
            unpair,
            set_auto_connect,
            set_permissions,
            get_autostart,
            set_autostart,
            discover,
            input_status,
            set_input_peer,
            switch_input,
            request_accessibility,
            rename_device,
            rename_peer,
            show_log,
            get_hotkeys,
            record_hotkey,
            cancel_hotkey_record,
            reset_hotkeys,
            get_logging,
            set_logging,
            get_swap,
            set_swap,
            get_pointer_speed,
            set_pointer_speed,
            get_edge,
            set_edge,
            set_edge_locked,
            remote::open_remote
        ])
        .run(tauri::generate_context!())
        .expect("error while running DisplayAnything");
}
