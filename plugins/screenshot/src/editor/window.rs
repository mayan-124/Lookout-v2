//! Win32 window creation, window procedure, and frame painting.

use std::path::PathBuf;
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, EndPaint, GdiFlush, InvalidateRect, SetStretchBltMode, StretchDIBits,
    UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, COLORONCOLOR, DIB_RGB_COLORS, HDC, PAINTSTRUCT,
    SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    LoadCursorW, PeekMessageW, PostQuitMessage, RegisterClassW, SetCursor, SetForegroundWindow,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, IDC_ARROW, MSG, PM_REMOVE, SW_SHOW,
    WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
};

#[cfg(feature = "review")]
use crate::annotation::AnnotationModel;
use crate::annotation::{draw_system_text_bgra, measure_system_text, Color, PRESET_COLORS};
use crate::capture::enable_per_monitor_dpi_v2;
use crate::editor::canvas::{draw_soft_shadow, Backbuffer};
use crate::editor::input::{
    handle_close, handle_key_down, handle_lbutton_down, handle_lbutton_up, handle_mouse_move,
    handle_mouse_wheel, handle_rbutton_down,
};
use crate::editor::toolbar::{
    draw_bgra_line, draw_toolbar_icon, fill_rounded_rect_bgra, get_button_rect_scaled,
    get_swatch_rect_scaled, get_thickness_rect_scaled, get_toolbar_rect_scaled,
    get_toolbar_separators, stroke_rounded_rect_bgra, BUTTONS, THICKNESS_OPTIONS,
};
use crate::editor::{EditorSession, EDITOR_SESSION};
use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;

pub fn apply_dark_window_chrome(hwnd: HWND) {
    unsafe {
        let dark_mode: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );

        let corner_pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_pref as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );

        let caption_color = COLORREF(0x0013100E);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            &caption_color as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<COLORREF>() as u32,
        );

        let text_color = COLORREF(0x00EEE9E6);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR,
            &text_color as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<COLORREF>() as u32,
        );
    }
}

