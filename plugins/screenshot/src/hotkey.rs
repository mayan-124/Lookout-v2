//! Global hotkey listener thread using RegisterHotKey and Win32 message loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN, VK_F1, VK_PRINT, VK_SNAPSHOT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, PostQuitMessage, RegisterClassW, SetTimer, ShowWindow, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE, WM_CREATE,
    WM_DESTROY, WM_HOTKEY, WM_LBUTTONDOWN, WM_PAINT, WM_QUIT, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::annotation::{draw_simple_text, Color};
use crate::geometry::PointI;
use crate::image::ImageBuffer;

const SCREENSHOT_HOTKEY_ID: i32 = 0x7501;
const SCROLLING_HOTKEY_ID: i32 = 0x7502;
const RECORDING_HOTKEY_ID: i32 = 0x7503;
const TEXT_HOTKEY_ID: i32 = 0x7504;

/// Validates and parses a hotkey string into Win32 modifiers and Virtual-Key code.
/// Rejects invalid hotkeys, empty strings, missing modifiers, and Windows Snipping Tool (Win+Shift+S).
pub fn parse_hotkey(s: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("Hotkey string cannot be empty".to_string());
    }

    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key_code = 0u32;
    let mut parts_count = 0;

    for part in trimmed.split('+') {
        let p = part.trim().to_lowercase();
        if p.is_empty() {
            continue;
        }
        parts_count += 1;
        match p.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "windows" => mods |= MOD_WIN,
            "prtscn" | "printscreen" | "snapshot" => key_code = VK_SNAPSHOT.0 as u32,
            "print" => key_code = VK_PRINT.0 as u32,
            k if k.len() == 1 => {
                let ch = k.chars().next().unwrap_or('S').to_ascii_uppercase();
                if ch.is_ascii_alphanumeric() {
                    key_code = ch as u32;
                } else {
                    return Err(format!("Unsupported hotkey character: '{ch}'"));
                }
            }
            k if k.starts_with('f') => {
                if let Ok(num) = k[1..].parse::<u16>() {
                    if (1..=12).contains(&num) {
                        key_code = (VK_F1.0 + num - 1) as u32;
                    } else {
                        return Err(format!("Function key out of range F1-F12: '{k}'"));
                    }
                } else {
                    return Err(format!("Invalid function key: '{k}'"));
                }
            }
            _ => return Err(format!("Unrecognized key token in hotkey: '{part}'")),
        }
    }

    if parts_count < 2 {
        return Err("Hotkey must combine a modifier and a key (e.g. Ctrl+Alt+S)".to_string());
    }

    let has_modifier = (mods.0 & (MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0 | MOD_WIN.0)) != 0;
    if !has_modifier {
        return Err(
            "Hotkey must include at least one modifier key (Ctrl, Alt, Shift, or Win)".to_string(),
        );
    }

    if key_code == 0 {
        return Err("Hotkey does not specify a valid trigger key".to_string());
    }

    // Do NOT hook, replace or disable Windows Snipping Tool (Win+Shift+S)
    let is_win_shift = (mods.0 & MOD_WIN.0 != 0) && (mods.0 & MOD_SHIFT.0 != 0);
    if is_win_shift && key_code == b'S' as u32 {
        return Err(
            "Win+Shift+S cannot be used as a hotkey (reserved for Windows Snipping Tool)"
                .to_string(),
        );
    }

    // Include MOD_NOREPEAT so holding the key doesn't spam screenshots
    mods |= MOD_NOREPEAT;

    Ok((mods, key_code))
}

pub struct HotkeyManager {
    stop_signal: Arc<AtomicBool>,
    _handle: Option<JoinHandle<()>>,
}

