//! Control-channel messages and framing.
//!
//! Each frame is a 4-byte big-endian length followed by a JSON message.
//! JSON keeps the control channel easy to debug; video and input (later
//! milestones) use their own binary formats on the UDP channel.

use crate::trust::Permissions;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Control messages are small; anything bigger is a bug or an attack.
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// First contact: run the pairing-code exchange.
    Pair,
    /// Already paired: open a session.
    Connect,
    /// Already paired: open the input channel (keyboard and mouse).
    Input,
    /// Already paired: view (and control) this device's screen.
    Remote,
}

/// What kind of device is on the other end of a connection. Desktops are
/// the default; a phone or tablet only views and controls a screen, so the
/// desktop saves it view-only and never dials it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    #[default]
    Desktop,
    /// iPhone or iPad running the DisplayAnything viewer.
    IosViewer,
}

impl DeviceKind {
    pub fn is_desktop(&self) -> bool {
        *self == DeviceKind::Desktop
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        version: u32,
        name: String,
        intent: Intent,
        /// Optional, and left out for desktops, so older builds see the
        /// same message they always did.
        #[serde(default, skip_serializing_if = "DeviceKind::is_desktop")]
        device_kind: DeviceKind,
    },
    /// Responder is now showing a pairing code to its user.
    PairCodeShown {
        expires_in_secs: u64,
    },
    /// SPAKE2 message (hex).
    PairSpake {
        msg: String,
    },
    /// Key confirmation MAC (hex).
    PairConfirm {
        mac: String,
    },
    /// Pairing finished; carries the responder's name and what it granted.
    PairOk {
        name: String,
        granted: Permissions,
        /// The sender's hardware (MAC) address on the network this
        /// connection uses, so the receiver can wake it with Wake-on-LAN
        /// later. Optional, so older builds still interoperate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake_mac: Option<String>,
    },
    /// Session accepted.
    Welcome {
        name: String,
        granted: Permissions,
        /// The sender's hardware (MAC) address on the network this
        /// connection uses, so the receiver can wake it with Wake-on-LAN
        /// later. Optional, so older builds still interoperate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake_mac: Option<String>,
    },
    /// Keepalive. `name` carries the sender's current device name so a
    /// rename reaches connected peers without reconnecting (optional, so
    /// older builds still interoperate).
    Ping {
        seq: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Pong {
        seq: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Bye,
    Error {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotPaired,
    PairingLocked,
    PairingBusy,
    PairingFailed,
    PairingTimeout,
    VersionMismatch,
    Protocol,
    /// Paired, but this device does not allow that feature for you.
    NotAllowed,
}

pub async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, msg: &Message) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    if body.len() > MAX_FRAME {
        bail!("frame too large");
    }
    w.write_all(&(body.len() as u32).to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_msg<R: AsyncRead + Unpin>(r: &mut R) -> Result<Message> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        bail!("frame too large ({len} bytes)");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let m = Message::Hello {
            version: 1,
            name: "pc".into(),
            intent: Intent::Pair,
            device_kind: DeviceKind::IosViewer,
        };
        write_msg(&mut a, &m).await.unwrap();
        assert_eq!(read_msg(&mut b).await.unwrap(), m);
    }

    #[test]
    fn desktop_hello_is_the_same_as_before() {
        let m = Message::Hello {
            version: 1,
            name: "pc".into(),
            intent: Intent::Connect,
            device_kind: DeviceKind::Desktop,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            !json.contains("device_kind"),
            "old builds must see no new field: {json}"
        );
        // A hello from an older build has no device_kind and reads as a desktop.
        let old = r#"{"type":"hello","version":1,"name":"pc","intent":"connect"}"#;
        assert_eq!(serde_json::from_str::<Message>(old).unwrap(), m);
    }

    #[test]
    fn phone_hello_says_what_it_is() {
        let m = Message::Hello {
            version: 1,
            name: "ipad".into(),
            intent: Intent::Pair,
            device_kind: DeviceKind::IosViewer,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""device_kind":"ios_viewer""#), "{json}");
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), m);
    }

    #[test]
    fn wake_address_is_optional_both_ways() {
        // An older desktop sends no wake_mac; it reads as None.
        let old = r#"{"type":"welcome","name":"mac","granted":{"remote_control":true,"display_takeover":true,"input_takeover":true}}"#;
        let Message::Welcome { wake_mac, .. } = serde_json::from_str(old).unwrap() else {
            panic!("not a welcome");
        };
        assert_eq!(wake_mac, None);
        // A newer one adds it, and an older reader ignores fields it doesn't know.
        let m = Message::PairOk {
            name: "mac".into(),
            granted: Permissions::default(),
            wake_mac: Some("a4:83:e7:0b:1c:2d".into()),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""wake_mac":"a4:83:e7:0b:1c:2d""#), "{json}");
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), m);
        let none = Message::Welcome {
            name: "pc".into(),
            granted: Permissions::default(),
            wake_mac: None,
        };
        assert!(!serde_json::to_string(&none).unwrap().contains("wake_mac"));
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&(10_000_000u32).to_be_bytes()).await.unwrap();
        assert!(read_msg(&mut b).await.is_err());
    }
}
