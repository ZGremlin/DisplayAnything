//! Remote screen control from the tray app: a native window per peer that
//! shows its screen and, while it has focus, sends this computer's keyboard
//! and mouse to it.
//!
//! The window is a bare Tauri window with no web view; the platform
//! presenter (Direct3D swap chain on Windows, AVSampleBufferDisplayLayer on
//! macOS) draws straight into it.
//!
//! Input comes from the same capture that shares the keyboard and mouse
//! ([`da_core::InputShare`]), switched into remote-control mode while any
//! viewer window is open: sharing is paused, keys are taken while a viewer
//! window has focus, and pointer positions are mapped into the picture.
//! Ctrl+Alt+Esc closes every viewer window.

use crate::{ensure_share, fp_from, AppState, UiEvent};
use da_core::da_input::{GrabEvent, InputEvent, MouseButton};
use da_core::da_screen::audio::AudioPlayer;
use da_core::da_screen::wire::{StartRequest, StreamSettings};
use da_core::da_screen::{viewer::fit, Presenter, ViewMsg};
use da_core::{InputShare, RemoteControl, DEFAULT_PORT};
use raw_window_handle::HasWindowHandle;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

pub const WINDOW_PREFIX: &str = "remote-";

/// The window's drawing area in physical pixels, plus its scale factor
/// (macOS pointer positions arrive in points).
#[derive(Clone, Copy, Default)]
struct Area {
    x: f64,
    y: f64,
    width: u32,
    height: u32,
    scale: f64,
}

struct Session {
    fingerprint: String,
    task: tauri::async_runtime::JoinHandle<()>,
    control: RemoteControl,
    presenter: Arc<Presenter>,
    window: tauri::Window,
    area: Area,
    /// The host can send sound and switch it on and off.
    host_audio: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    /// The viewer window with focus, which gets the keyboard and mouse.
    focused: Option<String>,
    /// Buttons pressed into the focused session, released on focus loss.
    held: Vec<MouseButton>,
    /// The pointer was over the picture at its last position.
    inside: bool,
    /// Where captured input goes while any viewer is open.
    grab: Option<UnboundedSender<GrabEvent>>,
    share: Option<Arc<InputShare>>,
    /// macOS: the "Remote screen" menu-bar menu.
    #[cfg(target_os = "macos")]
    mac_menu: Option<tauri::menu::Submenu<tauri::Wry>>,
    /// Per peer fingerprint: how many monitors it has (from its last
    /// stream) and the one last shown, for the monitor picker.
    screens: HashMap<String, Screens>,
}

#[derive(Clone, Copy, Default)]
struct Screens {
    count: u32,
    shown: u32,
}

/// What the main window shows for a peer's monitors.
pub struct PeerScreens {
    /// Monitors it had last time (0 = not known yet).
    pub count: u32,
    /// The one shown now, while its viewer window is open.
    pub shown: Option<u32>,
}

/// Running viewer sessions, by window label.
pub struct Viewers {
    inner: Arc<Mutex<Inner>>,
    /// Play the other computers' sound (the setting, shared with every
    /// open viewer).
    audio: Arc<AtomicBool>,
}

