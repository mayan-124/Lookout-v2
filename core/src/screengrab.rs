//! "Grab text from screen": the user drags a box over the screen, the pixels in
//! it are read with the Windows OCR engine and the text goes to the clipboard.
//!
//! The picture lives only in memory (a BMP byte buffer); it is never written to
//! disk. The selection overlay is a plain Win32 window over the whole virtual
//! screen, so it works across monitors. The thread runs per-monitor DPI aware,
//! which makes every coordinate a physical pixel.

use std::cell::Cell;
use std::ptr::null_mut;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, FillRect, FrameRect, GetDC, GetDIBits, InvalidateRect,
    ReleaseDC, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT,
    DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HDC, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, LoadCursorW, PostQuitMessage, RegisterClassW, SetCursor, SetForegroundWindow,
    SetLayeredWindowAttributes, SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    IDC_CROSS, LWA_ALPHA, MSG, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW, SW_SHOWNOACTIVATE, WM_DESTROY, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_TIMER,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::content::ocr::{OcrError, WindowsOcr};
use crate::pause::PauseState;

/// What a grab ended with.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Copied { words: usize },
    NoText,
    Cancelled,
    Paused,
    Failed(String),
}

impl Outcome {
    /// The short notice shown after the grab, if any.
    pub fn notice(&self) -> Option<String> {
        match self {
            Outcome::Copied { words: 1 } => Some("Copied 1 word".to_owned()),
            Outcome::Copied { words } => Some(format!("Copied {words} words")),
            Outcome::NoText => Some("No text found there".to_owned()),
            Outcome::Cancelled => None,
            Outcome::Paused => Some("Lookout is paused".to_owned()),
            Outcome::Failed(why) => Some(format!("Could not grab text: {why}")),
        }
    }
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Runs a whole grab (call it on its own thread: it blocks while the user
/// drags) and shows the notice.
pub fn run(pause: &PauseState) -> Outcome {
    // SAFETY: only changes this thread's DPI mode, before it creates any window.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let outcome = grab(pause);
    if let Some(text) = outcome.notice() {
        toast(&text);
    }
    outcome
}

fn grab(pause: &PauseState) -> Outcome {
    if pause.is_paused() {
        return Outcome::Paused;
    }
    let Some(rect) = select_region() else {
        return Outcome::Cancelled;
    };
    // Let the overlay finish disappearing before the screen is read.
    std::thread::sleep(Duration::from_millis(60));
    let bmp = match capture(rect) {
        Ok(b) => b,
        Err(e) => return Outcome::Failed(e),
    };
    let text = match ocr_text(WindowsOcr.recognize_bytes(&bmp)) {
        Ok(t) => t,
        Err(outcome) => return outcome,
    };
    let words = word_count(&text);
    match set_clipboard_text(&text) {
        Ok(()) => Outcome::Copied { words },
        Err(e) => Outcome::Failed(e),
    }
}

/// The OCR result as trimmed text, or the outcome to report instead.
fn ocr_text(result: Result<String, OcrError>) -> Result<String, Outcome> {
    match result {
        Ok(t) if word_count(&t) == 0 => Err(Outcome::NoText),
        Ok(t) => Ok(t.trim().to_owned()),
        Err(OcrError::Unavailable) => Err(Outcome::Failed(
            "no Windows OCR language is installed".to_owned(),
        )),
        Err(e) => Err(Outcome::Failed(e.to_string())),
    }
}

/// What to tell the user when the grab hotkey cannot be registered.
pub fn grab_hotkey_message(wanted: &str, error: &str) -> String {
    format!(
        "Lookout could not register the \"grab text from screen\" hotkey '{wanted}' ({error}).          Another app may be using it. Pick another one in settings (grab_hotkey) and restart          Lookout. Everything else keeps working."
    )
}

/// A selection (client coordinates of the overlay, which covers the whole
/// virtual screen) as a rectangle in screen pixels. `origin` is the top-left of
/// the virtual screen; it is negative when a monitor sits left of or above the
/// main one. The thread is per-monitor DPI aware, so these are physical pixels
/// at any scale and no scale factor belongs in this sum.
fn selection_to_screen(a: (i32, i32), b: (i32, i32), origin: (i32, i32)) -> Option<RECT> {
    let r = norm(a, b);
    if r.right - r.left < 4 || r.bottom - r.top < 4 {
        return None;
    }
    Some(RECT {
        left: r.left + origin.0,
        top: r.top + origin.1,
        right: r.right + origin.0,
        bottom: r.bottom + origin.1,
    })
}

// ---------------------------------------------------------------------------
// Selection overlay

#[derive(Clone, Copy, Default)]
struct Drag {
    active: bool,
    start: (i32, i32),
    cur: (i32, i32),
    /// None while running; Some(true) = finished a selection, Some(false) = cancelled.
    done: Option<bool>,
}

thread_local! {
    static DRAG: Cell<Drag> = const { Cell::new(Drag { active: false, start: (0, 0), cur: (0, 0), done: None }) };
    static TOAST: Cell<Option<&'static str>> = const { Cell::new(None) };
}

fn lparam_xy(l: LPARAM) -> (i32, i32) {
    let v = l.0 as u32;
    (
        i32::from((v & 0xFFFF) as u16 as i16),
        i32::from((v >> 16) as u16 as i16),
    )
}

fn norm(a: (i32, i32), b: (i32, i32)) -> RECT {
    RECT {
        left: a.0.min(b.0),
        top: a.1.min(b.1),
        right: a.0.max(b.0),
        bottom: a.1.max(b.1),
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            if let Ok(c) = LoadCursorW(None, IDC_CROSS) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let p = lparam_xy(lp);
            DRAG.with(|d| {
                d.set(Drag {
                    active: true,
                    start: p,
                    cur: p,
                    done: None,
                });
            });
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let was_active = DRAG.with(|d| {
                let mut v = d.get();
                if v.active {
                    v.cur = lparam_xy(lp);
                    d.set(v);
                }
                v.active
            });
            if was_active {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            DRAG.with(|d| {
                let mut v = d.get();
                if v.active {
                    v.cur = lparam_xy(lp);
                    v.active = false;
                    v.done = Some(true);
                    d.set(v);
                }
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            DRAG.with(|d| {
                let mut v = d.get();
                v.done = Some(false);
                d.set(v);
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == 0x1B => {
            DRAG.with(|d| {
                let mut v = d.get();
                v.done = Some(false);
                d.set(v);
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let all = ps.rcPaint;
            let black = CreateSolidBrush(COLORREF(0));
            FillRect(hdc, &all, black);
            let _ = DeleteObject(black.into());
            let d = DRAG.with(Cell::get);
            if d.active {
                let r = norm(d.start, d.cur);
                let white = CreateSolidBrush(COLORREF(0x00FF_FFFF));
                FrameRect(hdc, &r, white);
                let inner = RECT {
                    left: r.left + 1,
                    top: r.top + 1,
                    right: r.right - 1,
                    bottom: r.bottom - 1,
                };
                FrameRect(hdc, &inner, white);
                let _ = DeleteObject(white.into());
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn virtual_screen() -> RECT {
    // SAFETY: GetSystemMetrics only reads system state.
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        RECT {
            left: x,
            top: y,
            right: x + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: y + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// Shows the dim overlay and returns the chosen rectangle in screen pixels, or
/// None if the user cancelled or the box was tiny.
fn select_region() -> Option<RECT> {
    let vs = virtual_screen();
    DRAG.with(|d| d.set(Drag::default()));
    // SAFETY: standard window-class and window creation on this thread; the
    // window is destroyed before returning and the class is registered once per
    // call (a repeat registration fails harmlessly).
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let class = w!("LookoutGrabOverlay");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            class,
            w!(""),
            WS_POPUP,
            vs.left,
            vs.top,
            vs.right - vs.left,
            vs.bottom - vs.top,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?;
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 90, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyWindow(hwnd);
    }
    let d = DRAG.with(Cell::get);
    if d.done != Some(true) {
        return None;
    }
    selection_to_screen(d.start, d.cur, (vs.left, vs.top))
}

// ---------------------------------------------------------------------------
// Capture (in memory only)

/// The pixels of `r` (screen coordinates) as a BMP file in memory.
fn capture(r: RECT) -> Result<Vec<u8>, String> {
    // SAFETY: the screen DC is released right after use.
    let (w, h, pixels) = unsafe {
        let screen = GetDC(None);
        let out = capture_dc(screen, r);
        ReleaseDC(None, screen);
        out?
    };
    Ok(bmp_file(w, h, &pixels))
}

/// Pixels (width, height, BGRA bottom-up) read from any device context (the
/// screen, or a memory bitmap).
fn capture_dc(screen: HDC, r: RECT) -> Result<(usize, usize, Vec<u8>), String> {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return Err("empty selection".to_owned());
    }
    let (wu, hu) = (w as usize, h as usize);
    let mut pixels = vec![0u8; wu * hu * 4];
    // SAFETY: GDI calls on a screen DC and a compatible bitmap created here and
    // released below; `pixels` is exactly w*h*4 bytes as GetDIBits requires.
    let ok = unsafe {
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let copied = BitBlt(
            mem,
            0,
            0,
            w,
            h,
            Some(screen),
            r.left,
            r.top,
            SRCCOPY | CAPTUREBLT,
        )
        .is_ok();
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: h, // positive: rows bottom-up, as a BMP file wants
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        copied && lines != 0
    };
    if !ok {
        return Err("could not read the screen".to_owned());
    }
    for px in pixels.as_chunks_mut::<4>().0 {
        px[3] = 255; // BitBlt leaves alpha 0, which would read as transparent
    }
    Ok((wu, hu, pixels))
}

pub(crate) fn bmp_file(w: usize, h: usize, bgra_bottom_up: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(54 + bgra_bottom_up.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + bgra_bottom_up.len()) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    out.extend_from_slice(bgra_bottom_up);
    out
}

// ---------------------------------------------------------------------------
// Clipboard

pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    const CF_UNICODETEXT: u32 = 13;

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the clipboard is opened and closed here; the block is filled while
    // locked and then owned by the clipboard (freed here only if it refuses it).
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        if !opened {
            return Err("the clipboard is in use by another program".to_owned());
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        let result = if h.is_null() {
            Err("out of memory".to_owned())
        } else {
            let p = GlobalLock(h).cast::<u16>();
            if p.is_null() {
                GlobalFree(h);
                Err("could not lock memory".to_owned())
            } else {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                GlobalUnlock(h);
                if SetClipboardData(CF_UNICODETEXT, h).is_null() {
                    GlobalFree(h);
                    Err("the clipboard refused the text".to_owned())
                } else {
                    Ok(())
                }
            }
        };
        CloseClipboard();
        result
    }
}

// ---------------------------------------------------------------------------
// Notice

unsafe extern "system" fn toast_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let bg = CreateSolidBrush(COLORREF(0x0030_2A2A));
            FillRect(hdc, &ps.rcPaint, bg);
            let _ = DeleteObject(bg.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FF_FFFF));
            let mut text: Vec<u16> = TOAST.with(Cell::get).unwrap_or("").encode_utf16().collect();
            let mut rc = RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            DrawTextW(
                hdc,
                &mut text,
                &mut rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = DestroyWindow(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// A small dark box near the bottom of the main screen that closes itself.
fn toast(text: &str) {
    // The text must outlive the window; leaking a few bytes per notice is fine.
    let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
    TOAST.with(|t| t.set(Some(leaked)));
    // SAFETY: standard window creation on this thread; destroyed by its own timer.
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else {
            return;
        };
        let class = w!("LookoutGrabToast");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(toast_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let (ww, wh) = (320, 44);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            PCWSTR::null(),
            WS_POPUP,
            (sw - ww) / 2,
            sh - wh - 120,
            ww,
            wh,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            return;
        };
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), 1, 1600, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn words_are_counted_and_the_notice_is_worded() {
        assert_eq!(word_count("  two\nwords  "), 2);
        assert_eq!(
            Outcome::Copied { words: 1 }.notice().as_deref(),
            Some("Copied 1 word")
        );
        assert_eq!(
            Outcome::Copied { words: 12 }.notice().as_deref(),
            Some("Copied 12 words")
        );
        assert_eq!(Outcome::Cancelled.notice(), None);
    }

    #[test]
    fn a_paused_lookout_grabs_nothing() {
        let pause = PauseState::new();
        pause.set(true);
        assert_eq!(grab(&pause), Outcome::Paused);
    }

    #[test]
    fn a_failed_or_empty_ocr_gives_a_clear_notice_not_a_crash() {
        let no_language = ocr_text(Err(OcrError::Unavailable)).unwrap_err();
        assert_eq!(
            no_language.notice().as_deref(),
            Some("Could not grab text: no Windows OCR language is installed")
        );
        assert_eq!(
            ocr_text(Ok(" \n ".to_owned()))
                .unwrap_err()
                .notice()
                .as_deref(),
            Some("No text found there")
        );
        assert_eq!(
            ocr_text(Ok("  hi there \n".to_owned())).unwrap(),
            "hi there"
        );
        assert!(ocr_text(Err(OcrError::TooLarge(99999)))
            .unwrap_err()
            .notice()
            .is_some());
    }

    #[test]
    fn the_hotkey_failure_message_names_the_key_and_the_fix() {
        let m = grab_hotkey_message("Ctrl+Alt+T", "already registered");
        assert!(m.contains("Ctrl+Alt+T") && m.contains("grab_hotkey") && m.contains("restart"));
    }

    /// Fake monitors: (name, scale %, virtual-screen origin). The selection is
    /// in overlay client pixels; the overlay starts at the virtual-screen origin.
    #[test]
    fn region_to_pixels_at_every_scale_and_with_negative_origins() {
        let layouts: [(&str, i32, (i32, i32)); 4] = [
            ("main only", 100, (0, 0)),
            ("second monitor left", 150, (-2560, 0)),
            ("second monitor above", 200, (0, -2160)),
            ("left and above", 150, (-1920, -1080)),
        ];
        for (name, scale, origin) in layouts {
            let down_right = selection_to_screen((40, 30), (340, 150), origin).unwrap();
            let up_left = selection_to_screen((340, 150), (40, 30), origin).unwrap();
            let want = (40 + origin.0, 30 + origin.1, 340 + origin.0, 150 + origin.1);
            for r in [down_right, up_left] {
                assert_eq!(
                    (r.left, r.top, r.right, r.bottom),
                    want,
                    "{name} at {scale}%"
                );
                assert_eq!((r.right - r.left, r.bottom - r.top), (300, 120));
            }
        }
        let r = selection_to_screen((10, 10), (110, 60), (-1920, 0)).unwrap();
        assert!(r.left < 0 && r.right < 0);
        assert!(selection_to_screen((5, 5), (7, 20), (0, 0)).is_none());
    }

    // ---- real-machine experiments (need a desktop session) -----------------
    // Run: cargo test -p lookout-core --lib screengrab -- --ignored --nocapture

    #[test]
    #[ignore = "needs a desktop session; registers a global hotkey for a moment"]
    fn hotkey_ctrl_alt_t_free_and_second_registration_fails_cleanly() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
        };
        let mods = HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0 | MOD_NOREPEAT.0);
        // SAFETY: thread-level hotkey (no window), removed again below.
        unsafe {
            let first = RegisterHotKey(None, 0x7001, mods, u32::from(b'T'));
            eprintln!("Ctrl+Alt+T free right now: {}", first.is_ok());
            if first.is_ok() {
                let again = RegisterHotKey(None, 0x7002, mods, u32::from(b'T'));
                eprintln!("second registration (taken): {again:?}");
                assert!(again.is_err(), "a taken hotkey must fail, not succeed");
                let _ = UnregisterHotKey(None, 0x7001);
            }
        }
    }

    #[test]
    #[ignore = "needs a desktop session"]
    fn per_monitor_aware_thread_sees_physical_pixels() {
        use windows::Win32::Graphics::Gdi::{
            EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS,
        };
        use windows::Win32::UI::HiDpi::GetDpiForSystem;
        // SAFETY: only reads display state on this thread.
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let mut dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            assert!(EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut dm).as_bool());
            let (pw, ph) = (dm.dmPelsWidth as i32, dm.dmPelsHeight as i32);
            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let v = virtual_screen();
            eprintln!(
                "system DPI {} ({}%), physical {pw}x{ph}, metrics {sw}x{sh}, virtual {:?}",
                GetDpiForSystem(),
                GetDpiForSystem() * 100 / 96,
                (v.left, v.top, v.right, v.bottom)
            );
            assert_eq!((pw, ph), (sw, sh), "metrics are not physical pixels");
        }
    }

    pub(crate) mod ocr_sweep {
        use super::*;
        use windows::core::HSTRING;
        use windows::Win32::Foundation::SIZE;
        use windows::Win32::Graphics::Gdi::{
            CreateFontW, GetTextExtentPoint32W, SetBkColor, TextOutW, CLEARTYPE_QUALITY,
            CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL,
            OUT_DEFAULT_PRECIS,
        };

        pub(crate) const SENTENCE: &str =
            "The quick brown fox jumps over 48215 lazy dogs, mail john.doe@example.com now";

        /// Renders `text` with Segoe UI at `px` pixels into a memory bitmap and
        /// reads it back through the same `capture_dc` the feature uses.
        pub(crate) fn shot(text: &str, px: i32, dark: bool) -> (usize, usize, Vec<u8>) {
            let (fg, bg) = if dark {
                (COLORREF(0x00E6_E6E6), COLORREF(0x001E_1E1E))
            } else {
                (COLORREF(0), COLORREF(0x00FF_FFFF))
            };
            // SAFETY: GDI objects are created and destroyed in this function.
            unsafe {
                let screen = GetDC(None);
                let mem = CreateCompatibleDC(Some(screen));
                let font = CreateFontW(
                    -px,
                    0,
                    0,
                    0,
                    FW_NORMAL.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                    &HSTRING::from("Segoe UI"),
                );
                let old_font = SelectObject(mem, font.into());
                let wide: Vec<u16> = text.encode_utf16().collect();
                let mut size = SIZE::default();
                let _ = GetTextExtentPoint32W(mem, &wide, &mut size);
                let margin = 10;
                let (w, h) = (size.cx + 2 * margin, size.cy + 2 * margin);
                let bmp = CreateCompatibleBitmap(screen, w, h);
                let old_bmp = SelectObject(mem, bmp.into());
                let brush = CreateSolidBrush(bg);
                let all = RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                };
                FillRect(mem, &all, brush);
                let _ = DeleteObject(brush.into());
                SetBkColor(mem, bg);
                SetTextColor(mem, fg);
                let _ = TextOutW(mem, margin, margin, &wide);
                let out = capture_dc(mem, all);
                SelectObject(mem, old_bmp);
                SelectObject(mem, old_font);
                let _ = DeleteObject(bmp.into());
                let _ = DeleteObject(font.into());
                let _ = DeleteDC(mem);
                ReleaseDC(None, screen);
                out.unwrap_or_default()
            }
        }

        fn edit_distance(a: &[char], b: &[char]) -> usize {
            let mut prev: Vec<usize> = (0..=b.len()).collect();
            for (i, ca) in a.iter().enumerate() {
                let mut cur = vec![i + 1];
                for (j, cb) in b.iter().enumerate() {
                    cur.push(
                        (prev[j] + usize::from(ca != cb))
                            .min(prev[j + 1] + 1)
                            .min(cur[j] + 1),
                    );
                }
                prev = cur;
            }
            prev[b.len()]
        }

        /// Share of the expected characters that came out right (0..=100).
        pub(crate) fn accuracy(want: &str, got: &str) -> f64 {
            let norm = |s: &str| {
                s.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .collect::<Vec<_>>()
            };
            let (w, g) = (norm(want), norm(got));
            let d = edit_distance(&w, &g).min(w.len());
            100.0 * (w.len() - d) as f64 / w.len() as f64
        }

        fn read(bmp: &[u8], upscale: bool) -> String {
            WindowsOcr
                .recognize_bytes_with(bmp, upscale)
                .unwrap_or_default()
        }

        #[test]
        #[ignore = "needs a desktop session and a Windows OCR language"]
        fn accuracy_by_size_with_and_without_2x_upscale() {
            use windows::Media::Ocr::OcrEngine;
            let langs: Vec<String> = OcrEngine::AvailableRecognizerLanguages()
                .map(|v| {
                    v.into_iter()
                        .filter_map(|l| l.LanguageTag().ok())
                        .map(|t| t.to_string())
                        .collect()
                })
                .unwrap_or_default();
            eprintln!("OCR languages installed: {langs:?}");
            eprintln!(
                "profile engine available: {}",
                crate::content::ocr::Ocr::available(&WindowsOcr)
            );
            eprintln!("size | theme | plain % | 2x %");
            for px in [8, 10, 12, 14, 18, 24] {
                for dark in [false, true] {
                    let (w, h, p) = shot(SENTENCE, px, dark);
                    let plain = accuracy(SENTENCE, &read(&bmp_file(w, h, &p), false));
                    let up = accuracy(SENTENCE, &read(&bmp_file(w, h, &p), true));
                    eprintln!(
                        "{px:>2}px | {} | {plain:5.1} | {up:5.1}",
                        if dark { "dark " } else { "light" }
                    );
                }
            }
            let (w, h, p) = shot(SENTENCE, 8, false);
            eprintln!("8px 2x read: {:?}", read(&bmp_file(w, h, &p), true));
        }
    }

    #[test]
    fn the_in_memory_bmp_has_a_valid_header() {
        let b = bmp_file(2, 1, &[0; 8]);
        assert_eq!(&b[..2], b"BM");
        assert_eq!(b.len(), 54 + 8);
    }
}
