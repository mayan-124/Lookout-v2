//! Annotation data model, undo/redo stack, and software rasterizer.

use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;

#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, GdiFlush,
    GetTextExtentPoint32W, SelectObject, SetBkMode, SetTextColor, TextOutW, ANTIALIASED_QUALITY,
    BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, DIB_RGB_COLORS, FF_DONTCARE, OUT_DEFAULT_PRECIS, TRANSPARENT,
};

/// RGBA Color representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    // Color presets
    pub const AMBER: Self = Self::rgb(245, 165, 36); // #f5a524 (brand accent)
    pub const RED: Self = Self::rgb(239, 68, 68); // #ef4444
    pub const GREEN: Self = Self::rgb(16, 185, 129); // #10b981
    pub const BLUE: Self = Self::rgb(14, 165, 233); // #0ea5e9
    pub const PURPLE: Self = Self::rgb(139, 92, 246); // #8b5cf6
    pub const WHITE: Self = Self::rgb(255, 255, 255); // #ffffff
    pub const BLACK: Self = Self::rgb(0, 0, 0); // #000000
}

pub const PRESET_COLORS: [Color; 7] = [
    Color::AMBER,
    Color::RED,
    Color::GREEN,
    Color::BLUE,
    Color::PURPLE,
    Color::WHITE,
    Color::BLACK,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Select,
    Arrow,
    Line,
    Rectangle,
    Ellipse,
    Highlighter,
    Text,
    Step,
    Blur,
    Crop,
}

#[derive(Debug, Clone)]
pub enum Annotation {
    Arrow {
        start: PointI,
        end: PointI,
        color: Color,
        thickness: f32,
    },
    Line {
        start: PointI,
        end: PointI,
        color: Color,
        thickness: f32,
    },
    Rectangle {
        rect: RectI,
        color: Color,
        thickness: f32,
        fill: Option<Color>,
    },
    Ellipse {
        rect: RectI,
        color: Color,
        thickness: f32,
        fill: Option<Color>,
    },
    Highlighter {
        points: Vec<PointI>,
        color: Color,
        thickness: f32,
    },
    Text {
        pos: PointI,
        text: String,
        color: Color,
        size: u32,
        bold: bool,
    },
    StepMarker {
        pos: PointI,
        number: u32,
        color: Color,
        radius: f32,
    },
    Blur {
        rect: RectI,
        cell_size: u32,
        saved_pixels: (RectI, Vec<u8>),
    },
    Crop {
        prev_crop: RectI,
        new_crop: RectI,
    },
}

/// Manages active annotations and the undo/redo history.
#[derive(Debug, Clone)]
pub struct AnnotationModel {
    pub annotations: Vec<Annotation>,
    pub redo_stack: Vec<Annotation>,
    pub next_step_number: u32,
    pub active_tool: ToolKind,
    pub active_color: Color,
    pub active_thickness: f32,
    pub is_dirty: bool,
}

impl Default for AnnotationModel {
    fn default() -> Self {
        Self {
            annotations: Vec::new(),
            redo_stack: Vec::new(),
            next_step_number: 1,
            active_tool: ToolKind::Arrow,
            active_color: Color::AMBER,
            active_thickness: 3.0,
            is_dirty: false,
        }
    }
}

impl AnnotationModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, ann: Annotation) {
        if let Annotation::StepMarker { .. } = ann {
            self.next_step_number += 1;
        }
        self.annotations.push(ann);
        self.redo_stack.clear();
        self.is_dirty = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.annotations.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self, image: &mut ImageBuffer) -> Option<Annotation> {
        let ann = self.annotations.pop()?;
        match &ann {
            Annotation::StepMarker { .. } => {
                self.next_step_number = self.next_step_number.saturating_sub(1).max(1);
            }
            Annotation::Blur { saved_pixels, .. } => {
                image.set_rect_pixels(saved_pixels.0, &saved_pixels.1);
            }
            _ => {}
        }
        self.redo_stack.push(ann.clone());
        self.is_dirty = !self.annotations.is_empty();
        Some(ann)
    }

    pub fn redo(&mut self, image: &mut ImageBuffer) -> Option<Annotation> {
        let ann = self.redo_stack.pop()?;
        match &ann {
            Annotation::StepMarker { .. } => {
                self.next_step_number += 1;
            }
            Annotation::Blur {
                rect, cell_size, ..
            } => {
                image.pixelate(*rect, *cell_size);
            }
            _ => {}
        }
        self.annotations.push(ann.clone());
        self.is_dirty = true;
        Some(ann)
    }

    pub fn clear(&mut self) {
        self.annotations.clear();
        self.redo_stack.clear();
        self.next_step_number = 1;
        self.is_dirty = false;
    }

    /// Renders all annotations onto the image buffer destructively.
    pub fn render_onto(&self, image: &mut ImageBuffer) {
        for ann in &self.annotations {
            render_annotation(image, ann);
        }
    }
}

