//! Windows capture with low-level keyboard and mouse hooks.
//!
//! While input is remote the pointer is parked in the middle of the primary
//! screen and every move is blocked; the distance the OS *would* have moved
//! it is the delta we forward. Events we injected ourselves are ignored.

use super::windows_focus;
use super::Shared;
use crate::keymap;
use crate::switcher::Target;
use crate::wire::{split_move, InputEvent, MouseButton};
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX, VIRTUAL_KEY, VK_LCONTROL, VK_LMENU,
    VK_LSHIFT, VK_LWIN, VK_NUMLOCK, VK_PAUSE, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::*;

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
/// (where the pointer was before going remote, where it is parked)
static PARK: Mutex<Option<(POINT, POINT)>> = Mutex::new(None);
/// Last local pointer position seen by the mouse hook.
static PREV_PT: Mutex<Option<POINT>> = Mutex::new(None);
/// Thread that owns the hooks, so it can be asked to reinstall them.
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
/// Slow work on a switch (moving the pointer, releasing keys) runs here,
/// never inside a hook callback.
static SWITCH_JOBS: OnceLock<Mutex<Sender<SwitchJob>>> = OnceLock::new();
/// The new target, and the keys physically held at the moment of switching.
type SwitchJob = (Target, Vec<u16>);

/// Posted to the hook thread to reinstall both hooks.
const WM_REHOOK: u32 = WM_APP + 1;

const MODIFIERS: [(u16, VIRTUAL_KEY); 8] = [
    (keymap::LCTRL, VK_LCONTROL),
    (keymap::RCTRL, VK_RCONTROL),
    (keymap::LALT, VK_LMENU),
    (keymap::RALT, VK_RMENU),
    (keymap::LSHIFT, VK_LSHIFT),
    (keymap::RSHIFT, VK_RSHIFT),
    (keymap::LMETA, VK_LWIN),
    (keymap::RMETA, VK_RWIN),
];

unsafe fn install_hooks(module: HINSTANCE) -> Result<(HHOOK, HHOOK)> {
    let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0)
        .map_err(|e| anyhow!("could not install the keyboard hook: {e}"))?;
    let ms = match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0) {
        Ok(h) => h,
        Err(e) => {
            let _ = UnhookWindowsHookEx(kb);
            return Err(anyhow!("could not install the mouse hook: {e}"));
        }
    };
    Ok((kb, ms))
}

pub(super) fn start(shared: Arc<Shared>) -> Result<()> {
    SHARED
        .set(shared)
        .map_err(|_| anyhow!("input capture is already running"))?;

    let (job_tx, job_rx) = channel::<SwitchJob>();
    let _ = SWITCH_JOBS.set(Mutex::new(job_tx));
    std::thread::Builder::new()
        .name("da-input-switch".into())
        .spawn(move || {
            for (t, held) in job_rx {
                apply_switch(t, &held);
            }
        })?;

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
            // Make sure this thread has a message queue before anyone posts to it.
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            HOOK_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
            let mut hooks = match install_hooks(module) {
                Ok(h) => {
                    let _ = ready_tx.send(Ok(()));
                    h
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // Hooks are serviced by this thread's message loop.
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_REHOOK {
                    // Windows silently drops a low-level hook whose callback
                    // was ever too slow; reinstalling on each switch means a
                    // dropped keyboard hook can't leave keys going to the
                    // wrong machine.
                    let _ = UnhookWindowsHookEx(hooks.0);
                    let _ = UnhookWindowsHookEx(hooks.1);
                    match install_hooks(module) {
                        Ok(h) => {
                            tracing::debug!("input hooks reinstalled");
                            hooks = h
                        }
                        Err(e) => tracing::error!("{e:#}"),
                    }
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })?;
    ready_rx
        .recv()
        .map_err(|_| anyhow!("input hook thread exited"))??;
    windows_focus::start(
        || SHARED.get().is_some_and(|s| s.is_remote()),
        request_rehook,
    );
    Ok(())
}

/// Ask the hook thread to reinstall both hooks.
fn request_rehook() {
    let tid = HOOK_THREAD.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_REHOOK, WPARAM(0), LPARAM(0));
        }
    }
}

