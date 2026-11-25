//! Replays received input on this machine (target side).

use crate::geometry::{self, Rect};
use crate::keymap::{self, LCTRL, LMETA, RCTRL, RMETA};
use crate::wire::{Edge, Frame, InputEvent, MouseButton};
use anyhow::Result;
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "macos")]
pub(crate) use macos::send_key as macos_send_key;
#[cfg(windows)]
use windows as platform;
#[cfg(windows)]
pub(crate) use windows::send_key as windows_send_key;
#[cfg(not(any(windows, target_os = "macos")))]
mod unsupported;
#[cfg(not(any(windows, target_os = "macos")))]
use unsupported as platform;

#[derive(Clone, Copy, Debug)]
pub struct InjectorOptions {
    /// Swap Ctrl and Command/Windows keys, so Ctrl+C on a PC keyboard is
    /// Command+C on a Mac. On by default when this machine is a Mac.
    pub swap_ctrl_meta: bool,
}

#[allow(clippy::derivable_impls)] // the default depends on the OS
impl Default for InjectorOptions {
    fn default() -> Self {
        InjectorOptions {
            swap_ctrl_meta: cfg!(target_os = "macos"),
        }
    }
}

/// The OS-specific half: turns events into synthetic input.
/// Release a mouse button on this machine (capture uses it so a button held
/// when input moves away doesn't stay pressed locally).
pub(crate) fn release_local_button(button: MouseButton) {
    if let Ok(mut p) = platform::Platform::new() {
        p.button(button, false);
    }
}

pub(crate) trait Backend {
    fn key(&mut self, usage: u16, down: bool);
    fn mouse_move(&mut self, dx: i32, dy: i32);
    fn button(&mut self, button: MouseButton, down: bool);
    fn wheel(&mut self, dx: i32, dy: i32);
    /// Pointer position in global screen coordinates.
    fn cursor(&self) -> (f64, f64);
    /// Every display, in global screen coordinates.
    fn screens(&self) -> Vec<Rect>;
    /// Put the pointer at this position.
    fn warp(&mut self, x: f64, y: f64);
}

/// Displays of this machine (empty if unknown or unsupported).
pub fn screens() -> Vec<Rect> {
    platform::Platform::new()
        .map(|p| p.screens())
        .unwrap_or_default()
}

/// Move this machine's pointer (capture uses it when input comes back over
/// an edge).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn warp(x: f64, y: f64) {
    if let Ok(mut p) = platform::Platform::new() {
        p.warp(x, y);
    }
}

/// Tracks what it has pressed, so a disconnect or `ReleaseAll` never leaves
/// a key or button stuck down. Not `Send` on macOS: keep it on one thread.
pub struct Injector {
    backend: Box<dyn Backend>,
    opts: InjectorOptions,
    held_keys: HashSet<u16>,
    held_buttons: HashSet<MouseButton>,
    edge: EdgeReturn,
}

/// Pushes past the edge needed before input goes back (a deliberate shove,
/// not grazing the edge).
const LEAVE_PUSHES: u32 = 2;
const SCREENS_REFRESH: Duration = Duration::from_secs(2);

/// Watches for the pointer being pushed back out over the edge that faces
/// the controller.
#[derive(Default)]
struct EdgeReturn {
    /// The side of this machine's screens facing the controller.
    edge: Option<Edge>,
    pushes: u32,
    screens: Vec<Rect>,
    screens_at: Option<Instant>,
}

impl Injector {
    pub fn new(opts: InjectorOptions) -> Result<Self> {
        Ok(Injector {
            backend: Box::new(platform::Platform::new()?),
            opts,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            edge: EdgeReturn::default(),
        })
    }

    fn screens(&mut self) -> &[Rect] {
        let stale = self
            .edge
            .screens_at
            .is_none_or(|t| t.elapsed() > SCREENS_REFRESH);
        if stale {
            self.edge.screens = self.backend.screens();
            self.edge.screens_at = Some(Instant::now());
        }
        &self.edge.screens
    }

