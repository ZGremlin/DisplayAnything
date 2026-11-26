//! Grabs the local keyboard and mouse (controller side).
//!
//! A platform hook sees every event before the OS acts on it, hands it to
//! the [`Switcher`], and swallows it when input belongs to the peer.
//! Forwarded events leave through an unbounded channel the moment they are
//! captured, so the hook never waits on the network.

use crate::geometry::{self, Rect};
use crate::switcher::{Hotkeys, Outcome, Recording, Switcher, Target};
use crate::wire::Edge;
use crate::wire::InputEvent;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_focus;
#[cfg(windows)]
use windows as platform;
#[cfg(not(any(windows, target_os = "macos")))]
mod unsupported;
#[cfg(not(any(windows, target_os = "macos")))]
use unsupported as platform;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerMsg {
    /// Send this event to the peer now.
    Send(InputEvent),
    /// The active machine changed.
    Switched(Target),
    /// Progress while recording a hotkey combination.
    Recording(Recording),
    /// Tell the peer which of its edges input comes from (and whether the
    /// pointer crossed over there). Sent before any further input.
    Enter { edge: Edge, pos: u16, warp: bool },
    /// Edge crossing was paused (true) or resumed (false).
    EdgeLocked(bool),
}

/// Local input seen while a remote control session is on (see
/// [`Controller::set_remote_control`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GrabEvent {
    /// A key, while the remote window has focus (swallowed locally).
    Key { usage: u16, down: bool },
    /// The local pointer moved to this global position (not swallowed).
    Pointer { x: f64, y: f64 },
    /// A button or wheel event while the remote window has focus (not
    /// swallowed, so clicking still works locally).
    Mouse(InputEvent),
    /// The panic combination was pressed: end remote control.
    Panic,
}

/// Remote control state: sharing is paused while it is set.
struct RemoteMode {
    tx: UnboundedSender<GrabEvent>,
    /// The remote window has focus: keys go to the other computer.
    grabbing: bool,
    /// Keys held, to spot the panic combination.
    held: std::collections::HashSet<u16>,
}

/// Where to put the local pointer when input comes back over the edge;
/// the platform's switch-back code picks it up instead of the saved spot.
static RETURN_POS: Mutex<Option<(f64, f64)>> = Mutex::new(None);

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) fn take_return_pos() -> Option<(f64, f64)> {
    RETURN_POS.lock().unwrap().take()
}

pub(crate) struct Shared {
    switcher: Mutex<Switcher>,
    tx: UnboundedSender<ControllerMsg>,
    /// Displays, refreshed every couple of seconds for edge checks.
    screens: Mutex<(Vec<Rect>, Option<Instant>)>,
    /// Set while remote screen control is on; overrides the switcher.
    remote: Mutex<Option<RemoteMode>>,
}