/// Software rasterizer to draw an annotation directly into an ImageBuffer.
pub fn render_annotation(image: &mut ImageBuffer, ann: &Annotation) {
    match ann {
        Annotation::Line {
            start,
            end,
            color,
            thickness,
        } => {
            draw_thick_line(image, *start, *end, *color, *thickness);
        }
        Annotation::Arrow {
            start,
            end,
            color,
            thickness,
        } => {
            draw_arrow(image, *start, *end, *color, *thickness);
        }
        Annotation::Rectangle {
            rect,
            color,
            thickness,
            fill,
        } => {
            if let Some(f) = fill {
                fill_rect(image, *rect, *f);
            }
            draw_rect_outline(image, *rect, *color, *thickness);
        }
        Annotation::Ellipse {
            rect,
            color,
            thickness,
            fill,
        } => {
            if let Some(f) = fill {
                fill_ellipse(image, *rect, *f);
            }
            draw_ellipse_outline(image, *rect, *color, *thickness);
        }
        Annotation::Highlighter {
            points,
            color,
            thickness,
        } => {
            draw_highlighter_path(image, points, *color, *thickness);
        }
        Annotation::Text {
            pos,
            text,
            color,
            size,
            bold,
        } => {
            draw_system_text_rgba(image, *pos, text, *color, *size, *bold);
        }
        Annotation::StepMarker {
            pos,
            number,
            color,
            radius,
        } => {
            draw_step_marker(image, *pos, *number, *color, *radius);
        }
        Annotation::Blur {
            rect, cell_size, ..
        } => {
            image.pixelate(*rect, *cell_size);
        }
        Annotation::Crop { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Software Rasterization Functions

fn blend_pixel(image: &mut ImageBuffer, x: i32, y: i32, color: Color) {
    if x < 0 || y < 0 || x >= image.width as i32 || y >= image.height as i32 {
        return;
    }
    let idx = ((y as usize) * (image.width as usize) + (x as usize)) * 4;
    let alpha = color.a as u32;

    if alpha == 255 {
        image.pixels[idx] = color.r;
        image.pixels[idx + 1] = color.g;
        image.pixels[idx + 2] = color.b;
        image.pixels[idx + 3] = 255;
    } else if alpha > 0 {
        let inv_alpha = 255 - alpha;
        let dst_r = image.pixels[idx] as u32;
        let dst_g = image.pixels[idx + 1] as u32;
        let dst_b = image.pixels[idx + 2] as u32;

        image.pixels[idx] = ((color.r as u32 * alpha + dst_r * inv_alpha) / 255) as u8;
        image.pixels[idx + 1] = ((color.g as u32 * alpha + dst_g * inv_alpha) / 255) as u8;
        image.pixels[idx + 2] = ((color.b as u32 * alpha + dst_b * inv_alpha) / 255) as u8;
        image.pixels[idx + 3] = 255;
    }
}

fn draw_filled_circle(image: &mut ImageBuffer, cx: i32, cy: i32, r: f32, color: Color) {
    let r_ceil = r.ceil() as i32;
    let r2 = r * r;
    for dy in -r_ceil..=r_ceil {
        for dx in -r_ceil..=r_ceil {
            if (dx * dx + dy * dy) as f32 <= r2 {
                blend_pixel(image, cx + dx, cy + dy, color);
            }
        }
    }
}

fn draw_thick_line(image: &mut ImageBuffer, p1: PointI, p2: PointI, color: Color, thickness: f32) {
    let dx = (p2.x - p1.x) as f32;
    let dy = (p2.y - p1.y) as f32;
    let dist = (dx * dx + dy * dy).sqrt();

    if dist <= 0.001 {
        draw_filled_circle(image, p1.x, p1.y, thickness / 2.0, color);
        return;
    }

    let radius = (thickness / 2.0).max(0.5);
    let steps = (dist * 2.0).ceil() as usize;

    for i in 0..=steps {
        let t = (i as f32) / (steps as f32);
        let x = (p1.x as f32 + dx * t).round() as i32;
        let y = (p1.y as f32 + dy * t).round() as i32;
        draw_filled_circle(image, x, y, radius, color);
    }
}

fn draw_arrow(image: &mut ImageBuffer, start: PointI, end: PointI, color: Color, thickness: f32) {
    draw_thick_line(image, start, end, color, thickness);

    let dx = (end.x - start.x) as f32;
    let dy = (end.y - start.y) as f32;
    let angle = dy.atan2(dx);

    let head_len = (16.0 + thickness * 2.0).max(12.0);
    let wing_angle = 0.48f32; // ~27.5 degrees

    let w1_x = end.x as f32 - head_len * (angle - wing_angle).cos();
    let w1_y = end.y as f32 - head_len * (angle - wing_angle).sin();

    let w2_x = end.x as f32 - head_len * (angle + wing_angle).cos();
    let w2_y = end.y as f32 - head_len * (angle + wing_angle).sin();

    let w1 = PointI::new(w1_x.round() as i32, w1_y.round() as i32);
    let w2 = PointI::new(w2_x.round() as i32, w2_y.round() as i32);

    draw_thick_line(image, end, w1, color, thickness);
    draw_thick_line(image, end, w2, color, thickness);
}

fn fill_rect(image: &mut ImageBuffer, rect: RectI, color: Color) {
    let r = rect.clamp_to(RectI::new(0, 0, image.width as i32, image.height as i32));
    for y in r.top..r.bottom {
        for x in r.left..r.right {
            blend_pixel(image, x, y, color);
        }
    }
}

pub fn fill_rounded_rect_rgba(image: &mut ImageBuffer, rect: RectI, radius: i32, color: Color) {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    let r = radius
        .min((rect.right - rect.left) / 2)
        .min((rect.bottom - rect.top) / 2)
        .max(0);
    let r_f = r as f32;

    for y in rect.top..rect.bottom {
        if y < 0 || y >= image.height as i32 {
            continue;
        }
        for x in rect.left..rect.right {
            if x < 0 || x >= image.width as i32 {
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

            let a = (color.a as f32 * alpha_factor) as u8;
            blend_pixel(image, x, y, Color::rgba(color.r, color.g, color.b, a));
        }
    }
}

pub fn stroke_rounded_rect_rgba(image: &mut ImageBuffer, rect: RectI, radius: i32, color: Color) {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    let r = radius
        .min((rect.right - rect.left) / 2)
        .min((rect.bottom - rect.top) / 2)
        .max(0);
    let r_f = r as f32;

    for y in rect.top..rect.bottom {
        if y < 0 || y >= image.height as i32 {
            continue;
        }
        for x in rect.left..rect.right {
            if x < 0 || x >= image.width as i32 {
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
                let a = (color.a as f32 * alpha_factor) as u8;
                blend_pixel(image, x, y, Color::rgba(color.r, color.g, color.b, a));
            }
        }
    }
}

fn draw_rect_outline(image: &mut ImageBuffer, rect: RectI, color: Color, thickness: f32) {
    let t = thickness.round() as i32;
    let r = rect;

    // Top
    for y in r.top..(r.top + t) {
        for x in r.left..r.right {
            blend_pixel(image, x, y, color);
        }
    }
    // Bottom
    for y in (r.bottom - t)..r.bottom {
        for x in r.left..r.right {
            blend_pixel(image, x, y, color);
        }
    }
    // Left
    for y in r.top..r.bottom {
        for x in r.left..(r.left + t) {
            blend_pixel(image, x, y, color);
        }
    }
    // Right
    for y in r.top..r.bottom {
        for x in (r.right - t)..r.right {
            blend_pixel(image, x, y, color);
        }
    }
}

fn fill_ellipse(image: &mut ImageBuffer, rect: RectI, color: Color) {
    let rx = (rect.width() as f32) / 2.0;
    let ry = (rect.height() as f32) / 2.0;
    if rx <= 0.0 || ry <= 0.0 {
        return;
    }

    let cx = rect.left as f32 + rx;
    let cy = rect.top as f32 + ry;

    let y0 = rect.top.max(0);
    let y1 = rect.bottom.min(image.height as i32);

    for y in y0..y1 {
        let dy = (y as f32 - cy) / ry;
        if dy.abs() > 1.0 {
            continue;
        }
        let span = rx * (1.0 - dy * dy).sqrt();
        let x0 = (cx - span).round() as i32;
        let x1 = (cx + span).round() as i32;

        for x in x0.max(0)..x1.min(image.width as i32) {
            blend_pixel(image, x, y, color);
        }
    }
}

fn draw_ellipse_outline(image: &mut ImageBuffer, rect: RectI, color: Color, thickness: f32) {
    let rx = (rect.width() as f32) / 2.0;
    let ry = (rect.height() as f32) / 2.0;
    if rx <= 0.0 || ry <= 0.0 {
        return;
    }

    let cx = rect.left as f32 + rx;
    let cy = rect.top as f32 + ry;

    let perimeter =
        std::f32::consts::PI * (3.0 * (rx + ry) - ((3.0 * rx + ry) * (rx + 3.0 * ry)).sqrt());
    let steps = (perimeter * 2.0).ceil() as usize;

    let r_half = (thickness / 2.0).max(0.5);

    for i in 0..steps {
        let theta = (i as f32 / steps as f32) * std::f32::consts::TAU;
        let px = (cx + rx * theta.cos()).round() as i32;
        let py = (cy + ry * theta.sin()).round() as i32;
        draw_filled_circle(image, px, py, r_half, color);
    }
}

fn draw_highlighter_path(image: &mut ImageBuffer, points: &[PointI], color: Color, thickness: f32) {
    if points.is_empty() {
        return;
    }
    let stroke_w = thickness.max(16.0);
    let radius = stroke_w / 2.0;
    let r_ceil = radius.ceil() as i32 + 1;
    let r2 = radius * radius;

    // Bounding box of the entire highlighter stroke
    let min_x = points.iter().map(|p| p.x).min().unwrap_or(0);
    let max_x = points.iter().map(|p| p.x).max().unwrap_or(0);
    let min_y = points.iter().map(|p| p.y).min().unwrap_or(0);
    let max_y = points.iter().map(|p| p.y).max().unwrap_or(0);

    let bb_x0 = (min_x - r_ceil).max(0);
    let bb_y0 = (min_y - r_ceil).max(0);
    let bb_x1 = (max_x + r_ceil + 1).min(image.width as i32);
    let bb_y1 = (max_y + r_ceil + 1).min(image.height as i32);

    if bb_x1 <= bb_x0 || bb_y1 <= bb_y0 {
        return;
    }

    let bb_w = (bb_x1 - bb_x0) as usize;
    let bb_h = (bb_y1 - bb_y0) as usize;
    let mut mask = vec![0u8; bb_w * bb_h];

    let plot_circle = |cx: i32, cy: i32, mask: &mut [u8]| {
        for oy in -r_ceil..=r_ceil {
            let gy = cy + oy;
            if gy < bb_y0 || gy >= bb_y1 {
                continue;
            }
            let my = (gy - bb_y0) as usize;
            for ox in -r_ceil..=r_ceil {
                let gx = cx + ox;
                if gx < bb_x0 || gx >= bb_x1 {
                    continue;
                }
                let d2 = (ox * ox + oy * oy) as f32;
                if d2 <= r2 {
                    let d = d2.sqrt();
                    let cov = if d <= radius - 0.75 {
                        255u8
                    } else {
                        ((radius + 0.75 - d) / 1.5 * 255.0).clamp(0.0, 255.0) as u8
                    };
                    let mx = (gx - bb_x0) as usize;
                    let m_idx = my * bb_w + mx;
                    mask[m_idx] = mask[m_idx].max(cov);
                }
            }
        }
    };

    if points.len() == 1 {
        plot_circle(points[0].x, points[0].y, &mut mask);
    } else {
        for w in points.windows(2) {
            let p1 = w[0];
            let p2 = w[1];
            let dx = (p2.x - p1.x) as f32;
            let dy = (p2.y - p1.y) as f32;
            let dist = (dx * dx + dy * dy).sqrt();
            let steps = ((dist * 2.5).ceil() as usize).max(1);

            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let cx = (p1.x as f32 + dx * t).round() as i32;
                let cy = (p1.y as f32 + dy * t).round() as i32;
                plot_circle(cx, cy, &mut mask);
            }
        }
    }

    // Multiply-style blend with ~38% opacity so text beneath remains 100% crisp and readable
    for ly in 0..bb_h {
        let py = bb_y0 + ly as i32;
        let row_start = ly * bb_w;
        let img_row_start = (py as usize * image.width as usize) * 4;

        for lx in 0..bb_w {
            let cov = mask[row_start + lx];
            if cov > 0 {
                let px = bb_x0 + lx as i32;
                let p_idx = img_row_start + px as usize * 4;

                let dst_r = image.pixels[p_idx] as u32;
                let dst_g = image.pixels[p_idx + 1] as u32;
                let dst_b = image.pixels[p_idx + 2] as u32;

                // Multiply blend component
                let mul_r = (dst_r * color.r as u32) / 255;
                let mul_g = (dst_g * color.g as u32) / 255;
                let mul_b = (dst_b * color.b as u32) / 255;

                // 38% opacity scaled by edge coverage
                let alpha = (97u32 * cov as u32) / 255;
                let inv_alpha = 255 - alpha;

                image.pixels[p_idx] = ((dst_r * inv_alpha + mul_r * alpha) / 255) as u8;
                image.pixels[p_idx + 1] = ((dst_g * inv_alpha + mul_g * alpha) / 255) as u8;
                image.pixels[p_idx + 2] = ((dst_b * inv_alpha + mul_b * alpha) / 255) as u8;
            }
        }
    }
}

fn draw_step_marker(image: &mut ImageBuffer, pos: PointI, number: u32, color: Color, radius: f32) {
    // Outer filled circle with shadow
    let shadow_color = Color::rgba(0, 0, 0, 100);
    draw_filled_circle(image, pos.x + 1, pos.y + 2, radius + 0.5, shadow_color);
    draw_filled_circle(image, pos.x, pos.y, radius, color);

    // Inner number drawn with contrasting color (dark if light amber/white, white otherwise)
    let is_light = color.r > 200 && color.g > 150;
    let text_color = if is_light { Color::BLACK } else { Color::WHITE };

    let s = number.to_string();
    let font_size = (radius * 1.1).round().clamp(10.0, 36.0) as u32;
    let (tw, th) = measure_system_text(&s, font_size, true);
    let start_x = pos.x - tw / 2;
    let start_y = pos.y - th / 2;

    draw_system_text_rgba(
        image,
        PointI::new(start_x, start_y),
        &s,
        text_color,
        font_size,
        true,
    );
}

// ---------------------------------------------------------------------------
// Anti-Aliased System Font Rendering (Segoe UI Variable / Segoe UI)
// ---------------------------------------------------------------------------

pub const TEXT_SIZES: [u32; 5] = [14, 18, 24, 32, 44];

#[derive(Debug, Clone)]
pub struct TextMask {
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

#[cfg(windows)]
unsafe fn create_system_font(
    size: i32,
    bold: bool,
    cleartype: bool,
) -> windows::Win32::Graphics::Gdi::HFONT {
    let font = create_system_font_face(size, bold, cleartype, "Segoe UI Variable Text");
    if !font.is_invalid() {
        font
    } else {
        create_system_font_face(size, bold, cleartype, "Segoe UI")
    }
}

#[cfg(windows)]
unsafe fn create_system_font_face(
    size: i32,
    bold: bool,
    cleartype: bool,
    face: &str,
) -> windows::Win32::Graphics::Gdi::HFONT {
    let weight = if bold { 700 } else { 400 };
    let quality = if cleartype {
        CLEARTYPE_QUALITY
    } else {
        ANTIALIASED_QUALITY
    };
    let wide_face: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
    CreateFontW(
        -size,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        quality,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        windows::core::PCWSTR(wide_face.as_ptr()),
    )
}

pub fn render_system_text_mask_ex(
    text: &str,
    size: u32,
    bold: bool,
    cleartype: bool,
) -> Option<TextMask> {
    render_system_text_mask_with_font(text, size, bold, cleartype, "Segoe UI")
}

pub fn render_system_text_mask_with_font(
    text: &str,
    size: u32,
    bold: bool,
    cleartype: bool,
    face: &str,
) -> Option<TextMask> {
    if text.is_empty() {
        return None;
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() {
        return None;
    }

    #[cfg(windows)]
    unsafe {
        let mem_dc = CreateCompatibleDC(None);
        if mem_dc.is_invalid() {
            return None;
        }

        let font = create_system_font_face(size as i32, bold, cleartype, face);
        let old_font = SelectObject(mem_dc, font.into());

        let mut sz = windows::Win32::Foundation::SIZE::default();
        let _ = GetTextExtentPoint32W(mem_dc, &wide, &mut sz);
        let tw = (sz.cx.max(1) + 4) as u32;
        let th = (sz.cy.max(1) + 4) as u32;

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: tw as i32,
                biHeight: -(th as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                biSizeImage: tw * th * 4,
                ..Default::default()
            },
            bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default()],
        };

        let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
        {
            Ok(b) if !b.is_invalid() && !bits_ptr.is_null() => b,
            _ => {
                let _ = SelectObject(mem_dc, old_font);
                let _ = DeleteObject(font.into());
                let _ = DeleteDC(mem_dc);
                return None;
            }
        };

        std::ptr::write_bytes(bits_ptr, 0, (tw * th * 4) as usize);

        let old_bmp = SelectObject(mem_dc, dib.into());
        let _ = SetBkMode(mem_dc, TRANSPARENT);
        let _ = SetTextColor(mem_dc, windows::Win32::Foundation::COLORREF(0x00FFFFFF));

        let _ = TextOutW(mem_dc, 2, 2, &wide);
        let _ = GdiFlush();

        let raw_slice = std::slice::from_raw_parts(bits_ptr as *const u8, (tw * th * 4) as usize);
        let pixel_count = (tw * th) as usize;
        let mut coverage = vec![0u8; pixel_count];

        for (i, cov) in coverage.iter_mut().enumerate() {
            let s = i * 4;
            let r = raw_slice[s + 2] as u32;
            let g = raw_slice[s + 1] as u32;
            let b = raw_slice[s] as u32;
            *cov = ((r * 2 + g * 5 + b) / 8) as u8;
        }

        let _ = SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib.into());
        let _ = SelectObject(mem_dc, old_font);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(mem_dc);

        Some(TextMask {
            width: tw,
            height: th,
            coverage,
        })
    }

    #[cfg(not(windows))]
    None
}

#[allow(clippy::too_many_arguments)]
pub fn draw_system_text_with_font(
    image: &mut ImageBuffer,
    pos: PointI,
    text: &str,
    color: Color,
    size: u32,
    bold: bool,
    cleartype: bool,
    face: &str,
) {
    if let Some(mask) = render_system_text_mask_with_font(text, size, bold, cleartype, face) {
        for y in 0..mask.height {
            let py = pos.y + y as i32;
            if py < 0 || py >= image.height as i32 {
                continue;
            }
            for x in 0..mask.width {
                let px = pos.x + x as i32;
                if px < 0 || px >= image.width as i32 {
                    continue;
                }
                let cov = mask.coverage[(y * mask.width + x) as usize];
                if cov > 0 {
                    let a = ((cov as u32 * color.a as u32) / 255) as u8;
                    blend_pixel(image, px, py, Color::rgba(color.r, color.g, color.b, a));
                }
            }
        }
    }
}

pub fn render_system_text_mask(text: &str, size: u32, bold: bool) -> Option<TextMask> {
    render_system_text_mask_ex(text, size, bold, false)
}

pub fn measure_system_text(text: &str, size: u32, bold: bool) -> (i32, i32) {
    if text.is_empty() {
        return (0, 0);
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() {
        return (0, 0);
    }

    #[cfg(windows)]
    unsafe {
        let mem_dc = CreateCompatibleDC(None);
        if mem_dc.is_invalid() {
            return (text.len() as i32 * (size as i32 / 2), size as i32);
        }
        let font = create_system_font(size as i32, bold, false);
        let old = SelectObject(mem_dc, font.into());
        let mut sz = windows::Win32::Foundation::SIZE::default();
        let _ = GetTextExtentPoint32W(mem_dc, &wide, &mut sz);
        let _ = SelectObject(mem_dc, old);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(mem_dc);
        (sz.cx, sz.cy)
    }

    #[cfg(not(windows))]
    (text.len() as i32 * (size as i32 / 2), size as i32)
}

pub fn draw_system_text_rgba(
    image: &mut ImageBuffer,
    pos: PointI,
    text: &str,
    color: Color,
    size: u32,
    bold: bool,
) {
    if let Some(mask) = render_system_text_mask_ex(text, size, bold, false) {
        for y in 0..mask.height {
            let py = pos.y + y as i32;
            if py < 0 || py >= image.height as i32 {
                continue;
            }
            for x in 0..mask.width {
                let px = pos.x + x as i32;
                if px < 0 || px >= image.width as i32 {
                    continue;
                }
                let cov = mask.coverage[(y * mask.width + x) as usize];
                if cov > 0 {
                    let a = ((cov as u32 * color.a as u32) / 255) as u8;
                    blend_pixel(image, px, py, Color::rgba(color.r, color.g, color.b, a));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_system_text_bgra(
    buf: &mut [u8],
    win_w: i32,
    win_h: i32,
    pos: PointI,
    text: &str,
    color: Color,
    size: u32,
    bold: bool,
) {
    if let Some(mask) = render_system_text_mask_ex(text, size, bold, true) {
        for y in 0..mask.height {
            let py = pos.y + y as i32;
            if py < 0 || py >= win_h {
                continue;
            }
            for x in 0..mask.width {
                let px = pos.x + x as i32;
                if px < 0 || px >= win_w {
                    continue;
                }
                let cov = mask.coverage[(y * mask.width + x) as usize];
                if cov > 0 {
                    let a = (cov as u32 * color.a as u32) / 255;
                    let idx = (py as usize * win_w as usize + px as usize) * 4;
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

pub fn draw_simple_text(image: &mut ImageBuffer, pos: PointI, text: &str, color: Color, size: u32) {
    draw_system_text_rgba(image, pos, text, color, size, false);
}

pub fn draw_text_bgra(
    buf: &mut [u8],
    win_w: i32,
    win_h: i32,
    pos: PointI,
    text: &str,
    color: Color,
    size: u32,
) {
    draw_system_text_bgra(buf, win_w, win_h, pos, text, color, size, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_annotation_model_undo_redo() {
        let mut model = AnnotationModel::new();
        let mut img = ImageBuffer::new(100, 100);

        let p1 = PointI::new(10, 10);
        let p2 = PointI::new(50, 50);
        let arrow = Annotation::Arrow {
            start: p1,
            end: p2,
            color: Color::AMBER,
            thickness: 3.0,
        };

        model.push(arrow);
        assert_eq!(model.annotations.len(), 1);
        assert!(model.can_undo());
        assert!(!model.can_redo());

        // Undo
        let undone = model.undo(&mut img);
        assert!(undone.is_some());
        assert_eq!(model.annotations.len(), 0);
        assert!(!model.can_undo());
        assert!(model.can_redo());

        // Redo
        let redone = model.redo(&mut img);
        assert!(redone.is_some());
        assert_eq!(model.annotations.len(), 1);
        assert!(model.can_undo());
        assert!(!model.can_redo());
    }

    #[test]
    fn test_step_marker_auto_increment() {
        let mut model = AnnotationModel::new();
        let mut img = ImageBuffer::new(100, 100);

        assert_eq!(model.next_step_number, 1);

        model.push(Annotation::StepMarker {
            pos: PointI::new(20, 20),
            number: model.next_step_number,
            color: Color::AMBER,
            radius: 14.0,
        });
        assert_eq!(model.next_step_number, 2);

        model.push(Annotation::StepMarker {
            pos: PointI::new(50, 50),
            number: model.next_step_number,
            color: Color::AMBER,
            radius: 14.0,
        });
        assert_eq!(model.next_step_number, 3);

        model.undo(&mut img);
        assert_eq!(model.next_step_number, 2);
    }
}
