//! The DisplayAnything client core for iPhone and iPad.
//!
//! This is the phone's half of the desktop protocol, reusing `da-core` so
//! pairing, TLS pinning and the screen stream behave exactly as they do
//! between desktops. The phone only views a desktop's screen: it never
//! shares its own, never listens, and never dials a device it has not been
//! paired with. LAN only.
//!
//! Swift drives it through the UniFFI types below. Frames reach Swift
//! already converted to the AVCC form `AVSampleBufferDisplayLayer` takes.

use anyhow::anyhow;
use da_core::da_screen::h264;
use da_core::da_screen::wire::StreamSettings;
use da_core::da_screen::ViewMsg;
use da_core::identity::Fingerprint;
use da_core::protocol::DeviceKind;
use da_core::{
    paths::parse_addr, Config, Identity, Node, RemoteControl, TrustStore, TrustedPeer, DEFAULT_PORT,
};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

uniffi::setup_scaffolding!();

/// Everything Swift needs to know when something goes wrong. The message is
/// for the person, in plain words.
#[derive(Debug, Clone, uniffi::Error, thiserror::Error)]
pub enum DaError {
    /// This phone isn't paired with that desktop (any more).
    #[error("{message}")]
    NotPaired { message: String },
    /// Paired, but the desktop hasn't allowed remote control for this phone.
    #[error("{message}")]
    NotAllowed { message: String },
    /// No answer on the local network.
    #[error("{message}")]
    Unreachable { message: String },
    /// The code was wrong, pairing is locked, or the exchange failed.
    #[error("{message}")]
    PairingFailed { message: String },
    #[error("{message}")]
    Other { message: String },
}

fn classify(e: anyhow::Error) -> DaError {
    let message = format!("{e:#}");
    let lower = message.to_lowercase();
    if lower.contains("not paired") {
        DaError::NotPaired { message }
    } else if lower.contains("does not allow") {
        DaError::NotAllowed { message }
    } else if lower.contains("could not reach")
        || lower.contains("timed out")
        || lower.contains("no answer")
    {
        DaError::Unreachable { message }
    } else {
        DaError::Other { message }
    }
}

fn classify_pair(e: anyhow::Error) -> DaError {
    match classify(e) {
        DaError::Other { message } | DaError::NotPaired { message } => {
            DaError::PairingFailed { message }
        }
        other => other,
    }
}

/// The identity a phone keeps. Swift stores it in the Keychain (this device
/// only, never synced) and passes it back to [`Client::new`] on every launch.
#[derive(Debug, Clone, uniffi::Record)]
pub struct IdentityBytes {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

/// Make a new identity. Call once, when the Keychain has none yet.
#[uniffi::export]
pub fn generate_identity(device_name: String) -> Result<IdentityBytes, DaError> {
    let id = Identity::generate(&device_name).map_err(|e| DaError::Other {
        message: format!("could not create this device's identity: {e:#}"),
    })?;
    Ok(IdentityBytes {
        cert_der: id.cert_der,
        key_der: id.key_der,
    })
}

/// A desktop this phone is paired with.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Desktop {
    /// Full SHA-256 fingerprint of the desktop's certificate (hex).
    pub fingerprint: String,
    pub name: String,
    /// IP address last seen, if any.
    pub address: Option<String>,
    pub port: u16,
}

fn desktop_of(p: &TrustedPeer) -> Desktop {
    Desktop {
        fingerprint: p.fingerprint.clone(),
        name: p.name.clone(),
        address: p.last_addr.clone(),
        port: p.port.unwrap_or(DEFAULT_PORT),
    }
}

/// How to open a screen. Zero means "as the desktop has it".
#[derive(Debug, Clone, uniffi::Record)]
pub struct ScreenRequest {
    /// Which of the desktop's monitors (0 = main).
    pub display: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Largest picture to send, in pixels: set this to the view's size so
    /// the desktop doesn't send more than the phone can show.
    pub max_width: u32,
    pub max_height: u32,
}

