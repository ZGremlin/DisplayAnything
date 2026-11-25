//! Keys typed into one of our own windows (Windows only).
//!
//! The low-level keyboard hook can miss keys while a window of this app has
//! focus (seen on a real PC). The remote screen window is one of ours and is
//! meant to have focus, so it also catches the key messages Windows sends
//! it and forwards them. Keys the hook does see are swallowed by the hook
//! and never reach the window, so nothing is sent twice.

use crate::capture::GrabEvent;
use crate::keymap;
use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    WM_KEYDOWN, WM_KEYUP, WM_NCDESTROY, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

const SUBCLASS_ID: usize = 0x4441_4B59; // "DAKY"

/// Forward keys sent to `hwnd` as [`GrabEvent::Key`]. Call on the thread
/// that owns the window. The hook is removed when the window is destroyed.
pub fn attach(hwnd: isize, tx: UnboundedSender<GrabEvent>) -> bool {
    let data = Box::into_raw(Box::new(tx)) as usize;
    let ok = unsafe { SetWindowSubclass(HWND(hwnd as *mut _), Some(proc_), SUBCLASS_ID, data) };
    if !ok.as_bool() {
        drop(unsafe { Box::from_raw(data as *mut UnboundedSender<GrabEvent>) });
    }
    ok.as_bool()
}

unsafe extern "system" fn proc_(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    let tx = &*(data as *const UnboundedSender<GrabEvent>);
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let down = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN);
            let l = lparam.0 as usize;
            let mut sc = ((l >> 16) & 0xFF) as u16;
            if l & (1 << 24) != 0 {
                sc |= 0xE000;
            }
            if let Some(usage) = keymap::win_to_hid(sc) {
                let _ = tx.send(GrabEvent::Key { usage, down });
            }
            // Handled: no Alt menu or beeps in the viewer window.
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(proc_), id);
            drop(Box::from_raw(data as *mut UnboundedSender<GrabEvent>));
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}
