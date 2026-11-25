//! The input channel: a dedicated connection per controller->target pair
//! that carries keyboard and mouse events.
//!
//! It is its own TLS connection (separate from the control session) with
//! Nagle off and a high-priority DSCP mark, and each event is written and
//! flushed the moment it is captured. On a LAN that adds well under a
//! millisecond. The payload is the compact binary [`Frame`] format.

use crate::identity::Fingerprint;
use crate::net::{send_err, Event, Node};
use crate::protocol::{read_msg, write_msg, ErrorCode, Intent, Message};
use crate::trust::TrustedPeer;
use crate::PROTOCOL_VERSION;
use anyhow::{anyhow, bail, Result};
use da_input::{Frame, InjectorOptions, InputEvent};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;
use tokio_rustls::TlsStream;

/// Nagle off and DSCP "expedited forwarding" (46), so switches and Wi-Fi
/// access points that honour it send our packets first.
pub(crate) fn tune_socket(tcp: &TcpStream) {
    let _ = tcp.set_nodelay(true);
    let _ = socket2::SockRef::from(tcp).set_tos(46 << 2);
}

/// Receives input sent to this machine. Implemented by [`PlatformInputTarget`]
/// for real use and by test doubles.
pub trait InputTarget: Send + Sync {
    /// A peer opened an input channel. Frames sent on `back` go to that peer
    /// (for example when the pointer is pushed back out over the edge).
    fn begin(&self, _peer: &TrustedPeer, _back: UnboundedSender<Frame>) {}
    /// One frame from the peer. A returned frame is sent back (clipboard).
    fn frame(&self, peer: &TrustedPeer, frame: Frame) -> Option<Frame>;
    /// The channel closed: release anything still held.
    fn end(&self, peer: &TrustedPeer);
    /// Remote control: move the pointer to this global position (pixels on
    /// Windows, points on macOS). Held buttons make it a drag.
    fn pointer_at(&self, _peer: &TrustedPeer, _x: f64, _y: f64) {}
}

impl Node {
    pub(crate) async fn serve_input(
        &self,
        mut s: TlsStream<TcpStream>,
        fp: Fingerprint,
        addr: SocketAddr,
    ) -> Result<()> {
        let Some(peer) = self.trust().get(&fp).cloned() else {
            return send_err(
                &mut s,
                ErrorCode::NotPaired,
                "this device is not paired with you",
            )
            .await;
        };
        if !peer.permissions.input_takeover {
            return send_err(
                &mut s,
                ErrorCode::NotAllowed,
                "this device does not allow you to use its keyboard and mouse",
            )
            .await;
        }
        if self.being_viewed() {
            return send_err(
                &mut s,
                ErrorCode::NotAllowed,
                "this device's screen is being controlled; shared keyboard and mouse is paused",
            )
            .await;
        }
        let Some(target) = self.input_target() else {
            return send_err(
                &mut s,
                ErrorCode::NotAllowed,
                "this device cannot receive keyboard and mouse input",
            )
            .await;
        };
        write_msg(
            &mut s,
            &Message::Welcome {
                name: self.name(),
                granted: peer.permissions,
            },
        )
        .await?;
        let (back_tx, mut back_rx) = unbounded_channel::<Frame>();
        target.begin(&peer, back_tx);
        self.emit(Event::InputReceiving {
            peer: peer.name.clone(),
            active: true,
        });
        tracing::info!(peer = %peer.name, %addr, "input channel open");

        let result = async {
            let mut buf: Vec<u8> = Vec::with_capacity(4096);
            let mut chunk = [0u8; 4096];
            let mut reply = Vec::new();
            loop {
                let n = tokio::select! {
                    n = s.read(&mut chunk) => n?,
                    Some(f) = back_rx.recv() => {
                        reply.clear();
                        f.encode(&mut reply);
                        s.write_all(&reply).await?;
                        s.flush().await?;
                        continue;
                    }
                };
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&chunk[..n]);
                let mut used = 0;
                while let Some((frame, len)) = Frame::decode(&buf[used..])? {
                    used += len;
                    if let Frame::Active(active) = frame {
                        self.emit(Event::InputControlled {
                            peer: peer.name.clone(),
                            active,
                        });
                        continue;
                    }
                    if self.being_viewed() {
                        continue; // remote control has the keyboard and mouse
                    }
                    if let Some(r) = target.frame(&peer, frame) {
                        reply.clear();
                        r.encode(&mut reply);
                        s.write_all(&reply).await?;
                        s.flush().await?;
                    }
                }
                buf.drain(..used);
            }
        }
        .await;
        target.end(&peer);
        self.emit(Event::InputControlled {
            peer: peer.name.clone(),
            active: false,
        });
        self.emit(Event::InputReceiving {
            peer: peer.name.clone(),
            active: false,
        });
        result
    }

    /// Open an input channel to a paired peer that allows it.
    pub async fn open_input(&self, addr: SocketAddr) -> Result<InputLink> {
        let (mut s, fp, _) = self.dial(addr).await?;
        let peer = self
            .trust()
            .get(&fp)
            .cloned()
            .ok_or_else(|| anyhow!("the device at {addr} is not paired with this one"))?;
        write_msg(
            &mut s,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                name: self.name(),
                intent: Intent::Input,
            },
        )
        .await?;
        match timeout(Duration::from_secs(10), read_msg(&mut s)).await?? {
            Message::Welcome { .. } => {}
            Message::Error { message, .. } => bail!("{message}"),
            other => bail!("unexpected reply {other:?}"),
        }
        Ok(InputLink::spawn(s, peer))
    }
}

