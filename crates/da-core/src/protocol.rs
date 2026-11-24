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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        version: u32,
        name: String,
        intent: Intent,
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
    },
    /// Session accepted.
    Welcome {
        name: String,
        granted: Permissions,
    },
    Ping {
        seq: u64,
    },
    Pong {
        seq: u64,
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
        };
        write_msg(&mut a, &m).await.unwrap();
        assert_eq!(read_msg(&mut b).await.unwrap(), m);
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&(10_000_000u32).to_be_bytes()).await.unwrap();
        assert!(read_msg(&mut b).await.is_err());
    }
}
