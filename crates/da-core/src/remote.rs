//! Remote screen control: the screen stream between a viewer and a host.
//!
//! The viewer opens its own TLS connection (intent `remote`) to a paired
//! host that allows remote control. The host captures and encodes its
//! screen ([`da_screen::ScreenHost`]) and streams frames as
//! [`ViewMsg`]s; the viewer asks for keyframes when its decoder needs one.
//! Like everything else, this is mutual TLS with pinned fingerprints, by IP,
//! LAN only.

use crate::identity::Fingerprint;
use crate::net::{send_err, Event, Node};
use crate::protocol::{read_msg, write_msg, ErrorCode, Intent, Message};
use crate::trust::TrustedPeer;
use crate::PROTOCOL_VERSION;
use anyhow::{anyhow, bail, Context, Result};
use da_input::{Frame, InputEvent};
use da_screen::wire::StartRequest;
use da_screen::{DisplayBounds, HostOptions, ScreenHost, StreamEvent, ViewMsg};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;
use tokio_rustls::TlsStream;

/// A running capture as the host sees it: frames arrive through the sink
/// it was started with, and dropping it stops capture.
pub trait ScreenSource: Send + Sync {
    fn request_keyframe(&self);
    /// Where the captured monitor sits, for placing the pointer.
    fn bounds(&self) -> Option<DisplayBounds> {
        None
    }
}

impl ScreenSource for ScreenHost {
    fn request_keyframe(&self) {
        ScreenHost::request_keyframe(self);
    }

    fn bounds(&self) -> Option<DisplayBounds> {
        ScreenHost::bounds(self)
    }
}

/// Counts running screen sessions on the host; while any run, shared
/// keyboard and mouse is paused (see [`Node::being_viewed`]).
struct ViewGuard<'a>(&'a Node);

impl Drop for ViewGuard<'_> {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Starts a [`ScreenSource`]; see [`Node::set_screen_starter`].
pub type ScreenStarter =
    Arc<dyn Fn(HostOptions, da_screen::host::Sink) -> Result<Box<dyn ScreenSource>> + Send + Sync>;

/// If more frames than this are waiting to go out, the network can't keep
/// up: drop to the next keyframe instead of building a delay.
const MAX_QUEUED_FRAMES: usize = 3;

/// Stands in while switching monitors.
struct NoScreen;

impl ScreenSource for NoScreen {
    fn request_keyframe(&self) {}
}

/// Start capture off the async threads (macOS waits for the system).
async fn start_capture(
    starter: Option<ScreenStarter>,
    opts: HostOptions,
    ev_tx: UnboundedSender<StreamEvent>,
) -> Result<Box<dyn ScreenSource>> {
    tokio::task::spawn_blocking(move || {
        let sink: da_screen::host::Sink = Box::new(move |e| {
            let _ = ev_tx.send(e);
        });
        match starter {
            Some(start) => start(opts, sink),
            None => ScreenHost::start(opts, sink).map(|h| Box::new(h) as Box<dyn ScreenSource>),
        }
    })
    .await?
}