impl HotkeyManager {
    /// Starts the hotkey listener thread. Calls callbacks when hotkeys are pressed.
    #[allow(clippy::too_many_arguments)]
    pub fn start<F1, F2, F3, F4>(
        screenshot_hotkey: String,
        scrolling_hotkey: Option<String>,
        recording_hotkey: Option<String>,
        text_hotkey: Option<String>,
        data_dir: Option<PathBuf>,
        on_screenshot: F1,
        on_scrolling: F2,
        on_recording: F3,
        on_text: F4,
    ) -> Self
    where
        F1: Fn() + Send + Sync + 'static,
        F2: Fn() + Send + Sync + 'static,
        F3: Fn() + Send + Sync + 'static,
        F4: Fn() + Send + Sync + 'static,
    {
        let stop_signal = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_signal);

        let handle = thread::Builder::new()
            .name("lookout-screenshot-hotkey".to_string())
            .spawn(move || {
                run_hotkey_loop(
                    &screenshot_hotkey,
                    scrolling_hotkey.as_deref(),
                    recording_hotkey.as_deref(),
                    text_hotkey.as_deref(),
                    data_dir,
                    on_screenshot,
                    on_scrolling,
                    on_recording,
                    on_text,
                    stop_clone,
                );
            })
            .ok();

        Self {
            stop_signal,
            _handle: handle,
        }
    }

    pub fn stop(&mut self) {
        self.stop_signal.store(true, Ordering::Release);
    }
}

/// Checks whether the hotkey conflict notice should be shown.
/// Allowed at most once per 24 hours (remembered in data_dir).
pub fn should_show_conflict_notice(data_dir: Option<&Path>) -> bool {
    let state_file = match data_dir {
        Some(d) => d.join("hotkey_conflict_last_shown.txt"),
        None => std::env::temp_dir().join("lookout_screenshot_hotkey_conflict.txt"),
    };

    let now_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if let Ok(contents) = std::fs::read_to_string(&state_file) {
        if let Ok(last_secs) = contents.trim().parse::<u64>() {
            if now_secs.saturating_sub(last_secs) < 86400 {
                return false;
            }
        }
    }

    if let Some(parent) = state_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&state_file, now_secs.to_string());
    true
}

