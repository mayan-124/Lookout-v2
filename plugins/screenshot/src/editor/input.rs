//! Mouse and keyboard event processing for the screenshot editor window.

use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MessageBoxW, MB_ICONQUESTION, MB_YESNO,
};

use crate::annotation::{Annotation, ToolKind, PRESET_COLORS, TEXT_SIZES};
use crate::clipboard::copy_to_clipboard;
use crate::editor::canvas::screen_to_canvas;
use crate::editor::toolbar::{
    get_button_rect_scaled, get_swatch_rect_scaled, get_thickness_rect_scaled,
    get_toolbar_rect_scaled, BUTTONS, THICKNESS_OPTIONS,
};
use crate::editor::EDITOR_SESSION;
use crate::geometry::{PointI, RectI};
use crate::save::save_screenshot;

pub fn handle_lbutton_down(hwnd: HWND, lparam: LPARAM) {
    let pt = PointI::new(
        i32::from((lparam.0 as u32 & 0xFFFF) as u16 as i16),
        i32::from(((lparam.0 as u32 >> 16) & 0xFFFF) as u16 as i16),
    );

    let mut handled = false;

    EDITOR_SESSION.with(|s| {
        let mut borrowed = s.borrow_mut();
        let Some(sess) = borrowed.as_mut() else {
            return;
        };

        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let win_w = rc.right - rc.left;
        let scale = sess.dpi_scale;
        let tb_rect = get_toolbar_rect_scaled(win_w, scale);

        // Check click on toolbar buttons
        if tb_rect.contains(pt) {
            for btn in BUTTONS {
                let b_rect = get_button_rect_scaled(tb_rect, btn.id, scale);
                if b_rect.contains(pt) {
                    handled = true;
                    match btn.id {
                        0..=8 => {
                            if let Some(tool) = btn.tool {
                                sess.model.active_tool = tool;
                            }
                        }
                        9 => {
                            // Undo
                            sess.model.undo(&mut sess.image);
                            sess.cache_dirty = true;
                        }
                        10 => {
                            // Redo
                            sess.model.redo(&mut sess.image);
                            sess.cache_dirty = true;
                        }
                        11 => {
                            // Zoom Out
                            sess.zoom = (sess.zoom / 1.25).max(0.25);
                        }
                        12 => {
                            // Zoom In
                            sess.zoom = (sess.zoom * 1.25).min(5.0);
                        }
                        13 => {
                            // Copy to clipboard
                            let mut final_img = sess.image.clone();
                            sess.model.render_onto(&mut final_img);
                            match copy_to_clipboard(&final_img) {
                                Ok(()) => {
                                    sess.toast =
                                        Some(("Copied to clipboard".to_string(), Instant::now()));
                                }
                                Err(e) => {
                                    sess.toast = Some((e, Instant::now()));
                                }
                            }
                        }
                        14 => {
                            // Save as PNG
                            let mut final_img = sess.image.clone();
                            sess.model.render_onto(&mut final_img);
                            match save_screenshot(&final_img, &sess.save_folder) {
                                Ok(path) => {
                                    let fname =
                                        path.file_name().unwrap_or_default().to_string_lossy();
                                    sess.toast = Some((format!("Saved {fname}"), Instant::now()));
                                    sess.model.is_dirty = false;
                                }
                                Err(e) => {
                                    sess.toast = Some((e, Instant::now()));
                                }
                            }
                        }
                        15 => {
                            // Extract text from editor image (with markup)
                            let mut target_img = sess.image.clone();
                            sess.model.render_onto(&mut target_img);

                            match crate::ocr::execute_ocr(&target_img) {
                                Ok(extraction) => {
                                    let _ = crate::clipboard::copy_text_to_clipboard(
                                        &extraction.formatted_text,
                                    );
                                    let lines = extraction.line_count;
                                    let words = extraction.word_count;
                                    sess.toast = Some((
                                        format!("Copied {lines} lines ({words} words)"),
                                        Instant::now(),
                                    ));
                                    crate::ocr::show_ocr_preview_card(extraction, sess.dpi_scale);
                                }
                                Err(e) => {
                                    sess.toast = Some((format!("OCR failed: {e}"), Instant::now()));
                                }
                            }
                        }
                        _ => {}
                    }
                    break;
                }
            }

            // Check color swatches
            for (i, c) in PRESET_COLORS.iter().enumerate() {
                let rect = get_swatch_rect_scaled(tb_rect, i, scale);
                if rect.contains(pt) {
                    sess.active_color_idx = i;
                    sess.model.active_color = *c;
                    handled = true;
                    break;
                }
            }

            // Check thickness buttons
            if !handled {
                for (i, t) in THICKNESS_OPTIONS.iter().enumerate() {
                    let rect = get_thickness_rect_scaled(tb_rect, i, scale);
                    if rect.contains(pt) {
                        sess.active_thickness_idx = i;
                        sess.model.active_thickness = *t;
                        handled = true;
                        break;
                    }
                }
            }
        }

        // If not clicking toolbar, handle canvas drawing
        if !handled {
            let canvas_pt = screen_to_canvas(
                pt,
                sess.image.width,
                sess.image.height,
                sess.zoom,
                sess.pan_offset,
                win_w,
                rc.bottom - rc.top,
            );

            sess.is_dragging = true;
            sess.drag_start = canvas_pt;
            sess.drag_current = canvas_pt;

            match sess.model.active_tool {
                ToolKind::Step => {
                    sess.model.push(Annotation::StepMarker {
                        pos: canvas_pt,
                        number: sess.model.next_step_number,
                        color: sess.model.active_color,
                        radius: 14.0,
                    });
                    sess.cache_dirty = true;
                    sess.is_dragging = false;
                }
                ToolKind::Text => {
                    let text_sz = TEXT_SIZES[sess.active_text_size_idx.min(TEXT_SIZES.len() - 1)];
                    sess.model.push(Annotation::Text {
                        pos: canvas_pt,
                        text: "Text".to_string(),
                        color: sess.model.active_color,
                        size: text_sz,
                        bold: sess.is_text_bold,
                    });
                    sess.cache_dirty = true;
                    sess.is_dragging = false;
                }
                _ => {}
            }
        }
    });

    unsafe {
        let _ = SetCapture(hwnd);
    }
}

