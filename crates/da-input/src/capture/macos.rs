//! macOS capture with a Quartz event tap. Needs the Accessibility permission.
//!
//! While input is remote the cursor is detached from the mouse, hidden and
//! held in the middle of the main display, and the raw deltas Quartz
//! reports are forwarded. Detaching alone isn't reliable (macOS can
//! re-attach it, e.g. after an edge crossing), so any drift is undone.

use super::Shared;
use crate::keymap;
use crate::switcher::Target;
use crate::wire::{split_move, InputEvent, MouseButton};
use crate::INJECTED_MARKER;
use anyhow::{anyhow, Result};
use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::sync::{Arc, Mutex};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
    fn CGEventTapEnable(tap: core_foundation::mach_port::CFMachPortRef, enable: bool);
}

thread_local! {
    /// The tap of the thread that runs its callback, so it can be re-enabled.
    static TAP: std::cell::RefCell<Option<core_foundation::mach_port::CFMachPort>> =
        const { std::cell::RefCell::new(None) };
}

/// While remote: (where the pointer was before, where it is held).
type Point = (f64, f64);
static PARK: Mutex<Option<(Point, Point)>> = Mutex::new(None);

fn cursor_now() -> Option<(f64, f64)> {
    let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let p = CGEvent::new(src).ok()?.location();
    Some((p.x, p.y))
}

/// Hold the pointer still at `p` while it is detached from the mouse.
fn hold_at(p: (f64, f64)) {
    unsafe {
        CGWarpMouseCursorPosition(CGPoint::new(p.0, p.1));
        CGAssociateMouseAndMouseCursorPosition(0);
    }
}

pub(super) fn start(shared: Arc<Shared>) -> Result<()> {
    if !crate::permissions::accessibility_trusted(true) {
        return Err(anyhow!(
            "DisplayAnything needs the Accessibility permission to share this Mac's keyboard and mouse. \
             Turn it on in System Settings > Privacy & Security > Accessibility, then restart the app."
        ));
    }
    if !crate::permissions::input_monitoring_granted(true) {
        // The tap still works for the mouse, but macOS withholds keystrokes.
        tracing::warn!("Input Monitoring is not granted; keystrokes will not be captured");
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    std::thread::Builder::new()
        .name("da-input-tap".into())
        .spawn(move || {
            let events = vec![
                CGEventType::KeyDown,
                CGEventType::KeyUp,
                CGEventType::FlagsChanged,
                CGEventType::MouseMoved,
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
                CGEventType::LeftMouseDragged,
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
                CGEventType::RightMouseDragged,
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGEventType::OtherMouseDragged,
                CGEventType::ScrollWheel,
            ];
            let tap = CGEventTap::new(
                CGEventTapLocation::HID,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                events,
                move |_proxy, ty, event| {
                    if handle(&shared, ty, event) {
                        // core-graphics passes the original event through
                        // when the callback returns None, so blank it out:
                        // a Null event is dropped by macOS.
                        event.set_type(CGEventType::Null);
                    }
                    None
                },
            );
            let tap = match tap {
                Ok(t) => t,
                Err(()) => {
                    let _ = ready_tx.send(Err(anyhow!(
                        "could not create the keyboard/mouse event tap (is Accessibility allowed?)"
                    )));
                    return;
                }
            };
            TAP.with(|t| *t.borrow_mut() = Some(tap.mach_port.clone()));
            unsafe {
                let Ok(source) = tap.mach_port.create_runloop_source(0) else {
                    let _ = ready_tx.send(Err(anyhow!("could not attach the event tap")));
                    return;
                };
                CFRunLoop::get_current().add_source(&source, kCFRunLoopCommonModes);
                tap.enable();
                let _ = ready_tx.send(Ok(()));
                CFRunLoop::run_current();
            }
        })?;
    ready_rx
        .recv()
        .map_err(|_| anyhow!("event tap thread exited"))?
}

/// Whether macOS thinks this key is held (None if we can't tell).
pub(super) fn key_is_down(usage: u16) -> Option<bool> {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    }
    const HID_SYSTEM_STATE: i32 = 1;
    let code = keymap::hid_to_mac(usage)?;
    if code == 0x39 {
        return None; // Caps Lock reports its lock state, not the key
    }
    Some(unsafe { CGEventSourceKeyState(HID_SYSTEM_STATE, code) })
}

pub(super) fn release_local_key(usage: u16) {
    crate::inject::macos_send_key(usage, false);
}