unsafe extern "system" fn toast_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            // Auto-dismiss non-modal toast after 4.5 seconds
            let _ = SetTimer(Some(hwnd), 1, 4500, None);
            LRESULT(0)
        }
        WM_TIMER | WM_LBUTTONDOWN => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let w = 420u32;
            let h = 64u32;
            let mut img = ImageBuffer::new(w, h);

            // Card background #14171c
            for y in 0..h {
                for x in 0..w {
                    img.set_pixel(x, y, [20, 23, 28, 255]);
                }
            }
            // Card border #262b33
            for x in 0..w {
                img.set_pixel(x, 0, [38, 43, 51, 255]);
                img.set_pixel(x, h - 1, [38, 43, 51, 255]);
            }
            for y in 0..h {
                img.set_pixel(0, y, [38, 43, 51, 255]);
                img.set_pixel(w - 1, y, [38, 43, 51, 255]);
            }

            draw_simple_text(
                &mut img,
                PointI::new(14, 12),
                "LOOKOUT SCREENSHOT",
                Color::AMBER,
                11,
            );
            draw_simple_text(
                &mut img,
                PointI::new(14, 32),
                "Hotkey conflict. Change hotkey in add-on settings.",
                Color::rgb(230, 233, 238),
                11,
            );

            let mem_dc = CreateCompatibleDC(Some(hdc));
            if !mem_dc.is_invalid() {
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w as i32,
                        biHeight: -(h as i32),
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: 0,
                        biSizeImage: w * h * 4,
                        ..Default::default()
                    },
                    bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default()],
                };
                let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
                if let Ok(dib) =
                    CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
                {
                    if !dib.is_invalid() && !bits_ptr.is_null() {
                        let dst = std::slice::from_raw_parts_mut(
                            bits_ptr as *mut u8,
                            (w * h * 4) as usize,
                        );
                        for i in 0..(w * h) as usize {
                            let s = i * 4;
                            dst[s] = img.pixels[s + 2];
                            dst[s + 1] = img.pixels[s + 1];
                            dst[s + 2] = img.pixels[s];
                            dst[s + 3] = 255;
                        }
                        let old = SelectObject(mem_dc, dib.into());
                        let _ = BitBlt(hdc, 0, 0, w as i32, h as i32, Some(mem_dc), 0, 0, SRCCOPY);
                        let _ = SelectObject(mem_dc, old);
                        let _ = DeleteObject(dib.into());
                    }
                }
                let _ = DeleteDC(mem_dc);
            }

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn show_hotkey_conflict_toast(_hotkey_str: &str) {
    thread::Builder::new()
        .name("lookout-hotkey-toast".to_string())
        .spawn(move || unsafe {
            let hinst = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(_) => return,
            };
            let class_name = w!("LookoutScreenshotToast");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(toast_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let scr_w = GetSystemMetrics(SM_CXSCREEN);
            let scr_h = GetSystemMetrics(SM_CYSCREEN);
            let toast_w = 420;
            let toast_h = 64;
            let x = (scr_w - toast_w - 24).max(0);
            let y = (scr_h - toast_h - 48).max(0);

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("Lookout Notice"),
                WS_POPUP,
                x,
                y,
                toast_w,
                toast_h,
                None,
                None,
                Some(hinst.into()),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .ok();
}

#[allow(clippy::too_many_arguments)]
fn run_hotkey_loop<F1, F2, F3, F4>(
    screenshot_hotkey: &str,
    scrolling_hotkey: Option<&str>,
    recording_hotkey: Option<&str>,
    text_hotkey: Option<&str>,
    data_dir: Option<PathBuf>,
    on_screenshot: F1,
    on_scrolling: F2,
    on_recording: F3,
    on_text: F4,
    stop_signal: Arc<AtomicBool>,
) where
    F1: Fn() + Send + Sync + 'static,
    F2: Fn() + Send + Sync + 'static,
    F3: Fn() + Send + Sync + 'static,
    F4: Fn() + Send + Sync + 'static,
{
    let mut reg_shot = false;
    let mut reg_scroll = false;
    let mut reg_record = false;
    let mut reg_text = false;

    unsafe {
        match parse_hotkey(screenshot_hotkey) {
            Ok((mods, vk)) => {
                if let Err(e) = RegisterHotKey(None, SCREENSHOT_HOTKEY_ID, mods, vk) {
                    eprintln!("Lookout Screenshot could not register hotkey '{screenshot_hotkey}' ({e}). Another program is using it.");
                    if should_show_conflict_notice(data_dir.as_deref()) {
                        show_hotkey_conflict_toast(screenshot_hotkey);
                    }
                } else {
                    reg_shot = true;
                }
            }
            Err(e) => {
                eprintln!("Invalid screenshot hotkey '{screenshot_hotkey}': {e}");
            }
        }

        if let Some(scroll_hk) = scrolling_hotkey {
            if !scroll_hk.trim().is_empty() {
                match parse_hotkey(scroll_hk) {
                    Ok((mods, vk)) => {
                        if let Err(e) = RegisterHotKey(None, SCROLLING_HOTKEY_ID, mods, vk) {
                            eprintln!("Lookout Screenshot could not register scrolling hotkey '{scroll_hk}' ({e}). Another program is using it.");
                            if should_show_conflict_notice(data_dir.as_deref()) {
                                show_hotkey_conflict_toast(scroll_hk);
                            }
                        } else {
                            reg_scroll = true;
                        }
                    }
                    Err(e) => {
                        eprintln!("Invalid scrolling hotkey '{scroll_hk}': {e}");
                    }
                }
            }
        }

        if let Some(rec_hk) = recording_hotkey {
            if !rec_hk.trim().is_empty() {
                match parse_hotkey(rec_hk) {
                    Ok((mods, vk)) => {
                        if let Err(e) = RegisterHotKey(None, RECORDING_HOTKEY_ID, mods, vk) {
                            eprintln!("Lookout Screenshot could not register recording hotkey '{rec_hk}' ({e}). Another program is using it.");
                            if should_show_conflict_notice(data_dir.as_deref()) {
                                show_hotkey_conflict_toast(rec_hk);
                            }
                        } else {
                            reg_record = true;
                        }
                    }
                    Err(e) => {
                        eprintln!("Invalid recording hotkey '{rec_hk}': {e}");
                    }
                }
            }
        }

        if let Some(txt_hk) = text_hotkey {
            if !txt_hk.trim().is_empty() {
                match parse_hotkey(txt_hk) {
                    Ok((mods, vk)) => {
                        if let Err(e) = RegisterHotKey(None, TEXT_HOTKEY_ID, mods, vk) {
                            eprintln!("Lookout Screenshot could not register text extraction hotkey '{txt_hk}' ({e}). Another program is using it.");
                            if should_show_conflict_notice(data_dir.as_deref()) {
                                show_hotkey_conflict_toast(txt_hk);
                            }
                        } else {
                            reg_text = true;
                        }
                    }
                    Err(e) => {
                        eprintln!("Invalid text extraction hotkey '{txt_hk}': {e}");
                    }
                }
            }
        }

        if !reg_shot && !reg_scroll && !reg_record && !reg_text {
            return;
        }

        let mut msg = MSG::default();
        while !stop_signal.load(Ordering::Acquire) {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if !ret.as_bool() || msg.message == WM_QUIT {
                break;
            }

            if msg.message == WM_HOTKEY {
                if msg.wParam == WPARAM(SCREENSHOT_HOTKEY_ID as usize) {
                    on_screenshot();
                } else if msg.wParam == WPARAM(SCROLLING_HOTKEY_ID as usize) {
                    on_scrolling();
                } else if msg.wParam == WPARAM(RECORDING_HOTKEY_ID as usize) {
                    on_recording();
                } else if msg.wParam == WPARAM(TEXT_HOTKEY_ID as usize) {
                    on_text();
                }
            }

            DispatchMessageW(&msg);
        }

        if reg_shot {
            let _ = UnregisterHotKey(None, SCREENSHOT_HOTKEY_ID);
        }
        if reg_scroll {
            let _ = UnregisterHotKey(None, SCROLLING_HOTKEY_ID);
        }
        if reg_record {
            let _ = UnregisterHotKey(None, RECORDING_HOTKEY_ID);
        }
        if reg_text {
            let _ = UnregisterHotKey(None, TEXT_HOTKEY_ID);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_hotkeys() {
        let (mods, vk) = parse_hotkey("Ctrl+Alt+S").unwrap();
        assert_eq!(mods.0 & MOD_CONTROL.0, MOD_CONTROL.0);
        assert_eq!(mods.0 & MOD_ALT.0, MOD_ALT.0);
        assert_eq!(vk, 'S' as u32);

        let (mods2, vk2) = parse_hotkey("Ctrl+Shift+A").unwrap();
        assert_eq!(mods2.0 & MOD_CONTROL.0, MOD_CONTROL.0);
        assert_eq!(mods2.0 & MOD_SHIFT.0, MOD_SHIFT.0);
        assert_eq!(vk2, 'A' as u32);

        let (mods3, vk3) = parse_hotkey("Alt+F4").unwrap();
        assert_eq!(mods3.0 & MOD_ALT.0, MOD_ALT.0);
        assert_eq!(vk3, (VK_F1.0 + 3) as u32);
    }

    #[test]
    fn test_parse_invalid_hotkeys() {
        // Empty
        assert!(parse_hotkey("").is_err());
        assert!(parse_hotkey("   ").is_err());

        // Missing key
        assert!(parse_hotkey("Ctrl+Alt").is_err());

        // Single key without modifier
        assert!(parse_hotkey("S").is_err());

        // Snipping tool Win+Shift+S must be rejected
        assert!(parse_hotkey("Win+Shift+S").is_err());
        assert!(parse_hotkey("Windows+Shift+S").is_err());

        // Invalid token
        assert!(parse_hotkey("Ctrl+InvalidKey").is_err());
    }

    #[test]
    fn test_conflict_notice_rate_limiting() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path();

        // First call should return true and record time
        assert!(should_show_conflict_notice(Some(path)));

        // Immediate subsequent call should return false (within 24h)
        assert!(!should_show_conflict_notice(Some(path)));
    }
}