/// Whether Windows thinks this key is held (None if we can't tell).
pub(super) fn key_is_down(usage: u16) -> Option<bool> {
    let sc = keymap::hid_to_win(usage)?;
    let ext = if sc & 0xE000 == 0xE000 { 0xE000 } else { 0 };
    let vk = unsafe { MapVirtualKeyW(u32::from(sc & 0xFF) | ext, MAPVK_VSC_TO_VK_EX) };
    if vk == 0 {
        return None;
    }
    Some(unsafe { GetAsyncKeyState(vk as i32) } < 0)
}

pub(super) fn release_local_key(usage: u16) {
    crate::inject::windows_send_key(usage, false);
}

/// Called from the hook callback (hotkeys) or the app (buttons). Only queues
/// the work: a hook callback that blocks gets its hook removed by Windows.
pub(super) fn on_switch(t: Target, held: Vec<u16>) {
    if let Some(tx) = SWITCH_JOBS.get() {
        let _ = tx.lock().unwrap().send((t, held));
    }
}

fn apply_switch(t: Target, held: &[u16]) {
    unsafe {
        match t {
            Target::Remote => {
                let mut saved = POINT::default();
                let _ = GetCursorPos(&mut saved);
                let centre = POINT {
                    x: GetSystemMetrics(SM_CXSCREEN) / 2,
                    y: GetSystemMetrics(SM_CYSCREEN) / 2,
                };
                let _ = SetCursorPos(centre.x, centre.y);
                *PARK.lock().unwrap() = Some((saved, centre));
                windows_focus::switching_over();
            }
            Target::Local => {
                let saved = PARK.lock().unwrap().take();
                if let Some((x, y)) = super::take_return_pos() {
                    // Came back over the edge: appear where we crossed.
                    let _ = SetCursorPos(x.round() as i32, y.round() as i32);
                } else if let Some((saved, _)) = saved {
                    let _ = SetCursorPos(saved.x, saved.y);
                }
                // Windows may still think a modifier is down if its key-up
                // went to the other machine; let go of any modifier that
                // isn't physically held now.
                for (usage, vk) in MODIFIERS {
                    if !held.contains(&usage) && GetAsyncKeyState(vk.0 as i32) < 0 {
                        crate::inject::windows_send_key(usage, false);
                    }
                }
            }
        }
    }
    request_rehook();
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(shared) = SHARED.get() {
            let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            windows_focus::hook_saw(
                k.vkCode,
                matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN),
            );
            if k.flags.0 & LLKHF_INJECTED.0 != 0 && k.dwExtraInfo != crate::INJECTED_MARKER {
                // Injected by some other program (a remapper, a remote tool):
                // we pass these through untouched, which matters if keys go missing.
                tracing::debug!(vk = k.vkCode, "ignoring key injected by another program");
            }
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
                match usage {
                    Some(u) => {
                        if shared.key(u, down) {
                            return LRESULT(1);
                        }
                    }
                    None => tracing::debug!(
                        scancode = k.scanCode,
                        flags = k.flags.0,
                        "key with no mapping"
                    ),
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
                // Windows gives no raw delta here, so derive one from the
                // previous position. Pushing into an edge the pointer can't
                // pass reads as "no movement"; sliding along it does not.
                let mut prev = PREV_PT.lock().unwrap();
                let (dx, dy) = prev.map(|p| (m.pt.x - p.x, m.pt.y - p.y)).unwrap_or((0, 0));
                *prev = Some(m.pt);
                drop(prev);
                let pushing = dx == 0 && dy == 0;
                let delta = if pushing {
                    None
                } else {
                    Some((f64::from(dx), f64::from(dy)))
                };
                return shared.local_move(f64::from(m.pt.x), f64::from(m.pt.y), delta);
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
