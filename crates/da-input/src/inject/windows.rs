//! Windows injection via `SendInput`, using scancodes so the target's own
//! keyboard layout applies, and absolute pointer positions so Windows does
//! not apply pointer acceleration a second time.

use super::Backend;
use crate::geometry::Rect;
use crate::keymap;
use crate::wire::MouseButton;
use crate::INJECTED_MARKER;
use anyhow::Result;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, XBUTTON1, XBUTTON2,
};

pub struct Platform;

impl Platform {
    pub fn new() -> Result<Self> {
        Ok(Platform)
    }
}

fn send(input: INPUT) {
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_MARKER,
            },
        },
    }
}

/// Send a key-up or key-down by HID usage (also used by capture to release
/// local keys when input moves away).
pub(crate) fn send_key(usage: u16, down: bool) {
    let Some(sc) = keymap::hid_to_win(usage) else {
        return;
    };
    let mut flags = KEYEVENTF_SCANCODE;
    if sc & 0xE000 == 0xE000 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    send(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: sc & 0x00FF,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_MARKER,
            },
        },
    });
}

impl Backend for Platform {
    fn key(&mut self, usage: u16, down: bool) {
        send_key(usage, down);
    }

    fn cursor(&self) -> (f64, f64) {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        (f64::from(p.x), f64::from(p.y))
    }

    /// The virtual desktop (all monitors). Windows keeps the pointer on a
    /// real monitor by itself when we place it in a gap.
    fn screens(&self) -> Vec<Rect> {
        unsafe {
            vec![Rect::new(
                f64::from(GetSystemMetrics(SM_XVIRTUALSCREEN)),
                f64::from(GetSystemMetrics(SM_YVIRTUALSCREEN)),
                f64::from(GetSystemMetrics(SM_CXVIRTUALSCREEN)),
                f64::from(GetSystemMetrics(SM_CYVIRTUALSCREEN)),
            )]
        }
    }

    fn warp(&mut self, x: f64, y: f64) {
        unsafe {
            let _ = SetCursorPos(x.round() as i32, y.round() as i32);
        }
    }

    fn move_to(&mut self, x: f64, y: f64) {
        let mut p = POINT::default();
        unsafe {
            if GetCursorPos(&mut p).is_err() {
                return;
            }
        }
        let (dx, dy) = (x.round() as i32 - p.x, y.round() as i32 - p.y);
        if dx != 0 || dy != 0 {
            self.mouse_move(dx, dy);
        }
    }

    fn mouse_move(&mut self, dx: i32, dy: i32) {
        unsafe {
            let mut p = POINT::default();
            if GetCursorPos(&mut p).is_err() {
                return;
            }
            let (vx, vy) = (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
            );
            let (vw, vh) = (
                GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
                GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
            );
            let x = (p.x + dx).clamp(vx, vx + vw - 1);
            let y = (p.y + dy).clamp(vy, vy + vh - 1);
            // Absolute coordinates are 0..65535 across the virtual desktop.
            let nx = ((x - vx) as i64 * 65535 / (vw - 1).max(1) as i64) as i32;
            let ny = ((y - vy) as i64 * 65535 / (vh - 1).max(1) as i64) as i32;
            send(mouse(
                nx,
                ny,
                0,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            ));
        }
    }

    fn button(&mut self, button: MouseButton, down: bool) {
        let (flags, data) = match (button, down) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as i32),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1 as i32),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as i32),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2 as i32),
        };
        send(mouse(0, 0, data, flags));
    }

    fn wheel(&mut self, dx: i32, dy: i32) {
        if dy != 0 {
            send(mouse(0, 0, dy, MOUSEEVENTF_WHEEL));
        }
        if dx != 0 {
            send(mouse(0, 0, dx, MOUSEEVENTF_HWHEEL));
        }
    }
}
