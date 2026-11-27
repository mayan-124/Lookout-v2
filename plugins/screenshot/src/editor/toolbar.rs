//! Floating toolbar layout, responsive multi-row wrapping, button models, and vector icon rendering.

use crate::annotation::{Color, ToolKind};
use crate::geometry::RectI;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolbarButton {
    pub id: usize,
    pub label: &'static str,
    pub shortcut: &'static str,
    pub tool: Option<ToolKind>,
}

pub const BUTTONS: &[ToolbarButton] = &[
    ToolbarButton {
        id: 0,
        label: "Arrow",
        shortcut: "A",
        tool: Some(ToolKind::Arrow),
    },
    ToolbarButton {
        id: 1,
        label: "Line",
        shortcut: "L",
        tool: Some(ToolKind::Line),
    },
    ToolbarButton {
        id: 2,
        label: "Rectangle",
        shortcut: "R",
        tool: Some(ToolKind::Rectangle),
    },
    ToolbarButton {
        id: 3,
        label: "Ellipse",
        shortcut: "E",
        tool: Some(ToolKind::Ellipse),
    },
    ToolbarButton {
        id: 4,
        label: "Highlighter",
        shortcut: "H",
        tool: Some(ToolKind::Highlighter),
    },
    ToolbarButton {
        id: 5,
        label: "Text",
        shortcut: "T",
        tool: Some(ToolKind::Text),
    },
    ToolbarButton {
        id: 6,
        label: "Step Marker",
        shortcut: "S",
        tool: Some(ToolKind::Step),
    },
    ToolbarButton {
        id: 7,
        label: "Blur",
        shortcut: "B",
        tool: Some(ToolKind::Blur),
    },
    ToolbarButton {
        id: 8,
        label: "Crop",
        shortcut: "C",
        tool: Some(ToolKind::Crop),
    },
    ToolbarButton {
        id: 9,
        label: "Undo",
        shortcut: "Ctrl+Z",
        tool: None,
    },
    ToolbarButton {
        id: 10,
        label: "Redo",
        shortcut: "Ctrl+Y",
        tool: None,
    },
    ToolbarButton {
        id: 11,
        label: "Zoom Out",
        shortcut: "-",
        tool: None,
    },
    ToolbarButton {
        id: 12,
        label: "Zoom In",
        shortcut: "+",
        tool: None,
    },
    ToolbarButton {
        id: 13,
        label: "Copy",
        shortcut: "Ctrl+C",
        tool: None,
    },
    ToolbarButton {
        id: 14,
        label: "Save",
        shortcut: "Ctrl+S",
        tool: None,
    },
    ToolbarButton {
        id: 15,
        label: "Text from screen",
        shortcut: "O",
        tool: None,
    },
];

pub const THICKNESS_OPTIONS: [f32; 3] = [2.0, 4.0, 8.0];
pub const FONT_SIZE_OPTIONS: [(u32, &str); 4] = [(14, "14"), (18, "18"), (24, "24"), (32, "32")];

pub const TOOLBAR_SINGLE_ROW_BASE_WIDTH: f32 = 958.0;
pub const TOOLBAR_TWO_ROW_BASE_WIDTH: f32 = 640.0;
pub const TOOLBAR_THREE_ROW_BASE_WIDTH: f32 = 410.0;
pub const TOOLBAR_MULTI_ROW_BASE_WIDTH: f32 = 640.0;

pub const TOOLBAR_SINGLE_ROW_BASE_HEIGHT: f32 = 46.0;
pub const TOOLBAR_TWO_ROW_BASE_HEIGHT: f32 = 88.0;
pub const TOOLBAR_THREE_ROW_BASE_HEIGHT: f32 = 130.0;
pub const TOOLBAR_MULTI_ROW_BASE_HEIGHT: f32 = 88.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarWrapMode {
    SingleRow,
    TwoRow,
    ThreeRow,
}

/// Determines the wrap mode based on available window width in unscaled base units.
pub fn get_toolbar_wrap_mode(win_w: i32, scale: f32) -> ToolbarWrapMode {
    let avail_base_w = (win_w as f32 - 24.0) / scale;
    if avail_base_w >= TOOLBAR_SINGLE_ROW_BASE_WIDTH {
        ToolbarWrapMode::SingleRow
    } else if avail_base_w >= TOOLBAR_TWO_ROW_BASE_WIDTH {
        ToolbarWrapMode::TwoRow
    } else {
        ToolbarWrapMode::ThreeRow
    }
}

/// Returns true if the toolbar must wrap because the window cannot fit a single row.
pub fn is_multi_row(win_w: i32, scale: f32) -> bool {
    get_toolbar_wrap_mode(win_w, scale) != ToolbarWrapMode::SingleRow
}

