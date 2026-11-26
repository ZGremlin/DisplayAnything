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

    pub fn hotkeys(&self) -> Hotkeys {
        self.controller.hotkeys()
    }

    pub fn set_hotkeys(&self, hotkeys: Hotkeys) {
        self.controller.set_hotkeys(hotkeys);
    }

    /// Record the next combination held for two seconds; progress arrives
    /// as [`Event::HotkeyRecording`].
    pub fn start_recording(&self) {
        self.controller.start_recording();
    }

    pub fn cancel_recording(&self) {
        self.controller.cancel_recording();
    }

    /// Which side of this computer the peer sits on, for moving over by
    /// pushing the pointer past that edge (None turns it off).
    pub fn set_edge(&self, edge: Option<da_input::Edge>) {
        self.controller.set_edge(edge);
    }

    pub fn edge_locked(&self) -> bool {
        self.controller.edge_locked()
    }

    pub fn set_edge_locked(&self, locked: bool) {
        self.controller.set_edge_locked(locked);
    }

    /// Remote control of another screen started (Some: where keys and
    /// pointer positions go) or ended (None). Sharing is paused meanwhile.
    pub fn set_remote_control(
        &self,
        tx: Option<tokio::sync::mpsc::UnboundedSender<da_input::GrabEvent>>,
    ) {
        self.controller.set_remote_control(tx);
    }

    pub fn remote_control(&self) -> bool {
        self.controller.remote_control()
    }

    /// The remote window gained or lost focus.
    pub fn set_grabbing(&self, on: bool) {
        self.controller.set_grabbing(on);
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
            // Someone controls this screen: shared input waits until they stop.
            if self.node.being_viewed() {
                if self.link.lock().unwrap().is_some() {
                    self.drop_link();
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
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
                            let controller = self.controller.clone();
                            link.on_leave(move |pos| controller.return_over_edge(pos));
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
                ControllerMsg::Recording(r) => self.node.emit(Event::HotkeyRecording(r)),
                ControllerMsg::EdgeLocked(l) => self.node.emit(Event::EdgeLocked(l)),
                ControllerMsg::Enter { edge, pos, warp } => {
                    if let Some(l) = self.current_link() {
                        l.send(Frame::Enter { edge, pos, warp });
                    }
                }
                ControllerMsg::Switched(Target::Remote) => {
                    da_input::clipboard::begin_switch("to the other computer");
                    let link = self.current_link();
                    if let Some(l) = &link {
                        l.send(Frame::Scale(da_input::inject::pointer_scale_percent()));
                        l.send(Frame::Active(true));
                    }
                    if let (Some(l), Some(clip)) = (&link, da_input::clipboard::get()) {
                        // Only send what changed since we last synced, so an
                        // old copy here does not overwrite the other machine's
                        // newer copy.
                        if !clip.is_empty() && !da_input::clipboard::is_synced(&clip) {
                            tracing::info!(
                                "switch {}: sent {} hash={:016x}",
                                da_input::clipboard::current_switch(),
                                da_input::clipboard::describe(&clip),
                                da_input::clipboard::fingerprint(&clip)
                            );
                            da_input::clipboard::mark_synced(&clip);
                            l.send(clip.into_frame());
                        } else {
                            tracing::info!(
                                "switch {}: not sent, already synced hash={:016x}",
                                da_input::clipboard::current_switch(),
                                da_input::clipboard::fingerprint(&clip)
                            );
                        }
                    }
                    let peer = link.map(|l| l.peer.name.clone());
                    self.node.emit(Event::InputTargetChanged { peer });
                }
                ControllerMsg::Switched(Target::Local) => {
                    da_input::clipboard::begin_switch("back to this computer");
                    self.node.emit(Event::InputTargetChanged { peer: None });
                    if let Some(l) = self.current_link() {
                        l.send(Frame::Active(false));
                        tokio::spawn(async move {
                            if let Some(clip) = l.request_clipboard(CLIPBOARD_WAIT).await {
                                tracing::info!(
                                    "switch {}: reply received {} hash={:016x}",
                                    da_input::clipboard::current_switch(),
                                    da_input::clipboard::describe(&clip),
                                    da_input::clipboard::fingerprint(&clip)
                                );
                                if !clip.is_empty() && !da_input::clipboard::is_synced(&clip) {
                                    da_input::clipboard::set(&clip);
                                }
                            }
                        });
                    }
                }
            }
        }
    }
}
