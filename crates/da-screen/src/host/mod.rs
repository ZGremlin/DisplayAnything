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
    /// Largest picture to send (scaled to fit); None = full resolution.
    pub max_size: Option<(u32, u32)>,
}

impl From<&StartRequest> for HostOptions {
    fn from(r: &StartRequest) -> Self {
        HostOptions {
            display: r.display,
            fps: r.fps.clamp(1, 240),
            bitrate_kbps: r.bitrate_kbps.clamp(1_000, 400_000),
            max_size: max_size(r.max_width, r.max_height),
        }
    }
}

impl HostOptions {
    /// These options with new size and quality settings.
    pub fn with_settings(&self, s: &crate::wire::StreamSettings) -> Self {
        HostOptions {
            bitrate_kbps: s.bitrate_kbps.clamp(1_000, 400_000),
            max_size: max_size(s.max_width, s.max_height),
            ..self.clone()
        }
    }

    /// The picture size to try first for a screen of `full` pixels.
    pub fn target_size(&self, full: (u32, u32)) -> (u32, u32) {
        fit_within(full, self.max_size.unwrap_or((u32::MAX, u32::MAX)))
    }
}

fn max_size(w: u32, h: u32) -> Option<(u32, u32)> {
    (w > 0 && h > 0).then_some((w.max(320), h.max(200)))
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

/// A display mode the host's monitor offers, for switching its real
/// resolution to the one the viewer asked for. Sizes are in the units the
/// person sees (points on macOS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    /// macOS: drawn at 2x (sharper).
    pub hidpi: bool,
}

/// The mode to switch to for a `want` sized picture: that exact size if
/// offered, else the biggest that fits in it; same refresh rate and
/// HiDPI preferred. None when the current mode is already the best one or
/// nothing fits.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) fn pick_mode(modes: &[Mode], current: Mode, want: (u32, u32)) -> Option<usize> {
    let best = modes
        .iter()
        .enumerate()
        .filter(|(_, m)| m.width <= want.0 && m.height <= want.1)
        .max_by_key(|(_, m)| {
            (
                (m.width, m.height) == want,
                u64::from(m.width) * u64::from(m.height),
                m.hz == current.hz,
                m.hz,
                m.hidpi,
            )
        })?;
    let m = best.1;
    ((m.width, m.height) != (current.width, current.height)).then_some(best.0)
}

/// `(w, h)` scaled down (never up) to fit in `limit`, keeping its shape,
/// with even sides as NV12 needs.
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

    fn mode(width: u32, height: u32, hz: u32) -> Mode {
        Mode {
            width,
            height,
            hz,
            hidpi: false,
        }
    }

    #[test]
    fn picks_the_mode_to_switch_to() {
        let modes = [
            mode(5120, 1440, 120),
            mode(3840, 1080, 120),
            mode(2560, 1440, 60),
            mode(2560, 1440, 120),
            mode(1920, 1080, 120),
            mode(1920, 1080, 60),
            mode(1280, 720, 60),
        ];
        let cur = mode(5120, 1440, 120);
        // Exact size, same refresh rate.
        assert_eq!(pick_mode(&modes, cur, (1920, 1080)), Some(4));
        assert_eq!(pick_mode(&modes, cur, (2560, 1440)), Some(3));
        // No exact match: the biggest that fits.
        assert_eq!(pick_mode(&modes, cur, (3000, 2000)), Some(3));
        // Already there, or nothing fits.
        assert_eq!(pick_mode(&modes, cur, (5120, 1440)), None);
        assert_eq!(pick_mode(&modes, cur, (800, 600)), None);
    }

    #[test]
    fn wide_screens_fit_the_encoder() {
        assert_eq!(fit_within((5120, 1440), (4096, 4096)), (4096, 1152));
        assert_eq!(fit_within((2560, 1440), (4096, 4096)), (2560, 1440));
        assert_eq!(fit_within((3840, 2160), (2560, 1600)), (2560, 1440));
        assert_eq!(fit_within((1365, 767), (4096, 4096)), (1364, 766));
    }
}