pub fn get_toolbar_mode_from_tb_rect(tb_rect: RectI, scale: f32) -> ToolbarWrapMode {
    let base_h = tb_rect.height() as f32 / scale;
    if base_h > 105.0 {
        ToolbarWrapMode::ThreeRow
    } else if base_h > 60.0 {
        ToolbarWrapMode::TwoRow
    } else {
        ToolbarWrapMode::SingleRow
    }
}

pub fn get_mode_from_tb_rect(tb_rect: RectI, scale: f32) -> ToolbarWrapMode {
    get_toolbar_mode_from_tb_rect(tb_rect, scale)
}

pub fn get_toolbar_rect_scaled(win_w: i32, scale: f32) -> RectI {
    let mode = get_toolbar_wrap_mode(win_w, scale);
    let (base_w, base_h) = match mode {
        ToolbarWrapMode::SingleRow => (
            TOOLBAR_SINGLE_ROW_BASE_WIDTH,
            TOOLBAR_SINGLE_ROW_BASE_HEIGHT,
        ),
        ToolbarWrapMode::TwoRow => (TOOLBAR_TWO_ROW_BASE_WIDTH, TOOLBAR_TWO_ROW_BASE_HEIGHT),
        ToolbarWrapMode::ThreeRow => (TOOLBAR_THREE_ROW_BASE_WIDTH, TOOLBAR_THREE_ROW_BASE_HEIGHT),
    };
    let tb_w = (base_w * scale).round() as i32;
    let tb_h = (base_h * scale).round() as i32;
    let x = ((win_w - tb_w) / 2).max(12);
    let y = (16.0 * scale).round() as i32;
    RectI::new(x, y, x + tb_w, y + tb_h)
}

pub fn get_toolbar_rect(win_w: i32) -> RectI {
    get_toolbar_rect_scaled(win_w, 1.0)
}