impl Default for Viewers {
    fn default() -> Self {
        Viewers {
            inner: Default::default(),
            audio: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Viewers {
    /// The window closed: end its session (dropping the presenter and the
    /// connection, which tells the host to stop capturing).
    pub fn window_closed(&self, label: &str) {
        tracing::info!(window = label, "viewer window closed");
        let mut g = self.inner.lock().unwrap();
        if let Some(s) = g.sessions.remove(label) {
            s.task.abort();
        }
        if g.focused.as_deref() == Some(label) {
            g.focused = None;
            g.held.clear();
        }
        if g.sessions.is_empty() {
            // Back to normal keyboard and mouse sharing.
            if let Some(share) = g.share.take() {
                share.set_remote_control(None);
            }
            g.grab = None;
        }
    }

    /// A viewer window gained or lost focus: input follows it.
    pub fn window_focused(&self, label: &str, focused: bool) {
        let mut g = self.inner.lock().unwrap();
        if focused {
            if !g.sessions.contains_key(label) {
                return;
            }
            g.focused = Some(label.to_string());
            g.inside = false;
            if let Some(s) = &g.share {
                s.set_grabbing(true);
            }
        } else if g.focused.as_deref() == Some(label) {
            if let Some(s) = g.sessions.get(label) {
                // Nothing stays held on the other computer.
                s.control.send_input(InputEvent::ReleaseAll);
            }
            g.focused = None;
            g.held.clear();
            if let Some(s) = &g.share {
                s.set_grabbing(false);
            }
        }
    }

    pub fn screens_of(&self, fingerprint: &str) -> PeerScreens {
        let g = self.inner.lock().unwrap();
        let s = g.screens.get(fingerprint).copied().unwrap_or_default();
        let open = g.sessions.values().any(|x| x.fingerprint == fingerprint);
        PeerScreens {
            count: s.count,
            shown: open.then_some(s.shown),
        }
    }

    /// A viewer window moved or was resized.
    pub fn window_moved(&self, label: &str) {
        // Never ask the window anything while holding the lock: off the
        // main thread that waits for the main thread, which may be waiting
        // for this lock.
        let window = self
            .inner
            .lock()
            .unwrap()
            .sessions
            .get(label)
            .map(|s| s.window.clone());
        if let Some(w) = window {
            let area = area_of(&w);
            if let Some(s) = self.inner.lock().unwrap().sessions.get_mut(label) {
                s.area = area;
            }
        }
    }
}

fn area_of(w: &tauri::Window) -> Area {
    let pos = w.inner_position().unwrap_or_default();
    let size = w.inner_size().unwrap_or_default();
    Area {
        x: pos.x as f64,
        y: pos.y as f64,
        width: size.width,
        height: size.height,
        scale: w.scale_factor().unwrap_or(1.0),
    }
}

/// Handle one captured event for the focused viewer.
fn on_grab(app: &AppHandle, inner: &Mutex<Inner>, ev: GrabEvent) {
    let mut g = inner.lock().unwrap();
    if let GrabEvent::Panic = ev {
        tracing::info!("Ctrl+Alt+Esc: closing viewer windows");
        let windows: Vec<tauri::Window> = g.sessions.values().map(|s| s.window.clone()).collect();
        drop(g);
        for w in windows {
            let _ = w.destroy();
        }
        notice(app, "Remote control ended (Ctrl+Alt+Esc).".into());
        return;
    }
    let Some(label) = g.focused.clone() else {
        return;
    };
    let Some(s) = g.sessions.get(&label) else {
        return;
    };
    let control = s.control.clone();
    match ev {
        GrabEvent::Key { usage, down } => control.send_input(InputEvent::Key { usage, down }),
        GrabEvent::Pointer { x, y } => {
            let Some(video) = s.presenter.video_size() else {
                return;
            };
            let a = s.area;
            // Windows reports physical pixels; macOS reports points.
            let k = if cfg!(target_os = "macos") {
                a.scale
            } else {
                1.0
            };
            let (px, py) = (x * k - a.x, y * k - a.y);
            let (fx, fy, fw, fh) = fit(video, (a.width, a.height));
            let u = (px - fx as f64) / fw.max(1) as f64;
            let v = (py - fy as f64) / fh.max(1) as f64;
            let inside = (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v);
            g.inside = inside;
            // Dragging past the picture's edge keeps going, pinned to it.
            if inside || !g.held.is_empty() {
                let n = |f: f64| (f.clamp(0.0, 1.0) * 65535.0).round() as u16;
                control.pointer(n(u), n(v));
            }
        }
        GrabEvent::Mouse(e) => match e {
            InputEvent::Button { button, down: true } => {
                if g.inside && !g.held.contains(&button) {
                    g.held.push(button);
                    control.send_input(e);
                }
            }
            InputEvent::Button {
                button,
                down: false,
            } => {
                if let Some(i) = g.held.iter().position(|b| *b == button) {
                    g.held.remove(i);
                    control.send_input(e);
                }
            }
            InputEvent::Wheel { .. } if g.inside => control.send_input(e),
            _ => {}
        },
        GrabEvent::Panic => {}
    }
}

/// Switch input capture into remote-control mode (once, for the first
/// viewer) and return where captured events go. None when input capture
/// can't run here (the viewer is then view-only).
fn start_grab(
    app: &AppHandle,
    viewers: &Viewers,
    state: &AppState,
) -> Option<UnboundedSender<GrabEvent>> {
    if let Some(tx) = &viewers.inner.lock().unwrap().grab {
        return Some(tx.clone());
    }
    // Started without holding the lock (it may touch the main thread).
    let share = match ensure_share(state) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("viewer is view-only, input capture unavailable: {e}");
            notice(
                app,
                format!("Showing the screen only: keyboard and mouse capture is unavailable ({e})"),
            );
            return None;
        }
    };
    let mut g = viewers.inner.lock().unwrap();
    if let Some(tx) = &g.grab {
        return Some(tx.clone());
    }
    let (tx, mut rx) = unbounded_channel::<GrabEvent>();
    share.set_remote_control(Some(tx.clone()));
    g.share = Some(share);
    g.grab = Some(tx.clone());
    let inner = viewers.inner.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            on_grab(&app, &inner, ev);
        }
    });
    Some(tx)
}

