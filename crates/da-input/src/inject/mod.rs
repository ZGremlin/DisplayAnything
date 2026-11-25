//! Replays received input on this machine (target side).

use crate::keymap::{self, LCTRL, LMETA, RCTRL, RMETA};
use crate::wire::{InputEvent, MouseButton};
use anyhow::Result;
use std::collections::HashSet;

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
}

/// Tracks what it has pressed, so a disconnect or `ReleaseAll` never leaves
/// a key or button stuck down. Not `Send` on macOS: keep it on one thread.
pub struct Injector {
    backend: Box<dyn Backend>,
    opts: InjectorOptions,
    held_keys: HashSet<u16>,
    held_buttons: HashSet<MouseButton>,
}

impl Injector {
    pub fn new(opts: InjectorOptions) -> Result<Self> {
        Ok(Injector {
            backend: Box::new(platform::Platform::new()?),
            opts,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
        })
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

    pub fn inject(&mut self, ev: &InputEvent) {
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
    struct Log(Arc<Mutex<Vec<String>>>);
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
}