pub fn get_button_rect_scaled(tb_rect: RectI, index: usize, scale: f32) -> RectI {
    let mode = get_mode_from_tb_rect(tb_rect, scale);

    let btn_w = (34.0 * scale).round() as i32;
    let btn_h = (34.0 * scale).round() as i32;
    let gap = (2.0 * scale).round() as i32;
    let pad_left = (8.0 * scale).round() as i32;
    let sep_gap = (6.0 * scale).round() as i32;
    let sep_w = (1.0 * scale).round().max(1.0) as i32;
    let sep = sep_w + 2 * sep_gap;
    let action_btn_w = (62.0 * scale).round() as i32;
    let action_gap = (6.0 * scale).round() as i32;

    match mode {
        ToolbarWrapMode::SingleRow => {
            // Single row layout
            let btn_y = tb_rect.top + (tb_rect.height() - btn_h) / 2;
            let tools_w = 9 * btn_w + 8 * gap;

            let swatch_size = (16.0 * scale).round() as i32;
            let swatch_gap = (4.0 * scale).round() as i32;
            let swatches_w = 7 * swatch_size + 6 * swatch_gap;
            let thick_btn_w = (22.0 * scale).round() as i32;
            let thick_w = 3 * thick_btn_w + 2 * gap;
            let color_group_w = swatches_w + (8.0 * scale).round() as i32 + thick_w;

            let undo_start = tb_rect.left + pad_left + tools_w + sep + color_group_w + sep;
            let undo_w = 2 * btn_w + gap;
            let zoom_start = undo_start + undo_w + sep;
            let zoom_w = 2 * btn_w + gap;
            let action_start = zoom_start + zoom_w + sep;

            let x = if index < 9 {
                tb_rect.left + pad_left + (index as i32) * (btn_w + gap)
            } else if index < 11 {
                undo_start + ((index - 9) as i32) * (btn_w + gap)
            } else if index < 13 {
                zoom_start + ((index - 11) as i32) * (btn_w + gap)
            } else {
                action_start + ((index - 13) as i32) * (action_btn_w + action_gap)
            };

            let actual_w = if index >= 13 { action_btn_w } else { btn_w };
            RectI::new(x, btn_y, x + actual_w, btn_y + btn_h)
        }
        ToolbarWrapMode::TwoRow => {
            // Multi-row 2-row layout (Row 1: Tools, Undo/Redo, Copy/Save. Row 2: Colors, Thickness, Zoom)
            let row1_y = tb_rect.top + (6.0 * scale).round() as i32;
            let row2_y = row1_y + btn_h + (8.0 * scale).round() as i32;

            let tools_w = 9 * btn_w + 8 * gap;
            let undo_start = tb_rect.left + pad_left + tools_w + sep;
            let undo_w = 2 * btn_w + gap;
            let action_start = undo_start + undo_w + sep;

            let swatch_size = (18.0 * scale).round() as i32;
            let swatch_gap = (5.0 * scale).round() as i32;
            let swatches_w = 7 * swatch_size + 6 * swatch_gap;
            let thick_btn_w = (22.0 * scale).round() as i32;
            let thick_w = 3 * thick_btn_w + 2 * gap;
            let zoom_start =
                tb_rect.left + pad_left + swatches_w + (4.0 * scale).round() as i32 + thick_w + sep;

            let (x, y, w) = if index < 9 {
                (
                    tb_rect.left + pad_left + (index as i32) * (btn_w + gap),
                    row1_y,
                    btn_w,
                )
            } else if index < 11 {
                (
                    undo_start + ((index - 9) as i32) * (btn_w + gap),
                    row1_y,
                    btn_w,
                )
            } else if index < 13 {
                (
                    zoom_start + ((index - 11) as i32) * (btn_w + gap),
                    row2_y,
                    btn_w,
                )
            } else {
                (
                    action_start + ((index - 13) as i32) * (action_btn_w + action_gap),
                    row1_y,
                    action_btn_w,
                )
            };

            RectI::new(x, y, x + w, y + btn_h)
        }
        ToolbarWrapMode::ThreeRow => {
            // 3-row layout:
            // Row 1: Drawing tools (0..8)
            // Row 2: Colors, Thickness, Zoom (11..12)
            // Row 3: Undo, Redo (9..10), Copy, Save (13..14)
            let row1_y = tb_rect.top + (6.0 * scale).round() as i32;
            let row2_y = row1_y + btn_h + (8.0 * scale).round() as i32;
            let row3_y = row2_y + btn_h + (8.0 * scale).round() as i32;

            let swatch_size = (18.0 * scale).round() as i32;
            let swatch_gap = (5.0 * scale).round() as i32;
            let swatches_w = 7 * swatch_size + 6 * swatch_gap;
            let thick_btn_w = (22.0 * scale).round() as i32;
            let thick_w = 3 * thick_btn_w + 2 * gap;
            let zoom_start =
                tb_rect.left + pad_left + swatches_w + (4.0 * scale).round() as i32 + thick_w + sep;

            let undo_start = tb_rect.left + pad_left;
            let undo_w = 2 * btn_w + gap;
            let action_start = undo_start + undo_w + sep;

            let (x, y, w) = if index < 9 {
                (
                    tb_rect.left + pad_left + (index as i32) * (btn_w + gap),
                    row1_y,
                    btn_w,
                )
            } else if index < 11 {
                (
                    undo_start + ((index - 9) as i32) * (btn_w + gap),
                    row3_y,
                    btn_w,
                )
            } else if index < 13 {
                (
                    zoom_start + ((index - 11) as i32) * (btn_w + gap),
                    row2_y,
                    btn_w,
                )
            } else {
                (
                    action_start + ((index - 13) as i32) * (action_btn_w + action_gap),
                    row3_y,
                    action_btn_w,
                )
            };

            RectI::new(x, y, x + w, y + btn_h)
        }
    }
}

pub fn get_button_rect(tb_rect: RectI, index: usize) -> RectI {
    get_button_rect_scaled(tb_rect, index, 1.0)
}

pub fn get_swatch_rect_scaled(tb_rect: RectI, index: usize, scale: f32) -> RectI {
    let mode = get_mode_from_tb_rect(tb_rect, scale);

    let btn_w = (34.0 * scale).round() as i32;
    let btn_h = (34.0 * scale).round() as i32;
    let gap = (2.0 * scale).round() as i32;
    let pad_left = (8.0 * scale).round() as i32;
    let sep_gap = (6.0 * scale).round() as i32;
    let sep_w = (1.0 * scale).round().max(1.0) as i32;
    let sep = sep_w + 2 * sep_gap;

    match mode {
        ToolbarWrapMode::SingleRow => {
            let tools_w = 9 * btn_w + 8 * gap;
            let swatch_start_x = tb_rect.left + pad_left + tools_w + sep;
            let swatch_size = (16.0 * scale).round() as i32;
            let swatch_gap = (4.0 * scale).round() as i32;
            let swatch_y = tb_rect.top + (tb_rect.height() - swatch_size) / 2;
            let cx = swatch_start_x + (index as i32) * (swatch_size + swatch_gap);
            RectI::new(cx, swatch_y, cx + swatch_size, swatch_y + swatch_size)
        }
        ToolbarWrapMode::TwoRow | ToolbarWrapMode::ThreeRow => {
            let row1_y = tb_rect.top + (6.0 * scale).round() as i32;
            let row2_y = row1_y + btn_h + (8.0 * scale).round() as i32;
            let swatch_size = (18.0 * scale).round() as i32;
            let swatch_gap = (5.0 * scale).round() as i32;
            let swatch_y = row2_y + (btn_h - swatch_size) / 2;
            let cx = tb_rect.left + pad_left + (index as i32) * (swatch_size + swatch_gap);
            RectI::new(cx, swatch_y, cx + swatch_size, swatch_y + swatch_size)
        }
    }
}

