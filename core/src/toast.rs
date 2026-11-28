//! A small notice near the bottom of the screen the pointer is on, with at most
//! one button. It is a plain Win32 window that never takes the focus, so it can
//! stay up while the bar is closed. A new notice replaces the one showing.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    FrameRect, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_CALCRECT, DT_CENTER,
    DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, FW_SEMIBOLD, HFONT,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, ShowWindow,
    TranslateMessage, IDC_HAND, MA_NOACTIVATE, MSG, SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// What a paste answers when the text was copied but could not be typed in.
pub const PASTE_FAILED: &str = "Copied. Press Ctrl+V";

const SHORT_MS: u32 = 2_000;
const UNDO_MS: u32 = 8_000;
const FAILED_MS: u32 = 4_000;

pub struct Toast {
    pub text: String,
    /// The one button's word, if any.
    pub button: Option<&'static str>,
    pub ms: u32,
}

impl Toast {
    pub fn short(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            button: None,
            ms: SHORT_MS,
        }
    }
}

/// The notice a finished action shows, if any: copies, saves and pins say one
/// word, and anything that can be undone carries an Undo button.
pub fn for_action(action: &str, message: &str, undoable: bool, was_undo: bool) -> Option<Toast> {
    if was_undo {
        return None;
    }
    let verb = action.rsplit('/').next().unwrap_or(action);
    let said = match verb {
        "paste_clip" | "paste_plain" | "note_paste" => {
            return (message == PASTE_FAILED).then(|| Toast {
                text: message.to_owned(),
                button: None,
                ms: FAILED_MS,
            });
        }
        "copy_path" | "copy_file" | "copy_clip" | "copy_pretty" | "answer_copy" | "note_copy" => {
            Some("Copied")
        }
        v if v.starts_with("txt_") => Some("Copied"),
        "note_save" => Some("Saved"),
        "saved_add" | "pin_clip" => Some("Pinned"),
        _ => None,
    };
    if undoable {
        return Some(Toast {
            text: said.unwrap_or(message).to_owned(),
            button: Some("Undo"),
            ms: UNDO_MS,
        });
    }
    said.map(Toast::short)
}

struct Face {
    text: Vec<u16>,
    button: Option<(Vec<u16>, i32)>,
    font: HFONT,
    bold: HFONT,
    pad: i32,
    clicked: bool,
}

thread_local! {
    static FACE: RefCell<Option<Face>> = const { RefCell::new(None) };
}

/// The window now showing (raw HWND, 0 = none).
static CURRENT: AtomicIsize = AtomicIsize::new(0);

/// Shows `toast` on a thread of its own; `clicked` runs if the button is pressed.
pub fn show(toast: Toast, clicked: impl FnOnce() + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("lookout-toast".to_owned())
        .spawn(move || {
            if run(toast) {
                clicked();
            }
        });
    if spawned.is_err() {
        crate::log::warn("could not show a notice");
    }
}

