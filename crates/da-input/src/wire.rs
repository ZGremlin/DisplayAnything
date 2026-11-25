//! Wire format for the input connection.
//!
//! Every frame is `[tag: u8][payload]`, sized so a key press is 4 bytes and a
//! mouse move 5. Input frames are written to the socket the moment they are
//! captured; there is no batching.

use anyhow::{bail, Result};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    fn to_u8(self) -> u8 {
        match self {
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 3,
            MouseButton::Back => 4,
            MouseButton::Forward => 5,
        }
    }

    fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            1 => MouseButton::Left,
            2 => MouseButton::Right,
            3 => MouseButton::Middle,
            4 => MouseButton::Back,
            5 => MouseButton::Forward,
            _ => bail!("unknown mouse button {b}"),
        })
    }
}

/// One input event. Keys are USB HID usage codes (keyboard page 0x07), so
/// they mean the same physical key on every OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Key {
        usage: u16,
        down: bool,
    },
    /// Relative pointer motion in pixels.
    MouseMove {
        dx: i16,
        dy: i16,
    },
    Button {
        button: MouseButton,
        down: bool,
    },
    /// Scroll, 120 units per wheel notch (positive = up / right).
    Wheel {
        dx: i16,
        dy: i16,
    },
    /// Release every key and button the target is holding.
    ReleaseAll,
}

/// A side of a screen layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Edge::Left => 1,
            Edge::Right => 2,
            Edge::Top => 3,
            Edge::Bottom => 4,
        }
    }

    fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            1 => Edge::Left,
            2 => Edge::Right,
            3 => Edge::Top,
            4 => Edge::Bottom,
            _ => bail!("unknown edge {b}"),
        })
    }
}

/// Everything that travels on the input connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Input(InputEvent),
    /// Controller -> target: "here is my clipboard", sent when switching over.
    ClipboardText(String),
    /// Controller -> target: "send me your clipboard", sent when switching back.
    ClipboardRequest,
    /// Controller -> target: input now comes from beyond `edge` of the
    /// target's screens. With `warp`, the pointer crossed over at `pos`
    /// (0..=65535 along that edge) and should appear there.
    Enter {
        edge: Edge,
        pos: u16,
        warp: bool,
    },
    /// Target -> controller: the pointer was pushed out past `edge` (the
    /// one facing the controller) at `pos`; input should go back.
    Leave {
        edge: Edge,
        pos: u16,
    },
    /// Controller -> target: the controller started (true) or stopped
    /// (false) sending its keyboard and mouse here.
    Active(bool),
    /// Controller -> target: the controller's display scaling in percent
    /// (100 when its pointer moves are already in logical units, as on a
    /// Mac), so the target can match the pointer speed.
    Scale(u16),
    /// Like `ClipboardText`, for an image: PNG bytes.
    ClipboardImage(Vec<u8>),
}

const T_KEY: u8 = 1;
const T_MOVE: u8 = 2;
const T_BUTTON: u8 = 3;
const T_WHEEL: u8 = 4;
const T_RELEASE_ALL: u8 = 5;
const T_CLIP_TEXT: u8 = 0x10;
const T_CLIP_REQ: u8 = 0x11;
const T_ENTER: u8 = 0x20;
const T_LEAVE: u8 = 0x21;
const T_ACTIVE: u8 = 0x22;
const T_SCALE: u8 = 0x23;
const T_CLIP_IMG: u8 = 0x12;

/// Largest clipboard text we send (1 MiB).
pub const MAX_CLIPBOARD: usize = 1 << 20;

/// Largest clipboard image we send, as PNG (16 MiB).
pub const MAX_CLIPBOARD_IMAGE: usize = 16 << 20;

