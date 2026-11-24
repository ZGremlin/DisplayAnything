//! Which machine gets the keyboard and mouse, and the hotkeys that switch it.
//!
//! This is pure logic with no OS calls, so it is tested on every platform.
//! The capture layer feeds it every local event and does what it says:
//! pass the event to the local OS, swallow it, and/or forward it.

use crate::keymap::{self, LALT, LCTRL, RALT, RCTRL};
use crate::wire::InputEvent;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Input goes to this computer as normal.
    Local,
    /// Input is forwarded to the connected peer.
    Remote,
}

/// Hotkeys are Ctrl+Alt (Ctrl+Option on a Mac) plus one of these keys.
#[derive(Clone, Copy, Debug)]
pub struct Hotkeys {
    pub to_remote: u16,
    pub to_local: u16,
    /// Always returns input to this computer, even if the link is broken.
    pub panic: u16,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys {
            to_remote: keymap::RIGHT,
            to_local: keymap::LEFT,
            panic: keymap::ESC,
        }
    }
}

/// What to do with one local event.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Keep the event from reaching the local OS.
    pub swallow: bool,
    /// Events to send to the peer, in order.
    pub send: Vec<InputEvent>,
    /// The active machine changed to this.
    pub switched: Option<Target>,
    /// Keys the local OS believes are held; the caller should inject key-ups
    /// for them locally so nothing stays stuck while input is away.
    pub release_locally: Vec<u16>,
}

#[derive(Debug)]
pub struct Switcher {
    target: Target,
    hotkeys: Hotkeys,
    remote_available: bool,
    /// Keys physically held right now.
    held: HashSet<u16>,
    /// Key-downs we swallowed as hotkeys, so their key-ups are swallowed too.
    swallowed_hotkeys: HashSet<u16>,
}

impl Switcher {
    pub fn new(hotkeys: Hotkeys) -> Self {
        Switcher {
            target: Target::Local,
            hotkeys,
            remote_available: false,
            held: HashSet::new(),
            swallowed_hotkeys: HashSet::new(),
        }
    }

    pub fn target(&self) -> Target {
        self.target
    }

    pub fn remote_available(&self) -> bool {
        self.remote_available
    }

    /// Tell the switcher whether a peer is connected. Losing the peer while
    /// it has input brings input straight back here.
    pub fn set_remote_available(&mut self, available: bool) -> Outcome {
        self.remote_available = available;
        if !available && self.target == Target::Remote {
            self.switch_to(Target::Local)
        } else {
            Outcome::default()
        }
    }

    /// Switch from the UI (tray menu) rather than a hotkey.
    pub fn set_target(&mut self, t: Target) -> Outcome {
        if t == self.target || (t == Target::Remote && !self.remote_available) {
            return Outcome::default();
        }
        self.switch_to(t)
    }

    fn ctrl_alt_held(&self) -> bool {
        (self.held.contains(&LCTRL) || self.held.contains(&RCTRL))
            && (self.held.contains(&LALT) || self.held.contains(&RALT))
    }

    fn switch_to(&mut self, t: Target) -> Outcome {
        let mut out = Outcome {
            switched: Some(t),
            ..Default::default()
        };
        match t {
            Target::Remote => {
                // The local OS saw these go down; release them here so the
                // local machine has no stuck modifiers while input is away.
                out.release_locally = self.held.iter().copied().collect();
                out.release_locally.sort_unstable();
            }
            Target::Local => out.send.push(InputEvent::ReleaseAll),
        }
        self.target = t;
        out
    }

    pub fn on_key(&mut self, usage: u16, down: bool) -> Outcome {
        if down {
            let first_press = self.held.insert(usage);
            if first_press && self.ctrl_alt_held() {
                let hk = self.hotkeys;
                let wanted = if usage == hk.panic {
                    Some(Target::Local)
                } else if usage == hk.to_remote
                    && self.target == Target::Local
                    && self.remote_available
                {
                    Some(Target::Remote)
                } else if usage == hk.to_local && self.target == Target::Remote {
                    Some(Target::Local)
                } else {
                    None
                };
                if let Some(t) = wanted {
                    self.swallowed_hotkeys.insert(usage);
                    let mut out = if t == self.target {
                        Outcome::default()
                    } else {
                        self.switch_to(t)
                    };
                    if usage == hk.panic && out.send.is_empty() {
                        out.send.push(InputEvent::ReleaseAll);
                    }
                    // The hotkey itself never reaches either machine.
                    out.release_locally.retain(|&k| k != usage);
                    out.swallow = true;
                    return out;
                }
            } else if !first_press && self.swallowed_hotkeys.contains(&usage) {
                // Auto-repeat of a hotkey.
                return Outcome {
                    swallow: true,
                    ..Default::default()
                };
            }
        } else {
            self.held.remove(&usage);
            if self.swallowed_hotkeys.remove(&usage) {
                return Outcome {
                    swallow: true,
                    ..Default::default()
                };
            }
        }
        self.route(InputEvent::Key { usage, down })
    }

    pub fn on_mouse(&mut self, ev: InputEvent) -> Outcome {
        self.route(ev)
    }

