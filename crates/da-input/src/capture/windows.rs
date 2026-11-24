//! Windows capture with low-level keyboard and mouse hooks.
//!
//! While input is remote the pointer is parked in the middle of the primary
//! screen and every move is blocked; the distance the OS *would* have moved
//! it is the delta we forward. Events we injected ourselves are ignored.

use super::Shared;
use crate::keymap;
use crate::switcher::Target;
use crate::wire::{split_move, InputEvent, MouseButton};
use anyhow::{anyhow, Result};
use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_NUMLOCK, VK_PAUSE};
use windows::Win32::UI::WindowsAndMessaging::*;

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
/// (where the pointer was before going remote, where it is parked)
static PARK: Mutex<Option<(POINT, POINT)>> = Mutex::new(None);

pub(super) fn start(shared: Arc<Shared>) -> Result<()> {
    SHARED
        .set(shared)
        .map_err(|_| anyhow!("input capture is already running"))?;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    std::thread::Builder::new()
        .name("da-input-hooks".into())
        .spawn(move || unsafe {
            let module = match GetModuleHandleW(None) {
                Ok(m) => HINSTANCE(m.0),
                Err(e) => {
                    let _ = ready_tx.send(Err(anyhow!("GetModuleHandle failed: {e}")));
                    return;
                }
            };
            let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
            let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
            match (kb, ms) {
                (Ok(_), Ok(_)) => {
                    let _ = ready_tx.send(Ok(()));
                }
                (Err(e), _) | (_, Err(e)) => {
                    let _ = ready_tx.send(Err(anyhow!("could not install input hooks: {e}")));
                    return;
                }
            }
            // Hooks are serviced by this thread's message loop.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })?;
    ready_rx
        .recv()
        .map_err(|_| anyhow!("input hook thread exited"))?
}

pub(super) fn release_local_key(usage: u16) {
    crate::inject::windows_send_key(usage, false);
}

pub(super) fn on_switch(t: Target) {
    unsafe {
        let mut park = PARK.lock().unwrap();
        match t {
            Target::Remote => {
                let mut saved = POINT::default();
                let _ = GetCursorPos(&mut saved);
                let centre = POINT {
                    x: GetSystemMetrics(SM_CXSCREEN) / 2,
                    y: GetSystemMetrics(SM_CYSCREEN) / 2,
                };
                let _ = SetCursorPos(centre.x, centre.y);
                *park = Some((saved, centre));
            }
            Target::Local => {
                if let Some((saved, _)) = park.take() {
                    let _ = SetCursorPos(saved.x, saved.y);
                }
            }
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(shared) = SHARED.get() {
            let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if k.flags.0 & LLKHF_INJECTED.0 == 0 {
                let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let usage = if k.vkCode == VK_PAUSE.0 as u32 {
                    Some(0x48)
                } else if k.vkCode == VK_NUMLOCK.0 as u32 {
                    Some(0x53)
                } else {
                    let mut sc = (k.scanCode & 0xFF) as u16;
                    if k.flags.0 & LLKHF_EXTENDED.0 != 0 {
                        sc |= 0xE000;
                    }
                    keymap::win_to_hid(sc)
                };
                if let Some(u) = usage {
                    if shared.key(u, down) {
                        return LRESULT(1);
                    }
                }
            }
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(shared) = SHARED.get() {
            let m = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            if m.flags & LLMHF_INJECTED == 0 && handle_mouse(shared, wparam.0 as u32, m) {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

fn handle_mouse(shared: &Shared, msg: u32, m: &MSLLHOOKSTRUCT) -> bool {
    let wheel = ((m.mouseData >> 16) & 0xFFFF) as u16 as i16;
    let xbutton = if (m.mouseData >> 16) & 0xFFFF == 2 {
        MouseButton::Forward
    } else {
        MouseButton::Back
    };
    let ev = match msg {
        WM_MOUSEMOVE => {
            if !shared.is_remote() {
                return false;
            }
            let park = PARK.lock().unwrap().map(|p| p.1).unwrap_or(m.pt);
            let (dx, dy) = (m.pt.x - park.x, m.pt.y - park.y);
            if dx != 0 || dy != 0 {
                for e in split_move(dx, dy) {
                    shared.mouse(e);
                }
            }
            return true;
        }
        WM_LBUTTONDOWN => InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        },
        WM_LBUTTONUP => InputEvent::Button {
            button: MouseButton::Left,
            down: false,
        },
        WM_RBUTTONDOWN => InputEvent::Button {
            button: MouseButton::Right,
            down: true,
        },
        WM_RBUTTONUP => InputEvent::Button {
            button: MouseButton::Right,
            down: false,
        },
        WM_MBUTTONDOWN => InputEvent::Button {
            button: MouseButton::Middle,
            down: true,
        },
        WM_MBUTTONUP => InputEvent::Button {
            button: MouseButton::Middle,
            down: false,
        },
        WM_XBUTTONDOWN => InputEvent::Button {
            button: xbutton,
            down: true,
        },
        WM_XBUTTONUP => InputEvent::Button {
            button: xbutton,
            down: false,
        },
        WM_MOUSEWHEEL => InputEvent::Wheel { dx: 0, dy: wheel },
        WM_MOUSEHWHEEL => InputEvent::Wheel { dx: wheel, dy: 0 },
        _ => return false,
    };
    shared.mouse(ev)
}