/// Read one [`ViewMsg`] at a time from a stream.
struct MsgReader<R> {
    r: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> MsgReader<R> {
    fn new(r: R) -> Self {
        MsgReader {
            r,
            buf: Vec::with_capacity(64 * 1024),
        }
    }

    /// The next message, or None when the other side closed.
    async fn next(&mut self) -> Result<Option<ViewMsg>> {
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            if let Some((msg, used)) = ViewMsg::decode(&self.buf)? {
                self.buf.drain(..used);
                return Ok(Some(msg));
            }
            let n = self.r.read(&mut chunk).await?;
            if n == 0 {
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

impl Node {
    /// Host side: a viewer asked to see (and later control) this screen.
    pub(crate) async fn serve_remote(
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
        if !peer.permissions.remote_control {
            return send_err(
                &mut s,
                ErrorCode::NotAllowed,
                "this device does not allow you to control its screen \
                 (turn on \"May control this desktop\" for you on that device)",
            )
            .await;
        }
        let starter = self.screen_starter();
        if starter.is_none() && !da_screen::platform_supported() {
            return send_err(
                &mut s,
                ErrorCode::NotAllowed,
                "this device can't share its screen yet",
            )
            .await;
        }
        let wake_mac = self.own_wake_mac(s.get_ref().0.local_addr().ok());
        write_msg(
            &mut s,
            &Message::Welcome {
                name: self.name(),
                granted: peer.permissions,
                wake_mac,
            },
        )
        .await?;

        let (rd, mut wr) = tokio::io::split(s);
        let mut reader = MsgReader::new(rd);
        let req = match timeout(Duration::from_secs(10), reader.next()).await?? {
            Some(ViewMsg::Start(r)) => r,
            Some(other) => bail!("expected a start request, got {other:?}"),
            None => return Ok(()),
        };

        let (ev_tx, mut ev_rx) = unbounded_channel::<StreamEvent>();
        let mut opts = HostOptions::from(&req);
        let host = start_capture(starter.clone(), opts.clone(), ev_tx.clone()).await;
        let mut host = match host {
            Ok(h) => h,
            Err(e) => {
                let mut out = Vec::new();
                ViewMsg::Error(format!("{e:#}")).encode(&mut out);
                let _ = wr.write_all(&out).await;
                let _ = wr.flush().await;
                return Err(e);
            }
        };
        tracing::info!(peer = %peer.name, %addr, "sharing this screen");
        self.viewers.fetch_add(1, Ordering::SeqCst);
        let _guard = ViewGuard(self);
        self.emit(Event::RemoteViewed {
            peer: peer.name.clone(),
            active: true,
        });
        // Keyboard and mouse from the viewer go through the same injector
        // as shared input.
        let target = self.input_target();
        if let Some(t) = &target {
            let (back, _) = unbounded_channel::<Frame>();
            t.begin(&peer, back);
        }
        let mut bounds = host.bounds();

        let result: Result<()> = async {
            let mut out = Vec::with_capacity(256 * 1024);
            let mut skipping = false;
            loop {
                tokio::select! {
                    msg = reader.next() => match msg? {
                        Some(ViewMsg::KeyframeRequest) => host.request_keyframe(),
                        Some(ViewMsg::Stop) | None => return Ok(()),
                        Some(ViewMsg::Input(bytes)) => {
                            let Some(t) = &target else { continue };
                            let mut used = 0;
                            while let Some((frame, len)) = Frame::decode(&bytes[used..])? {
                                used += len;
                                // Only plain input: no edges or clipboard here.
                                if let Frame::Input(e) = frame {
                                    t.frame(&peer, Frame::Input(e));
                                }
                            }
                        }
                        Some(msg @ (ViewMsg::SelectDisplay(_) | ViewMsg::Configure(_))) => {
                            let next = match &msg {
                                ViewMsg::SelectDisplay(d) => HostOptions { display: *d, ..opts.clone() },
                                ViewMsg::Configure(c) => opts.with_settings(c),
                                _ => unreachable!(),
                            };
                            if next == opts {
                                continue;
                            }
                            // Stop the old capture, throw away what it
                            // already sent, then start with the new options.
                            let old = std::mem::replace(&mut host, Box::new(NoScreen));
                            let _ = tokio::task::spawn_blocking(move || drop(old)).await;
                            while ev_rx.try_recv().is_ok() {}
                            skipping = false;
                            host = match start_capture(starter.clone(), next.clone(), ev_tx.clone()).await {
                                Ok(h) => {
                                    opts = next;
                                    h
                                }
                                Err(e) => {
                                    tracing::warn!("could not apply {msg:?}: {e:#}");
                                    start_capture(starter.clone(), opts.clone(), ev_tx.clone()).await?
                                }
                            };
                            bounds = host.bounds();
                            tracing::info!(?opts, "screen stream options changed");
                        }
                        Some(ViewMsg::Pointer { x, y }) => {
                            if let (Some(t), Some(b)) = (&target, bounds) {
                                let (gx, gy) = b.point(x, y);
                                t.pointer_at(&peer, gx, gy);
                            }
                        }
                        Some(other) => tracing::debug!("ignoring {other:?} from viewer"),
                    },
                    ev = ev_rx.recv() => {
                        let Some(ev) = ev else { return Ok(()) };
                        let msg = match ev {
                            StreamEvent::Info(i) => ViewMsg::Info(i),
                            StreamEvent::Error(e) => {
                                out.clear();
                                ViewMsg::Error(e.clone()).encode(&mut out);
                                let _ = wr.write_all(&out).await;
                                let _ = wr.flush().await;
                                bail!("screen capture stopped: {e}");
                            }
                            StreamEvent::Frame(f) => {
                                // Behind? Drop frames until a fresh keyframe.
                                if ev_rx.len() >= MAX_QUEUED_FRAMES && !skipping {
                                    skipping = true;
                                    host.request_keyframe();
                                }
                                if skipping {
                                    if !f.keyframe {
                                        continue;
                                    }
                                    skipping = false;
                                }
                                ViewMsg::Video(f)
                            }
                        };
                        out.clear();
                        msg.encode(&mut out);
                        wr.write_all(&out).await?;
                        wr.flush().await?;
                    }
                }
            }
        }
        .await;

        if let Some(t) = &target {
            t.end(&peer);
        }
        // Stopping capture can block briefly; keep it off the async threads.
        let _ = tokio::task::spawn_blocking(move || drop(host)).await;
        self.emit(Event::RemoteViewed {
            peer: peer.name.clone(),
            active: false,
        });
        tracing::info!(peer = %peer.name, "stopped sharing this screen");
        result
    }

    /// Viewer side: open the screen of a paired peer that allows it.
    pub async fn open_remote(&self, addr: SocketAddr, req: StartRequest) -> Result<RemoteView> {
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
                intent: Intent::Remote,
                device_kind: self.device_kind(),
            },
        )
        .await?;
        match timeout(Duration::from_secs(10), read_msg(&mut s))
            .await
            .context("no answer from the other device")??
        {
            Message::Welcome { wake_mac, .. } => {
                // Remember how to wake this desktop; a phone only ever
                // connects this way, so this is where it learns it.
                if let Err(e) = self.trust().note_wake_mac(&fp, wake_mac.as_deref()) {
                    tracing::warn!("could not save the wake address: {e:#}");
                }
            }
            Message::Error { message, .. } => bail!("{message}"),
            other => bail!("unexpected reply {other:?}"),
        }
        let (rd, mut wr) = tokio::io::split(s);
        let mut first = Vec::new();
        ViewMsg::Start(req).encode(&mut first);
        wr.write_all(&first).await?;
        wr.flush().await?;

        let (in_tx, in_rx) = unbounded_channel::<ViewMsg>();
        let (out_tx, mut out_rx) = unbounded_channel::<ViewMsg>();
        tokio::spawn(async move {
            let mut reader = MsgReader::new(rd);
            loop {
                match reader.next().await {
                    Ok(Some(m)) => {
                        if in_tx.send(m).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!("screen stream ended: {e:#}");
                        break;
                    }
                }
            }
        });
        tokio::spawn(async move {
            let mut buf = Vec::new();
            while let Some(m) = out_rx.recv().await {
                buf.clear();
                m.encode(&mut buf);
                if wr.write_all(&buf).await.is_err() || wr.flush().await.is_err() {
                    break;
                }
                if m == ViewMsg::Stop {
                    break;
                }
            }
            let _ = wr.shutdown().await;
        });
        Ok(RemoteView {
            peer,
            rx: in_rx,
            tx: out_tx,
        })
    }
}

/// An open view of another computer's screen (viewer side).
pub struct RemoteView {
    pub peer: TrustedPeer,
    rx: UnboundedReceiver<ViewMsg>,
    tx: UnboundedSender<ViewMsg>,
}

impl RemoteView {
    /// The next message from the host (stream info, frames, errors), or
    /// None once the connection closed.
    pub async fn next(&mut self) -> Option<ViewMsg> {
        self.rx.recv().await
    }

    /// A handle for asking for keyframes or stopping from elsewhere.
    pub fn control(&self) -> RemoteControl {
        RemoteControl {
            tx: self.tx.clone(),
        }
    }
}

impl Drop for RemoteView {
    fn drop(&mut self) {
        let _ = self.tx.send(ViewMsg::Stop);
    }
}

#[derive(Clone)]
pub struct RemoteControl {
    tx: UnboundedSender<ViewMsg>,
}

impl RemoteControl {
    pub fn request_keyframe(&self) {
        let _ = self.tx.send(ViewMsg::KeyframeRequest);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(ViewMsg::Stop);
    }

    /// Send a key, button, wheel or release-all to the host.
    pub fn send_input(&self, e: InputEvent) {
        let mut b = Vec::with_capacity(8);
        Frame::Input(e).encode(&mut b);
        let _ = self.tx.send(ViewMsg::Input(b));
    }

    /// Change the picture size or quality.
    pub fn configure(&self, settings: da_screen::wire::StreamSettings) {
        let _ = self.tx.send(ViewMsg::Configure(settings));
    }

    /// Show another of the host's monitors (0 = main).
    pub fn select_display(&self, display: u32) {
        let _ = self.tx.send(ViewMsg::SelectDisplay(display));
    }

    /// Put the host's pointer at a fraction of its screen (0..=65535).
    pub fn pointer(&self, x: u16, y: u16) {
        let _ = self.tx.send(ViewMsg::Pointer { x, y });
    }
}
