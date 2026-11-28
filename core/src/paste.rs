//! Typing Ctrl+V into the program that was in front before Lookout. Nothing is
//! sent unless that window really took the focus, and a program running as
//! administrator is left alone: Windows drops keys sent to it without saying so.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    SetForegroundWindow, EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT,
};

/// Pastes into the app the user was in.
pub trait KeyPaste: Send + Sync {
    fn paste(&self) -> Result<(), String>;
}

pub struct SystemPaste;

impl KeyPaste for SystemPaste {
    fn paste(&self) -> Result<(), String> {
        paste_into_last_app()
    }
}

/// The last window to come to the front that was another program's (0 = none).
static TARGET: AtomicUsize = AtomicUsize::new(0);

/// The taskbar, the desktop and the tray overflow are never pasted into.
const SHELL: [&str; 5] = [
    "Shell_TrayWnd",
    "Progman",
    "WorkerW",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
];

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0;
    // SAFETY: `pid` is a valid out-pointer; any handle value is accepted.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

fn remember(hwnd: HWND) {
    let pid = pid_of(hwnd);
    // SAFETY: no arguments.
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return;
    }
    let mut class = [0u16; 64];
    // SAFETY: the buffer holds the 64 units named.
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 64) };
    let class = String::from_utf16_lossy(&class[..usize::try_from(len).unwrap_or(0)]);
    if !SHELL.contains(&class.as_str()) {
        TARGET.store(hwnd as usize, Ordering::Release);
    }
}

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    remember(hwnd);
}

/// Starts noting which window was in front. Call it once, on a thread that runs
/// a message loop.
pub fn watch_foreground() {
    // SAFETY: an out-of-context hook with a function that lives for the whole
    // process.
    unsafe {
        remember(GetForegroundWindow());
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            null_mut(),
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
    }
}

/// Whether `process` runs elevated; `None` if Windows will not say.
fn elevated(process: HANDLE) -> Option<bool> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is a valid out-pointer; it is closed below.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut value = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
    let mut got = 0;
    // SAFETY: `value` is exactly `size` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            std::ptr::from_mut(&mut value).cast(),
            size,
            &mut got,
        )
    } != 0;
    // SAFETY: opened above.
    unsafe { CloseHandle(token) };
    ok.then_some(value.TokenIsElevated != 0)
}

/// True if keys sent to this window (a raw `HWND`) would be dropped: it runs as
/// administrator and this program does not.
pub fn out_of_reach(hwnd: usize) -> bool {
    // SAFETY: a pseudo handle that needs no closing.
    if elevated(unsafe { GetCurrentProcess() }) == Some(true) {
        return false;
    }
    // SAFETY: query-only rights; closed below.
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid_of(hwnd as HWND)) };
    if process.is_null() {
        return true;
    }
    let theirs = elevated(process);
    // SAFETY: opened above.
    unsafe { CloseHandle(process) };
    theirs.unwrap_or(true)
}

/// Brings the remembered window forward and types Ctrl+V into it.
pub fn paste_into_last_app() -> Result<(), String> {
    let target = TARGET.load(Ordering::Acquire);
    let hwnd = target as HWND;
    // SAFETY: both calls accept any handle value.
    if target == 0 || unsafe { IsWindow(hwnd) == 0 || IsWindowVisible(hwnd) == 0 } {
        return Err("no earlier window to paste into".to_owned());
    }
    if out_of_reach(target) {
        return Err("that window runs as administrator".to_owned());
    }
    // SAFETY: a window handle checked above.
    unsafe { SetForegroundWindow(hwnd) };
    let until = Instant::now() + Duration::from_millis(400);
    // SAFETY: no arguments.
    while unsafe { GetForegroundWindow() } != hwnd {
        if Instant::now() >= until {
            return Err("the window did not take the focus".to_owned());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // The program puts the focus back on its text box before the keys arrive.
    std::thread::sleep(Duration::from_millis(100));
    let key = |vk: u16, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let v = u16::from(b'V');
    let inputs = [
        key(VK_CONTROL, 0),
        key(v, 0),
        key(v, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    // SAFETY: a valid array of 4 INPUT structs of the size named.
    let sent = unsafe { SendInput(4, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent == 4 {
        Ok(())
    } else {
        Err("Windows did not accept the keystrokes".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_typed_without_a_window_to_type_into() {
        TARGET.store(0, Ordering::Release);
        assert!(paste_into_last_app().is_err());
    }

    #[test]
    fn this_programs_own_windows_are_never_the_target() {
        TARGET.store(0, Ordering::Release);
        remember(null_mut());
        assert_eq!(TARGET.load(Ordering::Acquire), 0);
    }
}
