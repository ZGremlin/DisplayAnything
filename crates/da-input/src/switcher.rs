//! Which machine gets the keyboard and mouse, and the hotkeys that switch it.
//!
//! This is pure logic with no OS calls, so it is tested on every platform.
//! The capture layer feeds it every local event and does what it says:
//! pass the event to the local OS, swallow it, and/or forward it.
//!
//! A hotkey is any combination of keys and mouse buttons. It fires when the
//! last part of the combination goes down and exactly that combination is
//! held (left and right modifiers count as the same key). The switcher can
//! also record a new combination: the user presses it and holds it still
//! for [`RECORD_HOLD`].

use crate::keymap::{self, LALT, LCTRL, LMETA, LSHIFT, RALT, RCTRL, RMETA, RSHIFT};
use crate::wire::{Edge, InputEvent, MouseButton};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

/// How long a combination must be held, unchanged, to be recorded.
pub const RECORD_HOLD: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Input goes to this computer as normal.
    Local,
    /// Input is forwarded to the connected peer.
    Remote,
}

/// A set of keys (HID usages) and mouse buttons pressed together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Combo {
    #[serde(default)]
    pub keys: Vec<u16>,
    #[serde(default)]
    pub buttons: Vec<MouseButton>,
}

/// Left and right modifiers are the same key for hotkeys.
fn normalize_key(k: u16) -> u16 {
    match k {
        RCTRL => LCTRL,
        RSHIFT => LSHIFT,
        RALT => LALT,
        RMETA => LMETA,
        k => k,
    }
}

impl Combo {
    pub fn new(keys: &[u16], buttons: &[MouseButton]) -> Self {
        let keys: BTreeSet<u16> = keys.iter().copied().map(normalize_key).collect();
        let buttons: BTreeSet<MouseButton> = buttons.iter().copied().collect();
        // Modifiers first, then the rest, so labels read "Ctrl + Alt + Right".
        let mut keys: Vec<u16> = keys.into_iter().collect();
        keys.sort_by_key(|&k| (!keymap::is_modifier(k), k));
        Combo {
            keys,
            buttons: buttons.into_iter().collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len() + self.buttons.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn normalized(&self) -> Combo {
        Combo::new(&self.keys, &self.buttons)
    }

    /// Why this combination can't be used as a hotkey, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        let c = self.normalized();
        if c.is_empty() {
            return Some("Press at least one key or mouse button.");
        }
        let only_mods = c.buttons.is_empty() && c.keys.iter().all(|&k| keymap::is_modifier(k));
        if c.len() == 1 && !only_mods {
            return Some(
                "Use a combination (for example a modifier plus a key), so normal typing and clicking can't trigger it.",
            );
        }
        if c.len() == 1 {
            return Some("A single modifier key would fire every time you type a capital letter or a shortcut.");
        }
        if c.keys.is_empty() && c.buttons.contains(&MouseButton::Left) && c.len() <= 2 {
            return Some("That mouse combination is too easy to press by accident; add a key.");
        }
        None
    }
}

/// The three switching actions and their combinations. `to_remote` and
/// `to_local` may be the same combination, which then toggles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkeys {
    pub to_remote: Combo,
    pub to_local: Combo,
    /// Always returns input to this computer, even if the link is broken.
    pub panic: Combo,
    /// Pauses / resumes moving over by pushing the pointer past the edge.
    #[serde(default = "default_edge_lock")]
    pub edge_lock: Combo,
}

fn default_edge_lock() -> Combo {
    Combo::new(&[LCTRL, LALT, 0x0F], &[]) // Ctrl+Alt+L
}

impl Default for Hotkeys {
    /// Ctrl+Alt (Ctrl+Option on a Mac) plus Right, Left or Esc.
    fn default() -> Self {
        Hotkeys {
            to_remote: Combo::new(&[LCTRL, LALT, keymap::RIGHT], &[]),
            to_local: Combo::new(&[LCTRL, LALT, keymap::LEFT], &[]),
            panic: Combo::new(&[LCTRL, LALT, keymap::ESC], &[]),
            edge_lock: default_edge_lock(),
        }
    }
}

