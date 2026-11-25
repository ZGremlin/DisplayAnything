//! Keyboard health on Windows.
//!
//! Seen on a real PC: while a window of this app (the DisplayAnything
//! window, or any other window of ours) is in front, Windows sends keys
//! straight to it and the low-level keyboard hook never sees them, so typing
//! stays on the PC. Pressing the Windows key fixed it because the Start menu
//! took focus away from us.
//!
//! So when input moves to the other computer while one of our windows is in
//! front, focus moves to the taskbar instead. A watchdog also compares what
//! Windows says is held with what the hook saw; if the hook missed a key, it
//! reinstalls the hooks and, if one of our windows is in front, moves focus
//! off it the same way.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
    QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Keys the hook saw go down and not yet up, by virtual-key code.
static SEEN: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
/// Keys worth watching: ones people press to type or switch. Generic
/// Shift/Ctrl/Alt codes are left out (the hook reports the left/right ones).
fn watched() -> impl Iterator<Item = u8> {
    [
        0x08u8, 0x09, 0x0D, 0x1B, 0x20, 0x25, 0x26, 0x27, 0x28, 0x5B, 0x5C,
    ]
    .into_iter()
    .chain(0x30..=0x39)
    .chain(0x41..=0x5A)
    .chain(0x70..=0x7B)
    .chain(0xA0..=0xA5)
}

/// Called by the keyboard hook for every key event it sees.
pub(super) fn hook_saw(vk: u32, down: bool) {
    SEEN[(vk & 0xFF) as usize].store(down, Ordering::Relaxed);
}

/// Input is moving to the other computer (called off the hook thread).
pub(super) fn switching_over() {
    let fg = unsafe { GetForegroundWindow() };
    tracing::info!(window = %describe(fg), "focus when switching over");
    move_off_own_window(fg);
}

/// Start the watchdog. `is_remote` says where input is; `rehook`
/// reinstalls the hooks.
pub(super) fn start(is_remote: fn() -> bool, rehook: fn(), remote_control: fn() -> bool) {
    let _ = std::thread::Builder::new()
        .name("da-input-watchdog".into())
        .spawn(move || watchdog(is_remote, rehook, remote_control));
    tracing::info!(window = %describe(unsafe { GetForegroundWindow() }), "keyboard capture started");
}

/// Whether this window belongs to this app.
fn is_ours(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

/// If one of our windows is in front, give focus to the taskbar (what the
/// Start menu did for the user), so the keyboard hook gets keys again.
fn move_off_own_window(fg: HWND) {
    if !is_ours(fg) {
        return;
    }
    match unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) } {
        Ok(taskbar) if !taskbar.0.is_null() => {
            let ok = unsafe { focus(taskbar) };
            tracing::info!(ok, "moved focus off our window to the taskbar");
        }
        _ => tracing::warn!("could not find the taskbar to move focus to"),
    }
}

fn watchdog(is_remote: fn() -> bool, rehook: fn(), remote_control: fn() -> bool) {
    const POLL: Duration = Duration::from_millis(100);
    const COOLDOWN: Duration = Duration::from_secs(3);
    let mut missed = [0u8; 256];
    let mut last_fix: Option<Instant> = None;
    loop {
        std::thread::sleep(POLL);
        let mut culprit = None;
        for vk in watched() {
            let down = unsafe { GetAsyncKeyState(i32::from(vk)) } < 0;
            let seen = SEEN[vk as usize].load(Ordering::Relaxed);
            if down && !seen {
                // Twice in a row, so a key released between the hook and
                // this check isn't counted.
                missed[vk as usize] = missed[vk as usize].saturating_add(1);
                if missed[vk as usize] >= 2 {
                    culprit = Some(vk);
                }
            } else {
                missed[vk as usize] = 0;
            }
        }
        let Some(vk) = culprit else { continue };
        // During remote screen control our viewer window is meant to have
        // focus, and it forwards the keys the hook misses itself; moving
        // focus away would end the session.
        if remote_control() {
            continue;
        }
        let remote = is_remote();
        let fg = unsafe { GetForegroundWindow() };
        // Typing into our own window while input is local is expected to
        // bypass the hook (it's the window the user is using). Only step in
        // there for Ctrl / Alt / Windows, which start a switching shortcut.
        if !remote && is_ours(fg) && !matches!(vk, 0x5B | 0x5C | 0xA2..=0xA5) {
            continue;
        }
        if last_fix.is_some_and(|t| t.elapsed() < COOLDOWN) {
            continue;
        }
        last_fix = Some(Instant::now());
        tracing::warn!(
            vk = format_args!("{vk:#04x}"),
            remote,
            window = %describe(fg),
            "the keyboard hook missed a key; reinstalling it"
        );
        rehook();
        move_off_own_window(fg);
        // Don't report the same held key again.
        missed = [0; 256];
        SEEN[vk as usize].store(true, Ordering::Relaxed);
    }
}

/// Bring a window to the front. Windows only lets the app the user is
/// using do this, so borrow the current foreground thread's input state.
unsafe fn focus(target: HWND) -> bool {
    let fg = GetForegroundWindow();
    let fg_tid = GetWindowThreadProcessId(fg, None);
    let me = GetCurrentThreadId();
    let attached = fg_tid != 0 && fg_tid != me && AttachThreadInput(me, fg_tid, true).as_bool();
    let _ = BringWindowToTop(target);
    let ok = SetForegroundWindow(target).as_bool();
    if attached {
        let _ = AttachThreadInput(me, fg_tid, false);
    }
    if !ok {
        tracing::debug!(window = %describe(target), "Windows refused to change focus");
    }
    ok
}

/// "Title" (class, program) of a window, for the log.
fn describe(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return "(none)".into();
    }
    unsafe {
        let mut title = [0u16; 128];
        let n = GetWindowTextW(hwnd, &mut title).max(0) as usize;
        let mut class = [0u16; 128];
        let c = GetClassNameW(hwnd, &mut class).max(0) as usize;
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let program = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let mut buf = [0u16; 512];
                let mut len = buf.len() as u32;
                let name = if QueryFullProcessImageNameW(
                    h,
                    PROCESS_NAME_WIN32,
                    PWSTR(buf.as_mut_ptr()),
                    &mut len,
                )
                .is_ok()
                {
                    let full = String::from_utf16_lossy(&buf[..len as usize]);
                    full.rsplit('\\').next().unwrap_or(&full).to_string()
                } else {
                    format!("pid {pid}")
                };
                let _ = CloseHandle(h);
                name
            }
            Err(_) => format!("pid {pid}, no access (maybe running as administrator)"),
        };
        format!(
            "\"{}\" ({}, {})",
            String::from_utf16_lossy(&title[..n]),
            String::from_utf16_lossy(&class[..c]),
            program
        )
    }
}
