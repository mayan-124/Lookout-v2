//! Windows clipboard integration for copying images in standard CF_DIB format
//! with automatic retry on clipboard lock.

use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

use crate::image::ImageBuffer;

const CF_DIB: u32 = 8;

/// Copies the given ImageBuffer to the Windows clipboard as a CF_DIB image.
/// Retries if another application currently locks the clipboard.
pub fn copy_to_clipboard(image: &ImageBuffer) -> Result<(), String> {
    let dib_bytes = image.to_dib_bytes();
    let dib_len = dib_bytes.len();

    // Retry up to 5 times if clipboard is locked by another program
    let mut opened = false;
    for attempt in 0..5 {
        unsafe {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
        }
        if attempt < 4 {
            sleep(Duration::from_millis(25));
        }
    }

    if !opened {
        return Err(
            "The Windows clipboard is locked by another program. Please try again.".to_string(),
        );
    }

    unsafe {
        if let Err(e) = EmptyClipboard() {
            let _ = CloseClipboard();
            return Err(format!("Could not empty clipboard: {e}"));
        }

        // Allocate moveable global memory for CF_DIB
        let h_global = match GlobalAlloc(GMEM_MOVEABLE, dib_len) {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                let _ = CloseClipboard();
                return Err("Failed to allocate global memory for clipboard data".to_string());
            }
        };

        let ptr = GlobalLock(h_global);
        if ptr.is_null() {
            let _ = CloseClipboard();
            return Err("Failed to lock global memory for clipboard data".to_string());
        }

        std::ptr::copy_nonoverlapping(dib_bytes.as_ptr(), ptr as *mut u8, dib_len);
        let _ = GlobalUnlock(h_global);

        let res = SetClipboardData(CF_DIB, Some(HANDLE(h_global.0)));
        let _ = CloseClipboard();

        match res {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Failed to set clipboard data: {e}")),
        }
    }
}

const CF_UNICODETEXT: u32 = 13;

/// Copies plain text (such as a saved file path) to the Windows clipboard as CF_UNICODETEXT.
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = wide.len() * 2;

    let mut opened = false;
    for attempt in 0..5 {
        unsafe {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
        }
        if attempt < 4 {
            sleep(Duration::from_millis(25));
        }
    }

    if !opened {
        return Err("The Windows clipboard is locked by another program.".to_string());
    }

    unsafe {
        if let Err(e) = EmptyClipboard() {
            let _ = CloseClipboard();
            return Err(format!("Could not empty clipboard: {e}"));
        }

        let h_global = match GlobalAlloc(GMEM_MOVEABLE, byte_len) {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                let _ = CloseClipboard();
                return Err("Failed to allocate global memory for clipboard text".to_string());
            }
        };

        let ptr = GlobalLock(h_global);
        if ptr.is_null() {
            let _ = CloseClipboard();
            return Err("Failed to lock global memory for clipboard text".to_string());
        }

        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, byte_len);
        let _ = GlobalUnlock(h_global);

        let res = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h_global.0)));
        let _ = CloseClipboard();

        match res {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Failed to set clipboard data: {e}")),
        }
    }
}
