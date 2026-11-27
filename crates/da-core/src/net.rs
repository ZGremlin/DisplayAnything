//! Node: listens for peers, pairs with them, and opens authenticated sessions.

use crate::identity::{Fingerprint, Identity};
use crate::pairing::{self, Binding, GuardDenied, PairGuard, PakeState, Role};
use crate::protocol::{read_msg, write_msg, DeviceKind, ErrorCode, Intent, Message};
use crate::tls;
use crate::trust::{Permissions, TrustStore, TrustedPeer};
use crate::PROTOCOL_VERSION;
use anyhow::{anyhow, bail, Context, Result};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const TLS_SERVER_NAME: &str = "displayanything.local";

#[derive(Clone, Debug)]
pub struct Config {
    /// Accept connections from non-private addresses. Off by default so a
    /// router port-forward cannot expose the agent to the internet.
    pub allow_public_addresses: bool,
    /// How long a pairing code stays valid.
    pub pair_code_ttl: Duration,
    /// What this device is when it dials out (a phone sets `IosViewer`).
    pub device_kind: DeviceKind,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            allow_public_addresses: false,
            pair_code_ttl: pairing::CODE_TTL,
            device_kind: DeviceKind::Desktop,
        }
    }
}

/// Things the UI should show or log.
#[derive(Clone, Debug)]
pub enum Event {
    Listening {
        addr: SocketAddr,
    },
    /// Show this code to the person at this machine.
    PairCodeShown {
        peer_name: String,
        peer_addr: SocketAddr,
        code: String,
        expires_in: Duration,
    },
    Paired {
        name: String,
        fingerprint: Fingerprint,
        addr: SocketAddr,
    },
    PairFailed {
        peer_addr: SocketAddr,
        reason: String,
    },
    SessionOpened {
        name: String,
        fingerprint: Fingerprint,
        addr: SocketAddr,
    },
    SessionClosed {
        name: String,
        addr: SocketAddr,
    },
    Rejected {
        addr: SocketAddr,
        reason: String,
    },
    /// A saved peer is reachable and we hold a session with it.
    PeerOnline {
        name: String,
        fingerprint: Fingerprint,
        addr: SocketAddr,
    },
    /// A saved peer we were connected to went away; retrying in the background.
    PeerOffline {
        name: String,
        fingerprint: Fingerprint,
    },
    /// Our keyboard and mouse now drive `peer` (Some) or this computer (None).
    InputTargetChanged {
        peer: Option<String>,
    },
    /// The input link to the peer we share our keyboard and mouse with.
    InputLink {
        peer: String,
        connected: bool,
    },
    /// A peer started or stopped sending input to this machine.
    InputReceiving {
        peer: String,
        active: bool,
    },
    /// A connected peer started (true) or stopped (false) actually
    /// controlling this machine with its keyboard and mouse.
    InputControlled {
        peer: String,
        active: bool,
    },
    /// Edge crossing was paused (true) or resumed (false).
    EdgeLocked(bool),
    /// Progress while recording a new hotkey combination.
    HotkeyRecording(da_input::Recording),
    /// A saved peer's displayed name changed (it renamed itself, or the
    /// user renamed it here).
    PeerRenamed {
        fingerprint: Fingerprint,
        name: String,
    },
    /// A peer started (true) or stopped (false) viewing this screen with
    /// remote control.
    RemoteViewed {
        peer: String,
        active: bool,
    },
}

