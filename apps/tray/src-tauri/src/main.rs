//! DisplayAnything tray app: runs the agent in the background, shows pairing
//! codes, manages saved peers, and starts at login.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use da_core::{
    discovery, AutoConnector, Config, Event, Fingerprint, Identity, Node, Permissions, TrustStore,
    DEFAULT_PORT,
};
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

struct AppState {
    node: Arc<Node>,
    data_dir: PathBuf,
    listen_error: Mutex<Option<String>>,
    online: Mutex<HashSet<String>>,
    pending_code: Mutex<Option<oneshot::Sender<String>>>,
}

#[derive(Serialize, Clone)]
struct Status {
    name: String,
    fingerprint: String,
    port: u16,
    listen_error: Option<String>,
    data_dir: String,
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

#[tauri::command]
fn status(state: State<'_, AppState>) -> Status {
    let id = state.node.identity();
    Status {
        name: id.name.clone(),
        fingerprint: id.fingerprint().short(),
        port: DEFAULT_PORT,
        listen_error: state.listen_error.lock().unwrap().clone(),
        data_dir: state.data_dir.display().to_string(),
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
        })
        .collect();
    v.sort_by_key(|p| p.name.to_lowercase());
    v
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

    // mDNS advertisement lives as long as the app.
    match discovery::advertise(
        &node.identity().name,
        DEFAULT_PORT,
        &node.identity().fingerprint(),
    ) {
        Ok(adv) => {
            app.manage(adv);
        }
        Err(e) => tracing::warn!("mDNS unavailable: {e:#}"),
    }

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
                Event::SessionClosed { .. } | Event::Listening { .. } | Event::Rejected { .. } => {
                    None
                }
            };
            let n = state.online.lock().unwrap().len();
            if let Some(tray) = app_e.tray_by_id("main") {
                let tip = match n {
                    0 => "DisplayAnything: no devices connected".to_string(),
                    1 => "DisplayAnything: 1 device connected".to_string(),
                    n => format!("DisplayAnything: {n} devices connected"),
                };
                let _ = tray.set_tooltip(Some(tip));
            }
            if let Some(ui) = ui {
                let _ = app_e.emit("da-event", ui);
            }
        }
    });
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open DisplayAnything", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("DisplayAnything")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "open" => show_main(app),
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
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

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

            // Start at login by default, once; after that the user's choice stands.
            let marker = data_dir.join(AUTOSTART_SET_MARKER);
            if !marker.exists() {
                if let Err(e) = app.autolaunch().enable() {
                    tracing::warn!("could not enable start at login: {e}");
                }
                let _ = std::fs::write(&marker, b"1");
            }

            app.manage(AppState {
                node: node.clone(),
                data_dir,
                listen_error: Mutex::new(None),
                online: Mutex::new(HashSet::new()),
                pending_code: Mutex::new(None),
            });
            build_tray(app)?;
            start_background(app.handle(), node);

            if !std::env::args().any(|a| a == LAUNCHED_AT_LOGIN) {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
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
            discover
        ])
        .run(tauri::generate_context!())
        .expect("error while running DisplayAnything");
}