pub fn handle_mouse_move(hwnd: HWND, lparam: LPARAM) -> bool {
    let pt = PointI::new(
        i32::from((lparam.0 as u32 & 0xFFFF) as u16 as i16),
        i32::from(((lparam.0 as u32 >> 16) & 0xFFFF) as u16 as i16),
    );

    let mut state_changed = false;

    EDITOR_SESSION.with(|s| {
        let mut borrowed = s.borrow_mut();
        let Some(sess) = borrowed.as_mut() else {
            return;
        };

        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let win_w = rc.right - rc.left;
        let scale = sess.dpi_scale;
        let tb_rect = get_toolbar_rect_scaled(win_w, scale);

        // Update hovered button for tooltips
        let mut new_hovered_btn = None;
        let mut new_tooltip = None;

        if tb_rect.contains(pt) {
            for btn in BUTTONS {
                let b_rect = get_button_rect_scaled(tb_rect, btn.id, scale);
                if b_rect.contains(pt) {
                    new_hovered_btn = Some(btn.id);
                    new_tooltip = Some((
                        format!("{} ({})", btn.label, btn.shortcut),
                        PointI::new(
                            b_rect.center_x(),
                            b_rect.bottom + (8.0 * scale).round() as i32,
                        ),
                    ));
                    break;
                }
            }
        }

        if sess.hovered_btn != new_hovered_btn {
            sess.hovered_btn = new_hovered_btn;
            sess.tooltip = new_tooltip;
            state_changed = true;
        }

        if sess.is_dragging {
            let canvas_pt = screen_to_canvas(
                pt,
                sess.image.width,
                sess.image.height,
                sess.zoom,
                sess.pan_offset,
                win_w,
                rc.bottom - rc.top,
            );
            if sess.drag_current != canvas_pt {
                sess.drag_current = canvas_pt;
                state_changed = true;
            }
        } else if sess.is_panning {
            let dx = pt.x - sess.pan_start.x;
            let dy = pt.y - sess.pan_start.y;
            if dx != 0 || dy != 0 {
                sess.pan_offset.x += dx;
                sess.pan_offset.y += dy;
                sess.pan_start = pt;
                state_changed = true;
            }
        }
    });

    state_changed
}