/// Receives the stream. Called on a background thread: hop to the main
/// thread before touching UIKit.
#[uniffi::export(with_foreign)]
pub trait FrameSink: Send + Sync {
    /// The desktop's screen size in pixels and how many monitors it has.
    fn on_info(&self, width: u32, height: u32, displays: u32, display: u32);
    /// H.264 parameter sets, sent before the first frame and whenever they change.
    fn on_format(&self, sps: Vec<u8>, pps: Vec<u8>);
    /// One frame as AVCC (4-byte length prefixes), ready for a CMSampleBuffer.
    fn on_frame(&self, avcc: Vec<u8>, pts_us: u64, keyframe: bool);
    /// The desktop stopped the stream, with a message for the person.
    fn on_error(&self, message: String);
    /// The stream is over, for any reason.
    fn on_closed(&self);
}

/// Told when the desktop is showing its pairing code and wants it typed in.
#[uniffi::export(with_foreign)]
pub trait PairingListener: Send + Sync {
    fn code_needed(&self);
}

/// The phone's connection to the desktops it is paired with.
#[derive(uniffi::Object)]
pub struct Client {
    node: Arc<Node>,
    pending_code: Mutex<Option<oneshot::Sender<String>>>,
}

#[uniffi::export(async_runtime = "tokio")]
impl Client {
    /// Open this phone's trust list in `data_dir` (Application Support).
    #[uniffi::constructor]
    pub fn new(
        data_dir: String,
        device_name: String,
        identity: IdentityBytes,
    ) -> Result<Arc<Self>, DaError> {
        let dir = PathBuf::from(data_dir);
        let trust = TrustStore::load(&dir).map_err(|e| DaError::Other {
            message: format!("could not read the saved desktops: {e:#}"),
        })?;
        let id = Identity {
            name: device_name,
            cert_der: identity.cert_der,
            key_der: identity.key_der,
        };
        let config = Config {
            device_kind: DeviceKind::IosViewer,
            ..Config::default()
        };
        let node = Node::new(id, trust, config).map_err(|e| DaError::Other {
            message: format!("could not start: {e:#}"),
        })?;
        Ok(Arc::new(Client {
            node,
            pending_code: Mutex::new(None),
        }))
    }

    /// Short fingerprint of this phone, for showing to the person.
    pub fn fingerprint(&self) -> String {
        self.node.identity().fingerprint().short()
    }

    /// Desktops this phone is paired with.
    pub fn saved_desktops(&self) -> Vec<Desktop> {
        self.node.trust().peers().iter().map(desktop_of).collect()
    }

    /// Forget a desktop on this phone. The desktop keeps this phone paired
    /// until it removes it on its side.
    pub fn forget(&self, fingerprint: String) -> Result<(), DaError> {
        self.node
            .trust()
            .remove(&fingerprint)
            .map(|_| ())
            .map_err(|e| DaError::Other {
                message: format!("could not forget that desktop: {e:#}"),
            })
    }

    /// Pair with the desktop at `address` (an IP, optionally with a port).
    /// `listener.code_needed()` fires when that desktop is showing its code;
    /// then call [`Client::submit_pairing_code`]. Returns the new desktop.
    pub async fn pair(
        &self,
        address: String,
        listener: Arc<dyn PairingListener>,
    ) -> Result<Desktop, DaError> {
        let addr = parse_addr(&address).map_err(|e| DaError::PairingFailed {
            message: format!("{e:#}"),
        })?;
        let (tx, rx) = oneshot::channel::<String>();
        *self.pending_code.lock().unwrap() = Some(tx);
        let node = self.node.clone();
        let result = node
            .pair(addr, move || async move {
                listener.code_needed();
                rx.await
                    .map_err(|_| anyhow!("pairing was cancelled before a code was entered"))
            })
            .await;
        self.pending_code.lock().unwrap().take();
        result.map(|peer| desktop_of(&peer)).map_err(classify_pair)
    }