pub fn get_thickness_rect_scaled(tb_rect: RectI, index: usize, scale: f32) -> RectI {
    let mode = get_mode_from_tb_rect(tb_rect, scale);

    let btn_w = (34.0 * scale).round() as i32;
    let btn_h = (34.0 * scale).round() as i32;
    let gap = (2.0 * scale).round() as i32;
    let pad_left = (8.0 * scale).round() as i32;
    let sep_gap = (6.0 * scale).round() as i32;
    let sep_w = (1.0 * scale).round().max(1.0) as i32;
    let sep = sep_w + 2 * sep_gap;

    match mode {
        ToolbarWrapMode::SingleRow => {
            let tools_w = 9 * btn_w + 8 * gap;
            let swatch_size = (16.0 * scale).round() as i32;
            let swatch_gap = (4.0 * scale).round() as i32;
            let swatches_w = 7 * swatch_size + 6 * swatch_gap;
            let thick_start_x =
                tb_rect.left + pad_left + tools_w + sep + swatches_w + (8.0 * scale).round() as i32;
            let thick_btn_w = (22.0 * scale).round() as i32;
            let thick_btn_h = (24.0 * scale).round() as i32;
            let thick_y = tb_rect.top + (tb_rect.height() - thick_btn_h) / 2;
            let tx = thick_start_x + (index as i32) * (thick_btn_w + gap);
            RectI::new(tx, thick_y, tx + thick_btn_w, thick_y + thick_btn_h)
        }
        ToolbarWrapMode::TwoRow | ToolbarWrapMode::ThreeRow => {
            let row1_y = tb_rect.top + (6.0 * scale).round() as i32;
            let row2_y = row1_y + btn_h + (8.0 * scale).round() as i32;
            let swatch_size = (18.0 * scale).round() as i32;
            let swatch_gap = (5.0 * scale).round() as i32;
            let swatches_w = 7 * swatch_size + 6 * swatch_gap;
            let thick_start_x = tb_rect.left + pad_left + swatches_w + (4.0 * scale).round() as i32;
            let thick_btn_w = (22.0 * scale).round() as i32;
            let thick_btn_h = (24.0 * scale).round() as i32;
            let thick_y = row2_y + (btn_h - thick_btn_h) / 2;
            let tx = thick_start_x + (index as i32) * (thick_btn_w + gap);
            RectI::new(tx, thick_y, tx + thick_btn_w, thick_y + thick_btn_h)
        }
    }
}

