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
use da_core::da_input::keymap::LSHIFT;
use da_core::da_input::{InputEvent, MouseButton};
use da_core::da_screen::audio::AudioPlayer;
use da_core::da_screen::h264;
use da_core::da_screen::wire::{AudioFormat, StreamSettings};
use da_core::da_screen::ViewMsg;
use da_core::identity::Fingerprint;
use da_core::protocol::DeviceKind;
use da_core::{
    paths::parse_addr, Config, Identity, Node, RemoteControl, TrustStore, TrustedPeer, DEFAULT_PORT,
};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

uniffi::setup_scaffolding!();

fn clamp16(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

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
    /// The desktop told this phone its hardware address, so it can be
    /// woken from sleep with [`Client::wake`].
    pub can_wake: bool,
}

fn desktop_of(p: &TrustedPeer) -> Desktop {
    Desktop {
        fingerprint: p.fingerprint.clone(),
        name: p.name.clone(),
        address: p.last_addr.clone(),
        port: p.port.unwrap_or(DEFAULT_PORT),
        can_wake: p.wake_mac.is_some(),
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
    /// Play the desktop's sound on this phone. Can be changed later with
    /// [`Screen::set_audio`].
    pub audio: bool,
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
        // The trust list is written into this folder, so it must exist.
        std::fs::create_dir_all(&dir).map_err(|e| DaError::Other {
            message: format!("could not create {}: {e}", dir.display()),
        })?;
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

    /// Change the name this phone shows to desktops. Swift keeps the name
    /// and passes it again on launch.
    pub fn set_device_name(&self, name: String) {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            self.node.set_name(trimmed);
        }
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

    /// Wake a sleeping desktop with a Wake-on-LAN magic packet. Returns as
    /// soon as the packets are sent: there is no reply, so open the screen
    /// afterwards (a Mac takes a few seconds to wake) to find out whether
    /// it worked. Fails if the desktop never told this phone how to wake
    /// it, or nothing could be sent on this network.
    pub fn wake(&self, fingerprint: String) -> Result<(), DaError> {
        let fp = Fingerprint::from_hex(&fingerprint).map_err(|e| DaError::NotPaired {
            message: format!("{e:#}"),
        })?;
        self.node
            .wake_peer(&fp)
            .map(|_| ())
            .map_err(|e| match classify(e) {
                DaError::Unreachable { message } => DaError::Other { message },
                other => other,
            })
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
            audio: request.audio,
        };
        let mut view = self.node.open_remote(addr, start).await.map_err(classify)?;
        let control = view.control();
        let audio_on = Arc::new(AtomicBool::new(request.audio));
        let host_audio = Arc::new(AtomicBool::new(false));
        let (audio_on2, host_audio2) = (audio_on.clone(), host_audio.clone());

        tokio::spawn(async move {
            // Parameter sets we last handed to Swift, so they are sent once
            // and again only when the desktop changes them.
            let mut format: Option<(Vec<u8>, Vec<u8>)> = None;
            // The desktop's sound: its format, and the player while it's on.
            let mut sound: Option<AudioFormat> = None;
            let mut player: Option<AudioPlayer> = None;
            let mut player_failed = false;
            while let Some(msg) = view.next().await {
                match msg {
                    ViewMsg::Info(info) => {
                        host_audio2.store(info.audio, Ordering::Relaxed);
                        sink.on_info(info.width, info.height, info.displays, info.display)
                    }
                    ViewMsg::AudioInfo(f) => {
                        if sound != Some(f) {
                            sound = Some(f);
                            player = None;
                            player_failed = false;
                        }
                    }
                    ViewMsg::Audio(chunk) => {
                        if !audio_on2.load(Ordering::Relaxed) {
                            player = None;
                            continue;
                        }
                        if player.is_none() && !player_failed {
                            if let Some(f) = sound {
                                match AudioPlayer::new(f) {
                                    Ok(p) => player = Some(p),
                                    Err(e) => {
                                        player_failed = true;
                                        log_warning(&format!("can't play the sound: {e:#}"));
                                    }
                                }
                            }
                        }
                        if let Some(p) = &player {
                            p.push(&chunk.data);
                        }
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

        Ok(Arc::new(Screen {
            control,
            audio_on,
            host_audio,
        }))
    }
}

fn log_warning(message: &str) {
    eprintln!("DisplayAnything: {message}");
}

/// A mouse button the phone can press on the desktop.
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

impl From<PointerButton> for MouseButton {
    fn from(b: PointerButton) -> Self {
        match b {
            PointerButton::Left => MouseButton::Left,
            PointerButton::Right => MouseButton::Right,
            PointerButton::Middle => MouseButton::Middle,
        }
    }
}

/// An open screen. Keep it while the picture is on screen.
#[derive(uniffi::Object)]
pub struct Screen {
    control: RemoteControl,
    /// Play the desktop's sound (shared with the stream task).
    audio_on: Arc<AtomicBool>,
    /// The desktop can send sound and switch it on and off.
    host_audio: Arc<AtomicBool>,
}

#[uniffi::export]
impl Screen {
    /// Ask the desktop for a full-quality frame, e.g. after the picture
    /// was lost or the app came back from the background.
    pub fn request_keyframe(&self) {
        self.control.request_keyframe();
    }

    /// Turn the desktop's sound on or off on this phone. The desktop stops
    /// recording while it's off (desktops from before sound just stay
    /// silent).
    pub fn set_audio(&self, on: bool) {
        self.audio_on.store(on, Ordering::Relaxed);
        if self.host_audio.load(Ordering::Relaxed) {
            self.control.set_audio(on);
        }
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

    /// Move the desktop's pointer by a number of pixels on the desktop.
    pub fn move_pointer(&self, dx: i32, dy: i32) {
        self.control.send_input(InputEvent::MouseMove {
            dx: clamp16(dx),
            dy: clamp16(dy),
        });
    }

    /// Put the desktop's pointer at a fraction of its screen (0 to 1 each way).
    pub fn point_at(&self, fx: f64, fy: f64) {
        let scale = |f: f64| (f.clamp(0.0, 1.0) * 65535.0).round() as u16;
        self.control.pointer(scale(fx), scale(fy));
    }

    /// Press or release a mouse button on the desktop.
    pub fn click(&self, button: PointerButton, down: bool) {
        self.control.send_input(InputEvent::Button {
            button: button.into(),
            down,
        });
    }

    /// Scroll, 120 units per wheel notch (positive is up and right).
    /// Smaller steps give smooth scrolling where the desktop supports it.
    pub fn scroll(&self, dx: i32, dy: i32) {
        self.control.send_input(InputEvent::Wheel {
            dx: clamp16(dx),
            dy: clamp16(dy),
        });
    }

    /// Type text as key presses on a US keyboard layout, holding `modifiers`
    /// (HID usages, e.g. Ctrl) around each key. Characters with no key on
    /// that layout are skipped; returns how many were sent.
    pub fn type_text(&self, text: String, modifiers: Vec<u16>) -> u32 {
        let mut sent = 0;
        for ch in text.chars() {
            let Some((usage, shift)) = usage_for_char(ch) else {
                continue;
            };
            for &m in &modifiers {
                self.key(m, true);
            }
            if shift {
                self.key(LSHIFT, true);
            }
            self.key(usage, true);
            self.key(usage, false);
            if shift {
                self.key(LSHIFT, false);
            }
            for &m in modifiers.iter().rev() {
                self.key(m, false);
            }
            sent += 1;
        }
        sent
    }

    /// Press or release a key, by USB HID usage code (keyboard page).
    pub fn key(&self, usage: u16, down: bool) {
        self.control.send_input(InputEvent::Key { usage, down });
    }

    /// Let go of every key and button the desktop is holding. Send this
    /// when the view goes away, or if anything looks stuck.
    pub fn release_all(&self) {
        self.control.send_input(InputEvent::ReleaseAll);
    }

    /// End the stream. The desktop stops sending and its banner goes away.
    pub fn stop(&self) {
        self.release_all();
        self.control.stop();
    }
}

/// The HID usage for a character on a US layout, and whether it needs Shift.
fn usage_for_char(ch: char) -> Option<(u16, bool)> {
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    let r = match ch {
        'a'..='z' => (0x04 + (ch as u16 - 'a' as u16), false),
        'A'..='Z' => (0x04 + (ch as u16 - 'A' as u16), true),
        '1'..='9' => (0x1E + (ch as u16 - '1' as u16), false),
        '0' => (0x27, false),
        '\n' | '\r' => (0x28, false),
        '\t' => (0x2B, false),
        ' ' => (0x2C, false),
        '-' => (0x2D, false),
        '_' => (0x2D, true),
        '=' => (0x2E, false),
        '+' => (0x2E, true),
        '[' => (0x2F, false),
        '{' => (0x2F, true),
        ']' => (0x30, false),
        '}' => (0x30, true),
        '\\' => (0x31, false),
        '|' => (0x31, true),
        ';' => (0x33, false),
        ':' => (0x33, true),
        '\'' => (0x34, false),
        '"' => (0x34, true),
        '`' => (0x35, false),
        '~' => (0x35, true),
        ',' => (0x36, false),
        '<' => (0x36, true),
        '.' => (0x37, false),
        '>' => (0x37, true),
        '/' => (0x38, false),
        '?' => (0x38, true),
        // iOS "smart punctuation" substitutes these; send the plain key.
        '\u{2018}' | '\u{2019}' => (0x34, false),
        '\u{201C}' | '\u{201D}' => (0x34, true),
        '\u{2013}' | '\u{2014}' => (0x2D, false),
        _ => {
            let i = SHIFTED_DIGITS.find(ch)?;
            let usage = if i == 0 { 0x27 } else { 0x1E + i as u16 - 1 };
            (usage, true)
        }
    };
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_layout_characters() {
        assert_eq!(usage_for_char('a'), Some((0x04, false)));
        assert_eq!(usage_for_char('Z'), Some((0x1D, true)));
        assert_eq!(usage_for_char('1'), Some((0x1E, false)));
        assert_eq!(usage_for_char('0'), Some((0x27, false)));
        assert_eq!(usage_for_char('!'), Some((0x1E, true)));
        assert_eq!(usage_for_char('('), Some((0x26, true)));
        assert_eq!(usage_for_char(')'), Some((0x27, true)));
        assert_eq!(usage_for_char('?'), Some((0x38, true)));
        assert_eq!(usage_for_char('\n'), Some((0x28, false)));
        assert_eq!(usage_for_char('\u{e9}'), None);
    }
}
