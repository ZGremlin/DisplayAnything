//! Viewer side: hardware-decode the stream and show it in a native window.
//!
//! The presenter draws into a window someone else owns (the tray app makes
//! a bare window and passes its handle), scaling the picture to fit with
//! black bars, and showing each frame the moment it decodes.

use crate::wire::EncodedFrame;
use anyhow::Result;
use raw_window_handle::RawWindowHandle;

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

/// Draws the remote screen into a window. Safe to use from any thread.
pub struct Presenter {
    inner: platform::Presenter,
}

impl Presenter {
    /// Attach to a window.
    ///
    /// # Safety
    /// `window` must be a live window (an HWND on Windows, an NSView on
    /// macOS) that outlives the presenter. On macOS this must be called on
    /// the main thread.
    pub unsafe fn new(window: RawWindowHandle) -> Result<Self> {
        Ok(Presenter {
            inner: platform::Presenter::new(window)?,
        })
    }

    /// Queue one frame for decoding and display. Never blocks.
    pub fn push(&self, frame: EncodedFrame) {
        self.inner.push(frame);
    }

    /// True once if the decoder lost its place and needs a keyframe.
    pub fn take_keyframe_request(&self) -> bool {
        self.inner.take_keyframe_request()
    }

    /// The stream's picture size (from the host's stream info). Frames are
    /// cropped to it, since decoders pad to multiples of 16.
    pub fn set_video_size(&self, width: u32, height: u32) {
        self.inner.set_video_size(width, height);
    }

    /// The remote picture size in pixels, once known, for mapping the
    /// mouse inside the window.
    pub fn video_size(&self) -> Option<(u32, u32)> {
        self.inner.video_size()
    }
}

/// Where a `src` sized picture lands inside a `dst` sized window when it is
/// scaled to fit, keeping its shape: (x, y, width, height).
pub fn fit(src: (u32, u32), dst: (u32, u32)) -> (u32, u32, u32, u32) {
    let (sw, sh) = (src.0.max(1) as u64, src.1.max(1) as u64);
    let (dw, dh) = (dst.0 as u64, dst.1 as u64);
    // Compare sw/sh with dw/dh without floating point.
    let (w, h) = if sw * dh > dw * sh {
        (dw, sh * dw / sw)
    } else {
        (sw * dh / sh, dh)
    };
    (
        ((dw - w) / 2) as u32,
        ((dh - h) / 2) as u32,
        w as u32,
        h as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fits_with_bars() {
        // Same shape fills the window.
        assert_eq!(fit((1920, 1080), (960, 540)), (0, 0, 960, 540));
        // Wide picture in a square window: bars top and bottom.
        assert_eq!(fit((1920, 1080), (1000, 1000)), (0, 219, 1000, 562));
        // Tall window: bars left and right.
        assert_eq!(fit((1000, 1000), (1600, 900)), (350, 0, 900, 900));
    }
}
