//! Remote screen control from the tray app: a native window per peer that
//! shows its screen (step 1: view only).
//!
//! The window is a bare Tauri window with no web view; the platform
//! presenter (Direct3D swap chain on Windows, AVSampleBufferDisplayLayer on
//! macOS) draws straight into it.

use crate::{fp_from, AppState, UiEvent};
use da_core::da_screen::wire::StartRequest;
use da_core::da_screen::{Presenter, ViewMsg};
use da_core::DEFAULT_PORT;
use raw_window_handle::HasWindowHandle;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

pub const WINDOW_PREFIX: &str = "remote-";

/// Running viewer sessions, by window label.
#[derive(Default)]
pub struct Viewers {
    sessions: Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>,
}

impl Viewers {
    /// The window closed: end its session (dropping the presenter and the
    /// connection, which tells the host to stop capturing).
    pub fn window_closed(&self, label: &str) {
        if let Some(task) = self.sessions.lock().unwrap().remove(label) {
            task.abort();
        }
    }
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
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }

    // Connect first, so a refusal shows as a message, not an empty window.
    let mut view = state
        .node
        .open_remote(addr, StartRequest::default())
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
    let (tx, rx) = tokio::sync::oneshot::channel();
    let w = window.clone();
    app.run_on_main_thread(move || {
        let r = w
            .window_handle()
            .map_err(|e| anyhow::anyhow!("no window handle: {e}"))
            .and_then(|h| unsafe { Presenter::new(h.as_raw()) });
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
    let task = tauri::async_runtime::spawn(async move {
        let control = view.control();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut sized = false;
        let end: Option<String> = loop {
            tokio::select! {
                msg = view.next() => match msg {
                    Some(ViewMsg::Info(info)) => {
                        presenter.set_video_size(info.width, info.height);
                        if !sized {
                            sized = true;
                            fit_window(&win, info.width, info.height);
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
        drop(view);
        let _ = win.destroy();
    });
    viewers.sessions.lock().unwrap().insert(label, task);
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