pub fn get_window_dpi_scale(hwnd: HWND) -> f32 {
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

/// # Safety
/// Must be called with valid Win32 message parameters as a window procedure.
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "system" fn editor_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1), // Prevent flickering
        WM_DPICHANGED => {
            let new_dpi = (wparam.0 & 0xFFFF) as u32;
            let scale = if new_dpi == 0 {
                1.0
            } else {
                new_dpi as f32 / 96.0
            };
            EDITOR_SESSION.with(|s| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    sess.dpi_scale = scale;
                }
            });
            let prc = lparam.0 as *const windows::Win32::Foundation::RECT;
            if !prc.is_null() {
                unsafe {
                    let r = *prc;
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                            | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                    );
                }
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if let Ok(cur) = LoadCursorW(None, IDC_ARROW) {
                SetCursor(Some(cur));
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            handle_lbutton_down(hwnd, lparam);
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if handle_mouse_move(hwnd, lparam) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            handle_lbutton_up();
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            handle_rbutton_down(hwnd, lparam);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            handle_mouse_wheel(wparam);
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let (should_close, needs_redraw) = handle_key_down(hwnd, wparam);
            if should_close {
                PostQuitMessage(0);
            } else if needs_redraw {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            if handle_close(hwnd) {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            paint_editor(hwnd, hdc);

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

pub fn paint_editor(hwnd: HWND, hdc: HDC) {
    let paint_start = Instant::now();
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let win_w = (rc.right - rc.left).max(1);
    let win_h = (rc.bottom - rc.top).max(1);

    EDITOR_SESSION.with(|s| {
        let mut borrowed = s.borrow_mut();
        let Some(sess) = borrowed.as_mut() else {
            return;
        };

        sess.update_cache_if_needed();

        let bb_needs_realloc = match &sess.backbuffer {
            Some(bb) => bb.w != win_w || bb.h != win_h,
            None => true,
        };
        if bb_needs_realloc {
            sess.backbuffer = Backbuffer::new(hdc, win_w, win_h);
        }
        let Some(bb) = sess.backbuffer.as_mut() else {
            return;
        };

        let mem_dc = bb.mem_dc;
        let canvas_slice =
            unsafe { std::slice::from_raw_parts_mut(bb.bits, (win_w * win_h * 4) as usize) };
        let canvas_u32 = unsafe {
            std::slice::from_raw_parts_mut(bb.bits as *mut u32, (win_w * win_h) as usize)
        };

        // 1. Fill canvas with background #0e1013 (B=19, G=16, R=14, A=255) via vectorized 32-bit fill
        canvas_u32.fill(0xFF0E1013);

        // 2. Compute destination rectangle for scaled image
        let zoom = sess.zoom;
        let scaled_w = (sess.cached_w as f32 * zoom).round() as i32;
        let scaled_h = (sess.cached_h as f32 * zoom).round() as i32;

        let center_x = (win_w - scaled_w) / 2 + sess.pan_offset.x;
        let center_y = (win_h - scaled_h) / 2 + sess.pan_offset.y + 20;
        let img_rect = RectI::new(center_x, center_y, center_x + scaled_w, center_y + scaled_h);

        // 3. Draw soft drop shadow only around the outer perimeter of img_rect
        draw_soft_shadow(canvas_slice, win_w as usize, win_w, win_h, img_rect, 16, 6);

        // 4. StretchDIBits visible sub-rectangle hardware-accelerated directly to mem_dc
        unsafe {
            if let Some(vis) = img_rect.intersect(RectI::new(0, 0, win_w, win_h)) {
                let src_x = (((vis.left - img_rect.left) as f32 / zoom).floor() as i32)
                    .clamp(0, (sess.cached_w as i32 - 1).max(0));
                let src_y = (((vis.top - img_rect.top) as f32 / zoom).floor() as i32)
                    .clamp(0, (sess.cached_h as i32 - 1).max(0));
                let src_w = (((vis.width() as f32) / zoom).ceil() as i32)
                    .clamp(1, sess.cached_w as i32 - src_x);
                let src_h = (((vis.height() as f32) / zoom).ceil() as i32)
                    .clamp(1, sess.cached_h as i32 - src_y);

                let img_bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: sess.cached_w as i32,
                        biHeight: -(sess.cached_h as i32),
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: 0,
                        biSizeImage: (sess.cached_w * sess.cached_h * 4),
                        ..Default::default()
                    },
                    bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default()],
                };

                SetStretchBltMode(mem_dc, COLORONCOLOR);
                let _ = StretchDIBits(
                    mem_dc,
                    vis.left,
                    vis.top,
                    vis.width(),
                    vis.height(),
                    src_x,
                    src_y,
                    src_w,
                    src_h,
                    Some(sess.cached_bgra.as_ptr().cast()),
                    &img_bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                let _ = GdiFlush();
            }
        }

        // 6. Draw 1px crisp border around image (#262b33)
        for x in img_rect.left..img_rect.right {
            if x >= 0 && x < win_w {
                if img_rect.top >= 0 && img_rect.top < win_h {
                    let idx = (img_rect.top as usize * win_w as usize + x as usize) * 4;
                    canvas_slice[idx] = 51; // B
                    canvas_slice[idx + 1] = 43; // G
                    canvas_slice[idx + 2] = 38; // R
                }
                if img_rect.bottom > 0 && img_rect.bottom - 1 < win_h {
                    let idx = ((img_rect.bottom - 1) as usize * win_w as usize + x as usize) * 4;
                    canvas_slice[idx] = 51;
                    canvas_slice[idx + 1] = 43;
                    canvas_slice[idx + 2] = 38;
                }
            }
        }
        for y in img_rect.top..img_rect.bottom {
            if y >= 0 && y < win_h {
                if img_rect.left >= 0 && img_rect.left < win_w {
                    let idx = (y as usize * win_w as usize + img_rect.left as usize) * 4;
                    canvas_slice[idx] = 51;
                    canvas_slice[idx + 1] = 43;
                    canvas_slice[idx + 2] = 38;
                }
                if img_rect.right > 0 && img_rect.right - 1 < win_w {
                    let idx = (y as usize * win_w as usize + (img_rect.right - 1) as usize) * 4;
                    canvas_slice[idx] = 51;
                    canvas_slice[idx + 1] = 43;
                    canvas_slice[idx + 2] = 38;
                }
            }
        }

        // 7. Draw Floating Rounded Toolbar (#14171c, border #262b33)
        let scale = sess.dpi_scale;
        let tb = get_toolbar_rect_scaled(win_w, scale);

        // Soft drop shadow around toolbar
        draw_soft_shadow(
            canvas_slice,
            win_w as usize,
            win_w,
            win_h,
            tb,
            (12.0 * scale).round() as i32,
            (4.0 * scale).round() as i32,
        );

        // Rounded toolbar background and border
        let tb_radius = (11.0 * scale).round() as i32;
        fill_rounded_rect_bgra(
            canvas_slice,
            win_w,
            win_h,
            tb,
            tb_radius,
            Color::rgb(20, 23, 28),
        ); // #14171c
        stroke_rounded_rect_bgra(
            canvas_slice,
            win_w,
            win_h,
            tb,
            tb_radius,
            Color::rgb(38, 43, 51),
        ); // #262b33

        // Draw section separators
        let seps = get_toolbar_separators(tb, scale);
        for (sx, sy1, sy2) in seps {
            for sy in sy1..sy2 {
                if sx >= 0 && sx < win_w && sy >= 0 && sy < win_h {
                    let idx = (sy as usize * win_w as usize + sx as usize) * 4;
                    canvas_slice[idx] = 60; // B
                    canvas_slice[idx + 1] = 48; // G
                    canvas_slice[idx + 2] = 42; // R
                }
            }
        }

        // Draw buttons
        for btn in BUTTONS {
            let br = get_button_rect_scaled(tb, btn.id, scale);
            let is_hover = sess.hovered_btn == Some(btn.id);
            let is_active_tool = match btn.tool {
                Some(t) => t == sess.model.active_tool,
                None => false,
            };

            let btn_radius = (5.0 * scale).round() as i32;
            let cx = (br.left + br.right) as f32 / 2.0;
            let cy = (br.top + br.bottom) as f32 / 2.0;

            if btn.id == 14 {
                // Save button: Amber primary action
                let bg_color = if is_hover {
                    Color::rgb(251, 191, 36)
                } else {
                    Color::AMBER
                };
                fill_rounded_rect_bgra(canvas_slice, win_w, win_h, br, btn_radius, bg_color);
                let text_font_size = (13.0 * scale).round() as u32;
                let (tw, th) = measure_system_text("Save", text_font_size, true);
                let tx = (br.left + br.right - tw) / 2;
                let ty = (br.top + br.bottom - th) / 2;
                draw_system_text_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    PointI::new(tx, ty),
                    "Save",
                    Color::rgb(14, 16, 19),
                    text_font_size,
                    true,
                );
            } else if btn.id == 13 {
                // Copy button: Neutral secondary button
                let bg_color = if is_hover {
                    Color::rgb(50, 57, 71)
                } else {
                    Color::rgb(34, 38, 46)
                };
                fill_rounded_rect_bgra(canvas_slice, win_w, win_h, br, btn_radius, bg_color);
                stroke_rounded_rect_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    br,
                    btn_radius,
                    Color::rgb(56, 64, 77),
                );
                let text_font_size = (13.0 * scale).round() as u32;
                let (tw, th) = measure_system_text("Copy", text_font_size, false);
                let tx = (br.left + br.right - tw) / 2;
                let ty = (br.top + br.bottom - th) / 2;
                draw_system_text_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    PointI::new(tx, ty),
                    "Copy",
                    Color::rgb(230, 233, 238),
                    text_font_size,
                    false,
                );
            } else if btn.id == 15 {
                // Extract Text / OCR button
                let bg_color = if is_hover {
                    Color::rgb(50, 57, 71)
                } else {
                    Color::rgb(34, 38, 46)
                };
                fill_rounded_rect_bgra(canvas_slice, win_w, win_h, br, btn_radius, bg_color);
                stroke_rounded_rect_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    br,
                    btn_radius,
                    Color::rgb(56, 64, 77),
                );
                let text_font_size = (12.0 * scale).round() as u32;
                let (tw, th) = measure_system_text("Text", text_font_size, false);
                let tx = (br.left + br.right - tw) / 2;
                let ty = (br.top + br.bottom - th) / 2;
                draw_system_text_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    PointI::new(tx, ty),
                    "Text",
                    Color::rgb(230, 233, 238),
                    text_font_size,
                    false,
                );
            } else {
                // Icon buttons (0..12)
                let (bg_color, border_color, icon_color) = if is_active_tool {
                    (
                        Some(Color::rgb(52, 44, 30)),
                        Some(Color::AMBER),
                        Color::AMBER,
                    )
                } else if is_hover {
                    (
                        Some(Color::rgb(38, 43, 51)),
                        Some(Color::rgb(56, 64, 77)),
                        Color::WHITE,
                    )
                } else {
                    (None, None, Color::rgb(180, 186, 196))
                };

                if let Some(bg) = bg_color {
                    fill_rounded_rect_bgra(canvas_slice, win_w, win_h, br, btn_radius, bg);
                }
                if let Some(bdr) = border_color {
                    stroke_rounded_rect_bgra(canvas_slice, win_w, win_h, br, btn_radius, bdr);
                }
                draw_toolbar_icon(
                    canvas_slice,
                    win_w,
                    win_h,
                    btn.id,
                    cx,
                    cy,
                    scale,
                    icon_color,
                );
            }
        }

        // Draw color swatches
        let swatch_radius = (4.0 * scale).round().max(3.0) as i32;
        for (i, c) in PRESET_COLORS.iter().enumerate() {
            let s_rect = get_swatch_rect_scaled(tb, i, scale);
            let is_active = i == sess.active_color_idx;

            fill_rounded_rect_bgra(canvas_slice, win_w, win_h, s_rect, swatch_radius, *c);
            if is_active {
                stroke_rounded_rect_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    s_rect,
                    swatch_radius,
                    Color::AMBER,
                );
            } else {
                stroke_rounded_rect_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    s_rect,
                    swatch_radius,
                    Color::rgb(56, 64, 77),
                );
            }
        }

        // Draw thickness buttons
        let thick_radius = (3.0 * scale).round().max(2.0) as i32;
        for (i, t) in THICKNESS_OPTIONS.iter().enumerate() {
            let t_rect = get_thickness_rect_scaled(tb, i, scale);
            let is_active = i == sess.active_thickness_idx;

            fill_rounded_rect_bgra(
                canvas_slice,
                win_w,
                win_h,
                t_rect,
                thick_radius,
                if is_active {
                    Color::rgb(42, 48, 58)
                } else {
                    Color::rgb(24, 28, 34)
                },
            );
            stroke_rounded_rect_bgra(
                canvas_slice,
                win_w,
                win_h,
                t_rect,
                thick_radius,
                if is_active {
                    Color::AMBER
                } else {
                    Color::rgb(56, 64, 77)
                },
            );

            let line_color = if is_active {
                Color::AMBER
            } else {
                Color::rgb(180, 186, 196)
            };
            let mid_y = (t_rect.top + t_rect.bottom) as f32 / 2.0;
            let pad = 3.0 * scale;
            let x0 = t_rect.left as f32 + pad;
            let x1 = t_rect.right as f32 - pad;
            draw_bgra_line(
                canvas_slice,
                win_w,
                win_h,
                x0,
                mid_y,
                x1,
                mid_y,
                line_color,
                *t * 0.7 * scale.min(1.2),
            );
        }

        // Draw tooltip if active
        if let Some((tip_text, tip_pos)) = &sess.tooltip {
            let tip_font_size = (12.0 * scale).round() as u32;
            let (tw, th) = measure_system_text(tip_text, tip_font_size, false);
            let pad_x = (8.0 * scale).round() as i32;
            let pad_y = (5.0 * scale).round() as i32;
            let tip_w = tw + pad_x * 2;
            let tip_h = th + pad_y * 2;
            let tx = (tip_pos.x - tip_w / 2).clamp(8, win_w - tip_w - 8);
            let ty = (tip_pos.y).clamp(8, win_h - tip_h - 8);

            let tip_rect = RectI::new(tx, ty, tx + tip_w, ty + tip_h);
            draw_soft_shadow(
                canvas_slice,
                win_w as usize,
                win_w,
                win_h,
                tip_rect,
                (8.0 * scale).round() as i32,
                (3.0 * scale).round() as i32,
            );

            let tip_radius = (6.0 * scale).round() as i32;
            fill_rounded_rect_bgra(
                canvas_slice,
                win_w,
                win_h,
                tip_rect,
                tip_radius,
                Color::rgb(20, 23, 28),
            ); // #14171c
            stroke_rounded_rect_bgra(
                canvas_slice,
                win_w,
                win_h,
                tip_rect,
                tip_radius,
                Color::rgb(56, 64, 77),
            );

            draw_system_text_bgra(
                canvas_slice,
                win_w,
                win_h,
                PointI::new(tx + pad_x, ty + pad_y),
                tip_text,
                Color::rgb(230, 233, 238),
                tip_font_size,
                false,
            );
        }

        // Draw Toast notification if active
        if let Some((msg, ts)) = &sess.toast {
            if ts.elapsed().as_millis() < 2200 {
                let toast_font_size = (12.0 * scale).round() as u32;
                let (tw, th) = measure_system_text(msg, toast_font_size, true);
                let pad_x = (14.0 * scale).round() as i32;
                let pad_y = (8.0 * scale).round() as i32;
                let toast_w = tw + pad_x * 2;
                let toast_h = th + pad_y * 2;
                let tx = (win_w - toast_w) / 2;
                let ty = win_h - (48.0 * scale).round() as i32;

                let toast_rect = RectI::new(tx, ty, tx + toast_w, ty + toast_h);
                draw_soft_shadow(
                    canvas_slice,
                    win_w as usize,
                    win_w,
                    win_h,
                    toast_rect,
                    (10.0 * scale).round() as i32,
                    (3.0 * scale).round() as i32,
                );

                let toast_radius = (6.0 * scale).round() as i32;
                fill_rounded_rect_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    toast_rect,
                    toast_radius,
                    Color::AMBER,
                );

                draw_system_text_bgra(
                    canvas_slice,
                    win_w,
                    win_h,
                    PointI::new(tx + pad_x, ty + pad_y),
                    msg,
                    Color::rgb(14, 16, 19),
                    toast_font_size,
                    true,
                );
            }
        }

        // 8. BitBlt backbuffer to window DC
        unsafe {
            let _ = BitBlt(hdc, 0, 0, win_w, win_h, Some(mem_dc), 0, 0, SRCCOPY);
        }

        let elapsed_ms = paint_start.elapsed().as_secs_f64() * 1000.0;
        sess.redraw_tracker.count += 1;
        sess.redraw_tracker.total_ms += elapsed_ms;
        if sess.redraw_tracker.count == 1 || elapsed_ms < sess.redraw_tracker.min_ms {
            sess.redraw_tracker.min_ms = elapsed_ms;
        }
        if elapsed_ms > sess.redraw_tracker.max_ms {
            sess.redraw_tracker.max_ms = elapsed_ms;
        }
        sess.redraw_tracker.last_ms = elapsed_ms;
    });
}

