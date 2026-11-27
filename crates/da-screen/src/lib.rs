//! Remote screen control for DisplayAnything: the video half.
//!
//! - [`wire`]: the binary messages the screen stream travels in.
//! - [`h264`]: Annex B / AVCC helpers shared by the encoders and decoders.
//! - [`host`]: captures this computer's screen and hardware-encodes it
//!   (Windows: Windows.Graphics.Capture + Media Foundation; macOS:
//!   ScreenCaptureKit + VideoToolbox).
//! - [`audio`]: the host's sound (system audio capture and playback).
//! - [`viewer`]: decodes the stream in hardware and shows it in a native
//!   window (Windows: Media Foundation + Direct3D 11 flip swap chain;
//!   macOS: `AVSampleBufferDisplayLayer`).
//!
//! Everything here is LAN-only plumbing; authentication, permissions and
//! the connection itself live in da-core.

pub mod audio;
pub mod h264;
pub mod host;
pub mod viewer;
#[cfg(windows)]
mod win;
pub mod wire;

pub use host::{capture_permission, displays, HostOptions, ScreenHost, StreamEvent};
pub use viewer::Presenter;
pub use wire::{Codec, EncodedFrame, StreamInfo, ViewMsg};

/// True when this build can capture and show screens on the current OS.
pub fn platform_supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// Where the shared monitor sits, in the host's global pointer coordinates
/// (pixels on Windows, points on macOS).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl DisplayBounds {
    /// A fraction of the screen (0..=65535 per axis) as a global position.
    pub fn point(&self, x: u16, y: u16) -> (f64, f64) {
        let fx = f64::from(x) / 65535.0;
        let fy = f64::from(y) / 65535.0;
        (
            self.x + fx * (self.width - 1.0).max(0.0),
            self.y + fy * (self.height - 1.0).max(0.0),
        )
    }
}

/// One of the host's monitors.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DisplayInfo {
    /// Index used to pick it; 0 is the main display.
    pub index: u32,
    /// Size in pixels.
    pub width: u32,
    pub height: u32,
}

// Both are handed between threads by da-core and the tray app.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ScreenHost>();
    assert_send_sync::<Presenter>();
};