/// Is this address on a local network (private, loopback or link-local)?
pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local(),
        IpAddr::V6(v) => {
            let s0 = v.segments()[0];
            v.is_loopback()
                || (s0 & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (s0 & 0xffc0) == 0xfe80 // link-local fe80::/10
                || v.to_ipv4_mapped().is_some_and(|m| is_lan(IpAddr::V4(m)))
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct Node {
    identity: Identity,
    /// The name other devices see. Starts as the identity's name and can be
    /// changed at runtime with [`Node::set_name`].
    name: Arc<Mutex<String>>,
    trust: Mutex<TrustStore>,
    guard: Mutex<PairGuard>,
    config: Config,
    events: broadcast::Sender<Event>,
    acceptor: TlsAcceptor,
    connector: TlsConnector,
    input_target: Mutex<Option<Arc<dyn crate::input::InputTarget>>>,
    screen_starter: Mutex<Option<crate::remote::ScreenStarter>>,
    audio_starter: Mutex<Option<crate::remote::AudioStarter>>,
    /// Screen sessions this machine is serving right now.
    pub(crate) viewers: std::sync::atomic::AtomicUsize,
}

impl Node {
    pub fn new(identity: Identity, trust: TrustStore, config: Config) -> Result<Arc<Self>> {
        let acceptor = TlsAcceptor::from(tls::server_config(&identity)?);
        let connector = TlsConnector::from(tls::client_config(&identity)?);
        let (events, _) = broadcast::channel(64);
        Ok(Arc::new(Node {
            name: Arc::new(Mutex::new(identity.name.clone())),
            identity,
            trust: Mutex::new(trust),
            guard: Mutex::new(PairGuard::default()),
            config,
            events,
            acceptor,
            connector,
            input_target: Mutex::new(None),
            screen_starter: Mutex::new(None),
            audio_starter: Mutex::new(None),
            viewers: Default::default(),
        }))
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// What this device says it is when it dials another device.
    pub fn device_kind(&self) -> DeviceKind {
        self.config.device_kind
    }

    /// The name other devices currently see for this one.
    pub fn name(&self) -> String {
        self.name.lock().unwrap().clone()
    }

    /// Change the name other devices see. Connected peers pick it up with
    /// the next keepalive; the caller is responsible for saving it and for
    /// refreshing the mDNS advertisement.
    pub fn set_name(&self, name: &str) {
        *self.name.lock().unwrap() = name.trim().to_string();
    }

    /// Record the name a peer announced for itself. Ignored when the user
    /// gave that peer a custom name here.
    pub(crate) fn note_peer_name(&self, fp: &Fingerprint, announced: &str) {
        match self.trust().note_announced_name(fp, announced) {
            Ok(true) => self.emit(Event::PeerRenamed {
                fingerprint: *fp,
                name: announced.trim().to_string(),
            }),
            Ok(false) => {}
            Err(e) => tracing::warn!("could not save peer name: {e:#}"),
        }
    }

    /// Give a saved peer a custom name on this device, or (with `None` or an
    /// empty name) go back to the name it announces for itself.
    pub fn rename_peer(&self, fp: &Fingerprint, name: Option<&str>) -> Result<bool> {
        let changed = self.trust().rename(fp, name)?;
        if let Some(p) = self.trust().get(fp).cloned() {
            self.emit(Event::PeerRenamed {
                fingerprint: *fp,
                name: p.name,
            });
        }
        Ok(changed)
    }

    /// Replace how this machine's screen is captured for remote control
    /// (tests use a fake source). None uses the real screen.
    pub fn set_screen_starter(&self, starter: Option<crate::remote::ScreenStarter>) {
        *self.screen_starter.lock().unwrap() = starter;
    }

    /// Replace how this machine's sound is recorded for remote control
    /// (tests use a fake source). None uses the real sound output.
    pub fn set_audio_starter(&self, starter: Option<crate::remote::AudioStarter>) {
        *self.audio_starter.lock().unwrap() = starter;
    }

    pub(crate) fn audio_starter(&self) -> Option<crate::remote::AudioStarter> {
        self.audio_starter.lock().unwrap().clone()
    }

    /// Someone is viewing (and may be controlling) this screen. Shared
    /// keyboard and mouse is paused meanwhile, both ways.
    pub fn being_viewed(&self) -> bool {
        self.viewers.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    pub(crate) fn screen_starter(&self) -> Option<crate::remote::ScreenStarter> {
        self.screen_starter.lock().unwrap().clone()
    }

    /// Let paired peers (with the input permission) send keyboard and mouse
    /// input to this machine. Without a target, input requests are refused.
    pub fn set_input_target(&self, target: Option<Arc<dyn crate::input::InputTarget>>) {
        *self.input_target.lock().unwrap() = target;
    }

    pub(crate) fn input_target(&self) -> Option<Arc<dyn crate::input::InputTarget>> {
        self.input_target.lock().unwrap().clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn trust(&self) -> MutexGuard<'_, TrustStore> {
        self.trust.lock().expect("trust store lock poisoned")
    }

    pub(crate) fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    // ------------------------------------------------------------------
    // Responder side
    // ------------------------------------------------------------------

    /// Accept connections until the listener fails.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        if let Ok(addr) = listener.local_addr() {
            self.emit(Event::Listening { addr });
        }
        loop {
            let (tcp, addr) = listener.accept().await?;
            let node = self.clone();
            tokio::spawn(async move {
                if let Err(e) = node.handle_incoming(tcp, addr).await {
                    tracing::debug!(%addr, "connection ended: {e:#}");
                }
            });
        }
    }

    async fn handle_incoming(self: Arc<Self>, tcp: TcpStream, addr: SocketAddr) -> Result<()> {
        if !self.config.allow_public_addresses && !is_lan(addr.ip()) {
            self.emit(Event::Rejected {
                addr,
                reason: "not a local-network address".into(),
            });
            return Ok(());
        }
        crate::input::tune_socket(&tcp);
        let mut tls = timeout(HANDSHAKE_TIMEOUT, self.acceptor.accept(tcp))
            .await
            .context("TLS handshake timed out")??;
        let peer_fp = tls::peer_fingerprint(tls.get_ref().1).context("peer sent no certificate")?;
        let exporter = export_server(tls.get_ref().1)?;

        let hello = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut tls))
            .await
            .context("no hello")??;
        let Message::Hello {
            version,
            name,
            intent,
            device_kind,
        } = hello
        else {
            return send_err(&mut tls, ErrorCode::Protocol, "expected hello").await;
        };
        if version != PROTOCOL_VERSION {
            return send_err(
                &mut tls,
                ErrorCode::VersionMismatch,
                &format!("this device speaks v{PROTOCOL_VERSION}"),
            )
            .await;
        }
        let mut stream = TlsStream::from(tls);
        match intent {
            Intent::Connect => self.serve_session(&mut stream, peer_fp, addr, &name).await,
            Intent::Input => self.serve_input(stream, peer_fp, addr).await,
            Intent::Remote => self.serve_remote(stream, peer_fp, addr).await,
            Intent::Pair => {
                self.respond_pairing(&mut stream, name, device_kind, peer_fp, exporter, addr)
                    .await
            }
        }
    }

    async fn serve_session(
        &self,
        s: &mut TlsStream<TcpStream>,
        fp: Fingerprint,
        addr: SocketAddr,
        announced: &str,
    ) -> Result<()> {
        let peer = self.trust().get(&fp).cloned();
        let Some(peer) = peer else {
            self.emit(Event::Rejected {
                addr,
                reason: format!("unpaired device {}", fp.short()),
            });
            return send_err(
                s,
                ErrorCode::NotPaired,
                "this device is not paired with you",
            )
            .await;
        };
        self.trust()
            .note_address(&fp, addr.ip().to_string(), None)?;
        self.note_peer_name(&fp, announced);
        write_msg(
            s,
            &Message::Welcome {
                name: self.name(),
                granted: peer.permissions,
                wake_mac: self.own_wake_mac(s.get_ref().0.local_addr().ok()),
            },
        )
        .await?;
        self.emit(Event::SessionOpened {
            name: peer.name.clone(),
            fingerprint: fp,
            addr,
        });

        let result = async {
            loop {
                match timeout(IDLE_TIMEOUT, read_msg(s)).await {
                    Err(_) => bail!("session idle"),
                    Ok(Err(e)) => return Err(e),
                    Ok(Ok(Message::Ping { seq, name })) => {
                        if let Some(n) = name {
                            self.note_peer_name(&fp, &n);
                        }
                        let reply = Message::Pong {
                            seq,
                            name: Some(self.name()),
                        };
                        write_msg(s, &reply).await?
                    }
                    Ok(Ok(Message::Bye)) => return Ok(()),
                    Ok(Ok(other)) => tracing::debug!(?other, "ignoring message"),
                }
            }
        }
        .await;
        self.emit(Event::SessionClosed {
            name: peer.name,
            addr,
        });
        result
    }

    async fn respond_pairing(
        &self,
        s: &mut TlsStream<TcpStream>,
        peer_name: String,
        peer_kind: DeviceKind,
        peer_fp: Fingerprint,
        exporter: [u8; 32],
        addr: SocketAddr,
    ) -> Result<()> {
        let begun = self.guard.lock().unwrap().begin(Instant::now());
        match begun {
            Err(GuardDenied::Locked(left)) => {
                let msg = format!(
                    "pairing is locked for {} more seconds after failed attempts",
                    left.as_secs()
                );
                return send_err(s, ErrorCode::PairingLocked, &msg).await;
            }
            Err(GuardDenied::Busy) => {
                return send_err(s, ErrorCode::PairingBusy, "another pairing is in progress").await;
            }
            Ok(()) => {}
        }

        let code = pairing::new_code();
        let ttl = self.config.pair_code_ttl;
        self.emit(Event::PairCodeShown {
            peer_name: peer_name.clone(),
            peer_addr: addr,
            code: code.clone(),
            expires_in: ttl,
        });
        let outcome = self
            .run_responder(
                s, &code, ttl, &peer_name, peer_kind, peer_fp, exporter, addr,
            )
            .await;
        let mut guard = self.guard.lock().unwrap();
        match &outcome {
            PairOutcome::Ok => guard.succeeded(),
            PairOutcome::WrongCode => guard.failed(Instant::now()),
            PairOutcome::NoGuess(_) => guard.abandoned(),
        }
        drop(guard);
        match outcome {
            PairOutcome::Ok => Ok(()),
            PairOutcome::WrongCode => {
                self.emit(Event::PairFailed {
                    peer_addr: addr,
                    reason: "wrong code".into(),
                });
                Ok(())
            }
            PairOutcome::NoGuess(e) => {
                self.emit(Event::PairFailed {
                    peer_addr: addr,
                    reason: format!("{e:#}"),
                });
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_responder(
        &self,
        s: &mut TlsStream<TcpStream>,
        code: &str,
        ttl: Duration,
        peer_name: &str,
        peer_kind: DeviceKind,
        peer_fp: Fingerprint,
        exporter: [u8; 32],
        addr: SocketAddr,
    ) -> PairOutcome {
        if let Err(e) = write_msg(
            s,
            &Message::PairCodeShown {
                expires_in_secs: ttl.as_secs(),
            },
        )
        .await
        {
            return PairOutcome::NoGuess(e);
        }
        let their_spake = match timeout(ttl, read_msg(s)).await {
            Err(_) => {
                let _ = send_err(s, ErrorCode::PairingTimeout, "the pairing code expired").await;
                return PairOutcome::NoGuess(anyhow!("pairing code expired"));
            }
            Ok(Err(e)) => return PairOutcome::NoGuess(e),
            Ok(Ok(Message::PairSpake { msg })) => match hex::decode(msg) {
                Ok(m) => m,
                Err(_) => return PairOutcome::NoGuess(anyhow!("bad pairing message")),
            },
            Ok(Ok(_)) => return PairOutcome::NoGuess(anyhow!("unexpected message during pairing")),
        };
        // From here on the initiator has made its one guess.
        let (state, ours) = PakeState::start(Role::Responder, code);
        if write_msg(
            s,
            &Message::PairSpake {
                msg: hex::encode(ours),
            },
        )
        .await
        .is_err()
        {
            return PairOutcome::WrongCode;
        }
        let binding = Binding {
            tls_exporter: exporter,
            initiator_fp: peer_fp,
            responder_fp: self.identity.fingerprint(),
        };
        let Ok(confirmer) = state.finish(&their_spake, &binding) else {
            let _ = send_err(s, ErrorCode::PairingFailed, "pairing failed").await;
            return PairOutcome::WrongCode;
        };
        let their_mac = match timeout(CONFIRM_TIMEOUT, read_msg(s)).await {
            Ok(Ok(Message::PairConfirm { mac })) => hex::decode(mac).unwrap_or_default(),
            _ => return PairOutcome::WrongCode,
        };
        if !confirmer.verify_peer(&their_mac) {
            let _ = send_err(s, ErrorCode::PairingFailed, "wrong pairing code").await;
            return PairOutcome::WrongCode;
        }
        // A phone or tablet only views and controls this screen, so it
        // gets just the remote control permission (off until the person
        // turns it on) and is never dialled by this device.
        let (granted, auto_connect) = if peer_kind.is_desktop() {
            (Permissions::default(), true)
        } else {
            (
                Permissions {
                    remote_control: false,
                    display_takeover: false,
                    input_takeover: false,
                },
                false,
            )
        };
        let stored = self.trust().upsert(TrustedPeer {
            name: peer_name.to_string(),
            fingerprint: peer_fp.to_hex(),
            permissions: granted,
            paired_at_unix: now_unix(),
            last_addr: Some(addr.ip().to_string()),
            port: None,
            auto_connect,
            custom_name: false,
            device_kind: peer_kind,
            wake_mac: None,
        });
        if let Err(e) = stored {
            return PairOutcome::NoGuess(e.context("saving trusted peer"));
        }
        let sent = async {
            write_msg(
                s,
                &Message::PairConfirm {
                    mac: hex::encode(confirmer.own_mac()),
                },
            )
            .await?;
            write_msg(
                s,
                &Message::PairOk {
                    name: self.name(),
                    granted,
                    wake_mac: self.own_wake_mac(s.get_ref().0.local_addr().ok()),
                },
            )
            .await
        }
        .await;
        if let Err(e) = sent {
            // We already trust them; they will just need to retry.
            tracing::warn!("paired but could not confirm to peer: {e:#}");
        }
        self.emit(Event::Paired {
            name: peer_name.to_string(),
            fingerprint: peer_fp,
            addr,
        });
        PairOutcome::Ok
    }

    // ------------------------------------------------------------------
    // Initiator side
    // ------------------------------------------------------------------

    pub(crate) async fn dial(
        &self,
        addr: SocketAddr,
    ) -> Result<(TlsStream<TcpStream>, Fingerprint, [u8; 32])> {
        if !self.config.allow_public_addresses && !is_lan(addr.ip()) {
            bail!("{} is not a local-network address", addr.ip());
        }
        let tcp = timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(addr))
            .await
            .with_context(|| format!("timed out connecting to {addr}"))?
            .with_context(|| format!("could not reach {addr}"))?;
        crate::input::tune_socket(&tcp);
        let name = rustls::pki_types::ServerName::try_from(TLS_SERVER_NAME)?;
        let tls = timeout(HANDSHAKE_TIMEOUT, self.connector.connect(name, tcp))
            .await
            .context("TLS handshake timed out")??;
        let fp = tls::peer_fingerprint(tls.get_ref().1).context("peer sent no certificate")?;
        let exporter = export_client(tls.get_ref().1)?;
        Ok((TlsStream::from(tls), fp, exporter))
    }

    /// Pair with the device at `addr`. `ask_code` is called once the other
    /// device is showing its code, and should return what the user typed.
    pub async fn pair<F, Fut>(&self, addr: SocketAddr, ask_code: F) -> Result<TrustedPeer>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String>>,
    {
        let (mut s, responder_fp, exporter) = self.dial(addr).await?;
        write_msg(
            &mut s,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                name: self.name(),
                intent: Intent::Pair,
                device_kind: self.device_kind(),
            },
        )
        .await?;
        match read_msg(&mut s).await? {
            Message::PairCodeShown { .. } => {}
            Message::Error { message, .. } => bail!("{message}"),
            other => bail!("unexpected reply {other:?}"),
        }
        let code = ask_code().await?;
        let (state, ours) = PakeState::start(Role::Initiator, &code);
        write_msg(
            &mut s,
            &Message::PairSpake {
                msg: hex::encode(ours),
            },
        )
        .await?;
        let theirs = match read_msg(&mut s).await? {
            Message::PairSpake { msg } => hex::decode(msg).context("bad pairing message")?,
            Message::Error { message, .. } => bail!("{message}"),
            other => bail!("unexpected reply {other:?}"),
        };
        let binding = Binding {
            tls_exporter: exporter,
            initiator_fp: self.identity.fingerprint(),
            responder_fp,
        };
        let confirmer = state.finish(&theirs, &binding)?;
        write_msg(
            &mut s,
            &Message::PairConfirm {
                mac: hex::encode(confirmer.own_mac()),
            },
        )
        .await?;
        let their_mac = match timeout(CONFIRM_TIMEOUT, read_msg(&mut s)).await?? {
            Message::PairConfirm { mac } => hex::decode(mac).unwrap_or_default(),
            Message::Error { message, .. } => bail!("{message}"),
            other => bail!("unexpected reply {other:?}"),
        };
        if !confirmer.verify_peer(&their_mac) {
            bail!("the other device could not prove it knows the code; pairing aborted");
        }
        let (name, wake_mac) = match timeout(CONFIRM_TIMEOUT, read_msg(&mut s)).await?? {
            Message::PairOk { name, wake_mac, .. } => (name, wake_mac),
            other => bail!("unexpected reply {other:?}"),
        };
        let peer = TrustedPeer {
            name,
            fingerprint: responder_fp.to_hex(),
            permissions: Permissions::default(),
            paired_at_unix: now_unix(),
            last_addr: Some(addr.ip().to_string()),
            port: Some(addr.port()),
            auto_connect: true,
            custom_name: false,
            // The responder is the desktop that showed the code.
            device_kind: DeviceKind::Desktop,
            wake_mac: wake_mac
                .as_deref()
                .and_then(|m| crate::wol::MacAddr::parse(m).ok())
                .map(|m| m.to_string()),
        };
        self.trust().upsert(peer.clone())?;
        let _ = write_msg(&mut s, &Message::Bye).await;
        Ok(peer)
    }

    /// Open a session with an already-paired device. No code is needed; the
    /// peer is recognised by its certificate fingerprint.
    pub async fn connect(&self, addr: SocketAddr) -> Result<Session> {
        let (mut s, fp, _) = self.dial(addr).await?;
        let peer = self.trust().get(&fp).cloned().ok_or_else(|| {
            anyhow!(
                "the device at {addr} (fingerprint {}) is not paired with this one; pair with it first",
                fp.short()
            )
        })?;
        write_msg(
            &mut s,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                name: self.name(),
                intent: Intent::Connect,
                device_kind: self.device_kind(),
            },
        )
        .await?;
        match timeout(HANDSHAKE_TIMEOUT, read_msg(&mut s)).await?? {
            Message::Welcome {
                granted,
                name,
                wake_mac,
            } => {
                self.trust()
                    .note_address(&fp, addr.ip().to_string(), Some(addr.port()))?;
                self.trust().note_wake_mac(&fp, wake_mac.as_deref())?;
                self.note_peer_name(&fp, &name);
                let peer = self.trust().get(&fp).cloned().unwrap_or(peer);
                Ok(Session {
                    peer,
                    granted_to_us: granted,
                    stream: s,
                    seq: 0,
                    own_name: self.name.clone(),
                    announced_name: None,
                })
            }
            Message::Error { message, .. } => bail!("{message}"),
            other => bail!("unexpected reply {other:?}"),
        }
    }
}