/// Sending half of an input channel (controller side).
pub struct InputLink {
    pub peer: TrustedPeer,
    tx: UnboundedSender<Frame>,
    clipboard: tokio::sync::Mutex<UnboundedReceiver<String>>,
    closed: Arc<AtomicBool>,
    on_leave: LeaveHandler,
}

type LeaveHandler = Arc<std::sync::Mutex<Option<Box<dyn Fn(u16) + Send + Sync>>>>;

impl InputLink {
    fn spawn(s: TlsStream<TcpStream>, peer: TrustedPeer) -> Self {
        let (mut rd, mut wr) = tokio::io::split(s);
        let (tx, mut rx) = unbounded_channel::<Frame>();
        let (clip_tx, clip_rx) = unbounded_channel::<String>();
        let closed = Arc::new(AtomicBool::new(false));

        let closed_w = closed.clone();
        tokio::spawn(async move {
            let mut buf = Vec::with_capacity(64);
            while let Some(f) = rx.recv().await {
                buf.clear();
                f.encode(&mut buf);
                // Drain whatever else is already queued into the same write.
                while let Ok(more) = rx.try_recv() {
                    more.encode(&mut buf);
                }
                if wr.write_all(&buf).await.is_err() || wr.flush().await.is_err() {
                    break;
                }
            }
            closed_w.store(true, Ordering::SeqCst);
            let _ = wr.shutdown().await;
        });

        let on_leave: LeaveHandler = Arc::new(std::sync::Mutex::new(None));
        let on_leave_r = on_leave.clone();
        let closed_r = closed.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match rd.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                let mut used = 0;
                loop {
                    match Frame::decode(&buf[used..]) {
                        Ok(Some((Frame::ClipboardText(t), len))) => {
                            used += len;
                            let _ = clip_tx.send(t);
                        }
                        Ok(Some((Frame::Leave { pos, .. }, len))) => {
                            used += len;
                            if let Some(f) = on_leave_r.lock().unwrap().as_ref() {
                                f(pos);
                            }
                        }
                        Ok(Some((_, len))) => used += len,
                        Ok(None) => break,
                        Err(_) => {
                            closed_r.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                }
                buf.drain(..used);
            }
            closed_r.store(true, Ordering::SeqCst);
        });

        InputLink {
            peer,
            tx,
            clipboard: tokio::sync::Mutex::new(clip_rx),
            closed,
            on_leave,
        }
    }

    /// Called when the peer pushes the pointer back out over its edge, with
    /// the position along that edge.
    pub fn on_leave(&self, f: impl Fn(u16) + Send + Sync + 'static) {
        *self.on_leave.lock().unwrap() = Some(Box::new(f));
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.tx.is_closed()
    }

    /// Queue a frame; it is written immediately. Returns false if closed.
    pub fn send(&self, f: Frame) -> bool {
        !self.is_closed() && self.tx.send(f).is_ok()
    }

    pub fn send_event(&self, e: InputEvent) -> bool {
        self.send(Frame::Input(e))
    }

    /// Ask the peer for its clipboard text.
    pub async fn request_clipboard(&self, wait: Duration) -> Option<String> {
        let mut rx = self.clipboard.lock().await;
        while rx.try_recv().is_ok() {} // drop stale replies
        if !self.send(Frame::ClipboardRequest) {
            return None;
        }
        timeout(wait, rx.recv()).await.ok().flatten()
    }
}

