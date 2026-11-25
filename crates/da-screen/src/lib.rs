//! Remote screen control for DisplayAnything: the video half.
//!
//! - [`wire`]: the binary messages the screen stream travels in.
//! - [`h264`]: Annex B / AVCC helpers shared by the encoders and decoders.
//! - [`host`]: captures this computer's screen and hardware-encodes it
//!   (Windows: Windows.Graphics.Capture + Media Foundation; macOS:
//!   ScreenCaptureKit + VideoToolbox).
//! - [`viewer`]: decodes the stream in hardware and shows it in a native
//!   window (Windows: Media Foundation + Direct3D 11 flip swap chain;
//!   macOS: `AVSampleBufferDisplayLayer`).
//!
//! Everything here is LAN-only plumbing; authentication, permissions and
//! the connection itself live in da-core.

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

/// One of the host's monitors.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DisplayInfo {
    /// Index used to pick it; 0 is the main display.
    pub index: u32,
    /// Size in pixels.
    pub width: u32,
    pub height: u32,
}