pub fn get_toolbar_separators(tb_rect: RectI, scale: f32) -> Vec<(i32, i32, i32)> {
    let mode = get_mode_from_tb_rect(tb_rect, scale);

    let mut seps = Vec::new();
    let btn_w = (34.0 * scale).round() as i32;
    let btn_h = (34.0 * scale).round() as i32;
    let gap = (2.0 * scale).round() as i32;
    let pad_left = (8.0 * scale).round() as i32;
    let sep_gap = (6.0 * scale).round() as i32;
    let sep_w = (1.0 * scale).round().max(1.0) as i32;

    match mode {
        ToolbarWrapMode::SingleRow => {
            let tools_w = 9 * btn_w + 8 * gap;
            let swatch_size = (16.0 * scale).round() as i32;
            let swatch_gap = (4.0 * scale).round() as i32;
            let swatches_w = 7 * swatch_size + 6 * swatch_gap;
            let thick_btn_w = (22.0 * scale).round() as i32;
            let thick_w = 3 * thick_btn_w + 2 * gap;
            let color_group_w = swatches_w + (8.0 * scale).round() as i32 + thick_w;

            let sep_h = (20.0 * scale).round() as i32;
            let y1 = tb_rect.top + (tb_rect.height() - sep_h) / 2;
            let y2 = y1 + sep_h;

            let s1 = tb_rect.left + pad_left + tools_w + sep_gap;
            let s2 = s1 + sep_w + sep_gap + color_group_w + sep_gap;
            let undo_w = 2 * btn_w + gap;
            let s3 = s2 + sep_w + sep_gap + undo_w + sep_gap;
            let zoom_w = 2 * btn_w + gap;
            let s4 = s3 + sep_w + sep_gap + zoom_w + sep_gap;

            seps.push((s1, y1, y2));
            seps.push((s2, y1, y2));
            seps.push((s3, y1, y2));
            seps.push((s4, y1, y2));
        }
        ToolbarWrapMode::TwoRow => {
            let row1_y = tb_rect.top + (6.0 * scale).round() as i32;
            let row2_y = row1_y + btn_h + (8.0 * scale).round() as i32;
            let sep_h = (20.0 * scale).round() as i32;

            let r1_y1 = row1_y + (btn_h - sep_h) / 2;
            let r1_y2 = r1_y1 + sep_h;
            let tools_w = 9 * btn_w + 8 * gap;
            let s1 = tb_rect.left + pad_left + tools_w + sep_gap;
            let undo_w = 2 * btn_w + gap;
            let s2 = s1 + sep_w + sep_gap + undo_w + sep_gap;
            seps.push((s1, r1_y1, r1_y2));
            seps.push((s2, r1_y1, r1_y2));

            let r2_y1 = row2_y + (btn_h - sep_h) / 2;
            let r2_y2 = r2_y1 + sep_h;
            let swatch_size = (18.0 * scale).round() as i32;
            let swatch_gap = (5.0 * scale).round() as i32;
            let swatches_w = 7 * swatch_size + 6 * swatch_gap;
            let s3 = tb_rect.left + pad_left + swatches_w + (4.0 * scale).round() as i32;
            let thick_btn_w = (22.0 * scale).round() as i32;
            let thick_w = 3 * thick_btn_w + 2 * gap;
            let s4 = s3 + (4.0 * scale).round() as i32 + thick_w + sep_gap;
            seps.push((s3, r2_y1, r2_y2));
            seps.push((s4, r2_y1, r2_y2));
        }
        ToolbarWrapMode::ThreeRow => {
            let row1_y = tb_rect.top + (6.0 * scale).round() as i32;
            let row2_y = row1_y + btn_h + (8.0 * scale).round() as i32;
            let row3_y = row2_y + btn_h + (8.0 * scale).round() as i32;
            let sep_h = (20.0 * scale).round() as i32;

            // Row 2 separator before Zoom
            let r2_y1 = row2_y + (btn_h - sep_h) / 2;
            let r2_y2 = r2_y1 + sep_h;
            let swatch_size = (18.0 * scale).round() as i32;
            let swatch_gap = (5.0 * scale).round() as i32;
            let swatches_w = 7 * swatch_size + 6 * swatch_gap;
            let s_r2 = tb_rect.left + pad_left + swatches_w + (4.0 * scale).round() as i32;
            let thick_btn_w = (22.0 * scale).round() as i32;
            let thick_w = 3 * thick_btn_w + 2 * gap;
            let s_zoom = s_r2 + (4.0 * scale).round() as i32 + thick_w + sep_gap;
            seps.push((s_zoom, r2_y1, r2_y2));

            // Row 3 separator between Redo and Copy
            let r3_y1 = row3_y + (btn_h - sep_h) / 2;
            let r3_y2 = r3_y1 + sep_h;
            let undo_w = 2 * btn_w + gap;
            let s_action = tb_rect.left + pad_left + undo_w + sep_gap;
            seps.push((s_action, r3_y1, r3_y2));
        }
    }

    seps
}

