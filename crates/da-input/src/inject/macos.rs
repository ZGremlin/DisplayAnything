//! macOS injection via Quartz events. Needs the Accessibility permission.

use super::Backend;
use crate::geometry::Rect;
use crate::keymap::{self, LALT, LCTRL, LMETA, LSHIFT, RALT, RCTRL, RMETA, RSHIFT};
use crate::wire::MouseButton;
use crate::INJECTED_MARKER;
use anyhow::{anyhow, Result};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect};
use std::time::{Duration, Instant};

const DOUBLE_CLICK: Duration = Duration::from_millis(500);

pub struct Platform {
    source: CGEventSource,
    flags: CGEventFlags,
    buttons: [bool; 6],
    last_click: Option<(MouseButton, Instant, CGPoint, i64)>,
}

impl Platform {
    pub fn new() -> Result<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow!("could not create a Quartz event source"))?;
        Ok(Platform {
            source,
            flags: CGEventFlags::CGEventFlagNull,
            buttons: [false; 6],
            last_click: None,
        })
    }

    fn post(&self, e: CGEvent) {
        e.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, INJECTED_MARKER as i64);
        e.post(CGEventTapLocation::HID);
    }

    fn cursor(&self) -> CGPoint {
        CGEvent::new(self.source.clone())
            .map(|e| e.location())
            .unwrap_or(CGPoint::new(0.0, 0.0))
    }

    fn any_button_down(&self) -> Option<MouseButton> {
        if self.buttons[1] {
            Some(MouseButton::Left)
        } else if self.buttons[2] {
            Some(MouseButton::Right)
        } else if self.buttons[3] || self.buttons[4] || self.buttons[5] {
            Some(MouseButton::Middle)
        } else {
            None
        }
    }
}

fn modifier_flag(usage: u16) -> Option<CGEventFlags> {
    Some(match usage {
        LSHIFT | RSHIFT => CGEventFlags::CGEventFlagShift,
        LCTRL | RCTRL => CGEventFlags::CGEventFlagControl,
        LALT | RALT => CGEventFlags::CGEventFlagAlternate,
        LMETA | RMETA => CGEventFlags::CGEventFlagCommand,
        _ => return None,
    })
}

/// Clamp a point to the union of all displays.
fn clamp_to_displays(p: CGPoint) -> CGPoint {
    let ids = CGDisplay::active_displays().unwrap_or_default();
    let rects: Vec<CGRect> = ids
        .into_iter()
        .map(|id| CGDisplay::new(id).bounds())
        .collect();
    if rects.is_empty() {
        return p;
    }
    // Inside a display already: keep it.
    for r in &rects {
        if p.x >= r.origin.x
            && p.x < r.origin.x + r.size.width
            && p.y >= r.origin.y
            && p.y < r.origin.y + r.size.height
        {
            return p;
        }
    }
    // Otherwise snap to the nearest point on the nearest display.
    let mut best = p;
    let mut best_d = f64::MAX;
    for r in &rects {
        let x = p.x.clamp(r.origin.x, r.origin.x + r.size.width - 1.0);
        let y = p.y.clamp(r.origin.y, r.origin.y + r.size.height - 1.0);
        let d = (x - p.x).powi(2) + (y - p.y).powi(2);
        if d < best_d {
            best_d = d;
            best = CGPoint::new(x, y);
        }
    }
    best
}