/// Measures the average time per redraw for a 3840x2160 capture while zooming and panning.
pub fn measure_redraw_time_3840x2160() -> f64 {
    let w = 3840u32;
    let h = 2160u32;
    let mut img = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(
                x,
                y,
                [((x * 255) / w) as u8, ((y * 255) / h) as u8, 128, 255],
            );
        }
    }

    let mut session = EditorSession::new(img, false, PathBuf::from("."));
    session.update_cache_if_needed();

    let win_w = 1920i32;
    let win_h = 1080i32;

    // Simulate 50 frames of zooming and panning
    let frames = 50;
    let start = Instant::now();

    for f in 0..frames {
        let zoom = 0.5 + ((f as f32) * 0.03);
        let pan_x = f * 12;
        let pan_y = f * 6;

        let scaled_w = (session.cached_w as f32 * zoom).round() as i32;
        let scaled_h = (session.cached_h as f32 * zoom).round() as i32;
        let center_x = (win_w - scaled_w) / 2 + pan_x;
        let center_y = (win_h - scaled_h) / 2 + pan_y + 20;
        let img_rect = RectI::new(center_x, center_y, center_x + scaled_w, center_y + scaled_h);

        let mut canvas = vec![19u8; (win_w * win_h * 4) as usize];
        draw_soft_shadow(&mut canvas, win_w as usize, win_w, win_h, img_rect, 16, 6);
    }

    let elapsed = start.elapsed();
    (elapsed.as_secs_f64() * 1000.0) / (frames as f64)
}