/// Progress of recording a new combination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recording {
    /// What is held right now, and for how long it has been held unchanged.
    Held(Combo, Duration),
    /// This combination was held for [`RECORD_HOLD`]; recording is over.
    Done(Combo),
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
    /// Mouse buttons to release locally, for the same reason.
    pub release_buttons_locally: Vec<MouseButton>,
    /// Recording progress, while recording a hotkey.
    pub recording: Option<Recording>,
    /// Tell the peer which of its edges input is coming from: (edge on the
    /// peer, position along it, whether the pointer crossed there).
    pub enter: Option<(Edge, u16, bool)>,
    /// Input came back over the edge: put the local pointer at this
    /// position along our edge.
    pub place: Option<(Edge, u16)>,
    /// Edge crossing was paused (true) or resumed (false).
    pub edge_locked: Option<bool>,
}

/// Pushes against the edge needed to cross (arrive, then push once more),
/// so grazing the edge doesn't switch.
const CROSS_PUSHES: u32 = 2;

#[derive(Debug)]
struct Recorder {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<MouseButton>,
    /// When the held combination last changed.
    since: Instant,
}

impl Recorder {
    fn new(now: Instant) -> Self {
        Recorder {
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            since: now,
        }
    }

    fn combo(&self) -> Combo {
        let keys: Vec<u16> = self.keys.iter().copied().collect();
        let buttons: Vec<MouseButton> = self.buttons.iter().copied().collect();
        Combo::new(&keys, &buttons)
    }
}

#[derive(Debug)]
pub struct Switcher {
    target: Target,
    hotkeys: Hotkeys,
    remote_available: bool,
    /// Keys physically held right now.
    held: HashSet<u16>,
    /// Mouse buttons physically held right now.
    held_buttons: HashSet<MouseButton>,
    /// Presses we swallowed as hotkeys, so their releases (and key repeat)
    /// are swallowed too.
    swallowed_keys: HashSet<u16>,
    swallowed_buttons: HashSet<MouseButton>,
    /// Held keys whose key-down the OS never saw (we swallowed it), so the
    /// OS's key state can't be used to check them.
    hidden_from_os: HashSet<u16>,
    recorder: Option<Recorder>,
    /// Side of this computer's screens where the peer sits (None = off).
    edge: Option<Edge>,
    edge_locked: bool,
    edge_pushes: u32,
}

impl Switcher {
    pub fn new(hotkeys: Hotkeys) -> Self {
        Switcher {
            target: Target::Local,
            hotkeys,
            remote_available: false,
            held: HashSet::new(),
            held_buttons: HashSet::new(),
            swallowed_keys: HashSet::new(),
            swallowed_buttons: HashSet::new(),
            hidden_from_os: HashSet::new(),
            recorder: None,
            edge: None,
            edge_locked: false,
            edge_pushes: 0,
        }
    }

    pub fn target(&self) -> Target {
        self.target
    }

    pub fn remote_available(&self) -> bool {
        self.remote_available
    }

    /// Keys physically held right now.
    pub fn held(&self) -> Vec<u16> {
        self.held.iter().copied().collect()
    }

    /// Forget held keys the OS says are no longer down (only meaningful
    /// while input is local). A key-up we never
    /// saw (lost to another program, a lock screen, a crash) would otherwise
    /// leave a phantom key that stops every hotkey from matching exactly.
    pub fn prune_held(&mut self, still_down: impl Fn(u16) -> bool) {
        let stale: Vec<u16> = self
            .held
            .iter()
            .copied()
            .filter(|k| !self.hidden_from_os.contains(k))
            .filter(|&k| !still_down(k))
            .collect();
        for k in stale {
            tracing::debug!(
                key = format_args!("{k:#04x}"),
                "dropping a key we never saw released"
            );
            self.held.remove(&k);
            self.swallowed_keys.remove(&k);
        }
    }

    pub fn edge(&self) -> Option<Edge> {
        self.edge
    }

    /// Which side of this computer the peer is on, for moving over by
    /// pushing the pointer past that edge (None turns it off).
    pub fn set_edge(&mut self, edge: Option<Edge>) {
        tracing::info!(?edge, "edge crossing");
        self.edge = edge;
        self.edge_pushes = 0;
    }

    pub fn edge_locked(&self) -> bool {
        self.edge_locked
    }

    pub fn set_edge_locked(&mut self, locked: bool) -> Outcome {
        self.edge_locked = locked;
        self.edge_pushes = 0;
        Outcome {
            edge_locked: Some(locked),
            ..Default::default()
        }
    }

