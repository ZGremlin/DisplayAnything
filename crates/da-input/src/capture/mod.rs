//! Grabs the local keyboard and mouse (controller side).
//!
//! A platform hook sees every event before the OS acts on it, hands it to
//! the [`Switcher`], and swallows it when input belongs to the peer.
//! Forwarded events leave through an unbounded channel the moment they are
//! captured, so the hook never waits on the network.

use crate::switcher::{Hotkeys, Outcome, Switcher, Target};
use crate::wire::InputEvent;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
mod windows;
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
}

pub(crate) struct Shared {
    switcher: Mutex<Switcher>,
    tx: UnboundedSender<ControllerMsg>,
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
impl Shared {
    pub(crate) fn is_remote(&self) -> bool {
        self.switcher.lock().unwrap().target() == Target::Remote
    }

    /// Returns true if the event must be swallowed.
    pub(crate) fn key(&self, usage: u16, down: bool) -> bool {
        let out = self.switcher.lock().unwrap().on_key(usage, down);
        self.apply(out)
    }

    pub(crate) fn mouse(&self, ev: InputEvent) -> bool {
        let out = self.switcher.lock().unwrap().on_mouse(ev);
        self.apply(out)
    }

    fn apply(&self, out: Outcome) -> bool {
        for e in out.send {
            let _ = self.tx.send(ControllerMsg::Send(e));
        }
        if !out.release_locally.is_empty() {
            let keys = out.release_locally;
            // Inject outside the hook callback, which must return quickly.
            std::thread::spawn(move || {
                for k in keys {
                    platform::release_local_key(k);
                }
            });
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

    pub fn set_target(&self, t: Target) {
        let out = self.shared.switcher.lock().unwrap().set_target(t);
        self.shared.apply(out);
    }
}