impl Frame {
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Frame::Input(InputEvent::Key { usage, down }) => {
                out.push(T_KEY);
                out.extend_from_slice(&usage.to_be_bytes());
                out.push(*down as u8);
            }
            Frame::Input(InputEvent::MouseMove { dx, dy }) => {
                out.push(T_MOVE);
                out.extend_from_slice(&dx.to_be_bytes());
                out.extend_from_slice(&dy.to_be_bytes());
            }
            Frame::Input(InputEvent::Button { button, down }) => {
                out.push(T_BUTTON);
                out.push(button.to_u8());
                out.push(*down as u8);
            }
            Frame::Input(InputEvent::Wheel { dx, dy }) => {
                out.push(T_WHEEL);
                out.extend_from_slice(&dx.to_be_bytes());
                out.extend_from_slice(&dy.to_be_bytes());
            }
            Frame::Input(InputEvent::ReleaseAll) => out.push(T_RELEASE_ALL),
            Frame::ClipboardText(s) => {
                let bytes = truncate_utf8(s, MAX_CLIPBOARD).as_bytes();
                out.push(T_CLIP_TEXT);
                out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                out.extend_from_slice(bytes);
            }
            Frame::ClipboardImage(png) => {
                out.push(T_CLIP_IMG);
                out.extend_from_slice(&(png.len() as u32).to_be_bytes());
                out.extend_from_slice(png);
            }
            Frame::ClipboardRequest => out.push(T_CLIP_REQ),
            Frame::Enter { edge, pos, warp } => {
                out.push(T_ENTER);
                out.push(edge.to_u8());
                out.extend_from_slice(&pos.to_be_bytes());
                out.push(*warp as u8);
            }
            Frame::Leave { edge, pos } => {
                out.push(T_LEAVE);
                out.push(edge.to_u8());
                out.extend_from_slice(&pos.to_be_bytes());
            }
            Frame::Active(on) => {
                out.push(T_ACTIVE);
                out.push(*on as u8);
            }
            Frame::Scale(pct) => {
                out.push(T_SCALE);
                out.extend_from_slice(&pct.to_be_bytes());
            }
        }
    }

    /// Decode one frame from the front of `buf`. Returns `Ok(None)` if more
    /// bytes are needed, otherwise the frame and how many bytes it used.
    pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
        let Some(&tag) = buf.first() else {
            return Ok(None);
        };
        let need = |n: usize| buf.len() >= n;
        let i16_at = |i: usize| i16::from_be_bytes([buf[i], buf[i + 1]]);
        Ok(match tag {
            T_KEY if need(4) => Some((
                Frame::Input(InputEvent::Key {
                    usage: u16::from_be_bytes([buf[1], buf[2]]),
                    down: buf[3] != 0,
                }),
                4,
            )),
            T_MOVE if need(5) => Some((
                Frame::Input(InputEvent::MouseMove {
                    dx: i16_at(1),
                    dy: i16_at(3),
                }),
                5,
            )),
            T_BUTTON if need(3) => Some((
                Frame::Input(InputEvent::Button {
                    button: MouseButton::from_u8(buf[1])?,
                    down: buf[2] != 0,
                }),
                3,
            )),
            T_WHEEL if need(5) => Some((
                Frame::Input(InputEvent::Wheel {
                    dx: i16_at(1),
                    dy: i16_at(3),
                }),
                5,
            )),
            T_RELEASE_ALL => Some((Frame::Input(InputEvent::ReleaseAll), 1)),
            T_CLIP_REQ => Some((Frame::ClipboardRequest, 1)),
            T_CLIP_TEXT if need(5) => {
                let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
                if len > MAX_CLIPBOARD {
                    bail!("clipboard frame too large ({len} bytes)");
                }
                if !need(5 + len) {
                    return Ok(None);
                }
                let text = String::from_utf8_lossy(&buf[5..5 + len]).into_owned();
                Some((Frame::ClipboardText(text), 5 + len))
            }
            T_CLIP_IMG if need(5) => {
                let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
                if len > MAX_CLIPBOARD_IMAGE {
                    bail!("clipboard image too large ({len} bytes)");
                }
                if !need(5 + len) {
                    return Ok(None);
                }
                Some((Frame::ClipboardImage(buf[5..5 + len].to_vec()), 5 + len))
            }
            T_ENTER if need(5) => Some((
                Frame::Enter {
                    edge: Edge::from_u8(buf[1])?,
                    pos: u16::from_be_bytes([buf[2], buf[3]]),
                    warp: buf[4] != 0,
                },
                5,
            )),
            T_LEAVE if need(4) => Some((
                Frame::Leave {
                    edge: Edge::from_u8(buf[1])?,
                    pos: u16::from_be_bytes([buf[2], buf[3]]),
                },
                4,
            )),
            T_ACTIVE if need(2) => Some((Frame::Active(buf[1] != 0), 2)),
            T_SCALE if need(3) => Some((Frame::Scale(u16::from_be_bytes([buf[1], buf[2]])), 3)),
            T_KEY | T_MOVE | T_BUTTON | T_WHEEL | T_CLIP_TEXT | T_ENTER | T_LEAVE | T_ACTIVE
            | T_SCALE | T_CLIP_IMG => None,
            other => bail!("unknown input frame tag {other:#x}"),
        })
    }
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Split a large pointer delta into wire-sized moves.
pub fn split_move(mut dx: i32, mut dy: i32) -> Vec<InputEvent> {
    let mut out = Vec::with_capacity(1);
    loop {
        let sx = dx.clamp(i16::MIN as i32, i16::MAX as i32);
        let sy = dy.clamp(i16::MIN as i32, i16::MAX as i32);
        out.push(InputEvent::MouseMove {
            dx: sx as i16,
            dy: sy as i16,
        });
        dx -= sx;
        dy -= sy;
        if dx == 0 && dy == 0 {
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(f: Frame) {
        let mut buf = Vec::new();
        f.encode(&mut buf);
        let (g, used) = Frame::decode(&buf).unwrap().unwrap();
        assert_eq!(g, f);
        assert_eq!(used, buf.len());
        // Every strict prefix asks for more bytes rather than failing.
        for cut in 1..buf.len() {
            assert!(
                Frame::decode(&buf[..cut]).unwrap().is_none(),
                "{f:?} cut at {cut}"
            );
        }
    }

    #[test]
    fn frames_roundtrip() {
        roundtrip(Frame::Input(InputEvent::Key {
            usage: 0x04,
            down: true,
        }));
        roundtrip(Frame::Input(InputEvent::MouseMove { dx: -300, dy: 7 }));
        roundtrip(Frame::Input(InputEvent::Button {
            button: MouseButton::Forward,
            down: false,
        }));
        roundtrip(Frame::Input(InputEvent::Wheel { dx: 0, dy: -120 }));
        roundtrip(Frame::Input(InputEvent::ReleaseAll));
        roundtrip(Frame::ClipboardText("héllo 👋".into()));
        roundtrip(Frame::ClipboardRequest);
        roundtrip(Frame::Enter {
            edge: Edge::Right,
            pos: 40_000,
            warp: true,
        });
        roundtrip(Frame::Leave {
            edge: Edge::Top,
            pos: 1,
        });
        roundtrip(Frame::Active(true));
        roundtrip(Frame::Active(false));
        roundtrip(Frame::Scale(150));
        roundtrip(Frame::ClipboardImage(vec![0x89, b'P', b'N', b'G', 1]));
    }

    #[test]
    fn small_frames() {
        let mut b = Vec::new();
        Frame::Input(InputEvent::Key {
            usage: 4,
            down: true,
        })
        .encode(&mut b);
        assert_eq!(b.len(), 4);
        b.clear();
        Frame::Input(InputEvent::MouseMove { dx: 1, dy: 1 }).encode(&mut b);
        assert_eq!(b.len(), 5);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Frame::decode(&[0xEE]).is_err());
        assert!(Frame::decode(&[T_CLIP_TEXT, 0xFF, 0xFF, 0xFF, 0xFF]).is_err());
    }

    #[test]
    fn splits_big_moves() {
        let parts = split_move(70_000, -5);
        let sx: i32 = parts
            .iter()
            .map(|e| match e {
                InputEvent::MouseMove { dx, .. } => *dx as i32,
                _ => 0,
            })
            .sum();
        let sy: i32 = parts
            .iter()
            .map(|e| match e {
                InputEvent::MouseMove { dy, .. } => *dy as i32,
                _ => 0,
            })
            .sum();
        assert_eq!((sx, sy), (70_000, -5));
        assert_eq!(split_move(3, 4).len(), 1);
    }
}
