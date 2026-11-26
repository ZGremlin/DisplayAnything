//! Host side: capture this computer's screen and hardware-encode it.

use crate::wire::{EncodedFrame, StartRequest, StreamInfo};
use crate::DisplayInfo;
use anyhow::Result;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(windows, target_os = "macos")))]
mod unsupported;
#[cfg(not(any(windows, target_os = "macos")))]
use unsupported as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

/// What to capture and how hard to encode it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostOptions {
    pub display: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

impl From<&StartRequest> for HostOptions {
    fn from(r: &StartRequest) -> Self {
        HostOptions {
            display: r.display,
            fps: r.fps.clamp(1, 240),
            bitrate_kbps: r.bitrate_kbps.clamp(1_000, 400_000),
        }
    }
}

/// What the capture thread reports.
#[derive(Debug)]
pub enum StreamEvent {
    Info(StreamInfo),
    Frame(EncodedFrame),
    /// Capture stopped; the text is for the person.
    Error(String),
}

pub type Sink = Box<dyn FnMut(StreamEvent) + Send>;

/// A running screen stream. Dropping it stops capture.
pub struct ScreenHost {
    inner: platform::Host,
}

impl ScreenHost {
    /// Start capturing. `sink` is called from a capture thread with the
    /// stream info, then every encoded frame.
    pub fn start(opts: HostOptions, sink: Sink) -> Result<Self> {
        Ok(ScreenHost {
            inner: platform::Host::start(opts, sink)?,
        })
    }

    /// Make the next frame a keyframe (also re-sends the current picture
    /// when the screen isn't changing).
    pub fn request_keyframe(&self) {
        self.inner.request_keyframe();
    }

    /// Where the captured monitor is, for placing the pointer.
    pub fn bounds(&self) -> Option<crate::DisplayBounds> {
        self.inner.bounds()
    }
}

/// The monitors this computer has, main display first.
pub fn displays() -> Vec<DisplayInfo> {
    platform::displays()
}

/// Whether this computer allows its screen to be captured right now
/// (macOS Screen Recording permission; always true elsewhere). With
/// `prompt`, macOS shows its permission request.
pub fn capture_permission(prompt: bool) -> bool {
    platform::capture_permission(prompt)
}

/// `(w, h)` scaled down (never up) to fit in `limit`, keeping its shape,
/// with even sides as NV12 needs.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn fit_within((w, h): (u32, u32), (lw, lh): (u32, u32)) -> (u32, u32) {
    let k = (lw as f64 / w as f64).min(lh as f64 / h as f64).min(1.0);
    let scale = |v: u32| even(((v as f64 * k).round() as u32).max(2));
    (scale(w), scale(h))
}

/// Round down to an even number: 4:2:0 video needs even sizes.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) fn even(v: u32) -> u32 {
    v & !1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_screens_fit_the_encoder() {
        assert_eq!(fit_within((5120, 1440), (4096, 4096)), (4096, 1152));
        assert_eq!(fit_within((2560, 1440), (4096, 4096)), (2560, 1440));
        assert_eq!(fit_within((3840, 2160), (2560, 1600)), (2560, 1440));
        assert_eq!(fit_within((1365, 767), (4096, 4096)), (1364, 766));
    }
}