    /// A local pointer move. `at_edge` is the position along our edge if the
    /// pointer is on it; `toward` whether it is moving into that edge.
    pub fn edge_push(&mut self, at_edge: Option<u16>, toward: bool) -> Outcome {
        let ready = self.edge.is_some()
            && !self.edge_locked
            && self.target == Target::Local
            && self.remote_available
            && self.held_buttons.is_empty()
            && self.recorder.is_none();
        let (Some(edge), Some(pos), true, true) = (self.edge, at_edge, toward, ready) else {
            self.edge_pushes = 0;
            return Outcome::default();
        };
        self.edge_pushes += 1;
        if self.edge_pushes < CROSS_PUSHES {
            return Outcome::default();
        }
        self.edge_pushes = 0;
        tracing::info!(?edge, pos, "pointer crossed the edge");
        let mut out = self.switch_to(Target::Remote);
        out.enter = Some((edge.opposite(), pos, true));
        out.swallow = true;
        out
    }

    /// The peer says the pointer was pushed back out over its edge at `pos`.
    pub fn return_over_edge(&mut self, pos: u16) -> Outcome {
        let Some(edge) = self.edge else {
            return Outcome::default();
        };
        if self.target != Target::Remote {
            return Outcome::default();
        }
        let mut out = self.switch_to(Target::Local);
        out.place = Some((edge, pos));
        out
    }

    pub fn hotkeys(&self) -> &Hotkeys {
        &self.hotkeys
    }

    pub fn set_hotkeys(&mut self, hotkeys: Hotkeys) {
        tracing::info!(?hotkeys, "hotkeys changed");
        self.hotkeys = hotkeys;
    }

    /// Start recording a combination. Until it finishes, key and button
    /// presses are swallowed (they reach neither machine).
    pub fn start_recording(&mut self) {
        self.recorder = Some(Recorder::new(Instant::now()));
    }

    /// Call regularly while recording (held keys and buttons send no events
    /// of their own). Finishes once the combination was held long enough.
    pub fn tick(&mut self, now: Instant) -> Option<Recording> {
        let rec = self.recorder.as_ref()?;
        let combo = rec.combo();
        if combo.is_empty() {
            return None;
        }
        let held_for = now.saturating_duration_since(rec.since);
        if held_for >= RECORD_HOLD {
            self.recorder = None;
            Some(Recording::Done(combo))
        } else {
            Some(Recording::Held(combo, held_for))
        }
    }