    fn route(&mut self, ev: InputEvent) -> Outcome {
        match self.target {
            Target::Local => Outcome::default(),
            Target::Remote => Outcome {
                swallow: true,
                send: vec![ev],
                ..Default::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::MouseButton;

    const A: u16 = 0x04;

    fn press(s: &mut Switcher, k: u16) -> Outcome {
        s.on_key(k, true)
    }
    fn release(s: &mut Switcher, k: u16) -> Outcome {
        s.on_key(k, false)
    }
    fn ready() -> Switcher {
        let mut s = Switcher::new(Hotkeys::default());
        s.set_remote_available(true);
        s
    }

    #[test]
    fn typing_stays_local_by_default() {
        let mut s = ready();
        assert_eq!(press(&mut s, A), Outcome::default());
        assert_eq!(
            s.on_mouse(InputEvent::MouseMove { dx: 1, dy: 1 }),
            Outcome::default()
        );
    }

    #[test]
    fn hotkey_switches_and_releases_local_modifiers() {
        let mut s = ready();
        assert!(!press(&mut s, LCTRL).swallow);
        assert!(!press(&mut s, LALT).swallow);
        let out = press(&mut s, keymap::RIGHT);
        assert!(out.swallow);
        assert_eq!(out.switched, Some(Target::Remote));
        assert_eq!(out.release_locally, vec![LCTRL, LALT]);
        assert!(out.send.is_empty(), "the hotkey is not forwarded");

        // Releasing the hotkey and modifiers: the arrow-up is swallowed, the
        // modifier ups are forwarded (the peer ignores ups it never saw down).
        assert_eq!(
            release(&mut s, keymap::RIGHT),
            Outcome {
                swallow: true,
                ..Default::default()
            }
        );
        let out = release(&mut s, LALT);
        assert!(out.swallow);
        assert_eq!(
            out.send,
            vec![InputEvent::Key {
                usage: LALT,
                down: false
            }]
        );
    }

    #[test]
    fn remote_mode_forwards_everything() {
        let mut s = ready();
        s.set_target(Target::Remote);
        let out = press(&mut s, A);
        assert!(out.swallow);
        assert_eq!(
            out.send,
            vec![InputEvent::Key {
                usage: A,
                down: true
            }]
        );
        let click = InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        };
        assert_eq!(s.on_mouse(click).send, vec![click]);
    }

    #[test]
    fn switching_back_releases_everything_on_the_peer() {
        let mut s = ready();
        s.set_target(Target::Remote);
        press(&mut s, LCTRL);
        press(&mut s, LALT);
        let out = press(&mut s, keymap::LEFT);
        assert_eq!(out.switched, Some(Target::Local));
        assert_eq!(out.send, vec![InputEvent::ReleaseAll]);
        assert!(out.swallow);
        // Back to local: modifier ups go to the local OS.
        assert!(!release(&mut s, LCTRL).swallow);
    }

    #[test]
    fn panic_key_always_comes_home() {
        let mut s = ready();
        s.set_target(Target::Remote);
        press(&mut s, RCTRL);
        press(&mut s, RALT);
        let out = press(&mut s, keymap::ESC);
        assert_eq!(out.switched, Some(Target::Local));
        assert_eq!(out.send, vec![InputEvent::ReleaseAll]);
        // Already local: still swallowed and still releases the peer.
        release(&mut s, keymap::ESC);
        let out = press(&mut s, keymap::ESC);
        assert!(out.swallow);
        assert_eq!(out.switched, None);
        assert_eq!(out.send, vec![InputEvent::ReleaseAll]);
    }

    #[test]
    fn no_switch_without_a_peer() {
        let mut s = Switcher::new(Hotkeys::default());
        press(&mut s, LCTRL);
        press(&mut s, LALT);
        let out = press(&mut s, keymap::RIGHT);
        assert!(
            !out.swallow,
            "Ctrl+Alt+Right goes to the local OS when there is no peer"
        );
        assert_eq!(s.target(), Target::Local);
    }

    #[test]
    fn losing_the_peer_returns_input() {
        let mut s = ready();
        s.set_target(Target::Remote);
        let out = s.set_remote_available(false);
        assert_eq!(out.switched, Some(Target::Local));
        assert_eq!(s.target(), Target::Local);
    }

    #[test]
    fn hotkey_autorepeat_is_swallowed() {
        let mut s = ready();
        press(&mut s, LCTRL);
        press(&mut s, LALT);
        press(&mut s, keymap::RIGHT);
        // OS auto-repeat sends more key-downs for the held arrow.
        let out = press(&mut s, keymap::RIGHT);
        assert!(out.swallow);
        assert!(out.send.is_empty());
    }

    #[test]
    fn hundred_toggles_leave_nothing_held() {
        let mut s = ready();
        for _ in 0..100 {
            press(&mut s, LCTRL);
            press(&mut s, LALT);
            press(&mut s, keymap::RIGHT);
            release(&mut s, keymap::RIGHT);
            press(&mut s, keymap::LEFT);
            release(&mut s, keymap::LEFT);
            release(&mut s, LALT);
            release(&mut s, LCTRL);
        }
        assert_eq!(s.target(), Target::Local);
        assert!(s.held.is_empty());
        assert!(s.swallowed_hotkeys.is_empty());
    }
}