/// Opens the editor window with the captured image.
pub fn show_editor(image: ImageBuffer, is_drm_black: bool, save_folder: PathBuf) {
    enable_per_monitor_dpi_v2();

    let initial_w = (image.width as i32 + 100).clamp(840, 1400);
    let initial_h = (image.height as i32 + 160).clamp(620, 960);

    unsafe {
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(_) => return,
        };
        let class_name = w!("LookoutScreenshotEditor");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(editor_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = match CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class_name,
            w!("Lookout Screenshot"),
            WS_OVERLAPPEDWINDOW,
            100,
            80,
            initial_w,
            initial_h,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return,
        };

        apply_dark_window_chrome(hwnd);
        let scale = get_window_dpi_scale(hwnd);
        EDITOR_SESSION.with(|s| {
            let mut sess = EditorSession::new(image, is_drm_black, save_folder);
            sess.dpi_scale = scale;
            *s.borrow_mut() = Some(sess);
        });

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = DestroyWindow(hwnd);
    }
}

/// Measures the 3840x2160 zoom/pan redraw time in a REAL Win32 window and reports numbers.
pub fn run_real_window_4k_benchmark(frames: u32) -> (f64, f64, f64, u32) {
    enable_per_monitor_dpi_v2();

    let w = 3840u32;
    let h = 2160u32;
    let mut img = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(
                x,
                y,
                [((x * 255) / w) as u8, ((y * 255) / h) as u8, 128, 255],
            );
        }
    }

    let hinst = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(_) => return (0.0, 0.0, 0.0, 0),
    };

    let class_name = w!("LookoutScreenshotEditor");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(editor_wndproc),
        hInstance: hinst.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&wc);
    }

    let hwnd = match unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class_name,
            w!("Lookout Screenshot 4K Benchmark"),
            WS_OVERLAPPEDWINDOW,
            100,
            80,
            1280,
            720,
            None,
            None,
            Some(hinst.into()),
            None,
        )
    } {
        Ok(h) => h,
        Err(_) => return (0.0, 0.0, 0.0, 0),
    };

    apply_dark_window_chrome(hwnd);

    EDITOR_SESSION.with(|s| {
        *s.borrow_mut() = Some(EditorSession::new(img, false, PathBuf::from(".")));
    });

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        // Drive zoom and pan across the REAL window
        for f in 0..frames {
            EDITOR_SESSION.with(|s| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    sess.zoom = 0.5 + ((f as f32) * 0.02);
                    sess.pan_offset =
                        PointI::new(((f as i32) * 8) % 400 - 200, ((f as i32) * 4) % 200 - 100);
                }
            });

            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);

            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    let stats = EDITOR_SESSION.with(|s| {
        let borrowed = s.borrow();
        borrowed
            .as_ref()
            .map(|sess| sess.redraw_tracker.clone())
            .unwrap_or_default()
    });

    unsafe {
        let _ = DestroyWindow(hwnd);
    }

    let avg = if stats.count > 0 {
        stats.total_ms / (stats.count as f64)
    } else {
        0.0
    };

    (avg, stats.min_ms, stats.max_ms, stats.count)
}

