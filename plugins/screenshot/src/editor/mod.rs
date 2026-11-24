//! Screenshot Editor window.
//! Features a floating rounded toolbar, neutral canvas (#0e1013),
//! 1.5px custom line icons, tooltips, color/thickness presets, zoom/pan,
//! undo/redo (Ctrl+Z / Ctrl+Y), clipboard export (Ctrl+C), and PNG saving (Ctrl+S).
//! Optimized with cached rendering and fast perimeter soft drop shadow (< 8 ms per redraw for 4K).

pub mod canvas;
pub mod input;
pub mod toolbar;
pub mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use crate::annotation::AnnotationModel;
use crate::geometry::PointI;
use crate::image::ImageBuffer;

pub use canvas::{draw_soft_shadow, screen_to_canvas, Backbuffer};
pub use toolbar::{
    get_button_rect, get_button_rect_scaled, get_swatch_rect_scaled, get_thickness_rect_scaled,
    get_toolbar_mode_from_tb_rect, get_toolbar_rect, get_toolbar_rect_scaled,
    get_toolbar_separators, get_toolbar_wrap_mode, is_multi_row, ToolbarButton, ToolbarWrapMode,
    BUTTONS, FONT_SIZE_OPTIONS, THICKNESS_OPTIONS, TOOLBAR_SINGLE_ROW_BASE_WIDTH,
    TOOLBAR_THREE_ROW_BASE_WIDTH, TOOLBAR_TWO_ROW_BASE_WIDTH,
};
pub use window::{
    apply_dark_window_chrome, get_window_dpi_scale, measure_redraw_time_3840x2160, paint_editor,
    run_real_window_4k_benchmark, show_editor,
};

#[cfg(feature = "review")]
pub use window::{open_real_editor_for_demo, open_real_editor_for_demo_scaled};

#[derive(Debug, Clone, Default)]
pub struct RedrawTracker {
    pub count: u32,
    pub total_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub last_ms: f64,
}

pub struct EditorSession {
    pub image: ImageBuffer,
    pub model: AnnotationModel,
    pub save_folder: PathBuf,
    pub zoom: f32,
    pub pan_offset: PointI,
    pub is_dragging: bool,
    pub drag_start: PointI,
    pub drag_current: PointI,
    pub is_panning: bool,
    pub pan_start: PointI,
    pub toast: Option<(String, Instant)>,
    pub tooltip: Option<(String, PointI)>,
    pub hovered_btn: Option<usize>,
    pub is_drm_black: bool,
    pub active_color_idx: usize,
    pub active_thickness_idx: usize,
    pub cached_bgra: Vec<u8>,
    pub cached_w: u32,
    pub cached_h: u32,
    pub cache_dirty: bool,
    pub redraw_tracker: RedrawTracker,
    pub backbuffer: Option<Backbuffer>,
    pub active_text_size_idx: usize,
    pub is_text_bold: bool,
    pub dpi_scale: f32,
}

impl EditorSession {
    pub fn new(image: ImageBuffer, is_drm_black: bool, save_folder: PathBuf) -> Self {
        let mut s = Self {
            image,
            model: AnnotationModel::new(),
            save_folder,
            zoom: 1.0,
            pan_offset: PointI::default(),
            is_dragging: false,
            drag_start: PointI::default(),
            drag_current: PointI::default(),
            is_panning: false,
            pan_start: PointI::default(),
            toast: None,
            tooltip: None,
            hovered_btn: None,
            is_drm_black,
            active_color_idx: 0,
            active_thickness_idx: 1,
            cached_bgra: Vec::new(),
            cached_w: 0,
            cached_h: 0,
            cache_dirty: true,
            redraw_tracker: RedrawTracker::default(),
            backbuffer: None,
            active_text_size_idx: 1,
            is_text_bold: false,
            dpi_scale: 1.0,
        };
        s.update_cache_if_needed();
        s
    }

    pub fn update_cache_if_needed(&mut self) {
        if !self.cache_dirty && !self.cached_bgra.is_empty() {
            return;
        }

        let mut annotated = self.image.clone();
        self.model.render_onto(&mut annotated);

        let pixel_count = (annotated.width * annotated.height) as usize;
        let mut bgra = vec![0u8; pixel_count * 4];
        for i in 0..pixel_count {
            let src_idx = i * 4;
            bgra[src_idx] = annotated.pixels[src_idx + 2]; // B
            bgra[src_idx + 1] = annotated.pixels[src_idx + 1]; // G
            bgra[src_idx + 2] = annotated.pixels[src_idx]; // R
            bgra[src_idx + 3] = 255;
        }

        self.cached_w = annotated.width;
        self.cached_h = annotated.height;
        self.cached_bgra = bgra;
        self.cache_dirty = false;
    }
}

thread_local! {
    pub static EDITOR_SESSION: RefCell<Option<EditorSession>> = const { RefCell::new(None) };
}