/// Shows `toast` and blocks until it is gone. True if its button was clicked.
pub fn run(toast: Toast) -> bool {
    // SAFETY: standard window creation, painting and message loop on this
    // thread; every pointer passed refers to a local that outlives the call, and
    // the fonts are deleted only after the window is gone.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }
        let (mut dpi, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
        let px = |v: i32| (v * dpi as i32 + 48) / 96;

        let font = |weight: u32| {
            CreateFontW(
                -px(14),
                0,
                0,
                0,
                weight as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            )
        };
        let (plain, bold) = (font(FW_NORMAL.0), font(FW_SEMIBOLD.0));
        let mut text: Vec<u16> = toast.text.encode_utf16().collect();
        let width_of = |t: &mut Vec<u16>, f: HFONT| {
            let dc = GetDC(None);
            let old = SelectObject(dc, f.into());
            let mut rc = RECT::default();
            DrawTextW(dc, t, &mut rc, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(dc, old);
            ReleaseDC(None, dc);
            rc.right - rc.left
        };
        let pad = px(18);
        let button = toast.button.map(|b| {
            let mut word: Vec<u16> = b.encode_utf16().collect();
            let wide = width_of(&mut word, bold) + pad * 2;
            (word, wide)
        });
        let button_w = button.as_ref().map_or(0, |b| b.1);
        let width = (width_of(&mut text, plain) + pad * 2 + button_w).clamp(px(180), px(520));
        let height = px(44);
        let work = info.rcWork;
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.bottom - height - px(32);

        FACE.with(|f| {
            *f.borrow_mut() = Some(Face {
                text,
                button,
                font: plain,
                bold,
                pad,
                clicked: false,
            });
        });
        let Ok(hinst) = GetModuleHandleW(None) else {
            return false;
        };
        let class = w!("LookoutNotice");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(notice_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            PCWSTR::null(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            return false;
        };
        let before = CURRENT.swap(hwnd.0 as isize, Ordering::AcqRel);
        if before != 0 {
            let _ = PostMessageW(Some(HWND(before as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), 1, toast.ms, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = CURRENT.compare_exchange(hwnd.0 as isize, 0, Ordering::AcqRel, Ordering::Acquire);
        let face = FACE.with(|f| f.borrow_mut().take());
        let _ = DeleteObject(plain.into());
        let _ = DeleteObject(bold.into());
        face.is_some_and(|f| f.clicked)
    }
}

unsafe extern "system" fn notice_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            let back = CreateSolidBrush(COLORREF(0x001C_1714));
            FillRect(hdc, &rc, back);
            let _ = DeleteObject(back.into());
            let edge = CreateSolidBrush(COLORREF(0x0040_342D));
            FrameRect(hdc, &rc, edge);
            let _ = DeleteObject(edge.into());
            SetBkMode(hdc, TRANSPARENT);
            FACE.with(|f| {
                if let Some(face) = f.borrow_mut().as_mut() {
                    let button_w = face.button.as_ref().map_or(0, |b| b.1);
                    let mut left = RECT {
                        left: face.pad,
                        right: rc.right - button_w.max(face.pad),
                        ..rc
                    };
                    let old = SelectObject(hdc, face.font.into());
                    SetTextColor(hdc, COLORREF(0x00EE_E9E6));
                    DrawTextW(
                        hdc,
                        &mut face.text,
                        &mut left,
                        DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                    );
                    if let Some((word, wide)) = face.button.as_mut() {
                        let mut right = RECT {
                            left: rc.right - *wide,
                            ..rc
                        };
                        SelectObject(hdc, face.bold.into());
                        // Lookout's amber, as 0x00BBGGRR.
                        SetTextColor(hdc, COLORREF(0x0024_A5F5));
                        DrawTextW(
                            hdc,
                            word,
                            &mut right,
                            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                        );
                    }
                    SelectObject(hdc, old);
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = i32::from((lp.0 as u32 & 0xFFFF) as u16 as i16);
            let mut rc = RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            FACE.with(|f| {
                if let Some(face) = f.borrow_mut().as_mut() {
                    face.clicked = face.button.as_ref().is_some_and(|b| x >= rc.right - b.1);
                }
            });
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(action: &str, message: &str, undoable: bool) -> Option<(String, bool, u32)> {
        for_action(action, message, undoable, false).map(|t| (t.text, t.button.is_some(), t.ms))
    }

    #[test]
    fn copies_saves_pins_and_undoable_actions_get_a_notice() {
        let copied = Some(("Copied".to_owned(), false, 2_000));
        assert_eq!(
            said("plugin/com.lookout.clipboard/copy_clip", "x", false),
            copied
        );
        assert_eq!(said("copy_path", "Path copied", false), copied);
        assert_eq!(
            said("note_save", "Note saved", true),
            Some(("Saved".to_owned(), true, 8_000))
        );
        assert_eq!(
            said("saved_add", "Pinned", true),
            Some(("Pinned".to_owned(), true, 8_000))
        );
        assert_eq!(
            said("note_delete", "Note deleted", true),
            Some(("Note deleted".to_owned(), true, 8_000))
        );
        assert_eq!(said("open", "Opened", false), None);
        assert!(for_action("note_delete", "Note deleted", true, true).is_none());
    }

    #[test]
    fn a_paste_speaks_only_when_it_failed() {
        assert_eq!(said("note_paste", "Pasted", false), None);
        assert_eq!(
            said("plugin/x/paste_clip", PASTE_FAILED, false),
            Some((PASTE_FAILED.to_owned(), false, 4_000))
        );
    }
}