const SCREENS_REFRESH: Duration = Duration::from_secs(2);

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
impl Shared {
    pub(crate) fn is_remote(&self) -> bool {
        self.switcher.lock().unwrap().target() == Target::Remote
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn in_remote_control(&self) -> bool {
        self.remote.lock().unwrap().is_some()
    }

    /// Remote control is on: hotkeys and edge crossing are off, and keys go
    /// to the remote window while it has focus. Returns Some(swallow).
    fn remote_key(&self, usage: u16, down: bool) -> Option<bool> {
        use crate::keymap::{ESC, LALT, LCTRL, RALT, RCTRL};
        let mut guard = self.remote.lock().unwrap();
        let r = guard.as_mut()?;
        if down {
            r.held.insert(usage);
        } else {
            r.held.remove(&usage);
        }
        let h = &r.held;
        if down
            && usage == ESC
            && (h.contains(&LCTRL) || h.contains(&RCTRL))
            && (h.contains(&LALT) || h.contains(&RALT))
        {
            let _ = r.tx.send(GrabEvent::Panic);
            return Some(r.grabbing);
        }
        if r.grabbing {
            let _ = r.tx.send(GrabEvent::Key { usage, down });
        }
        Some(r.grabbing)
    }

    fn remote_mouse(&self, ev: GrabEvent) -> Option<bool> {
        let guard = self.remote.lock().unwrap();
        let r = guard.as_ref()?;
        let send = match ev {
            GrabEvent::Pointer { .. } => true, // tracked even without focus
            _ => r.grabbing,
        };
        if send {
            let _ = r.tx.send(ev);
        }
        Some(false)
    }

    /// Returns true if the event must be swallowed.
    pub(crate) fn key(&self, usage: u16, down: bool) -> bool {
        if let Some(swallow) = self.remote_key(usage, down) {
            return swallow;
        }
        let out = {
            let mut sw = self.switcher.lock().unwrap();
            if down {
                sw.prune_held(|k| k == usage || platform::key_is_down(k).unwrap_or(true));
            }
            sw.on_key(usage, down)
        };
        self.apply(out)
    }

    fn screens(&self) -> Vec<Rect> {
        let mut c = self.screens.lock().unwrap();
        if c.1.is_none_or(|t| t.elapsed() > SCREENS_REFRESH) {
            *c = (crate::inject::screens(), Some(Instant::now()));
        }
        c.0.clone()
    }

    /// A local pointer move to (x, y), with its delta when the OS gives one.
    /// Returns true if the move must be swallowed (it crossed over).
    pub(crate) fn local_move(&self, x: f64, y: f64, delta: Option<(f64, f64)>) -> bool {
        if let Some(swallow) = self.remote_mouse(GrabEvent::Pointer { x, y }) {
            return swallow;
        }
        let Some(edge) = self.switcher.lock().unwrap().edge() else {
            return false;
        };
        let at = geometry::at_edge(&self.screens(), edge, x, y);
        let toward = delta.is_none_or(|(dx, dy)| match edge {
            Edge::Left => dx < 0.0,
            Edge::Right => dx > 0.0,
            Edge::Top => dy < 0.0,
            Edge::Bottom => dy > 0.0,
        });
        let out = self.switcher.lock().unwrap().edge_push(at, toward);
        self.apply(out)
    }

    pub(crate) fn mouse(&self, ev: InputEvent) -> bool {
        if let Some(swallow) = self.remote_mouse(GrabEvent::Mouse(ev)) {
            return swallow;
        }
        let out = self.switcher.lock().unwrap().on_mouse(ev);
        self.apply(out)
    }

    fn apply(&self, out: Outcome) -> bool {
        if let Some((edge, pos, warp)) = out.enter {
            let _ = self.tx.send(ControllerMsg::Enter { edge, pos, warp });
        }
        if let Some(locked) = out.edge_locked {
            let _ = self.tx.send(ControllerMsg::EdgeLocked(locked));
        }
        if let Some((edge, pos)) = out.place {
            *RETURN_POS.lock().unwrap() = geometry::entry_point(&self.screens(), edge, pos);
        }
        for e in out.send {
            let _ = self.tx.send(ControllerMsg::Send(e));
        }
        if !out.release_locally.is_empty() || !out.release_buttons_locally.is_empty() {
            let keys = out.release_locally;
            let buttons = out.release_buttons_locally;
            // Inject outside the hook callback, which must return quickly.
            std::thread::spawn(move || {
                for k in keys {
                    platform::release_local_key(k);
                }
                for b in buttons {
                    crate::inject::release_local_button(b);
                }
            });
        }
        if let Some(r) = out.recording {
            let _ = self.tx.send(ControllerMsg::Recording(r));
        }
        if let Some(t) = out.switched {
            let held = self.switcher.lock().unwrap().held();
            platform::on_switch(t, held);
            let _ = self.tx.send(ControllerMsg::Switched(t));
        }
        out.swallow
    }
}

/// Handle to the running capture. Cheap to clone.
#[derive(Clone)]
pub struct Controller {
    shared: Arc<Shared>,
}

impl Controller {
    /// Install the keyboard and mouse hooks. Input stays local until a peer
    /// is available and the user switches.
    pub fn start(hotkeys: Hotkeys) -> Result<(Controller, UnboundedReceiver<ControllerMsg>)> {
        let (tx, rx) = unbounded_channel();
        let shared = Arc::new(Shared {
            switcher: Mutex::new(Switcher::new(hotkeys)),
            tx,
            screens: Mutex::new((Vec::new(), None)),
            remote: Mutex::new(None),
        });
        platform::start(shared.clone())?;
        Ok((Controller { shared }, rx))
    }