impl Drop for InputLink {
    fn drop(&mut self) {
        let _ = self.tx.send(Frame::Input(InputEvent::ReleaseAll));
    }
}

/// Replays received input on this machine with the OS injector.
///
/// The injector lives on its own thread (macOS event sources must stay on
/// one thread); frames reach it through a channel.
pub struct PlatformInputTarget {
    tx: std::sync::mpsc::Sender<Cmd>,
}

enum Cmd {
    Event(InputEvent),
    SetClipboard(String),
    Enter {
        edge: da_input::Edge,
        pos: u16,
        warp: bool,
    },
    /// A new channel: send frames for the controller here.
    Begin(UnboundedSender<Frame>),
    Options(InjectorOptions),
    Scale(u16),
    MoveTo(f64, f64),
    End,
}

impl PlatformInputTarget {
    pub fn new(opts: InjectorOptions) -> Result<Arc<Self>> {
        let (tx, rx) = std::sync::mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        std::thread::Builder::new()
            .name("da-input-inject".into())
            .spawn(move || {
                let mut injector = match da_input::Injector::new(opts) {
                    Ok(i) => {
                        let _ = ready_tx.send(Ok(()));
                        i
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let mut back: Option<UnboundedSender<Frame>> = None;
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        Cmd::Event(e) => {
                            if let Some(f) = injector.inject(&e) {
                                tracing::info!(?f, "pointer pushed back over the edge");
                                if let Some(b) = &back {
                                    let _ = b.send(f);
                                }
                            }
                        }
                        Cmd::SetClipboard(t) => da_input::clipboard::set_text(&t),
                        Cmd::Enter { edge, pos, warp } => injector.enter(edge, pos, warp),
                        Cmd::Begin(b) => back = Some(b),
                        Cmd::Options(o) => injector.set_options(o),
                        Cmd::Scale(p) => injector.set_sender_scale(p),
                        Cmd::MoveTo(x, y) => injector.move_to(x, y),
                        Cmd::End => {
                            injector.release_all();
                            injector.reset_edge();
                            back = None;
                        }
                    }
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("input thread exited"))??;
        Ok(Arc::new(PlatformInputTarget { tx }))
    }

    /// Change how received input is replayed (e.g. the Ctrl/Command swap).
    pub fn set_options(&self, opts: InjectorOptions) {
        let _ = self.tx.send(Cmd::Options(opts));
    }
}

impl InputTarget for PlatformInputTarget {
    fn begin(&self, _peer: &TrustedPeer, back: UnboundedSender<Frame>) {
        let _ = self.tx.send(Cmd::Begin(back));
    }

    fn frame(&self, _peer: &TrustedPeer, frame: Frame) -> Option<Frame> {
        match frame {
            Frame::Enter { edge, pos, warp } => {
                let _ = self.tx.send(Cmd::Enter { edge, pos, warp });
                None
            }
            Frame::Leave { .. } => None, // only travels the other way
            Frame::Active(_) => None,    // handled by the channel itself
            Frame::Scale(p) => {
                let _ = self.tx.send(Cmd::Scale(p));
                None
            }
            Frame::Input(e) => {
                let _ = self.tx.send(Cmd::Event(e));
                None
            }
            Frame::ClipboardText(t) => {
                let _ = self.tx.send(Cmd::SetClipboard(t));
                None
            }
            Frame::ClipboardRequest => Some(Frame::ClipboardText(
                da_input::clipboard::get_text().unwrap_or_default(),
            )),
        }
    }

    fn end(&self, _peer: &TrustedPeer) {
        let _ = self.tx.send(Cmd::End);
    }

    fn pointer_at(&self, _peer: &TrustedPeer, x: f64, y: f64) {
        let _ = self.tx.send(Cmd::MoveTo(x, y));
    }
}