    /// The code the person typed in from the desktop. Returns false if no
    /// pairing is waiting for one.
    pub fn submit_pairing_code(&self, code: String) -> bool {
        match self.pending_code.lock().unwrap().take() {
            Some(tx) => tx.send(code).is_ok(),
            None => false,
        }
    }

    /// Stop a pairing that is waiting for a code.
    pub fn cancel_pairing(&self) {
        self.pending_code.lock().unwrap().take();
    }

    /// Open a desktop's screen. Frames arrive on `sink` until the returned
    /// [`Screen`] is stopped or the desktop closes the stream.
    pub async fn open_screen(
        &self,
        fingerprint: String,
        request: ScreenRequest,
        sink: Arc<dyn FrameSink>,
    ) -> Result<Arc<Screen>, DaError> {
        let fp = Fingerprint::from_hex(&fingerprint).map_err(|e| DaError::NotPaired {
            message: format!("{e:#}"),
        })?;
        let peer = self
            .node
            .trust()
            .get(&fp)
            .cloned()
            .ok_or_else(|| DaError::NotPaired {
                message: "this desktop is not paired with this phone".into(),
            })?;
        let ip: IpAddr = peer
            .last_addr
            .as_deref()
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| DaError::Unreachable {
                message: format!("no address saved for {}; pair with it again", peer.name),
            })?;
        let addr = SocketAddr::new(ip, peer.port.unwrap_or(DEFAULT_PORT));

        let start = da_core::da_screen::wire::StartRequest {
            display: request.display,
            fps: request.fps,
            bitrate_kbps: request.bitrate_kbps,
            max_width: request.max_width,
            max_height: request.max_height,
        };
        let mut view = self.node.open_remote(addr, start).await.map_err(classify)?;
        let control = view.control();

        tokio::spawn(async move {
            // Parameter sets we last handed to Swift, so they are sent once
            // and again only when the desktop changes them.
            let mut format: Option<(Vec<u8>, Vec<u8>)> = None;
            while let Some(msg) = view.next().await {
                match msg {
                    ViewMsg::Info(info) => {
                        sink.on_info(info.width, info.height, info.displays, info.display)
                    }
                    ViewMsg::Video(frame) => {
                        if frame.keyframe {
                            if let Some((sps, pps)) = h264::parameter_sets(&frame.data) {
                                if format.as_ref() != Some(&(sps.clone(), pps.clone())) {
                                    sink.on_format(sps.clone(), pps.clone());
                                    format = Some((sps, pps));
                                }
                            }
                        }
                        sink.on_frame(
                            h264::annexb_to_avcc(&frame.data),
                            frame.pts_us,
                            frame.keyframe,
                        );
                    }
                    ViewMsg::Error(message) => sink.on_error(message),
                    ViewMsg::Stop => break,
                    _ => {}
                }
            }
            // Dropping `view` tells the desktop we are done.
            sink.on_closed();
        });

        Ok(Arc::new(Screen { control }))
    }
}

/// An open screen. Keep it while the picture is on screen.
#[derive(uniffi::Object)]
pub struct Screen {
    control: RemoteControl,
}

#[uniffi::export]
impl Screen {
    /// Ask the desktop for a full-quality frame, e.g. after the picture
    /// was lost or the app came back from the background.
    pub fn request_keyframe(&self) {
        self.control.request_keyframe();
    }

    /// Show another of the desktop's monitors.
    pub fn select_display(&self, display: u32) {
        self.control.select_display(display);
    }

    /// Change the picture size or quality while the stream runs.
    pub fn configure(&self, max_width: u32, max_height: u32, bitrate_kbps: u32) {
        self.control.configure(StreamSettings {
            max_width,
            max_height,
            bitrate_kbps,
        });
    }

    /// End the stream. The desktop stops sending and its banner goes away.
    pub fn stop(&self) {
        self.control.stop();
    }
}