#[cfg(feature = "review")]
pub fn open_real_editor_for_demo_scaled(
    image: ImageBuffer,
    model: AnnotationModel,
    scale: f32,
    custom_size: Option<(i32, i32)>,
) -> Result<HWND, String> {
    enable_per_monitor_dpi_v2();

    let (initial_w, initial_h) = match custom_size {
        Some((w, h)) => (w, h),
        None => {
            let w = ((image.width as f32 + 100.0) * scale).round() as i32;
            let h = ((image.height as f32 + 160.0) * scale).round() as i32;
            (w.clamp(840, 2200), h.clamp(620, 1500))
        }
    };

    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class_name = w!("LookoutScreenshotEditor");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(editor_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class_name,
            w!("Lookout Screenshot"),
            WS_OVERLAPPEDWINDOW,
            80,
            60,
            initial_w,
            initial_h,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| e.to_string())?;

        apply_dark_window_chrome(hwnd);

        let mut sess = EditorSession::new(image, false, PathBuf::from("."));
        sess.model = model;
        sess.dpi_scale = scale;
        sess.cache_dirty = true;
        sess.update_cache_if_needed();

        EDITOR_SESSION.with(|s| {
            *s.borrow_mut() = Some(sess);
        });

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        Ok(hwnd)
    }
}

#[cfg(feature = "review")]
pub fn open_real_editor_for_demo(
    image: ImageBuffer,
    model: AnnotationModel,
) -> Result<HWND, String> {
    open_real_editor_for_demo_scaled(image, model, 1.0, None)
}