enum PairOutcome {
    Ok,
    /// The initiator made a guess and it was wrong (counts toward lockout).
    WrongCode,
    /// The attempt ended before any guess was made.
    NoGuess(anyhow::Error),
}

fn export_server(conn: &rustls::ServerConnection) -> Result<[u8; 32]> {
    Ok(conn.export_keying_material([0u8; 32], pairing::EXPORTER_LABEL, None)?)
}

fn export_client(conn: &rustls::ClientConnection) -> Result<[u8; 32]> {
    Ok(conn.export_keying_material([0u8; 32], pairing::EXPORTER_LABEL, None)?)
}

pub(crate) async fn send_err(
    s: &mut (impl tokio::io::AsyncWrite + Unpin),
    code: ErrorCode,
    message: &str,
) -> Result<()> {
    write_msg(
        s,
        &Message::Error {
            code,
            message: message.to_string(),
        },
    )
    .await?;
    Ok(())
}

/// An open session with a paired peer (initiator side).
pub struct Session {
    pub peer: TrustedPeer,
    /// What the peer allows us to do on it.
    pub granted_to_us: Permissions,
    stream: TlsStream<TcpStream>,
    seq: u64,
    own_name: Arc<Mutex<String>>,
    /// The name the peer reported in its latest keepalive reply.
    pub announced_name: Option<String>,
}

impl Session {
    /// Round-trip a ping; returns the measured latency.
    pub async fn ping(&mut self) -> Result<Duration> {
        self.seq += 1;
        let seq = self.seq;
        let t0 = Instant::now();
        let name = Some(self.own_name.lock().unwrap().clone());
        write_msg(&mut self.stream, &Message::Ping { seq, name }).await?;
        loop {
            match timeout(HANDSHAKE_TIMEOUT, read_msg(&mut self.stream)).await?? {
                Message::Pong { seq: s, name } if s == seq => {
                    if name.is_some() {
                        self.announced_name = name;
                    }
                    return Ok(t0.elapsed());
                }
                Message::Pong { .. } => continue,
                other => bail!("unexpected reply {other:?}"),
            }
        }
    }

    pub async fn close(mut self) -> Result<()> {
        write_msg(&mut self.stream, &Message::Bye).await
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("peer", &self.peer.name)
            .field("granted_to_us", &self.granted_to_us)
            .finish_non_exhaustive()
    }
}