fn button_index(b: MouseButton) -> usize {
    match b {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 3,
        MouseButton::Back => 4,
        MouseButton::Forward => 5,
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
    fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
}

impl Backend for Platform {
    fn cursor(&self) -> (f64, f64) {
        let p = Platform::cursor(self);
        (p.x, p.y)
    }

    fn screens(&self) -> Vec<Rect> {
        CGDisplay::active_displays()
            .unwrap_or_default()
            .into_iter()
            .map(|id| {
                let b = CGDisplay::new(id).bounds();
                Rect::new(b.origin.x, b.origin.y, b.size.width, b.size.height)
            })
            .collect()
    }

    fn warp(&mut self, x: f64, y: f64) {
        unsafe {
            CGWarpMouseCursorPosition(CGPoint::new(x, y));
            // Without this macOS ignores real mouse moves for a moment.
            CGAssociateMouseAndMouseCursorPosition(1);
        }
    }

    fn key(&mut self, usage: u16, down: bool) {
        if let Some(f) = modifier_flag(usage) {
            if down {
                self.flags |= f;
            } else {
                self.flags.remove(f);
            }
        }
        let Some(code) = keymap::hid_to_mac(usage) else {
            return;
        };
        if let Ok(e) = CGEvent::new_keyboard_event(self.source.clone(), code, down) {
            e.set_flags(self.flags);
            self.post(e);
        }
    }

    fn move_to(&mut self, x: f64, y: f64) {
        let cur = self.cursor();
        let (dx, dy) = ((x - cur.x).round() as i32, (y - cur.y).round() as i32);
        if dx != 0 || dy != 0 {
            self.mouse_move(dx, dy);
        }
    }

    fn mouse_move(&mut self, dx: i32, dy: i32) {
        let cur = self.cursor();
        let p = clamp_to_displays(CGPoint::new(cur.x + dx as f64, cur.y + dy as f64));
        let (ty, btn) = match self.any_button_down() {
            Some(MouseButton::Left) => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
            Some(MouseButton::Right) => (CGEventType::RightMouseDragged, CGMouseButton::Right),
            Some(_) => (CGEventType::OtherMouseDragged, CGMouseButton::Center),
            None => (CGEventType::MouseMoved, CGMouseButton::Left),
        };
        if let Ok(e) = CGEvent::new_mouse_event(self.source.clone(), ty, p, btn) {
            e.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, dx as i64);
            e.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, dy as i64);
            e.set_flags(self.flags);
            self.post(e);
        }
    }

    fn button(&mut self, button: MouseButton, down: bool) {
        let p = self.cursor();
        let (ty, cg, number) = match (button, down) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left, 0),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left, 0),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right, 1),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right, 1),
            (b, true) => (
                CGEventType::OtherMouseDown,
                CGMouseButton::Center,
                other_number(b),
            ),
            (b, false) => (
                CGEventType::OtherMouseUp,
                CGMouseButton::Center,
                other_number(b),
            ),
        };
        // Count clicks so double- and triple-clicks work.
        let clicks = if down {
            let n = match self.last_click {
                Some((b, t, at, n))
                    if b == button
                        && t.elapsed() < DOUBLE_CLICK
                        && (at.x - p.x).abs() < 4.0
                        && (at.y - p.y).abs() < 4.0 =>
                {
                    n + 1
                }
                _ => 1,
            };
            self.last_click = Some((button, Instant::now(), p, n));
            n
        } else {
            self.last_click.map(|c| c.3).unwrap_or(1)
        };
        self.buttons[button_index(button)] = down;
        if let Ok(e) = CGEvent::new_mouse_event(self.source.clone(), ty, p, cg) {
            e.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, clicks);
            e.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, number);
            e.set_flags(self.flags);
            self.post(e);
        }
    }

    fn wheel(&mut self, dx: i32, dy: i32) {
        // 120 units = one notch = one line.
        let (lx, ly) = (dx / 120, dy / 120);
        let e = if lx != 0 || ly != 0 {
            CGEvent::new_scroll_event(self.source.clone(), ScrollEventUnit::LINE, 2, ly, lx, 0)
        } else {
            // Smooth (high-resolution) scrolling: send pixels.
            CGEvent::new_scroll_event(
                self.source.clone(),
                ScrollEventUnit::PIXEL,
                2,
                dy / 4,
                dx / 4,
                0,
            )
        };
        if let Ok(e) = e {
            self.post(e);
        }
    }
}

fn other_number(b: MouseButton) -> i64 {
    match b {
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
        _ => 2,
    }
}

/// Post one key event (used by capture to release local keys).
pub(crate) fn send_key(usage: u16, down: bool) {
    if let Ok(mut p) = Platform::new() {
        p.key(usage, down);
    }
}
