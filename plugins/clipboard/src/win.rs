//! The real Windows clipboard.
//!
//! Formats that mean "do not record this", from Microsoft's documentation
//! ("Clipboard Formats", Win32 data exchange; the three cloud/history formats):
//!
//! * `ExcludeClipboardContentFromMonitorProcessing`: if present, nothing on the
//!   clipboard may be included in clipboard history or synchronised.
//! * `CanIncludeInClipboardHistory`: a DWORD; 0 means keep it out of history.
//! * `CanUploadToCloudClipboard`: a DWORD; 0 means do not sync it. Microsoft says
//!   this does not govern the local history; we treat 0 as "do not record" too,
//!   which is stricter than required.
//!
//! Also honoured, though **not in Microsoft's documentation**: `Clipboard Viewer
//! Ignore`, the older convention that password managers (KeePass and others)
//! still set.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, Once, PoisonError};
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, GlobalFree, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardOwner, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, IsWindow, RegisterClassW, SetForegroundWindow, TranslateMessage,
    EVENT_SYSTEM_FOREGROUND, HWND_MESSAGE, MSG, SW_SHOWNORMAL, WINEVENT_OUTOFCONTEXT,
    WM_CLIPBOARDUPDATE, WNDCLASSW,
};

use crate::monitor::{ClipboardIo, Read};

const CF_UNICODETEXT: u32 = 13;

/// Present means "do not record".
pub const EXCLUDE_IF_PRESENT: &[&str] = &[
    "ExcludeClipboardContentFromMonitorProcessing",
    "Clipboard Viewer Ignore",
];
/// A DWORD of 0 means "do not record".
pub const EXCLUDE_IF_ZERO: &[&str] = &["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"];

fn register(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that lives through the call.
    unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
}

pub struct WindowsClipboard {
    if_present: Vec<u32>,
    if_zero: Vec<u32>,
}

/// Set by the listener window on every clipboard change.
static CHANGED: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// The last window to come to the front that was not Lookout (raw HWND, 0 = none).
static LAST_APP_WINDOW: AtomicUsize = AtomicUsize::new(0);
static LISTENER: Once = Once::new();
/// The hidden listener window (raw HWND, 0 until created); owns our clipboard writes.
static LISTENER_WINDOW: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn listener_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        *CHANGED.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        CHANGED.1.notify_all();
        return 0;
    }
    // SAFETY: forwards the message unchanged.
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
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
    if program_of(hwnd).is_some_and(|p| !is_lookout(&p)) {
        LAST_APP_WINDOW.store(hwnd as usize, Ordering::Release);
    }
}