pub fn handle_lbutton_up() {
    unsafe {
        let _ = ReleaseCapture();
    }

    EDITOR_SESSION.with(|s| {
        let mut borrowed = s.borrow_mut();
        let Some(sess) = borrowed.as_mut() else {
            return;
        };

        if sess.is_dragging {
            sess.is_dragging = false;
            let p1 = sess.drag_start;
            let p2 = sess.drag_current;
            let color = sess.model.active_color;
            let thickness = sess.model.active_thickness;

            match sess.model.active_tool {
                ToolKind::Arrow => {
                    if (p1.x - p2.x).abs() >= 2 || (p1.y - p2.y).abs() >= 2 {
                        sess.model.push(Annotation::Arrow {
                            start: p1,
                            end: p2,
                            color,
                            thickness,
                        });
                        sess.cache_dirty = true;
                    }
                }
                ToolKind::Line => {
                    if (p1.x - p2.x).abs() >= 2 || (p1.y - p2.y).abs() >= 2 {
                        sess.model.push(Annotation::Line {
                            start: p1,
                            end: p2,
                            color,
                            thickness,
                        });
                        sess.cache_dirty = true;
                    }
                }
                ToolKind::Rectangle => {
                    let rect = RectI::from_points(p1, p2);
                    if rect.width() >= 2 && rect.height() >= 2 {
                        sess.model.push(Annotation::Rectangle {
                            rect,
                            color,
                            thickness,
                            fill: None,
                        });
                        sess.cache_dirty = true;
                    }
                }
                ToolKind::Ellipse => {
                    let rect = RectI::from_points(p1, p2);
                    if rect.width() >= 2 && rect.height() >= 2 {
                        sess.model.push(Annotation::Ellipse {
                            rect,
                            color,
                            thickness,
                            fill: None,
                        });
                        sess.cache_dirty = true;
                    }
                }
                ToolKind::Highlighter => {
                    sess.model.push(Annotation::Highlighter {
                        points: vec![p1, p2],
                        color,
                        thickness: thickness * 4.0,
                    });
                    sess.cache_dirty = true;
                }
                ToolKind::Blur => {
                    let rect = RectI::from_points(p1, p2);
                    if rect.width() >= 4 && rect.height() >= 4 {
                        let saved = sess.image.get_rect_pixels(rect);
                        sess.image.pixelate(rect, 10);
                        sess.model.push(Annotation::Blur {
                            rect,
                            cell_size: 10,
                            saved_pixels: saved,
                        });
                        sess.cache_dirty = true;
                    }
                }
                ToolKind::Crop => {
                    let rect = RectI::from_points(p1, p2);
                    if rect.width() >= 10 && rect.height() >= 10 {
                        if let Ok(cropped) = sess.image.crop(rect) {
                            sess.image = cropped;
                            sess.model.clear();
                            sess.cache_dirty = true;
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

pub fn handle_rbutton_down(hwnd: HWND, lparam: LPARAM) {
    let pt = PointI::new(
        i32::from((lparam.0 as u32 & 0xFFFF) as u16 as i16),
        i32::from(((lparam.0 as u32 >> 16) & 0xFFFF) as u16 as i16),
    );
    EDITOR_SESSION.with(|s| {
        if let Some(sess) = s.borrow_mut().as_mut() {
            sess.is_panning = true;
            sess.pan_start = pt;
        }
    });
    unsafe {
        let _ = SetCapture(hwnd);
    }
}

pub fn handle_mouse_wheel(wparam: WPARAM) {
    let delta = ((wparam.0 >> 16) as u16) as i16;
    EDITOR_SESSION.with(|s| {
        if let Some(sess) = s.borrow_mut().as_mut() {
            if delta > 0 {
                sess.zoom = (sess.zoom * 1.15).min(5.0);
            } else {
                sess.zoom = (sess.zoom / 1.15).max(0.25);
            }
        }
    });
}

pub fn handle_key_down(hwnd: HWND, wparam: WPARAM) -> (bool, bool) {
    let key = wparam.0;
    let mut should_close = false;
    let mut needs_redraw = true;

    EDITOR_SESSION.with(|s| {
        let mut borrowed = s.borrow_mut();
        let Some(sess) = borrowed.as_mut() else {
            return;
        };

        let ctrl = unsafe { (GetKeyState(0x11) as u16 & 0x8000) != 0 };

        if ctrl {
            match key {
                0x43 => {
                    // Ctrl+C: Copy
                    let mut final_img = sess.image.clone();
                    sess.model.render_onto(&mut final_img);
                    let _ = copy_to_clipboard(&final_img);
                    sess.toast = Some(("Copied to clipboard".to_string(), Instant::now()));
                }
                0x53 => {
                    // Ctrl+S: Save
                    let mut final_img = sess.image.clone();
                    sess.model.render_onto(&mut final_img);
                    if let Ok(p) = save_screenshot(&final_img, &sess.save_folder) {
                        let name = p.file_name().unwrap_or_default().to_string_lossy();
                        sess.toast = Some((format!("Saved {name}"), Instant::now()));
                        sess.model.is_dirty = false;
                    }
                }
                0x5A => {
                    // Ctrl+Z: Undo
                    sess.model.undo(&mut sess.image);
                    sess.cache_dirty = true;
                }
                0x59 => {
                    // Ctrl+Y: Redo
                    sess.model.redo(&mut sess.image);
                    sess.cache_dirty = true;
                }
                _ => {}
            }
        } else {
            match key {
                0x1B => {
                    // Esc: Close editor (ask only if there are unsaved edits)
                    if sess.model.is_dirty {
                        unsafe {
                            let res = MessageBoxW(
                                Some(hwnd),
                                w!("You have unsaved changes. Discard and exit?"),
                                w!("Lookout Screenshot"),
                                MB_YESNO | MB_ICONQUESTION,
                            );
                            if res.0 == 6 {
                                should_close = true;
                            }
                        }
                    } else {
                        should_close = true;
                    }
                    needs_redraw = false;
                }
                0x41 => sess.model.active_tool = ToolKind::Arrow,
                0x4C => sess.model.active_tool = ToolKind::Line,
                0x52 => sess.model.active_tool = ToolKind::Rectangle,
                0x45 => sess.model.active_tool = ToolKind::Ellipse,
                0x48 => sess.model.active_tool = ToolKind::Highlighter,
                0x54 => sess.model.active_tool = ToolKind::Text,
                0x53 => sess.model.active_tool = ToolKind::Step,
                0x42 => sess.model.active_tool = ToolKind::Blur,
                0x43 => sess.model.active_tool = ToolKind::Crop,
                _ => {}
            }
        }
    });

    (should_close, needs_redraw)
}

pub fn handle_close(hwnd: HWND) -> bool {
    let mut can_close = true;
    EDITOR_SESSION.with(|s| {
        if let Some(sess) = s.borrow().as_ref() {
            if sess.model.is_dirty {
                unsafe {
                    let res = MessageBoxW(
                        Some(hwnd),
                        w!("You have unsaved changes. Discard and exit?"),
                        w!("Lookout Screenshot"),
                        MB_YESNO | MB_ICONQUESTION,
                    );
                    can_close = res.0 == 6;
                }
            }
        }
    });
    can_close
}