fn notice(app: &AppHandle, text: String) {
    let _ = app.emit("da-event", UiEvent::Notice { text });
}

/// Open a window showing the screen of a paired device.
#[tauri::command]
pub async fn open_remote(
    app: AppHandle,
    state: State<'_, AppState>,
    viewers: State<'_, Viewers>,
    fingerprint: String,
    display: Option<u32>,
) -> Result<(), String> {
    let fp = fp_from(&fingerprint)?;
    let peer = state
        .node
        .trust()
        .get(&fp)
        .cloned()
        .ok_or("that device is not paired")?;
    let addr = peer
        .last_addr
        .as_deref()
        .and_then(|a| a.parse::<IpAddr>().ok())
        .map(|ip| SocketAddr::new(ip, peer.port.unwrap_or(DEFAULT_PORT)))
        .ok_or("no address for that device yet; wait until it shows as connected")?;

    let label = format!(
        "{WINDOW_PREFIX}{}",
        &fingerprint[..fingerprint.len().min(16)]
    );
    if let Some(w) = app.get_window(&label) {
        // Already open: switch monitors if asked, and bring it forward.
        if let Some(d) = display {
            if let Some(s) = viewers.inner.lock().unwrap().sessions.get(&label) {
                s.control.select_display(d);
            }
        }
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }

    let monitor = display;
    tracing::info!(peer = %peer.name, %addr, ?monitor, "opening viewer");
    let settings = stream_settings(&app, &state, None);
    let audio_on = state.settings.lock().unwrap().remote_audio;
    viewers.audio.store(audio_on, Ordering::Relaxed);
    // Connect first, so a refusal shows as a message, not an empty window.
    let mut view = state
        .node
        .open_remote(
            addr,
            StartRequest {
                max_width: settings.max_width,
                max_height: settings.max_height,
                bitrate_kbps: settings.bitrate_kbps,
                // The monitor asked for, else the one shown last time.
                display: display.unwrap_or_else(|| {
                    let g = viewers.inner.lock().unwrap();
                    g.screens.get(&fingerprint).map_or(0, |s| s.shown)
                }),
                audio: audio_on && da_core::da_screen::audio::playback_supported(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Could not open {}'s screen: {e:#}", peer.name))?;

    let builder = tauri::window::WindowBuilder::new(&app, &label)
        .title(format!("{} (DisplayAnything)", peer.name))
        .inner_size(1280.0, 800.0)
        .min_inner_size(320.0, 200.0);
    // Resolution and quality menus: on the window itself on Windows, in
    // the menu bar on macOS.
    #[cfg(not(target_os = "macos"))]
    let builder = match build_menu(&app) {
        Ok(m) => builder.menu(m),
        Err(e) => {
            tracing::warn!("no viewer menu: {e}");
            builder
        }
    };
    #[cfg(target_os = "macos")]
    refresh_mac_menu(&app, &viewers);
    let window = builder
        .build()
        .map_err(|e| format!("could not open the viewer window: {e}"))?;

    // The presenter attaches to the window on the main thread (macOS needs
    // that for its layer; harmless on Windows).
    tracing::info!(window = %label, "viewer window created");
    let grab = start_grab(&app, &viewers, &state);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let w = window.clone();
    let grab_keys = grab.clone();
    app.run_on_main_thread(move || {
        let r = w
            .window_handle()
            .map_err(|e| anyhow::anyhow!("no window handle: {e}"))
            .and_then(|h| {
                let raw = h.as_raw();
                // Windows: keys typed into the viewer window itself (the
                // system hook can miss them while our window has focus).
                #[cfg(windows)]
                if let (Some(tx), raw_window_handle::RawWindowHandle::Win32(h)) = (&grab_keys, raw)
                {
                    da_core::da_input::window_keys::attach(h.hwnd.get(), tx.clone());
                }
                #[cfg(not(windows))]
                let _ = &grab_keys;
                unsafe { Presenter::new(raw) }
            });
        let _ = tx.send(r);
    })
    .map_err(|e| e.to_string())?;
    let presenter = match rx.await.map_err(|e| e.to_string())? {
        Ok(p) => Arc::new(p),
        Err(e) => {
            let _ = window.destroy();
            tracing::warn!("viewer presenter failed: {e:#}");
            return Err(format!("could not show the remote screen: {e:#}"));
        }
    };

    tracing::info!("viewer presenter ready");
    let name = peer.name.clone();
    let app2 = app.clone();
    let win = window.clone();
    let control = view.control();
    let presenter2 = presenter.clone();
    let inner = viewers.inner.clone();
    let label2 = label.clone();
    let fp2 = fingerprint.clone();
    let host_audio = Arc::new(AtomicBool::new(false));
    let host_audio2 = host_audio.clone();
    let audio_on = viewers.audio.clone();
    let task = tauri::async_runtime::spawn(async move {
        let control = view.control();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut sized = false;
        // The host's sound: its format, and the player while sound is on.
        let mut sound: Option<da_core::da_screen::wire::AudioFormat> = None;
        let mut player: Option<AudioPlayer> = None;
        let mut player_failed = false;
        let end: Option<String> = loop {
            tokio::select! {
                msg = view.next() => match msg {
                    Some(ViewMsg::Info(info)) => {
                        host_audio2.store(info.audio, Ordering::Relaxed);
                        let resized = presenter.video_size() != Some((info.width, info.height));
                        presenter.set_video_size(info.width, info.height);
                        let now = Screens { count: info.displays.max(1), shown: info.display };
                        let changed = {
                            let mut g = inner.lock().unwrap();
                            let old = g.screens.insert(fp2.clone(), now);
                            old.is_none_or(|o| o.count != now.count || o.shown != now.shown)
                        };
                        if changed {
                            let _ = app2.emit("da-event", UiEvent::PeersChanged);
                        }
                        if !sized || resized {
                            sized = true;
                            fit_window(&win, info.width, info.height);
                            let area = area_of(&win);
                            if let Some(s) = inner.lock().unwrap().sessions.get_mut(&label2) {
                                s.area = area;
                            }
                        }
                    }
                    Some(ViewMsg::Video(f)) => {
                        presenter.push(f);
                        if presenter.take_keyframe_request() {
                            control.request_keyframe();
                        }
                    }
                    Some(ViewMsg::AudioInfo(f)) => {
                        if sound != Some(f) {
                            sound = Some(f);
                            player = None;
                            player_failed = false;
                        }
                    }
                    Some(ViewMsg::Audio(chunk)) => {
                        if !audio_on.load(Ordering::Relaxed) {
                            player = None;
                            continue;
                        }
                        if player.is_none() && !player_failed {
                            if let Some(f) = sound {
                                match AudioPlayer::new(f) {
                                    Ok(p) => player = Some(p),
                                    Err(e) => {
                                        // Don't retry on every chunk.
                                        player_failed = true;
                                        tracing::warn!("can't play {name}'s sound: {e:#}");
                                    }
                                }
                            }
                        }
                        if let Some(p) = &player {
                            p.push(&chunk.data);
                        }
                    }
                    Some(ViewMsg::Error(e)) => break Some(format!("{name} stopped sharing its screen: {e}")),
                    Some(ViewMsg::Stop) | None => break Some(format!("{name} ended the screen session")),
                    Some(_) => {}
                },
                _ = tick.tick() => {
                    if presenter.take_keyframe_request() {
                        control.request_keyframe();
                    }
                    // Sound switched off: stop playing what is queued.
                    if player.is_some() && !audio_on.load(Ordering::Relaxed) {
                        player = None;
                    }
                }
            }
        };
        tracing::info!(reason = ?end, "viewer session ended");
        if let Some(text) = end {
            notice(&app2, text);
        }
        let _ = app2.emit("da-event", UiEvent::PeersChanged);
        drop(view);
        let _ = win.destroy();
    });
    let area = area_of(&window);
    viewers.inner.lock().unwrap().sessions.insert(
        label.clone(),
        Session {
            fingerprint: fingerprint.clone(),
            task,
            control,
            presenter: presenter2,
            window: window.clone(),
            area,
            host_audio,
        },
    );
    if window.is_focused().unwrap_or(false) {
        viewers.window_focused(&label, true);
    }
    if grab.is_some() {
        notice(
            &app,
            format!(
                "Controlling {}: click into its window to use your keyboard and mouse there. \
                 Ctrl+Alt+Esc ends it. Keyboard and mouse sharing is paused meanwhile.",
                peer.name
            ),
        );
    }
    Ok(())
}

/// Size the window to the remote screen's shape, at most 80% of the
/// monitor it is on.
fn fit_window(w: &tauri::Window, width: u32, height: u32) {
    let Ok(Some(mon)) = w.current_monitor() else {
        return;
    };
    let area = mon.size();
    let (max_w, max_h) = (area.width as f64 * 0.8, area.height as f64 * 0.8);
    let k = (max_w / width as f64).min(max_h / height as f64).min(1.0);
    let size = tauri::PhysicalSize::new((width as f64 * k) as u32, (height as f64 * k) as u32);
    let _ = w.set_size(size);
    let _ = w.center();
}

// ----------------------------------------------------------------------
// Picture size and quality
// ----------------------------------------------------------------------

/// Resolution choices: id and label. Sizes are the largest picture; the
/// remote screen is scaled down to fit, keeping its shape.
pub const RESOLUTIONS: [(&str, &str); 7] = [
    ("native", "Full resolution"),
    ("match", "Match this screen"),
    ("3840x2160", "3840 x 2160 (4K)"),
    ("2560x1440", "2560 x 1440"),
    ("1920x1080", "1920 x 1080"),
    ("1600x900", "1600 x 900"),
    ("1280x720", "1280 x 720"),
];

/// Quality choices: id, label and bitrate in kbit/s.
pub const QUALITIES: [(&str, &str, u32); 4] = [
    ("highest", "Highest", 80_000),
    ("high", "High", 40_000),
    ("medium", "Medium", 20_000),
    ("low", "Low", 8_000),
];

fn bitrate(quality: &str) -> u32 {
    QUALITIES
        .iter()
        .find(|q| q.0 == quality)
        .map_or(QUALITIES[0].2, |q| q.2)
}

/// The picture size limit for a resolution choice; (0, 0) = full size.
/// "Match this screen" uses the monitor `window` is on, else the main
/// window's, else the primary monitor.
fn max_size(app: &AppHandle, resolution: &str, window: Option<&tauri::Window>) -> (u32, u32) {
    match resolution {
        "native" => (0, 0),
        "match" => {
            let mon = window
                .and_then(|w| w.current_monitor().ok().flatten())
                .or_else(|| {
                    app.get_webview_window("main")
                        .and_then(|w| w.current_monitor().ok().flatten())
                })
                .or_else(|| app.primary_monitor().ok().flatten());
            mon.map_or((0, 0), |m| (m.size().width, m.size().height))
        }
        wh => wh
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .unwrap_or((0, 0)),
    }
}

fn stream_settings(
    app: &AppHandle,
    state: &AppState,
    window: Option<&tauri::Window>,
) -> StreamSettings {
    let (res, q) = {
        let s = state.settings.lock().unwrap();
        (s.remote_resolution.clone(), s.remote_quality.clone())
    };
    let (max_width, max_height) = max_size(app, &res, window);
    StreamSettings {
        max_width,
        max_height,
        bitrate_kbps: bitrate(&q),
    }
}

#[derive(serde::Serialize)]
pub struct Choice {
    id: &'static str,
    label: &'static str,
}

#[derive(serde::Serialize)]
pub struct RemoteSettings {
    resolution: String,
    quality: String,
    /// Play the other computer's sound.
    audio: bool,
    resolutions: Vec<Choice>,
    qualities: Vec<Choice>,
}

/// The remote screen resolution and quality, with the choices.
#[tauri::command]
pub fn remote_settings(state: State<'_, AppState>) -> RemoteSettings {
    let s = state.settings.lock().unwrap();
    RemoteSettings {
        resolution: s.remote_resolution.clone(),
        quality: s.remote_quality.clone(),
        audio: s.remote_audio,
        resolutions: RESOLUTIONS
            .iter()
            .map(|&(id, label)| Choice { id, label })
            .collect(),
        qualities: QUALITIES
            .iter()
            .map(|&(id, label, _)| Choice { id, label })
            .collect(),
    }
}

/// Change the remote screen resolution, quality or sound. Open viewer
/// windows switch right away.
#[tauri::command]
pub fn set_remote_settings(
    app: AppHandle,
    resolution: Option<String>,
    quality: Option<String>,
    audio: Option<bool>,
) -> Result<(), String> {
    if let Some(on) = audio {
        set_audio(&app, on);
    }
    if resolution.is_none() && quality.is_none() {
        return Ok(());
    }
    change_settings(&app, resolution, quality)
}

/// Turn the other computers' sound on or off, saved and applied to open
/// viewer windows (the host stops or starts recording).
fn set_audio(app: &AppHandle, on: bool) {
    let state = app.state::<AppState>();
    {
        let mut s = state.settings.lock().unwrap();
        s.remote_audio = on;
        s.save(&state.data_dir);
    }
    let viewers = app.state::<Viewers>();
    viewers.audio.store(on, Ordering::Relaxed);
    let open: Vec<RemoteControl> = viewers
        .inner
        .lock()
        .unwrap()
        .sessions
        .values()
        .filter(|s| s.host_audio.load(Ordering::Relaxed))
        .map(|s| s.control.clone())
        .collect();
    for control in &open {
        control.set_audio(on);
    }
    tracing::info!(on, "remote screen sound changed");
    // Refresh the menus' check marks.
    #[cfg(not(target_os = "macos"))]
    {
        let windows: Vec<tauri::Window> = viewers
            .inner
            .lock()
            .unwrap()
            .sessions
            .values()
            .map(|s| s.window.clone())
            .collect();
        for w in windows {
            if let Ok(m) = build_menu(app) {
                let _ = w.set_menu(m);
            }
        }
    }
    #[cfg(target_os = "macos")]
    refresh_mac_menu(app, &viewers);
    let _ = app.emit("da-event", UiEvent::PeersChanged);
}

fn change_settings(
    app: &AppHandle,
    resolution: Option<String>,
    quality: Option<String>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    {
        let mut s = state.settings.lock().unwrap();
        if let Some(r) = resolution {
            if !RESOLUTIONS.iter().any(|x| x.0 == r) {
                return Err(format!("unknown resolution {r}"));
            }
            s.remote_resolution = r;
        }
        if let Some(q) = quality {
            if !QUALITIES.iter().any(|x| x.0 == q) {
                return Err(format!("unknown quality {q}"));
            }
            s.remote_quality = q;
        }
        s.save(&state.data_dir);
    }
    let viewers = app.state::<Viewers>();
    let open: Vec<(RemoteControl, tauri::Window)> = viewers
        .inner
        .lock()
        .unwrap()
        .sessions
        .values()
        .map(|s| (s.control.clone(), s.window.clone()))
        .collect();
    for (control, window) in &open {
        let settings = stream_settings(app, &state, Some(window));
        tracing::info!(?settings, "changing remote screen settings");
        control.configure(settings);
        #[cfg(not(target_os = "macos"))]
        if let Ok(m) = build_menu(app) {
            let _ = window.set_menu(m);
        }
    }
    #[cfg(target_os = "macos")]
    refresh_mac_menu(app, &viewers);
    let _ = app.emit("da-event", UiEvent::PeersChanged);
    Ok(())
}

/// A viewer menu item was chosen.
pub fn on_menu(app: &AppHandle, id: &str) {
    let r = if let Some(r) = id.strip_prefix("rv-res:") {
        change_settings(app, Some(r.to_string()), None)
    } else if let Some(q) = id.strip_prefix("rv-q:") {
        change_settings(app, None, Some(q.to_string()))
    } else if id == "rv-audio" {
        let on = app
            .state::<AppState>()
            .settings
            .lock()
            .unwrap()
            .remote_audio;
        set_audio(app, !on);
        Ok(())
    } else {
        return;
    };
    if let Err(e) = r {
        tracing::warn!("menu: {e}");
    }
}

type Submenus = (
    tauri::menu::Submenu<tauri::Wry>,
    tauri::menu::Submenu<tauri::Wry>,
    tauri::menu::Submenu<tauri::Wry>,
);

fn build_submenus(app: &AppHandle) -> tauri::Result<Submenus> {
    use tauri::menu::{CheckMenuItem, IsMenuItem, Submenu};
    let (res, q, audio) = {
        let state = app.state::<AppState>();
        let s = state.settings.lock().unwrap();
        (
            s.remote_resolution.clone(),
            s.remote_quality.clone(),
            s.remote_audio,
        )
    };
    let res_items = RESOLUTIONS
        .iter()
        .map(|&(id, label)| {
            CheckMenuItem::with_id(
                app,
                format!("rv-res:{id}"),
                label,
                true,
                id == res,
                None::<&str>,
            )
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let q_items = QUALITIES
        .iter()
        .map(|&(id, label, _)| {
            CheckMenuItem::with_id(
                app,
                format!("rv-q:{id}"),
                label,
                true,
                id == q,
                None::<&str>,
            )
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    fn refs(v: &[CheckMenuItem<tauri::Wry>]) -> Vec<&dyn IsMenuItem<tauri::Wry>> {
        v.iter().map(|i| i as &dyn IsMenuItem<tauri::Wry>).collect()
    }
    let res_menu = Submenu::with_items(app, "Resolution", true, &refs(&res_items))?;
    let q_menu = Submenu::with_items(app, "Quality", true, &refs(&q_items))?;
    let sound =
        CheckMenuItem::with_id(app, "rv-audio", "Play its sound", true, audio, None::<&str>)?;
    let sound_menu = Submenu::with_items(app, "Sound", true, &[&sound])?;
    Ok((res_menu, q_menu, sound_menu))
}

#[cfg(not(target_os = "macos"))]
fn build_menu(app: &AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let (r, q, a) = build_submenus(app)?;
    tauri::menu::Menu::with_items(app, &[&r, &q, &a])
}

/// macOS: a "Remote screen" menu in the menu bar, rebuilt when the
/// settings change.
#[cfg(target_os = "macos")]
fn refresh_mac_menu(app: &AppHandle, viewers: &Viewers) {
    use tauri::menu::Submenu;
    let Some(bar) = app.menu() else { return };
    let built = build_submenus(app)
        .and_then(|(r, q, a)| Submenu::with_items(app, "Remote screen", true, &[&r, &q, &a]));
    // Menu calls wait for the main thread: never hold the lock over them.
    let old = viewers.inner.lock().unwrap().mac_menu.take();
    if let Some(old) = old {
        let _ = bar.remove(&old);
    }
    match built {
        Ok(m) => {
            if bar.append(&m).is_ok() {
                viewers.inner.lock().unwrap().mac_menu = Some(m);
            }
        }
        Err(e) => tracing::warn!("no viewer menu: {e}"),
    }
}