#[allow(clippy::too_many_arguments)]
pub fn draw_bgra_line(
    buf: &mut [u8],
    win_w: i32,
    win_h: i32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: Color,
    thickness: f32,
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let dist = (dx * dx + dy * dy).sqrt();
    let r = (thickness * 0.5).max(0.6);
    let r_ceil = r.ceil() as i32;
    let r2 = r * r;

    let steps = ((dist * 2.5).ceil() as usize).max(1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let cx = x0 + dx * t;
        let cy = y0 + dy * t;
        let icx = cx.round() as i32;
        let icy = cy.round() as i32;

        for oy in -r_ceil..=r_ceil {
            let py = icy + oy;
            if py < 0 || py >= win_h {
                continue;
            }
            for ox in -r_ceil..=r_ceil {
                let px = icx + ox;
                if px < 0 || px >= win_w {
                    continue;
                }
                let d2 = (px as f32 - cx) * (px as f32 - cx) + (py as f32 - cy) * (py as f32 - cy);
                if d2 <= r2 {
                    let idx = (py as usize * win_w as usize + px as usize) * 4;
                    let a = color.a as u32;
                    if a == 255 {
                        buf[idx] = color.b;
                        buf[idx + 1] = color.g;
                        buf[idx + 2] = color.r;
                    } else {
                        let inv_a = 255 - a;
                        buf[idx] = ((color.b as u32 * a + buf[idx] as u32 * inv_a) / 255) as u8;
                        buf[idx + 1] =
                            ((color.g as u32 * a + buf[idx + 1] as u32 * inv_a) / 255) as u8;
                        buf[idx + 2] =
                            ((color.r as u32 * a + buf[idx + 2] as u32 * inv_a) / 255) as u8;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_bgra_circle(
    buf: &mut [u8],
    win_w: i32,
    win_h: i32,
    cx: f32,
    cy: f32,
    radius: f32,
    color: Color,
    thickness: f32,
) {
    let segments = 24;
    for i in 0..segments {
        let a1 = (i as f32) * std::f32::consts::TAU / (segments as f32);
        let a2 = ((i + 1) as f32) * std::f32::consts::TAU / (segments as f32);
        draw_bgra_line(
            buf,
            win_w,
            win_h,
            cx + radius * a1.cos(),
            cy + radius * a1.sin(),
            cx + radius * a2.cos(),
            cy + radius * a2.sin(),
            color,
            thickness,
        );
    }
}

pub fn fill_rounded_rect_bgra(
    buf: &mut [u8],
    win_w: i32,
    win_h: i32,
    rect: RectI,
    radius: i32,
    bg: Color,
) {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    let r = radius
        .min((rect.right - rect.left) / 2)
        .min((rect.bottom - rect.top) / 2)
        .max(0);
    let r_f = r as f32;

    for y in rect.top..rect.bottom {
        if y < 0 || y >= win_h {
            continue;
        }
        for x in rect.left..rect.right {
            if x < 0 || x >= win_w {
                continue;
            }

            let in_tl = x < rect.left + r && y < rect.top + r;
            let in_tr = x >= rect.right - r && y < rect.top + r;
            let in_bl = x < rect.left + r && y >= rect.bottom - r;
            let in_br = x >= rect.right - r && y >= rect.bottom - r;

            let mut alpha_factor = 1.0f32;
            if in_tl || in_tr || in_bl || in_br {
                let corner_cx = if in_tl || in_bl {
                    rect.left + r
                } else {
                    rect.right - r - 1
                } as f32;
                let corner_cy = if in_tl || in_tr {
                    rect.top + r
                } else {
                    rect.bottom - r - 1
                } as f32;
                let dx = x as f32 - corner_cx;
                let dy = y as f32 - corner_cy;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > r_f + 0.5 {
                    continue;
                } else if dist > r_f - 0.5 {
                    alpha_factor = (r_f + 0.5 - dist).clamp(0.0, 1.0);
                }
            }

            let idx = (y as usize * win_w as usize + x as usize) * 4;
            let a = ((bg.a as f32 * alpha_factor) as u32).min(255);
            if a == 255 {
                buf[idx] = bg.b;
                buf[idx + 1] = bg.g;
                buf[idx + 2] = bg.r;
            } else if a > 0 {
                let inv_a = 255 - a;
                buf[idx] = ((bg.b as u32 * a + buf[idx] as u32 * inv_a) / 255) as u8;
                buf[idx + 1] = ((bg.g as u32 * a + buf[idx + 1] as u32 * inv_a) / 255) as u8;
                buf[idx + 2] = ((bg.r as u32 * a + buf[idx + 2] as u32 * inv_a) / 255) as u8;
            }
        }
    }
}

pub fn stroke_rounded_rect_bgra(
    buf: &mut [u8],
    win_w: i32,
    win_h: i32,
    rect: RectI,
    radius: i32,
    color: Color,
) {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    let r = radius
        .min((rect.right - rect.left) / 2)
        .min((rect.bottom - rect.top) / 2)
        .max(0);
    let r_f = r as f32;

    for y in rect.top..rect.bottom {
        if y < 0 || y >= win_h {
            continue;
        }
        for x in rect.left..rect.right {
            if x < 0 || x >= win_w {
                continue;
            }

            let is_edge_x = x == rect.left || x == rect.right - 1;
            let is_edge_y = y == rect.top || y == rect.bottom - 1;

            let in_tl = x < rect.left + r && y < rect.top + r;
            let in_tr = x >= rect.right - r && y < rect.top + r;
            let in_bl = x < rect.left + r && y >= rect.bottom - r;
            let in_br = x >= rect.right - r && y >= rect.bottom - r;

            let mut alpha_factor = 0.0f32;
            if in_tl || in_tr || in_bl || in_br {
                let corner_cx = if in_tl || in_bl {
                    rect.left + r
                } else {
                    rect.right - r - 1
                } as f32;
                let corner_cy = if in_tl || in_tr {
                    rect.top + r
                } else {
                    rect.bottom - r - 1
                } as f32;
                let dx = x as f32 - corner_cx;
                let dy = y as f32 - corner_cy;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= r_f + 0.5 && dist >= r_f - 1.0 {
                    alpha_factor = 1.0 - (dist - (r_f - 0.5)).abs().min(1.0);
                }
            } else if is_edge_x || is_edge_y {
                alpha_factor = 1.0;
            }

            if alpha_factor > 0.0 {
                let idx = (y as usize * win_w as usize + x as usize) * 4;
                let a = ((color.a as f32 * alpha_factor) as u32).min(255);
                if a == 255 {
                    buf[idx] = color.b;
                    buf[idx + 1] = color.g;
                    buf[idx + 2] = color.r;
                } else if a > 0 {
                    let inv_a = 255 - a;
                    buf[idx] = ((color.b as u32 * a + buf[idx] as u32 * inv_a) / 255) as u8;
                    buf[idx + 1] = ((color.g as u32 * a + buf[idx + 1] as u32 * inv_a) / 255) as u8;
                    buf[idx + 2] = ((color.r as u32 * a + buf[idx + 2] as u32 * inv_a) / 255) as u8;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_toolbar_icon(
    buf: &mut [u8],
    win_w: i32,
    win_h: i32,
    btn_id: usize,
    cx: f32,
    cy: f32,
    scale: f32,
    color: Color,
) {
    let stroke = (1.5 * scale).max(1.0);
    let s = 7.0 * scale;

    match btn_id {
        0 => {
            // Arrow (clean diagonal vector with angled head)
            let x0 = cx - s * 0.9;
            let y0 = cy + s * 0.9;
            let x1 = cx + s * 0.9;
            let y1 = cy - s * 0.9;
            draw_bgra_line(buf, win_w, win_h, x0, y0, x1, y1, color, stroke);

            let head_len = 5.5 * scale;
            let angle = (y1 - y0).atan2(x1 - x0);
            let a1 = angle + std::f32::consts::FRAC_PI_4 * 2.8;
            let a2 = angle - std::f32::consts::FRAC_PI_4 * 2.8;
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                x1,
                y1,
                x1 + head_len * a1.cos(),
                y1 + head_len * a1.sin(),
                color,
                stroke,
            );
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                x1,
                y1,
                x1 + head_len * a2.cos(),
                y1 + head_len * a2.sin(),
                color,
                stroke,
            );
        }
        1 => {
            // Line
            let x0 = cx - s * 0.9;
            let y0 = cy + s * 0.9;
            let x1 = cx + s * 0.9;
            let y1 = cy - s * 0.9;
            draw_bgra_line(buf, win_w, win_h, x0, y0, x1, y1, color, stroke);
        }
        2 => {
            // Rectangle outline
            let x0 = cx - s * 0.9;
            let y0 = cy - s * 0.7;
            let x1 = cx + s * 0.9;
            let y1 = cy + s * 0.7;
            draw_bgra_line(buf, win_w, win_h, x0, y0, x1, y0, color, stroke);
            draw_bgra_line(buf, win_w, win_h, x1, y0, x1, y1, color, stroke);
            draw_bgra_line(buf, win_w, win_h, x1, y1, x0, y1, color, stroke);
            draw_bgra_line(buf, win_w, win_h, x0, y1, x0, y0, color, stroke);
        }
        3 => {
            // Ellipse outline
            draw_bgra_circle(buf, win_w, win_h, cx, cy, s * 0.85, color, stroke);
        }
        4 => {
            // Highlighter pen icon (tilted marker tip)
            let x0 = cx - s * 0.7;
            let y0 = cy + s * 0.7;
            let x1 = cx + s * 0.4;
            let y1 = cy - s * 0.7;
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                x0,
                y0,
                x1,
                y1,
                Color::rgba(color.r, color.g, color.b, 200),
                stroke * 2.8,
            );
            draw_bgra_line(buf, win_w, win_h, x0, y0, x1, y1, color, stroke);
        }
        5 => {
            // Text tool "T" icon
            let x0 = cx - s * 0.8;
            let x1 = cx + s * 0.8;
            let y_top = cy - s * 0.8;
            let y_bot = cy + s * 0.8;
            draw_bgra_line(buf, win_w, win_h, x0, y_top, x1, y_top, color, stroke);
            draw_bgra_line(buf, win_w, win_h, cx, y_top, cx, y_bot, color, stroke);
        }
        6 => {
            // Step marker icon (numbered circle)
            draw_bgra_circle(buf, win_w, win_h, cx, cy, s * 0.85, color, stroke);
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                cx,
                cy - s * 0.45,
                cx,
                cy + s * 0.45,
                color,
                stroke,
            );
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                cx - s * 0.25,
                cy - s * 0.25,
                cx,
                cy - s * 0.45,
                color,
                stroke,
            );
        }
        7 => {
            // Blur icon (3x3 dot grid pattern representing pixelation)
            let spacing = 3.5 * scale;
            for oy in -1..=1 {
                for ox in -1..=1 {
                    let dot_x = cx + (ox as f32) * spacing;
                    let dot_y = cy + (oy as f32) * spacing;
                    draw_bgra_circle(
                        buf,
                        win_w,
                        win_h,
                        dot_x,
                        dot_y,
                        0.9 * scale,
                        color,
                        1.0 * scale,
                    );
                }
            }
        }
        8 => {
            // Crop icon (two overlapping right angles)
            let left = cx - s * 0.8;
            let right = cx + s * 0.8;
            let top = cy - s * 0.8;
            let bot = cy + s * 0.8;
            let ext = 3.0 * scale;

            draw_bgra_line(
                buf,
                win_w,
                win_h,
                left - ext,
                top,
                right,
                top,
                color,
                stroke,
            );
            draw_bgra_line(buf, win_w, win_h, left, top - ext, left, bot, color, stroke);
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                left,
                bot,
                right + ext,
                bot,
                color,
                stroke,
            );
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                right,
                top,
                right,
                bot + ext,
                color,
                stroke,
            );
        }
        9 => {
            // Undo icon (curved back arrow)
            let arc_r = s * 0.65;
            let arc_cx = cx + 1.0 * scale;
            let arc_cy = cy;
            let segments = 12;
            for i in 0..segments {
                let a1 = std::f32::consts::PI
                    + (i as f32) * (std::f32::consts::PI * 0.9) / (segments as f32);
                let a2 = std::f32::consts::PI
                    + ((i + 1) as f32) * (std::f32::consts::PI * 0.9) / (segments as f32);
                draw_bgra_line(
                    buf,
                    win_w,
                    win_h,
                    arc_cx + arc_r * a1.cos(),
                    arc_cy + arc_r * a1.sin(),
                    arc_cx + arc_r * a2.cos(),
                    arc_cy + arc_r * a2.sin(),
                    color,
                    stroke,
                );
            }
            let arrow_tip_x = arc_cx - arc_r;
            let arrow_tip_y = arc_cy;
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                arrow_tip_x,
                arrow_tip_y,
                arrow_tip_x - 3.5 * scale,
                arrow_tip_y - 3.5 * scale,
                color,
                stroke,
            );
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                arrow_tip_x,
                arrow_tip_y,
                arrow_tip_x + 3.5 * scale,
                arrow_tip_y - 3.5 * scale,
                color,
                stroke,
            );
        }
        10 => {
            // Redo icon (curved forward arrow)
            let arc_r = s * 0.65;
            let arc_cx = cx - 1.0 * scale;
            let arc_cy = cy;
            let segments = 12;
            for i in 0..segments {
                let a1 = -std::f32::consts::PI * 0.9 * (i as f32) / (segments as f32);
                let a2 = -std::f32::consts::PI * 0.9 * ((i + 1) as f32) / (segments as f32);
                draw_bgra_line(
                    buf,
                    win_w,
                    win_h,
                    arc_cx + arc_r * a1.cos(),
                    arc_cy + arc_r * a1.sin(),
                    arc_cx + arc_r * a2.cos(),
                    arc_cy + arc_r * a2.sin(),
                    color,
                    stroke,
                );
            }
            let arrow_tip_x = arc_cx + arc_r;
            let arrow_tip_y = arc_cy;
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                arrow_tip_x,
                arrow_tip_y,
                arrow_tip_x + 3.5 * scale,
                arrow_tip_y - 3.5 * scale,
                color,
                stroke,
            );
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                arrow_tip_x,
                arrow_tip_y,
                arrow_tip_x - 3.5 * scale,
                arrow_tip_y - 3.5 * scale,
                color,
                stroke,
            );
        }
        11 => {
            // Zoom Out icon (magnifier minus)
            let cr = s * 0.55;
            let ccx = cx - 1.5 * scale;
            let ccy = cy - 1.5 * scale;
            draw_bgra_circle(buf, win_w, win_h, ccx, ccy, cr, color, stroke);
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                ccx + cr * 0.7,
                ccy + cr * 0.7,
                cx + s * 0.8,
                cy + s * 0.8,
                color,
                stroke * 1.3,
            );
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                ccx - cr * 0.5,
                ccy,
                ccx + cr * 0.5,
                ccy,
                color,
                stroke,
            );
        }
        12 => {
            // Zoom In icon (magnifier plus)
            let cr = s * 0.55;
            let ccx = cx - 1.5 * scale;
            let ccy = cy - 1.5 * scale;
            draw_bgra_circle(buf, win_w, win_h, ccx, ccy, cr, color, stroke);
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                ccx + cr * 0.7,
                ccy + cr * 0.7,
                cx + s * 0.8,
                cy + s * 0.8,
                color,
                stroke * 1.3,
            );
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                ccx - cr * 0.5,
                ccy,
                ccx + cr * 0.5,
                ccy,
                color,
                stroke,
            );
            draw_bgra_line(
                buf,
                win_w,
                win_h,
                ccx,
                ccy - cr * 0.5,
                ccx,
                ccy + cr * 0.5,
                color,
                stroke,
            );
        }
        _ => {}
    }
}
