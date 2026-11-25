//! Messages on the remote-control video connection.
//!
//! After the usual hello/welcome, both sides speak a tiny binary framing:
//! a 1-byte tag, a 4-byte big-endian length, then the payload. Video
//! frames are raw H.264 (Annex B) with a short header; the few control
//! messages carry JSON so they stay easy to read in logs.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// A frame bigger than this is a bug or an attack (a 4K keyframe at very
/// high quality is a few MB).
pub const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

const TAG_START: u8 = 1;
const TAG_KEYFRAME: u8 = 2;
const TAG_INFO: u8 = 3;
const TAG_VIDEO: u8 = 4;
const TAG_ERROR: u8 = 5;
const TAG_STOP: u8 = 6;

const FLAG_KEYFRAME: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    H264,
}

/// What the viewer asks for when it opens the stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRequest {
    /// Which of the host's monitors (0 = main).
    #[serde(default)]
    pub display: u32,
    /// Frames per second to aim for.
    pub fps: u32,
    /// Target bitrate in kbit/s.
    pub bitrate_kbps: u32,
}

impl Default for StartRequest {
    fn default() -> Self {
        StartRequest {
            display: 0,
            fps: 60,
            // Comfortable on wired gigabit, still fine on good Wi-Fi.
            bitrate_kbps: 40_000,
        }
    }
}

/// What the host is sending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
    /// How many monitors the host has, for the viewer's monitor picker.
    #[serde(default)]
    pub displays: u32,
}

/// One encoded video frame (an H.264 access unit in Annex B form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedFrame {
    pub keyframe: bool,
    /// Capture time on the host, in microseconds from the stream start.
    pub pts_us: u64,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewMsg {
    /// Viewer -> host: start streaming.
    Start(StartRequest),
    /// Viewer -> host: the decoder lost track; send a keyframe.
    KeyframeRequest,
    /// Host -> viewer: stream format (sent before the first frame and
    /// whenever it changes).
    Info(StreamInfo),
    /// Host -> viewer: one frame.
    Video(EncodedFrame),
    /// Host -> viewer: capture failed; the message is for the person.
    Error(String),
    /// Either way: end the session.
    Stop,
}

impl ViewMsg {
    pub fn encode(&self, out: &mut Vec<u8>) {
        let (tag, body): (u8, Vec<u8>) = match self {
            ViewMsg::Start(r) => (TAG_START, serde_json::to_vec(r).unwrap_or_default()),
            ViewMsg::KeyframeRequest => (TAG_KEYFRAME, Vec::new()),
            ViewMsg::Info(i) => (TAG_INFO, serde_json::to_vec(i).unwrap_or_default()),
            ViewMsg::Error(m) => (TAG_ERROR, m.as_bytes().to_vec()),
            ViewMsg::Stop => (TAG_STOP, Vec::new()),
            ViewMsg::Video(f) => {
                // Written in place: frames are big, so skip the extra copy.
                out.push(TAG_VIDEO);
                out.extend_from_slice(&((9 + f.data.len()) as u32).to_be_bytes());
                out.push(if f.keyframe { FLAG_KEYFRAME } else { 0 });
                out.extend_from_slice(&f.pts_us.to_be_bytes());
                out.extend_from_slice(&f.data);
                return;
            }
        };
        out.push(tag);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
    }

    /// Decode one message from the front of `buf`. Returns the message and
    /// how many bytes it used, or `None` if more bytes are needed.
    pub fn decode(buf: &[u8]) -> Result<Option<(ViewMsg, usize)>> {
        if buf.len() < 5 {
            return Ok(None);
        }
        let tag = buf[0];
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if len > MAX_PAYLOAD {
            bail!("screen message too large ({len} bytes)");
        }
        if buf.len() < 5 + len {
            return Ok(None);
        }
        let body = &buf[5..5 + len];
        let msg = match tag {
            TAG_START => ViewMsg::Start(serde_json::from_slice(body)?),
            TAG_KEYFRAME => ViewMsg::KeyframeRequest,
            TAG_INFO => ViewMsg::Info(serde_json::from_slice(body)?),
            TAG_ERROR => ViewMsg::Error(String::from_utf8_lossy(body).into_owned()),
            TAG_STOP => ViewMsg::Stop,
            TAG_VIDEO => {
                if body.len() < 9 {
                    bail!("short video frame");
                }
                let mut pts = [0u8; 8];
                pts.copy_from_slice(&body[1..9]);
                ViewMsg::Video(EncodedFrame {
                    keyframe: body[0] & FLAG_KEYFRAME != 0,
                    pts_us: u64::from_be_bytes(pts),
                    data: body[9..].to_vec(),
                })
            }
            other => bail!("unknown screen message {other}"),
        };
        Ok(Some((msg, 5 + len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(m: ViewMsg) {
        let mut buf = Vec::new();
        m.encode(&mut buf);
        // Partial input asks for more.
        assert!(ViewMsg::decode(&buf[..buf.len() - 1]).unwrap().is_none());
        let (back, used) = ViewMsg::decode(&buf).unwrap().unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(back, m);
    }

    #[test]
    fn every_message_roundtrips() {
        roundtrip(ViewMsg::Start(StartRequest::default()));
        roundtrip(ViewMsg::KeyframeRequest);
        roundtrip(ViewMsg::Info(StreamInfo {
            width: 2560,
            height: 1440,
            codec: Codec::H264,
            displays: 2,
        }));
        roundtrip(ViewMsg::Video(EncodedFrame {
            keyframe: true,
            pts_us: 123_456_789,
            data: vec![0, 0, 0, 1, 0x67, 1, 2, 3],
        }));
        roundtrip(ViewMsg::Error("no permission".into()));
        roundtrip(ViewMsg::Stop);
    }

    #[test]
    fn back_to_back_messages_split_cleanly() {
        let mut buf = Vec::new();
        ViewMsg::KeyframeRequest.encode(&mut buf);
        ViewMsg::Stop.encode(&mut buf);
        let (a, n) = ViewMsg::decode(&buf).unwrap().unwrap();
        let (b, m) = ViewMsg::decode(&buf[n..]).unwrap().unwrap();
        assert_eq!((a, b), (ViewMsg::KeyframeRequest, ViewMsg::Stop));
        assert_eq!(n + m, buf.len());
    }

    #[test]
    fn rejects_oversized_and_unknown() {
        let mut buf = vec![TAG_VIDEO];
        buf.extend_from_slice(&(u32::MAX).to_be_bytes());
        assert!(ViewMsg::decode(&buf).is_err());
        let buf = [99u8, 0, 0, 0, 0];
        assert!(ViewMsg::decode(&buf).is_err());
    }
}
