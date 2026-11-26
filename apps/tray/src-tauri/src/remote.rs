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
use da_core::da_screen::wire::StartRequest;
use da_core::da_screen::{viewer::fit, Presenter, ViewMsg};
use da_core::{InputShare, RemoteControl, DEFAULT_PORT};
use raw_window_handle::HasWindowHandle;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
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
#[derive(Default)]
pub struct Viewers {
    inner: Arc<Mutex<Inner>>,
}

impl Viewers {
    /// The window closed: end its session (dropping the presenter and the
    /// connection, which tells the host to stop capturing).
    pub fn window_closed(&self, label: &str) {
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

    // Connect first, so a refusal shows as a message, not an empty window.
    let mut view = state
        .node
        .open_remote(
            addr,
            StartRequest {
                // The monitor asked for, else the one shown last time.
                display: display.unwrap_or_else(|| {
                    let g = viewers.inner.lock().unwrap();
                    g.screens.get(&fingerprint).map_or(0, |s| s.shown)
                }),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Could not open {}'s screen: {e:#}", peer.name))?;

    let window = tauri::window::WindowBuilder::new(&app, &label)
        .title(format!("{} (DisplayAnything)", peer.name))
        .inner_size(1280.0, 800.0)
        .min_inner_size(320.0, 200.0)
        .build()
        .map_err(|e| format!("could not open the viewer window: {e}"))?;

    // The presenter attaches to the window on the main thread (macOS needs
    // that for its layer; harmless on Windows).
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
            return Err(format!("could not show the remote screen: {e:#}"));
        }
    };

    let name = peer.name.clone();
    let app2 = app.clone();
    let win = window.clone();
    let control = view.control();
    let presenter2 = presenter.clone();
    let inner = viewers.inner.clone();
    let label2 = label.clone();
    let fp2 = fingerprint.clone();
    let task = tauri::async_runtime::spawn(async move {
        let control = view.control();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut sized = false;
        let end: Option<String> = loop {
            tokio::select! {
                msg = view.next() => match msg {
                    Some(ViewMsg::Info(info)) => {
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
                    Some(ViewMsg::Error(e)) => break Some(format!("{name} stopped sharing its screen: {e}")),
                    Some(ViewMsg::Stop) | None => break Some(format!("{name} ended the screen session")),
                    Some(_) => {}
                },
                _ = tick.tick() => {
                    if presenter.take_keyframe_request() {
                        control.request_keyframe();
                    }
                }
            }
        };
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