    pub fn target(&self) -> Target {
        self.shared.switcher.lock().unwrap().target()
    }

    pub fn remote_available(&self) -> bool {
        self.shared.switcher.lock().unwrap().remote_available()
    }

    pub fn set_remote_available(&self, available: bool) {
        let out = self
            .shared
            .switcher
            .lock()
            .unwrap()
            .set_remote_available(available);
        self.shared.apply(out);
    }

    pub fn hotkeys(&self) -> Hotkeys {
        self.shared.switcher.lock().unwrap().hotkeys().clone()
    }

    pub fn set_hotkeys(&self, hotkeys: Hotkeys) {
        self.shared.switcher.lock().unwrap().set_hotkeys(hotkeys);
    }

    /// Record the next combination the user presses. Progress arrives as
    /// [`ControllerMsg::Recording`]; presses are swallowed until it is done.
    pub fn start_recording(&self) {
        self.shared.switcher.lock().unwrap().start_recording();
        // Held keys and buttons send no further events, so poll the hold
        // timer until the recording finishes or is cancelled.
        let shared = self.shared.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut sw = shared.switcher.lock().unwrap();
            if !sw.is_recording() {
                return;
            }
            if let Some(r) = sw.tick(std::time::Instant::now()) {
                drop(sw);
                let _ = shared.tx.send(ControllerMsg::Recording(r));
            }
        });
    }

    pub fn cancel_recording(&self) {
        self.shared.switcher.lock().unwrap().cancel_recording();
    }

    pub fn set_edge(&self, edge: Option<Edge>) {
        self.shared.switcher.lock().unwrap().set_edge(edge);
    }

    pub fn edge_locked(&self) -> bool {
        self.shared.switcher.lock().unwrap().edge_locked()
    }

    pub fn set_edge_locked(&self, locked: bool) {
        let out = self.shared.switcher.lock().unwrap().set_edge_locked(locked);
        self.shared.apply(out);
    }

    /// The peer pushed the pointer back out over its edge at `pos`.
    pub fn return_over_edge(&self, pos: u16) {
        let out = self.shared.switcher.lock().unwrap().return_over_edge(pos);
        self.shared.apply(out);
    }

    /// Start (Some) or stop (None) remote screen control. While it is on,
    /// input stays on this computer: the switch shortcuts, edge crossing and
    /// [`Controller::set_target`] do nothing, and local input is reported on
    /// `tx` (keys only while [`Controller::set_grabbing`] is on).
    pub fn set_remote_control(&self, tx: Option<UnboundedSender<GrabEvent>>) {
        if tx.is_some() {
            self.set_target(Target::Local);
        }
        *self.shared.remote.lock().unwrap() = tx.map(|tx| RemoteMode {
            tx,
            grabbing: false,
            held: Default::default(),
        });
    }

    pub fn remote_control(&self) -> bool {
        self.shared.remote.lock().unwrap().is_some()
    }

    /// The remote window gained (true) or lost (false) focus.
    pub fn set_grabbing(&self, on: bool) {
        if let Some(r) = self.shared.remote.lock().unwrap().as_mut() {
            r.grabbing = on;
        }
    }

    pub fn set_target(&self, t: Target) {
        if t == Target::Remote && self.remote_control() {
            return;
        }
        let out = self.shared.switcher.lock().unwrap().set_target(t);
        self.shared.apply(out);
    }
}