/// A hidden message-only window that receives `WM_CLIPBOARDUPDATE`, plus a hook
/// that remembers the foreground window. Runs its own message loop forever. If
/// anything fails the watcher still works from the sequence number.
fn listen() {
    let class = wide("LookoutClipboardListener");
    // SAFETY: plain window-class and window creation with valid, NUL-terminated
    // strings that outlive the calls; the loop below owns the window.
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(listener_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        LISTENER_WINDOW.store(hwnd as usize, Ordering::Release);
        AddClipboardFormatListener(hwnd);
        let fg = GetForegroundWindow();
        if program_of(fg).is_some_and(|p| !is_lookout(&p)) {
            LAST_APP_WINDOW.store(fg as usize, Ordering::Release);
        }
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            null_mut(),
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Lower-case program name without `.exe` of the process owning `hwnd`.
fn program_of(hwnd: *mut core::ffi::c_void) -> Option<String> {
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out-pointer for the call.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    // SAFETY: opens a handle with query-only rights; closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let mut buf = [0u16; 520];
    let mut len = 520u32;
    // SAFETY: `buf` holds `len` UTF-16 units; the handle is valid.
    let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) } != 0;
    // SAFETY: the handle was opened above.
    unsafe { CloseHandle(h) };
    if !ok {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let file = path.rsplit(['\\', '/']).next()?.to_lowercase();
    Some(file.strip_suffix(".exe").unwrap_or(&file).to_owned())
}

fn is_lookout(program: &str) -> bool {
    program.contains("lookout")
}

impl WindowsClipboard {
    pub fn new() -> Self {
        LISTENER.call_once(|| {
            let _ = std::thread::Builder::new()
                .name("clipboard-listener".into())
                .spawn(listen);
        });
        Self {
            if_present: EXCLUDE_IF_PRESENT.iter().map(|n| register(n)).collect(),
            if_zero: EXCLUDE_IF_ZERO.iter().map(|n| register(n)).collect(),
        }
    }

    fn with_open<T>(f: impl FnOnce() -> T) -> Option<T> {
        Self::with_open_as(null_mut(), f)
    }

    /// Opens the clipboard for `owner` (a window of ours). Writing needs a real
    /// owner: with a null one, `EmptyClipboard` leaves no owner and Windows may
    /// refuse `SetClipboardData` (documented behaviour).
    fn with_open_as<T>(owner: HWND, f: impl FnOnce() -> T) -> Option<T> {
        for _ in 0..40 {
            // SAFETY: `owner` is null or one of our own windows.
            if unsafe { OpenClipboard(owner) } != 0 {
                let out = f();
                // SAFETY: the clipboard was opened above by this thread.
                unsafe { CloseClipboard() };
                return Some(out);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// The first four bytes of a format's data, if it has any.
    fn dword(format: u32) -> Option<u32> {
        // SAFETY: the clipboard is open (callers hold it); the handle is a global
        // memory block we only lock for the read and unlock again.
        unsafe {
            let h = GetClipboardData(format);
            if h.is_null() || GlobalSize(h) < 4 {
                return None;
            }
            let p = GlobalLock(h).cast::<u32>();
            if p.is_null() {
                return None;
            }
            let v = p.read_unaligned();
            GlobalUnlock(h);
            Some(v)
        }
    }

    fn text() -> Option<String> {
        // SAFETY: as in `dword`; the slice is bounded by the block's real size.
        unsafe {
            let h = GetClipboardData(CF_UNICODETEXT);
            if h.is_null() {
                return None;
            }
            let bytes = GlobalSize(h);
            let p = GlobalLock(h).cast::<u16>();
            if p.is_null() {
                return None;
            }
            let all = std::slice::from_raw_parts(p, bytes / 2);
            let end = all.iter().position(|&c| c == 0).unwrap_or(all.len());
            // Far past the longest clip kept: do not build a huge string just
            // for the store to ignore it.
            let s = if end > 2 * crate::store::MAX_CHARS {
                String::new()
            } else {
                String::from_utf16_lossy(&all[..end])
            };
            GlobalUnlock(h);
            Some(s)
        }
    }

    /// With the clipboard open: replace its content with `wide` (NUL-terminated).
    fn set_text(wide: &[u16], bytes: usize) -> Result<(), String> {
        // SAFETY: the block is allocated, filled while locked and handed to the
        // clipboard, which then owns it; on failure it is freed here.
        unsafe {
            EmptyClipboard();
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if h.is_null() {
                return Err("out of memory".to_owned());
            }
            let p = GlobalLock(h).cast::<u16>();
            if p.is_null() {
                GlobalFree(h);
                return Err("could not lock memory".to_owned());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            GlobalUnlock(h);
            if SetClipboardData(CF_UNICODETEXT, h).is_null() {
                GlobalFree(h);
                return Err("the clipboard refused the text".to_owned());
            }
            Ok(())
        }
    }
}

impl Default for WindowsClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardIo for WindowsClipboard {
    fn sequence(&self) -> u32 {
        // SAFETY: no arguments; reads a counter.
        unsafe { GetClipboardSequenceNumber() }
    }

    fn read(&self) -> Read {
        Self::with_open(|| {
            // SAFETY: the clipboard is open for the duration of this closure.
            let available = |f: u32| unsafe { IsClipboardFormatAvailable(f) } != 0;
            if self.if_present.iter().any(|&f| available(f)) {
                return Read::Excluded;
            }
            if self
                .if_zero
                .iter()
                .any(|&f| available(f) && Self::dword(f) == Some(0))
            {
                return Read::Excluded;
            }
            if !available(CF_UNICODETEXT) {
                return Read::NoText;
            }
            Self::text().map_or(Read::NoText, Read::Text)
        })
        .unwrap_or(Read::Busy)
    }

    fn write_text(&self, text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * 2;
        let mut last = Err("the clipboard is in use by another program".to_owned());
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(40));
            }
            let owner = LISTENER_WINDOW.load(Ordering::Acquire) as HWND;
            last = Self::with_open_as(owner, || Self::set_text(&wide, bytes))
                .unwrap_or_else(|| Err("the clipboard is in use by another program".to_owned()));
            if last.is_ok() {
                break;
            }
        }
        last
    }

    /// The program that owns the clipboard, and nothing else: if the owner window
    /// is gone or unreadable there is no name (the front window is never a guess).
    fn source_app(&self) -> Option<String> {
        // SAFETY: no arguments; the owner may be null.
        let app = program_of(unsafe { GetClipboardOwner() })?;
        (!is_lookout(&app)).then_some(app)
    }

    fn wait_change(&self, timeout: Duration) {
        let guard = CHANGED.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut changed, _) = CHANGED
            .1
            .wait_timeout_while(guard, timeout, |c| !*c)
            .unwrap_or_else(PoisonError::into_inner);
        *changed = false;
    }

    /// Ctrl+V to the window that was in front before Lookout. Nothing is sent
    /// unless the window about to receive it is a known, non-Lookout window.
    fn paste(&self) -> Result<(), String> {
        let front = || {
            // SAFETY: no arguments.
            let fg = unsafe { GetForegroundWindow() };
            (fg, program_of(fg))
        };
        if front().1.is_none_or(|p| is_lookout(&p)) {
            let target = LAST_APP_WINDOW.load(Ordering::Acquire) as *mut core::ffi::c_void;
            // SAFETY: IsWindow accepts any handle value.
            if target.is_null() || unsafe { IsWindow(target) } == 0 {
                return Err("no earlier window to paste into".to_owned());
            }
            // SAFETY: a window handle checked above.
            unsafe { SetForegroundWindow(target) };
            std::thread::sleep(Duration::from_millis(100));
        }
        let (fg, program) = front();
        if fg.is_null() || program.is_none_or(|p| is_lookout(&p)) {
            return Err("no suitable window to paste into".to_owned());
        }
        std::thread::sleep(Duration::from_millis(150));
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
        // SAFETY: `inputs` is a valid array of 4 INPUT structs.
        let sent = unsafe {
            SendInput(
                4,
                inputs.as_ptr(),
                i32::try_from(std::mem::size_of::<INPUT>()).unwrap_or(40),
            )
        };
        if sent == 4 {
            Ok(())
        } else {
            Err("Windows did not accept the keystrokes".to_owned())
        }
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("mailto:"))
        {
            return Err("only web links and email addresses can be opened".to_owned());
        }
        let (op, file) = (wide("open"), wide(url));
        // SAFETY: both strings are NUL-terminated and live through the call.
        let r = unsafe {
            ShellExecuteW(
                null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if r as usize > 32 {
            Ok(())
        } else {
            Err("Windows could not open the link".to_owned())
        }
    }
}
