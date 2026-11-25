//! Keyboard health on Windows: a watchdog that notices when the keyboard
//! hook stops seeing keys, and a tiny window of our own that takes focus.
//!
//! On some PCs the low-level keyboard hook gets no keys (they go straight to
//! the focused app) until focus moves, for example by pressing the Windows
//! key. The watchdog compares what Windows says is held with what the hook
//! saw; if the hook missed a key, it reinstalls the hooks and moves focus to
//! our window and back, which is what pressing the Windows key did by hand.
//!
//! While input is on the other computer our window keeps focus, so a key
//! that slips past the hook lands there instead of in a document.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{CloseHandle, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Keys the hook saw go down and not yet up, by virtual-key code.
static SEEN: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
static FOCUS_THREAD: AtomicU32 = AtomicU32::new(0);

const WM_FOCUS_GRAB: u32 = WM_APP + 2;
const WM_FOCUS_RESTORE: u32 = WM_APP + 3;
/// wparam 1: give focus back afterwards (input is local).
const WM_FOCUS_NUDGE: u32 = WM_APP + 4;

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

/// Input moved to the other computer: put focus on our window.
pub(super) fn grab() {
    post(WM_FOCUS_GRAB, 0);
}

/// Input came back: give focus back to whatever had it.
pub(super) fn restore() {
    post(WM_FOCUS_RESTORE, 0);
}

fn post(msg: u32, wparam: usize) {
    let tid = FOCUS_THREAD.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, msg, WPARAM(wparam), LPARAM(0));
        }
    }
}

/// Start the focus window thread and the watchdog. `is_remote` says where
/// input is; `rehook` reinstalls the hooks.
pub(super) fn start(is_remote: fn() -> bool, rehook: fn()) {
    let _ = std::thread::Builder::new()
        .name("da-input-focus".into())
        .spawn(focus_thread);
    let _ = std::thread::Builder::new()
        .name("da-input-watchdog".into())
        .spawn(move || watchdog(is_remote, rehook));
    tracing::info!(window = %describe(unsafe { GetForegroundWindow() }), "keyboard capture started");
}

fn watchdog(is_remote: fn() -> bool, rehook: fn()) {
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
        if last_fix.is_some_and(|t| t.elapsed() < COOLDOWN) {
            continue;
        }
        last_fix = Some(Instant::now());
        let remote = is_remote();
        tracing::warn!(
            vk = format_args!("{vk:#04x}"),
            remote,
            window = %describe(unsafe { GetForegroundWindow() }),
            "the keyboard hook missed a key; reinstalling it and moving focus"
        );
        rehook();
        post(WM_FOCUS_NUDGE, usize::from(!remote));
        // Don't report the same held key again.
        missed = [0; 256];
        SEEN[vk as usize].store(true, Ordering::Relaxed);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn focus_thread() {
    unsafe {
        let module = match GetModuleHandleW(None) {
            Ok(m) => HINSTANCE(m.0),
            Err(_) => return,
        };
        let class = w!("DisplayAnythingFocus");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TOPMOST,
            class,
            w!("DisplayAnything"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            module,
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("could not create the focus window: {e}");
                return;
            }
        };
        // Practically invisible, but still a window that can have focus.
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        FOCUS_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);

        let mut saved: Option<HWND> = None;
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_FOCUS_GRAB => {
                    let fg = GetForegroundWindow();
                    if fg != hwnd {
                        tracing::info!(window = %describe(fg), "focus when switching over");
                        saved = Some(fg);
                    }
                    show(hwnd);
                }
                WM_FOCUS_RESTORE => {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    if let Some(w) = saved.take() {
                        if IsWindow(w).as_bool() {
                            focus(w);
                        }
                    }
                }
                WM_FOCUS_NUDGE => {
                    let fg = GetForegroundWindow();
                    show(hwnd);
                    if msg.wParam.0 == 1 {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                        if fg != hwnd && IsWindow(fg).as_bool() {
                            focus(fg);
                        }
                    } else if fg != hwnd && saved.is_none() {
                        saved = Some(fg);
                    }
                }
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// Show our window where the pointer is and give it focus.
unsafe fn show(hwnd: HWND) {
    let mut pt = Default::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, pt.x, pt.y, 1, 1, SWP_SHOWWINDOW);
    focus(hwnd);
}

/// Bring a window to the front. Windows only lets the app the user is
/// using do this, so borrow the current foreground thread's input state.
unsafe fn focus(target: HWND) {
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
