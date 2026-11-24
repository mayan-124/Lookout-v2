//! Screen capture across multi-monitor virtual desktop with Per-Monitor DPI v2,
//! GDI BitBlt with CAPTUREBLT and Windows.Graphics.Capture support.

use std::ptr::null_mut;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetWindowDC,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowRect, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;

/// Information about the captured virtual desktop.
#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub image: ImageBuffer,
    pub bounds: RectI,
    pub origin: PointI,
    pub is_drm_black: bool,
}

/// Retrieves the full virtual desktop bounding rectangle in physical pixels.
pub fn get_virtual_desktop_bounds() -> RectI {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        RectI::new(x, y, x + w, y + h)
    }
}

/// Sets thread DPI awareness to Per-Monitor Aware v2.
pub fn enable_per_monitor_dpi_v2() {
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Captures the entire virtual desktop across all active monitors.
pub fn capture_virtual_desktop() -> Result<CaptureResult, String> {
    enable_per_monitor_dpi_v2();

    let bounds = get_virtual_desktop_bounds();
    let w = bounds.width() as u32;
    let h = bounds.height() as u32;

    if w == 0 || h == 0 {
        return Err("Virtual desktop has zero dimensions".to_string());
    }

    let origin = PointI::new(bounds.left, bounds.top);

    // GDI BitBlt capture with CAPTUREBLT
    let image = capture_gdi_rect(bounds.left, bounds.top, w, h)?;

    let is_drm_black = image.is_black(0.999);

    Ok(CaptureResult {
        image,
        bounds,
        origin,
        is_drm_black,
    })
}

/// Captures a specific physical rectangle from the screen via GDI BitBlt, optionally overlaying the system cursor.
pub fn capture_gdi_rect_with_cursor(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    with_cursor: bool,
) -> Result<ImageBuffer, String> {
    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("GetDC(None) failed".to_string());
        }

        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        if mem_dc.is_invalid() {
            ReleaseDC(None, screen_dc);
            return Err("CreateCompatibleDC failed".to_string());
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // Top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                biSizeImage: w * h * 4,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default()],
        };

        let mut bits_ptr: *mut std::ffi::c_void = null_mut();
        let dib_bmp = CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0);

        let dib_bmp = match dib_bmp {
            Ok(b) if !b.is_invalid() && !bits_ptr.is_null() => b,
            _ => {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return Err("CreateDIBSection failed".to_string());
            }
        };

        let old_bmp = SelectObject(mem_dc, dib_bmp.into());

        // SRCCOPY | CAPTUREBLT (0x40000000)
        let rop = SRCCOPY | CAPTUREBLT;
        let blt_res = BitBlt(mem_dc, 0, 0, w as i32, h as i32, Some(screen_dc), x, y, rop);

        if with_cursor && blt_res.is_ok() {
            let mut ci = windows::Win32::UI::WindowsAndMessaging::CURSORINFO {
                cbSize: std::mem::size_of::<windows::Win32::UI::WindowsAndMessaging::CURSORINFO>()
                    as u32,
                ..Default::default()
            };
            if windows::Win32::UI::WindowsAndMessaging::GetCursorInfo(&mut ci).is_ok()
                && (ci.flags.0 & windows::Win32::UI::WindowsAndMessaging::CURSOR_SHOWING.0) != 0
                && !ci.hCursor.is_invalid()
            {
                let mut ii = windows::Win32::UI::WindowsAndMessaging::ICONINFO::default();
                if windows::Win32::UI::WindowsAndMessaging::GetIconInfo(ci.hCursor.into(), &mut ii)
                    .is_ok()
                {
                    let cur_x = ci.ptScreenPos.x - x - (ii.xHotspot as i32);
                    let cur_y = ci.ptScreenPos.y - y - (ii.yHotspot as i32);
                    let _ = windows::Win32::UI::WindowsAndMessaging::DrawIconEx(
                        mem_dc,
                        cur_x,
                        cur_y,
                        ci.hCursor.into(),
                        0,
                        0,
                        0,
                        None,
                        windows::Win32::UI::WindowsAndMessaging::DI_NORMAL,
                    );
                    if !ii.hbmMask.is_invalid() {
                        let _ = DeleteObject(ii.hbmMask.into());
                    }
                    if !ii.hbmColor.is_invalid() {
                        let _ = DeleteObject(ii.hbmColor.into());
                    }
                }
            }
        }

        let _ = SelectObject(mem_dc, old_bmp);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        if let Err(e) = blt_res {
            let _ = DeleteObject(dib_bmp.into());
            return Err(format!("BitBlt failed: {e}"));
        }

        let total_bytes = (w as usize) * (h as usize) * 4;
        let bgra_slice = std::slice::from_raw_parts(bits_ptr as *const u8, total_bytes);

        let image = ImageBuffer::from_bgra(w, h, bgra_slice);
        let _ = DeleteObject(dib_bmp.into());

        image
    }
}

/// Captures a specific physical rectangle from the screen via GDI BitBlt.
pub fn capture_gdi_rect(x: i32, y: i32, w: u32, h: u32) -> Result<ImageBuffer, String> {
    capture_gdi_rect_with_cursor(x, y, w, h, false)
}

/// Captures a running window directly via PrintWindow (or GetWindowDC/screen fallback).
pub fn capture_window_hwnd(hwnd: HWND) -> Result<ImageBuffer, String> {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        let w = (rc.right - rc.left).max(1) as u32;
        let h = (rc.bottom - rc.top).max(1) as u32;

        let win_dc = GetWindowDC(Some(hwnd));
        if !win_dc.is_invalid() {
            let mem_dc = CreateCompatibleDC(Some(win_dc));
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
                let mut bits_ptr: *mut std::ffi::c_void = null_mut();
                if let Ok(dib) =
                    CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
                {
                    if !dib.is_invalid() && !bits_ptr.is_null() {
                        let old_bmp = SelectObject(mem_dc, dib.into());
                        let _ = BitBlt(
                            mem_dc,
                            0,
                            0,
                            w as i32,
                            h as i32,
                            Some(win_dc),
                            0,
                            0,
                            SRCCOPY,
                        );
                        let slice =
                            std::slice::from_raw_parts(bits_ptr as *const u8, (w * h * 4) as usize);
                        let img = ImageBuffer::from_bgra(w, h, slice);
                        let _ = SelectObject(mem_dc, old_bmp);
                        let _ = DeleteObject(dib.into());
                        let _ = DeleteDC(mem_dc);
                        ReleaseDC(Some(hwnd), win_dc);
                        if let Ok(res) = img {
                            if !res.is_black(0.99) {
                                return Ok(res);
                            }
                        }
                    } else {
                        let _ = DeleteDC(mem_dc);
                        ReleaseDC(Some(hwnd), win_dc);
                    }
                } else {
                    let _ = DeleteDC(mem_dc);
                    ReleaseDC(Some(hwnd), win_dc);
                }
            } else {
                ReleaseDC(Some(hwnd), win_dc);
            }
        }

        // Fallback to screen capture of the window's rect
        capture_gdi_rect(rc.left, rc.top, w, h)
    }
}