pub(super) fn on_switch(t: Target, _held: Vec<u16>) {
    let display = CGDisplay::main();
    unsafe {
        match t {
            Target::Remote => {
                let b = display.bounds();
                let centre = (
                    b.origin.x + b.size.width / 2.0,
                    b.origin.y + b.size.height / 2.0,
                );
                let saved = cursor_now().unwrap_or(centre);
                *PARK.lock().unwrap() = Some((saved, centre));
                hold_at(centre);
                let _ = display.hide_cursor();
            }
            Target::Local => {
                let park = PARK.lock().unwrap().take();
                if let Some((x, y)) = super::take_return_pos() {
                    // Came back over the edge: appear where we crossed.
                    crate::inject::warp(x, y);
                } else if let Some(((x, y), _)) = park {
                    // Came back by shortcut: appear where we left.
                    crate::inject::warp(x, y);
                }
                CGAssociateMouseAndMouseCursorPosition(1);
                let _ = display.show_cursor();
            }
        }
    }
}

/// Device-specific modifier bits in the event flags, per keycode, used to
/// tell a modifier press from a release in FlagsChanged events.
fn modifier_mask(keycode: u16) -> Option<u64> {
    Some(match keycode {
        0x3B => 0x0000_0001, // left control
        0x38 => 0x0000_0002, // left shift
        0x3C => 0x0000_0004, // right shift
        0x37 => 0x0000_0008, // left command
        0x36 => 0x0000_0010, // right command
        0x3A => 0x0000_0020, // left option
        0x3D => 0x0000_0040, // right option
        0x3E => 0x0000_2000, // right control
        _ => return None,
    })
}

/// Returns true if the event must be swallowed.
fn handle(shared: &Shared, ty: CGEventType, e: &CGEvent) -> bool {
    // macOS turns a tap off if its callback is slow, or when the user input
    // is locked down (e.g. a password prompt). Turn it back on at once,
    // otherwise all input capture stays dead until the app restarts.
    if matches!(
        ty,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        tracing::warn!(?ty, "macOS turned the input tap off; switching it back on");
        TAP.with(|t| {
            if let Some(port) = t.borrow().as_ref() {
                unsafe { CGEventTapEnable(port.as_concrete_TypeRef(), true) };
            }
        });
        return false;
    }
    if e.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == INJECTED_MARKER as i64 {
        return false;
    }
    let keycode = || e.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
    match ty {
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let Some(usage) = keymap::mac_to_hid(keycode()) else {
                return shared.is_remote();
            };
            shared.key(usage, matches!(ty, CGEventType::KeyDown))
        }
        CGEventType::FlagsChanged => {
            let kc = keycode();
            let Some(usage) = keymap::mac_to_hid(kc) else {
                return false;
            };
            if kc == 0x39 {
                // Caps Lock reports one event per press; send a full tap.
                let a = shared.key(usage, true);
                let b = shared.key(usage, false);
                return a || b;
            }
            let Some(mask) = modifier_mask(kc) else {
                return false;
            };
            let down = e.get_flags().bits() & mask != 0;
            shared.key(usage, down)
        }
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let dx = e.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32;
            let dy = e.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32;
            if !shared.is_remote() {
                let p = e.location();
                return shared.local_move(p.x, p.y, Some((f64::from(dx), f64::from(dy))));
            }
            if dx != 0 || dy != 0 {
                for m in split_move(dx, dy) {
                    shared.mouse(m);
                }
            }
            // If macOS let the pointer move anyway, put it back.
            let park = PARK.lock().unwrap().map(|p| p.1);
            if let Some(c) = park {
                let p = e.location();
                if (p.x - c.0).abs() > 1.0 || (p.y - c.1).abs() > 1.0 {
                    hold_at(c);
                }
            }
            true
        }
        CGEventType::LeftMouseDown => shared.mouse(InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        }),
        CGEventType::LeftMouseUp => shared.mouse(InputEvent::Button {
            button: MouseButton::Left,
            down: false,
        }),
        CGEventType::RightMouseDown => shared.mouse(InputEvent::Button {
            button: MouseButton::Right,
            down: true,
        }),
        CGEventType::RightMouseUp => shared.mouse(InputEvent::Button {
            button: MouseButton::Right,
            down: false,
        }),
        CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => {
            let button = match e.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER) {
                3 => MouseButton::Back,
                4 => MouseButton::Forward,
                _ => MouseButton::Middle,
            };
            shared.mouse(InputEvent::Button {
                button,
                down: matches!(ty, CGEventType::OtherMouseDown),
            })
        }
        CGEventType::ScrollWheel => {
            let continuous =
                e.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0;
            let (dx, dy) = if continuous {
                (
                    e.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2)
                        * 4,
                    e.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1)
                        * 4,
                )
            } else {
                (
                    e.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2) * 120,
                    e.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1) * 120,
                )
            };
            let clamp = |v: i64| v.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
            shared.mouse(InputEvent::Wheel {
                dx: clamp(dx),
                dy: clamp(dy),
            })
        }
        _ => false,
    }
}
