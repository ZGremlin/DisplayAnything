//! Keyboard and mouse sharing for DisplayAnything.
//!
//! - [`wire`]: the compact binary format input travels in.
//! - [`keymap`]: USB HID usage codes <-> Windows scancodes and macOS keycodes,
//!   so a PC keyboard types the right keys on a Mac and the other way round.
//! - [`switcher`]: the pure decision logic for which machine gets input,
//!   hotkeys, and stuck-key safety (unit tested on every platform).
//! - [`capture`]: grabs the local keyboard and mouse (controller side).
//! - [`inject`]: replays received events (target side).
//! - [`clipboard`], [`permissions`]: small platform helpers.

pub mod capture;
pub mod clipboard;
pub mod geometry;
pub mod inject;
pub mod keymap;
pub mod permissions;
pub mod switcher;
pub mod wire;

pub use capture::{Controller, ControllerMsg};
pub use inject::{Injector, InjectorOptions};
pub use switcher::{Combo, Hotkeys, Recording, Target};
pub use wire::{Edge, Frame, InputEvent, MouseButton};

/// Marker put on events we synthesise, so our own capture hooks ignore them.
pub const INJECTED_MARKER: usize = 0x4441_5F49; // "DA_I"

/// True when this build can capture and inject input on the current OS.
pub fn platform_supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}