    /// Handle an edge frame from the controller. Returns nothing; the
    /// pointer is moved if it crossed over.
    pub fn enter(&mut self, edge: Edge, pos: u16, warp: bool) {
        self.edge.edge = Some(edge);
        self.edge.pushes = 0;
        self.edge.screens_at = None;
        if warp {
            if let Some((x, y)) = geometry::entry_point(self.screens(), edge, pos) {
                self.backend.warp(x, y);
            }
        }
    }

    /// The input channel closed: stop watching the edge.
    pub fn reset_edge(&mut self) {
        self.edge.edge = None;
        self.edge.pushes = 0;
    }

    /// Before a move: is it a deliberate push out past the controller's edge?
    fn check_leave(&mut self, dx: i32, dy: i32) -> Option<Frame> {
        let edge = self.edge.edge?;
        if !self.held_buttons.is_empty() {
            return None; // dragging something: stay here
        }
        let toward = match edge {
            Edge::Left => dx < 0,
            Edge::Right => dx > 0,
            Edge::Top => dy < 0,
            Edge::Bottom => dy > 0,
        };
        if !toward {
            self.edge.pushes = 0;
            return None;
        }
        let (cx, cy) = self.backend.cursor();
        let (x, y) = (cx + f64::from(dx), cy + f64::from(dy));
        let screens = self.screens().to_vec();
        match geometry::overshoot(&screens, edge, x, y) {
            Some(pos) => {
                self.edge.pushes += 1;
                if self.edge.pushes >= LEAVE_PUSHES {
                    self.edge.pushes = 0;
                    return Some(Frame::Leave { edge, pos });
                }
                None
            }
            None => {
                self.edge.pushes = 0;
                None
            }
        }
    }

    fn map_key(&self, usage: u16) -> u16 {
        if !self.opts.swap_ctrl_meta {
            return usage;
        }
        match usage {
            LCTRL => LMETA,
            LMETA => LCTRL,
            RCTRL => RMETA,
            RMETA => RCTRL,
            u => u,
        }
    }

    /// Replay one event. Returns a frame to send back to the controller when
    /// the pointer was pushed out over its edge.
    pub fn inject(&mut self, ev: &InputEvent) -> Option<Frame> {
        if let InputEvent::MouseMove { dx, dy } = *ev {
            let leave = self.check_leave(dx as i32, dy as i32);
            if leave.is_some() {
                // The controller takes input back; don't move further here.
                return leave;
            }
        }
        match *ev {
            InputEvent::Key { usage, down } => {
                let usage = self.map_key(usage);
                if down {
                    self.held_keys.insert(usage);
                    self.backend.key(usage, true);
                } else if self.held_keys.remove(&usage) {
                    // Ups for keys we never pressed (e.g. the modifiers of the
                    // hotkey that switched input here) are ignored.
                    self.backend.key(usage, false);
                }
            }
            InputEvent::MouseMove { dx, dy } => self.backend.mouse_move(dx as i32, dy as i32),
            InputEvent::Button { button, down } => {
                if down {
                    self.held_buttons.insert(button);
                    self.backend.button(button, true);
                } else if self.held_buttons.remove(&button) {
                    self.backend.button(button, false);
                }
            }
            InputEvent::Wheel { dx, dy } => self.backend.wheel(dx as i32, dy as i32),
            InputEvent::ReleaseAll => self.release_all(),
        }
        None
    }