    pub fn cancel_recording(&mut self) {
        self.recorder = None;
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    /// Tell the switcher whether a peer is connected. Losing the peer while
    /// it has input brings input straight back here.
    pub fn set_remote_available(&mut self, available: bool) -> Outcome {
        if available != self.remote_available {
            tracing::info!(available, "peer available for input changed");
        }
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

    fn switch_to(&mut self, t: Target) -> Outcome {
        tracing::info!(target = ?t, held = ?self.held, buttons = ?self.held_buttons, "switching input");
        let mut out = Outcome {
            switched: Some(t),
            ..Default::default()
        };
        match t {
            Target::Remote => {
                // The local OS saw these go down; release them here so the
                // local machine has nothing stuck while input is away.
                out.release_locally = self.held.iter().copied().collect();
                out.release_locally.sort_unstable();
                out.release_buttons_locally = self.held_buttons.iter().copied().collect();
                out.release_buttons_locally.sort_unstable();
            }
            Target::Local => out.send.push(InputEvent::ReleaseAll),
        }
        // Let the peer know which edge faces us, so pushing back out over it
        // returns input even after a switch by shortcut.
        if let (Target::Remote, Some(e)) = (t, self.edge) {
            out.enter = Some((e.opposite(), 32_768, false));
        }
        self.target = t;
        self.edge_pushes = 0;
        out
    }

    /// What is held now, as a combination.
    fn held_combo(&self) -> Combo {
        let keys: Vec<u16> = self.held.iter().copied().collect();
        let buttons: Vec<MouseButton> = self.held_buttons.iter().copied().collect();
        Combo::new(&keys, &buttons)
    }

    /// Which switch (if any) the combination held right now asks for.
    fn hotkey_target(&self) -> Option<(Target, bool)> {
        let held = self.held_combo();
        let hk = &self.hotkeys;
        if held == hk.panic.normalized() {
            return Some((Target::Local, true));
        }
        // Checked by current target first, so one combination can toggle.
        if self.target == Target::Remote && held == hk.to_local.normalized() {
            return Some((Target::Local, false));
        }
        if self.target == Target::Local
            && self.remote_available
            && held == hk.to_remote.normalized()
        {
            return Some((Target::Remote, false));
        }
        None
    }

    /// A first press just completed a combination: switch and swallow it.
    fn fire(&mut self, t: Target, panic: bool) -> Outcome {
        // The panic combination always runs a full switch back here, even if
        // input already looks local, so any stuck state is reset everywhere.
        let mut out = if t == self.target && !panic {
            Outcome::default()
        } else {
            self.switch_to(t)
        };
        out.swallow = true;
        out
    }

    fn record(&mut self, usage: Option<u16>, button: Option<MouseButton>, down: bool) -> Outcome {
        let rec = self.recorder.as_mut().expect("recording");
        match (usage, button, down) {
            (Some(k), _, true) => {
                rec.keys.insert(k);
            }
            (Some(k), _, false) => {
                rec.keys.remove(&k);
            }
            (_, Some(b), true) => {
                rec.buttons.insert(b);
            }
            (_, Some(b), false) => {
                rec.buttons.remove(&b);
            }
            _ => {}
        }
        // Any change restarts the hold timer.
        rec.since = Instant::now();
        Outcome {
            swallow: true,
            recording: Some(Recording::Held(rec.combo(), Duration::ZERO)),
            ..Default::default()
        }
    }

    pub fn on_key(&mut self, usage: u16, down: bool) -> Outcome {
        let out = self.on_key_inner(usage, down);
        if down {
            if out.swallow {
                self.hidden_from_os.insert(usage);
            }
        } else {
            self.hidden_from_os.remove(&usage);
        }
        out
    }

    fn on_key_inner(&mut self, usage: u16, down: bool) -> Outcome {
        let first_press = if down {
            self.held.insert(usage)
        } else {
            self.held.remove(&usage);
            false
        };
        if self.recorder.is_some() {
            let known = self
                .recorder
                .as_ref()
                .is_some_and(|r| r.keys.contains(&usage));
            if (down && first_press) || (!down && known) {
                return self.record(Some(usage), None, down);
            }
            if known {
                // Key repeat of a key in the combination being recorded.
                return Outcome {
                    swallow: true,
                    ..Default::default()
                };
            }
            // Otherwise (e.g. releasing a key pressed before recording
            // began) handle it normally so nothing sticks.
        }
        if down {
            if first_press {
                if self.held_combo() == self.hotkeys.edge_lock.normalized() {
                    self.swallowed_keys.insert(usage);
                    let mut out = self.set_edge_locked(!self.edge_locked);
                    out.swallow = true;
                    return out;
                }
                if let Some((t, panic)) = self.hotkey_target() {
                    tracing::debug!(key = format_args!("{usage:#04x}"), target = ?t, "hotkey");
                    self.swallowed_keys.insert(usage);
                    let mut out = self.fire(t, panic);
                    // The hotkey itself never reaches either machine.
                    out.release_locally.retain(|&k| k != usage);
                    return out;
                }
            } else if self.swallowed_keys.contains(&usage) {
                // Auto-repeat of a hotkey.
                return Outcome {
                    swallow: true,
                    ..Default::default()
                };
            }
        } else if self.swallowed_keys.remove(&usage) {
            return Outcome {
                swallow: true,
                ..Default::default()
            };
        }
        self.route(InputEvent::Key { usage, down })
    }

    pub fn on_mouse(&mut self, ev: InputEvent) -> Outcome {
        if let InputEvent::Button { button, down } = ev {
            let first_press = if down {
                self.held_buttons.insert(button)
            } else {
                self.held_buttons.remove(&button);
                false
            };
            if self.recorder.is_some() {
                let known = self
                    .recorder
                    .as_ref()
                    .is_some_and(|r| r.buttons.contains(&button));
                if (down && first_press) || (!down && known) {
                    return self.record(None, Some(button), down);
                }
            } else if down && first_press {
                if self.held_combo() == self.hotkeys.edge_lock.normalized() {
                    self.swallowed_buttons.insert(button);
                    let mut out = self.set_edge_locked(!self.edge_locked);
                    out.swallow = true;
                    return out;
                }
                if let Some((t, panic)) = self.hotkey_target() {
                    tracing::debug!(?button, target = ?t, "hotkey");
                    self.swallowed_buttons.insert(button);
                    let mut out = self.fire(t, panic);
                    out.release_buttons_locally.retain(|&b| b != button);
                    return out;
                }
            } else if !down && self.swallowed_buttons.remove(&button) {
                return Outcome {
                    swallow: true,
                    ..Default::default()
                };
            }
        }
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
        // Already local: still a full reset (swallowed, peer released, and
        // the platform's switch-back work runs again).
        release(&mut s, keymap::ESC);
        let out = press(&mut s, keymap::ESC);
        assert!(out.swallow);
        assert_eq!(out.switched, Some(Target::Local));
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
        assert!(s.swallowed_keys.is_empty());
    }

    fn btn(s: &mut Switcher, b: MouseButton, down: bool) -> Outcome {
        s.on_mouse(InputEvent::Button { button: b, down })
    }

    #[test]
    fn right_hand_modifiers_also_work() {
        let mut s = ready();
        press(&mut s, RCTRL);
        press(&mut s, RALT);
        assert_eq!(press(&mut s, keymap::RIGHT).switched, Some(Target::Remote));
    }

    #[test]
    fn extra_keys_held_do_not_fire() {
        let mut s = ready();
        press(&mut s, LCTRL);
        press(&mut s, LALT);
        press(&mut s, LSHIFT);
        let out = press(&mut s, keymap::RIGHT);
        assert_eq!(out.switched, None);
        assert!(!out.swallow);
    }

    #[test]
    fn mouse_button_combo_switches() {
        let mut s = ready();
        s.set_hotkeys(Hotkeys {
            to_remote: Combo::new(&[], &[MouseButton::Back, MouseButton::Forward]),
            ..Hotkeys::default()
        });
        assert!(!btn(&mut s, MouseButton::Back, true).swallow);
        let out = btn(&mut s, MouseButton::Forward, true);
        assert!(out.swallow);
        assert_eq!(out.switched, Some(Target::Remote));
        // Back went down locally first, so it is released locally.
        assert_eq!(out.release_buttons_locally, vec![MouseButton::Back]);
        // The trigger's release is swallowed, not forwarded.
        let up = btn(&mut s, MouseButton::Forward, false);
        assert!(up.swallow && up.send.is_empty());
    }

    #[test]
    fn same_combo_for_both_directions_toggles() {
        let mut s = ready();
        let f8 = 0x41;
        let combo = Combo::new(&[LCTRL, f8], &[]);
        s.set_hotkeys(Hotkeys {
            to_remote: combo.clone(),
            to_local: combo,
            ..Hotkeys::default()
        });
        for expect in [Target::Remote, Target::Local, Target::Remote] {
            press(&mut s, LCTRL);
            let out = press(&mut s, f8);
            assert_eq!(out.switched, Some(expect));
            release(&mut s, f8);
            release(&mut s, LCTRL);
        }
    }

    #[test]
    fn recording_needs_the_combo_held_for_two_seconds() {
        let mut s = ready();
        // A key held before recording starts is released normally.
        press(&mut s, A);
        s.start_recording();
        let out = release(&mut s, A);
        assert!(out.recording.is_none() && !out.swallow);
        let out = press(&mut s, RCTRL);
        assert!(out.swallow);
        assert_eq!(
            out.recording,
            Some(Recording::Held(Combo::new(&[LCTRL], &[]), Duration::ZERO))
        );
        let out = btn(&mut s, MouseButton::Middle, true);
        assert!(out.swallow && out.send.is_empty());
        let start = s.recorder.as_ref().unwrap().since;
        let combo = Combo::new(&[LCTRL], &[MouseButton::Middle]);
        assert_eq!(
            s.tick(start + Duration::from_millis(1500)),
            Some(Recording::Held(combo.clone(), Duration::from_millis(1500)))
        );
        assert_eq!(s.tick(start + RECORD_HOLD), Some(Recording::Done(combo)));
        assert!(!s.is_recording());
        // Releasing afterwards behaves normally again.
        assert!(!btn(&mut s, MouseButton::Middle, false).swallow);
    }

    #[test]
    fn letting_go_early_restarts_the_hold() {
        let mut s = ready();
        s.start_recording();
        press(&mut s, LCTRL);
        press(&mut s, A);
        release(&mut s, A);
        let since = s.recorder.as_ref().unwrap().since;
        // Only Ctrl is held now, and the timer restarted at the release.
        assert_eq!(
            s.tick(since + Duration::from_millis(500)),
            Some(Recording::Held(
                Combo::new(&[LCTRL], &[]),
                Duration::from_millis(500)
            ))
        );
        release(&mut s, LCTRL);
        assert_eq!(s.tick(since + RECORD_HOLD * 2), None, "nothing held");
        assert!(s.is_recording());
    }

    #[test]
    fn unsafe_combos_are_rejected() {
        assert!(Combo::new(&[A], &[]).problem().is_some());
        assert!(Combo::new(&[LSHIFT], &[]).problem().is_some());
        assert!(Combo::new(&[], &[MouseButton::Left, MouseButton::Right])
            .problem()
            .is_some());
        assert!(Combo::new(&[LCTRL, A], &[]).problem().is_none());
        assert!(Combo::new(&[], &[MouseButton::Back, MouseButton::Forward])
            .problem()
            .is_none());
    }

    #[test]
    fn phantom_keys_are_dropped_but_swallowed_ones_kept() {
        let mut s = ready();
        // A key-up we never saw leaves A "held"; the OS says it is up.
        press(&mut s, A);
        s.prune_held(|_| false);
        assert!(s.held.is_empty());
        // Keys pressed while input was on the peer: the OS never saw them, so
        // they survive pruning and the come-back hotkey still matches.
        s.set_target(Target::Remote);
        press(&mut s, LCTRL);
        press(&mut s, LALT);
        s.prune_held(|_| false);
        assert_eq!(press(&mut s, keymap::LEFT).switched, Some(Target::Local));
        s.prune_held(|_| false);
        release(&mut s, keymap::LEFT);
        assert_eq!(press(&mut s, keymap::RIGHT).switched, Some(Target::Remote));
    }

    #[test]
    fn pushing_past_the_edge_crosses_over_and_comes_back() {
        let mut s = ready();
        s.set_edge(Some(Edge::Left));
        // Arriving at the edge isn't enough; one more push crosses.
        assert_eq!(s.edge_push(Some(100), true), Outcome::default());
        let out = s.edge_push(Some(100), true);
        assert_eq!(out.switched, Some(Target::Remote));
        assert_eq!(out.enter, Some((Edge::Right, 100, true)));
        assert!(out.swallow);
        // The peer pushes us back out.
        let out = s.return_over_edge(500);
        assert_eq!(out.switched, Some(Target::Local));
        assert_eq!(out.place, Some((Edge::Left, 500)));
        assert_eq!(out.send, vec![InputEvent::ReleaseAll]);
    }

    #[test]
    fn grazing_dragging_or_locked_does_not_cross() {
        let mut s = ready();
        s.set_edge(Some(Edge::Left));
        // Touch the edge, move along it away, touch again: no crossing.
        s.edge_push(Some(1), true);
        s.edge_push(None, true);
        assert_eq!(s.edge_push(Some(1), true).switched, None);
        // Dragging something.
        s.on_mouse(InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        });
        for _ in 0..5 {
            assert_eq!(s.edge_push(Some(1), true).switched, None);
        }
        s.on_mouse(InputEvent::Button {
            button: MouseButton::Left,
            down: false,
        });
        // Locked with Ctrl+Alt+L.
        press(&mut s, LCTRL);
        press(&mut s, LALT);
        let out = press(&mut s, 0x0F);
        assert_eq!(out.edge_locked, Some(true));
        assert!(out.swallow);
        release(&mut s, 0x0F);
        release(&mut s, LALT);
        release(&mut s, LCTRL);
        for _ in 0..5 {
            assert_eq!(s.edge_push(Some(1), true).switched, None);
        }
        s.set_edge_locked(false);
        s.edge_push(Some(1), true);
        assert_eq!(s.edge_push(Some(1), true).switched, Some(Target::Remote));
    }

    #[test]
    fn shortcut_switch_still_tells_the_peer_its_edge() {
        let mut s = ready();
        s.set_edge(Some(Edge::Right));
        let out = s.set_target(Target::Remote);
        assert_eq!(out.enter, Some((Edge::Left, 32_768, false)));
    }
}
