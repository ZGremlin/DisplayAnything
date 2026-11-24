//! Sharing this computer's keyboard and mouse with one paired peer.
//!
//! Glues the platform capture ([`Controller`]) to an [`InputLink`]: keeps
//! the link open while the peer is reachable, forwards captured events, and
//! moves the clipboard along with the active machine.

use crate::identity::Fingerprint;
use crate::input::InputLink;
use crate::net::{Event, Node};
use crate::DEFAULT_PORT;
use anyhow::Result;
use da_input::{Controller, ControllerMsg, Frame, Hotkeys, Target};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

const RECONNECT_EVERY: Duration = Duration::from_secs(2);
const CLIPBOARD_WAIT: Duration = Duration::from_millis(500);

pub struct InputShare {
    node: Arc<Node>,
    controller: Controller,
    peer: Mutex<Option<Fingerprint>>,
    link: Mutex<Option<Arc<InputLink>>>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ShareStatus {
    /// Peer we share with, if any.
    pub peer: Option<String>,
    /// The input link to it is open, so switching works.
    pub connected: bool,
    /// Our keyboard and mouse currently drive the peer.
    pub remote_active: bool,
}

impl InputShare {
    /// Start capturing local input (it stays local until switched).
    /// Must be called from inside a Tokio runtime (it spawns tasks).
    pub fn start(node: Arc<Node>, hotkeys: Hotkeys) -> Result<Arc<Self>> {
        if tokio::runtime::Handle::try_current().is_err() {
            anyhow::bail!("InputShare::start must run inside the Tokio runtime");
        }
        let (controller, msgs) = Controller::start(hotkeys)?;
        let share = Arc::new(InputShare {
            node,
            controller,
            peer: Mutex::new(None),
            link: Mutex::new(None),
        });
        tokio::spawn(share.clone().forward(msgs));
        tokio::spawn(share.clone().maintain_link());
        Ok(share)
    }

    /// Choose the peer to share with (None stops sharing).
    pub fn set_peer(&self, fp: Option<Fingerprint>) {
        let changed = *self.peer.lock().unwrap() != fp;
        if changed {
            *self.peer.lock().unwrap() = fp;
            self.drop_link();
        }
    }

    pub fn peer(&self) -> Option<Fingerprint> {
        *self.peer.lock().unwrap()
    }

    /// Switch from the UI. Hotkeys do the same from the keyboard.
    pub fn switch(&self, t: Target) {
        self.controller.set_target(t);
    }

    pub fn status(&self) -> ShareStatus {
        let link = self.link.lock().unwrap().clone();
        let peer = self
            .peer()
            .and_then(|fp| self.node.trust().get(&fp).map(|p| p.name.clone()));
        ShareStatus {
            peer,
            connected: link.as_ref().is_some_and(|l| !l.is_closed()),
            remote_active: self.controller.target() == Target::Remote,
        }
    }

    fn drop_link(&self) {
        let old = self.link.lock().unwrap().take();
        self.controller.set_remote_available(false);
        if let Some(l) = old {
            self.node.emit(Event::InputLink {
                peer: l.peer.name.clone(),
                connected: false,
            });
        }
    }

    fn current_link(&self) -> Option<Arc<InputLink>> {
        self.link.lock().unwrap().clone().filter(|l| !l.is_closed())
    }

    async fn maintain_link(self: Arc<Self>) {
        loop {
            let has_link = self.current_link().is_some();
            if !has_link && self.link.lock().unwrap().is_some() {
                self.drop_link(); // it closed underneath us
            }
            if let (false, Some(fp)) = (has_link, self.peer()) {
                let peer = self.node.trust().get(&fp).cloned();
                let addr = peer.as_ref().and_then(|p| {
                    let ip = p.last_addr.as_deref()?.parse::<IpAddr>().ok()?;
                    Some(SocketAddr::new(ip, p.port.unwrap_or(DEFAULT_PORT)))
                });
                if let Some(addr) = addr {
                    match self.node.open_input(addr).await {
                        Ok(link) if self.peer() == Some(fp) => {
                            let name = link.peer.name.clone();
                            *self.link.lock().unwrap() = Some(Arc::new(link));
                            self.controller.set_remote_available(true);
                            self.node.emit(Event::InputLink {
                                peer: name,
                                connected: true,
                            });
                        }
                        Ok(_) => {} // peer changed while connecting
                        Err(e) => tracing::debug!("input link to {addr} failed: {e:#}"),
                    }
                }
            }
            tokio::time::sleep(RECONNECT_EVERY).await;
        }
    }

    async fn forward(self: Arc<Self>, mut msgs: UnboundedReceiver<ControllerMsg>) {
        while let Some(msg) = msgs.recv().await {
            match msg {
                ControllerMsg::Send(e) => {
                    if let Some(l) = self.current_link() {
                        l.send_event(e);
                    }
                }
                ControllerMsg::Switched(Target::Remote) => {
                    let link = self.current_link();
                    if let (Some(l), Some(text)) = (&link, da_input::clipboard::get_text()) {
                        l.send(Frame::ClipboardText(text));
                    }
                    let peer = link.map(|l| l.peer.name.clone());
                    self.node.emit(Event::InputTargetChanged { peer });
                }
                ControllerMsg::Switched(Target::Local) => {
                    self.node.emit(Event::InputTargetChanged { peer: None });
                    if let Some(l) = self.current_link() {
                        tokio::spawn(async move {
                            if let Some(text) = l.request_clipboard(CLIPBOARD_WAIT).await {
                                if !text.is_empty() {
                                    da_input::clipboard::set_text(&text);
                                }
                            }
                        });
                    }
                }
            }
        }
    }
}