    /// Release every key and button we are holding: modifiers last, so a
    /// held Ctrl does not turn the other releases into shortcuts.
    pub fn release_all(&mut self) {
        let mut keys: Vec<u16> = self.held_keys.drain().collect();
        keys.sort_by_key(|&k| keymap::is_modifier(k));
        for k in keys {
            self.backend.key(k, false);
        }
        for b in self.held_buttons.drain().collect::<Vec<_>>() {
            self.backend.button(b, false);
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<String>>>, Arc<Mutex<(f64, f64)>>);
    impl Backend for Log {
        fn key(&mut self, u: u16, d: bool) {
            self.0.lock().unwrap().push(format!("key {u:#x} {d}"));
        }
        fn mouse_move(&mut self, dx: i32, dy: i32) {
            self.0.lock().unwrap().push(format!("move {dx} {dy}"));
        }
        fn button(&mut self, b: MouseButton, d: bool) {
            self.0.lock().unwrap().push(format!("button {b:?} {d}"));
        }
        fn wheel(&mut self, dx: i32, dy: i32) {
            self.0.lock().unwrap().push(format!("wheel {dx} {dy}"));
        }
        fn cursor(&self) -> (f64, f64) {
            *self.1.lock().unwrap()
        }
        fn screens(&self) -> Vec<Rect> {
            vec![Rect::new(0.0, 0.0, 1000.0, 800.0)]
        }
        fn warp(&mut self, x: f64, y: f64) {
            *self.1.lock().unwrap() = (x, y);
            self.0.lock().unwrap().push(format!("warp {x} {y}"));
        }
    }

    fn injector(swap: bool) -> (Injector, Log) {
        let log = Log::default();
        let inj = Injector {
            backend: Box::new(log.clone()),
            opts: InjectorOptions {
                swap_ctrl_meta: swap,
            },
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            edge: EdgeReturn::default(),
        };
        (inj, log)
    }

    #[test]
    fn ignores_ups_it_never_pressed() {
        let (mut inj, log) = injector(false);
        inj.inject(&InputEvent::Key {
            usage: LCTRL,
            down: false,
        });
        assert!(log.0.lock().unwrap().is_empty());
    }

    #[test]
    fn release_all_lets_go_of_everything_modifiers_last() {
        let (mut inj, log) = injector(false);
        inj.inject(&InputEvent::Key {
            usage: LCTRL,
            down: true,
        });
        inj.inject(&InputEvent::Key {
            usage: 0x06,
            down: true,
        });
        inj.inject(&InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        });
        log.0.lock().unwrap().clear();
        inj.inject(&InputEvent::ReleaseAll);
        assert_eq!(
            *log.0.lock().unwrap(),
            vec!["key 0x6 false", "key 0xe0 false", "button Left false"]
        );
        inj.inject(&InputEvent::ReleaseAll);
        assert_eq!(
            log.0.lock().unwrap().len(),
            3,
            "second release does nothing"
        );
    }

    #[test]
    fn drop_releases_held_keys() {
        let (mut inj, log) = injector(false);
        inj.inject(&InputEvent::Key {
            usage: 0x04,
            down: true,
        });
        drop(inj);
        assert_eq!(log.0.lock().unwrap().last().unwrap(), "key 0x4 false");
    }

    #[test]
    fn swaps_ctrl_and_command_when_asked() {
        let (mut inj, log) = injector(true);
        inj.inject(&InputEvent::Key {
            usage: LCTRL,
            down: true,
        });
        inj.inject(&InputEvent::Key {
            usage: LCTRL,
            down: false,
        });
        assert_eq!(
            *log.0.lock().unwrap(),
            vec!["key 0xe3 true", "key 0xe3 false"]
        );
    }

    #[test]
    fn enters_at_the_edge_and_leaves_on_a_push_back() {
        let (mut inj, log) = injector(false);
        // Came in over the right edge, at the top.
        inj.enter(Edge::Right, 0, true);
        assert_eq!(log.0.lock().unwrap().last().unwrap(), "warp 997 0");
        // Moving away from the edge: nothing.
        assert!(inj
            .inject(&InputEvent::MouseMove { dx: -5, dy: 0 })
            .is_none());
        // Pushing past it twice sends input back, at the same height.
        *log.1.lock().unwrap() = (999.0, 0.0);
        assert!(inj
            .inject(&InputEvent::MouseMove { dx: 5, dy: 0 })
            .is_none());
        assert_eq!(
            inj.inject(&InputEvent::MouseMove { dx: 5, dy: 0 }),
            Some(Frame::Leave {
                edge: Edge::Right,
                pos: 0
            })
        );
    }

    #[test]
    fn no_leaving_while_dragging() {
        let (mut inj, log) = injector(false);
        inj.enter(Edge::Left, 0, false);
        *log.1.lock().unwrap() = (0.0, 10.0);
        inj.inject(&InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        });
        for _ in 0..5 {
            assert!(inj
                .inject(&InputEvent::MouseMove { dx: -9, dy: 0 })
                .is_none());
        }
    }
}
