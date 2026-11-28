//! Screen region selection overlay across all monitors.
//! Features dimming mask, bright selection border, resize handles,
//! crosshair guides, and live size badge.

use std::cell::RefCell;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU8, Ordering};

use windows::core::w;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
#[cfg(feature = "review")]
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
    EnumDisplayMonitors, GetMonitorInfoW, InvalidateRect, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HDC, HMONITOR, MONITORINFO, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, LoadCursorW,
    PostQuitMessage, RegisterClassW, SetCursor, SetForegroundWindow, ShowWindow, TranslateMessage,
    CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, IDC_ARROW, IDC_CROSS, IDC_SIZEALL, IDC_SIZENESW,
    IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, MSG, SW_SHOW, WM_CHAR, WM_DESTROY, WM_KEYDOWN,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN,
    WM_SETCURSOR, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::annotation::{
    draw_system_text_rgba, fill_rounded_rect_rgba, measure_system_text, render_annotation,
    stroke_rounded_rect_rgba, Annotation, Color,
};
use crate::capture::{capture_virtual_desktop, enable_per_monitor_dpi_v2, CaptureResult};
use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;
use crate::marks::{self, Button, Marks, Monitor};

static LAST_USED_MODE: AtomicU8 = AtomicU8::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureMode {
    #[default]
    Screenshot, // S
    RecordVideo,   // R
    ScrollCapture, // L
    TextExtract,   // T
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DelayTimer {
    #[default]
    Off,
    Sec3,
    Sec5,
}

impl DelayTimer {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Sec3,
            Self::Sec3 => Self::Sec5,
            Self::Sec5 => Self::Off,
        }
    }
    pub fn seconds(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Sec3 => 3,
            Self::Sec5 => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Destination {
    #[default]
    Clipboard,
    File,
    Both,
}

impl Destination {
    pub fn next(self) -> Self {
        match self {
            Self::Clipboard => Self::File,
            Self::File => Self::Both,
            Self::Both => Self::Clipboard,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    /// The marked picture is on the clipboard already.
    Copy,
    Save,
    Edit,
    Record,
    Scroll,
    Text,
}

pub struct CaptureOverlayOutcome {
    pub action: OverlayAction,
    pub screen_rect: RectI,
    pub cropped_image: ImageBuffer,
    pub is_drm_black: bool,
    pub delay_seconds: u32,
    pub include_cursor: bool,
    pub destination: Destination,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayMode {
    #[default]
    Idle,
    Selecting,
    Resizing(usize),
    Moving(PointI),
}

#[derive(Clone, Copy)]
pub struct SelectionState {
    pub rect: Option<RectI>,
    pub mode: OverlayMode,
    pub drag_start: PointI,
    pub current_cursor: PointI,
    pub confirmed: bool,
    pub cancelled: bool,
    pub dpi_scale: f32,
    pub is_scrolling: bool,
    pub is_recording: bool,
    pub capture_mode: CaptureMode,
    pub delay_timer: DelayTimer,
    pub include_cursor: bool,
    pub destination: Destination,
    pub chosen_action: Option<OverlayAction>,
}

impl Default for SelectionState {
    fn default() -> Self {
        let mode_u8 = LAST_USED_MODE.load(Ordering::Acquire);
        let capture_mode = match mode_u8 {
            1 => CaptureMode::RecordVideo,
            2 => CaptureMode::ScrollCapture,
            3 => CaptureMode::TextExtract,
            _ => CaptureMode::Screenshot,
        };
        Self {
            rect: None,
            mode: OverlayMode::Idle,
            drag_start: PointI::default(),
            current_cursor: PointI::default(),
            confirmed: false,
            cancelled: false,
            dpi_scale: 1.0,
            is_scrolling: false,
            is_recording: false,
            capture_mode,
            delay_timer: DelayTimer::Off,
            include_cursor: true,
            destination: Destination::Clipboard,
            chosen_action: None,
        }
    }
}

pub struct OverlaySession {
    /// The screen as it was when the overlay opened. Held only in memory, and
    /// dropped with the session.
    pub capture: CaptureResult,
    pub state: SelectionState,
    pub marks: Marks,
    /// In overlay pixels; empty means the whole overlay is one screen.
    pub monitors: Vec<Monitor>,
}

impl OverlaySession {
    fn monitor(&self, sel: RectI) -> Monitor {
        let whole = Monitor {
            rect: RectI::new(
                0,
                0,
                self.capture.bounds.width(),
                self.capture.bounds.height(),
            ),
            scale: self.state.dpi_scale,
        };
        marks::monitor_for(&self.monitors, sel, whole)
    }

    /// Drawing tools are offered for screenshots only.
    fn draws(&self) -> bool {
        self.state.capture_mode == CaptureMode::Screenshot
    }

    /// The toolbar beside the box, if there is a box big enough for one.
    fn toolbar(&self) -> Option<RectI> {
        let sel = self
            .state
            .rect
            .filter(|r| r.width() >= 16 && r.height() >= 16)?;
        Some(marks::toolbar_rect(sel, self.monitor(sel), self.draws()))
    }

    /// Ends the session with `action`, if there is a box to act on.
    fn finish(&mut self, action: OverlayAction) -> bool {
        let big = self
            .state
            .rect
            .is_some_and(|r| r.width() >= 4 && r.height() >= 4);
        if big {
            self.marks.finish_text(&self.capture.image);
            self.state.chosen_action = Some(action);
            self.state.confirmed = true;
        }
        big
    }

    /// What Enter and a double-click do in the mode chosen above.
    fn confirm(&mut self) -> bool {
        let action = default_action(self.state.capture_mode);
        self.finish(action)
    }

    /// Runs a toolbar button. True if the overlay is done.
    fn press_button(&mut self, button: Button) -> bool {
        self.marks.finish_text(&self.capture.image);
        match button {
            Button::Tool(tool) => self.marks.pick(tool),
            Button::Undo => self.marks.undo(&self.capture.image),
            Button::Redo => self.marks.redo(&self.capture.image),
            Button::Copy => return copy_selection(self),
            Button::Save => return self.finish(OverlayAction::Save),
            Button::Editor => return self.finish(OverlayAction::Edit),
            Button::Record => return self.finish(OverlayAction::Record),
            Button::Scroll => return self.finish(OverlayAction::Scroll),
            Button::ReadText => return self.finish(OverlayAction::Text),
            Button::Close => {
                self.state.cancelled = true;
                return true;
            }
        }
        false
    }
}

fn default_action(mode: CaptureMode) -> OverlayAction {
    match mode {
        CaptureMode::Screenshot => OverlayAction::Edit,
        CaptureMode::RecordVideo => OverlayAction::Record,
        CaptureMode::ScrollCapture => OverlayAction::Scroll,
        CaptureMode::TextExtract => OverlayAction::Text,
    }
}

/// Close enough to an edge of the box to move it while a tool is in hand.
fn on_border(r: RectI, p: PointI, tolerance: i32) -> bool {
    let near = RectI::new(
        r.left - tolerance,
        r.top - tolerance,
        r.right + tolerance,
        r.bottom + tolerance,
    );
    let core = RectI::new(
        r.left + tolerance,
        r.top + tolerance,
        r.right - tolerance,
        r.bottom - tolerance,
    );
    near.contains(p) && !core.contains(p)
}

/// The screens, in the overlay's own pixels.
fn monitors_in(bounds: RectI) -> Vec<Monitor> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let (mut x, mut y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);
            let r = info.rcMonitor;
            // The pointer is the vector passed below, which outlives the call.
            let list = &mut *(list.0 as *mut Vec<Monitor>);
            list.push(Monitor {
                rect: RectI::new(r.left, r.top, r.right, r.bottom),
                scale: x as f32 / 96.0,
            });
        }
        true.into()
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(each),
            LPARAM(std::ptr::from_mut(&mut list) as isize),
        );
    }
    for m in &mut list {
        let r = m.rect;
        m.rect = RectI::new(
            r.left - bounds.left,
            r.top - bounds.top,
            r.right - bounds.left,
            r.bottom - bounds.top,
        );
    }
    list
}

thread_local! {
    static SESSION: RefCell<Option<OverlaySession>> = const { RefCell::new(None) };
}

/// Copies the marked selection and ends the session. False (the overlay stays)
/// without a selection or when the clipboard refuses.
fn copy_selection(sess: &mut OverlaySession) -> bool {
    sess.marks.finish_text(&sess.capture.image);
    let Some(r) = sess
        .state
        .rect
        .filter(|r| r.width() >= 4 && r.height() >= 4)
    else {
        return false;
    };
    let copied = sess
        .marks
        .picture(&sess.capture.image)
        .crop(r)
        .is_ok_and(|img| crate::clipboard::copy_to_clipboard(&img).is_ok());
    copied && sess.finish(OverlayAction::Copy)
}

fn lparam_to_point(lp: LPARAM) -> PointI {
    let val = lp.0 as u32;
    PointI {
        x: i32::from((val & 0xFFFF) as u16 as i16),
        y: i32::from((val >> 16) as u16 as i16),
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

pub fn get_selection_handle_size(scale: f32) -> i32 {
    (8.0 * scale).round() as i32
}

pub fn get_selection_handles(r: RectI) -> [PointI; 8] {
    [
        PointI::new(r.left, r.top),                    // 0: Top-Left
        PointI::new((r.left + r.right) / 2, r.top),    // 1: Top-Center
        PointI::new(r.right, r.top),                   // 2: Top-Right
        PointI::new(r.left, (r.top + r.bottom) / 2),   // 3: Middle-Left
        PointI::new(r.right, (r.top + r.bottom) / 2),  // 4: Middle-Right
        PointI::new(r.left, r.bottom),                 // 5: Bottom-Left
        PointI::new((r.left + r.right) / 2, r.bottom), // 6: Bottom-Center
        PointI::new(r.right, r.bottom),                // 7: Bottom-Right
    ]
}

pub fn hit_test_handle(handles: &[PointI; 8], pt: PointI, tolerance: i32) -> Option<usize> {
    for (i, h) in handles.iter().enumerate() {
        if (pt.x - h.x).abs() <= tolerance && (pt.y - h.y).abs() <= tolerance {
            return Some(i);
        }
    }
    None
}

pub fn apply_handle_drag(mut r: RectI, handle_idx: usize, pt: PointI) -> RectI {
    match handle_idx {
        0 => {
            r.left = pt.x;
            r.top = pt.y;
        }
        1 => {
            r.top = pt.y;
        }
        2 => {
            r.right = pt.x;
            r.top = pt.y;
        }
        3 => {
            r.left = pt.x;
        }
        4 => {
            r.right = pt.x;
        }
        5 => {
            r.left = pt.x;
            r.bottom = pt.y;
        }
        6 => {
            r.bottom = pt.y;
        }
        7 => {
            r.right = pt.x;
            r.bottom = pt.y;
        }
        _ => {}
    }
    r
}

pub fn get_top_bar_rect(screen_w: i32, scale: f32) -> RectI {
    let bar_w = (568.0 * scale).round() as i32;
    let bar_h = (42.0 * scale).round() as i32;
    let bar_x = (screen_w - bar_w) / 2;
    let bar_y = (14.0 * scale).round() as i32;
    RectI::new(bar_x, bar_y, bar_x + bar_w, bar_y + bar_h)
}

pub fn get_selection_toolbar_rect(r: RectI, screen_w: i32, screen_h: i32, scale: f32) -> RectI {
    let screen = Monitor {
        rect: RectI::new(0, 0, screen_w, screen_h),
        scale,
    };
    marks::toolbar_rect(r, screen, true)
}

/// A press of the left button, on a bar, on the box or on the picture.
unsafe fn press(hwnd: HWND, p: PointI) -> LRESULT {
    let mut handled_ui = false;
    let mut should_close = false;

    SESSION.with(|s| {
        if let Some(sess) = s.borrow_mut().as_mut() {
            let scale = sess.state.dpi_scale;
            let scr_w = sess.capture.bounds.width();

            // 1. Hit test Top Bar
            let top_bar = get_top_bar_rect(scr_w, scale);
            if top_bar.contains(p) {
                handled_ui = true;
                let bx = top_bar.left;
                let by = top_bar.top;

                let btn_shot = RectI::new(
                    bx + (6.0 * scale).round() as i32,
                    by + (6.0 * scale).round() as i32,
                    bx + (80.0 * scale).round() as i32,
                    by + (36.0 * scale).round() as i32,
                );
                let btn_rec = RectI::new(
                    bx + (84.0 * scale).round() as i32,
                    by + (6.0 * scale).round() as i32,
                    bx + (166.0 * scale).round() as i32,
                    by + (36.0 * scale).round() as i32,
                );
                let btn_scroll = RectI::new(
                    bx + (170.0 * scale).round() as i32,
                    by + (6.0 * scale).round() as i32,
                    bx + (248.0 * scale).round() as i32,
                    by + (36.0 * scale).round() as i32,
                );
                let btn_text = RectI::new(
                    bx + (252.0 * scale).round() as i32,
                    by + (6.0 * scale).round() as i32,
                    bx + (324.0 * scale).round() as i32,
                    by + (36.0 * scale).round() as i32,
                );
                let btn_timer = RectI::new(
                    bx + (336.0 * scale).round() as i32,
                    by + (6.0 * scale).round() as i32,
                    bx + (384.0 * scale).round() as i32,
                    by + (36.0 * scale).round() as i32,
                );
                let btn_cursor = RectI::new(
                    bx + (388.0 * scale).round() as i32,
                    by + (6.0 * scale).round() as i32,
                    bx + (442.0 * scale).round() as i32,
                    by + (36.0 * scale).round() as i32,
                );
                let btn_dest = RectI::new(
                    bx + (446.0 * scale).round() as i32,
                    by + (6.0 * scale).round() as i32,
                    bx + (534.0 * scale).round() as i32,
                    by + (36.0 * scale).round() as i32,
                );
                let btn_close = RectI::new(
                    bx + (538.0 * scale).round() as i32,
                    by + (6.0 * scale).round() as i32,
                    bx + (562.0 * scale).round() as i32,
                    by + (36.0 * scale).round() as i32,
                );

                if btn_shot.contains(p) {
                    sess.state.capture_mode = CaptureMode::Screenshot;
                    LAST_USED_MODE.store(0, Ordering::Release);
                } else if btn_rec.contains(p) {
                    sess.state.capture_mode = CaptureMode::RecordVideo;
                    LAST_USED_MODE.store(1, Ordering::Release);
                } else if btn_scroll.contains(p) {
                    sess.state.capture_mode = CaptureMode::ScrollCapture;
                    LAST_USED_MODE.store(2, Ordering::Release);
                } else if btn_text.contains(p) {
                    sess.state.capture_mode = CaptureMode::TextExtract;
                    LAST_USED_MODE.store(3, Ordering::Release);
                } else if btn_timer.contains(p) {
                    sess.state.delay_timer = sess.state.delay_timer.next();
                } else if btn_cursor.contains(p) {
                    sess.state.include_cursor = !sess.state.include_cursor;
                } else if btn_dest.contains(p) {
                    sess.state.destination = sess.state.destination.next();
                } else if btn_close.contains(p) {
                    sess.state.cancelled = true;
                    should_close = true;
                }
            }

            // 2. Hit test the toolbar beside the box
            if !handled_ui {
                if let Some(tb) = sess.toolbar().filter(|tb| tb.contains(p)) {
                    handled_ui = true;
                    let hit = marks::buttons(tb, sess.draws())
                        .into_iter()
                        .find(|(_, _, b)| b.contains(p));
                    if let Some((button, _, _)) = hit {
                        should_close = sess.press_button(button);
                    }
                }
            }

            if !handled_ui {
                sess.marks.finish_text(&sess.capture.image);
                let tol = get_selection_handle_size(scale);
                if let Some(r) = sess.state.rect {
                    let handles = get_selection_handles(r);
                    let drawing = sess.draws() && sess.marks.tool.is_some();
                    if let Some(h_idx) = hit_test_handle(&handles, p, tol) {
                        sess.state.mode = OverlayMode::Resizing(h_idx);
                        sess.state.drag_start = p;
                    } else if drawing && r.contains(p) && !on_border(r, p, tol) {
                        sess.marks.press(p, r, sess.monitor(r).scale);
                    } else if r.contains(p) || (drawing && on_border(r, p, tol)) {
                        sess.state.mode =
                            OverlayMode::Moving(PointI::new(p.x - r.left, p.y - r.top));
                        sess.state.drag_start = p;
                    } else {
                        sess.state.rect = None;
                        sess.state.mode = OverlayMode::Selecting;
                        sess.state.drag_start = p;
                    }
                } else {
                    sess.state.mode = OverlayMode::Selecting;
                    sess.state.drag_start = p;
                }
            }
            sess.state.current_cursor = p;
        }
    });

    if should_close {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    if !handled_ui {
        let _ = SetCapture(hwnd);
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
    LRESULT(0)
}

unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            let mut cursor_handled = false;
            SESSION.with(|s| {
                if let Some(sess) = s.borrow().as_ref() {
                    let cur = sess.state.current_cursor;
                    let scale = sess.state.dpi_scale;
                    let tol = get_selection_handle_size(scale);

                    if let Some(r) = sess.state.rect {
                        let handles = get_selection_handles(r);
                        if let Some(h_idx) = hit_test_handle(&handles, cur, tol) {
                            let idc = match h_idx {
                                0 | 7 => IDC_SIZENWSE,
                                1 | 6 => IDC_SIZENS,
                                2 | 5 => IDC_SIZENESW,
                                3 | 4 => IDC_SIZEWE,
                                _ => IDC_CROSS,
                            };
                            if let Ok(c) = LoadCursorW(None, idc) {
                                SetCursor(Some(c));
                                cursor_handled = true;
                            }
                        } else if sess.toolbar().is_some_and(|tb| tb.contains(cur)) {
                            if let Ok(c) = LoadCursorW(None, IDC_ARROW) {
                                SetCursor(Some(c));
                                cursor_handled = true;
                            }
                        } else if (sess.marks.tool.is_none()
                            || !sess.draws()
                            || on_border(r, cur, tol))
                            && (r.contains(cur) || sess.marks.tool.is_some())
                        {
                            if let Ok(c) = LoadCursorW(None, IDC_SIZEALL) {
                                SetCursor(Some(c));
                                cursor_handled = true;
                            }
                        }
                    }
                }
            });

            if !cursor_handled {
                if let Ok(cur) = LoadCursorW(None, IDC_CROSS) {
                    SetCursor(Some(cur));
                }
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => press(hwnd, lparam_to_point(lparam)),
        WM_CHAR => {
            SESSION.with(|s| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    if let Some(c) = char::from_u32(wparam.0 as u32) {
                        sess.marks.type_char(c);
                    }
                }
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = lparam_to_point(lparam);
            let mut need_redraw = false;
            SESSION.with(|s| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    sess.state.current_cursor = p;
                    if sess.marks.drawing() {
                        if let Some(r) = sess.state.rect {
                            sess.marks.drag(p, r);
                            need_redraw = true;
                        }
                    }
                    match sess.state.mode {
                        OverlayMode::Selecting => {
                            sess.state.rect = Some(RectI::from_points(sess.state.drag_start, p));
                            need_redraw = true;
                        }
                        OverlayMode::Resizing(h_idx) => {
                            if let Some(r) = sess.state.rect {
                                sess.state.rect = Some(apply_handle_drag(r, h_idx, p));
                                need_redraw = true;
                            }
                        }
                        OverlayMode::Moving(offset) => {
                            if let Some(r) = sess.state.rect {
                                let w = r.width();
                                let h = r.height();
                                let nl = p.x - offset.x;
                                let nt = p.y - offset.y;
                                sess.state.rect = Some(RectI::new(nl, nt, nl + w, nt + h));
                                need_redraw = true;
                            }
                        }
                        OverlayMode::Idle => {
                            // Update crosshairs
                            need_redraw = true;
                        }
                    }
                }
            });
            if need_redraw {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            SESSION.with(|s| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    if let Some(mut r) = sess.state.rect {
                        if r.left > r.right {
                            std::mem::swap(&mut r.left, &mut r.right);
                        }
                        if r.top > r.bottom {
                            std::mem::swap(&mut r.top, &mut r.bottom);
                        }
                        sess.state.rect = Some(r);
                    }
                    sess.state.mode = OverlayMode::Idle;
                    sess.marks.release(&sess.capture.image);
                }
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let p = lparam_to_point(lparam);
            // Two quick clicks on a button or with a tool are two presses.
            let mut again = false;
            let mut should_close = false;
            SESSION.with(|s| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    let scr_w = sess.capture.bounds.width();
                    again = (sess.draws() && sess.marks.tool.is_some())
                        || sess.toolbar().is_some_and(|tb| tb.contains(p))
                        || get_top_bar_rect(scr_w, sess.state.dpi_scale).contains(p);
                    if !again {
                        should_close = sess.confirm();
                    }
                }
            });
            if again {
                return press(hwnd, p);
            }
            if should_close {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            SESSION.with(|s| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    sess.state.cancelled = true;
                }
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0;
            let mut should_close = false;
            let mut need_redraw = false;
            // VK_CONTROL; the high bit says it is held down.
            let ctrl = GetKeyState(0x11) < 0;

            SESSION.with(|s| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    if sess.marks.typing() {
                        // Letters go into the text; Enter keeps it, Esc drops it.
                        need_redraw = true;
                        match key {
                            0x43 if ctrl => should_close = copy_selection(sess),
                            0x0D => sess.marks.finish_text(&sess.capture.image),
                            0x1B => sess.marks.drop_text(),
                            _ => {}
                        }
                        return;
                    }
                    match key {
                        // Ctrl+C copies the selection and closes
                        0x43 if ctrl => should_close = copy_selection(sess),
                        // Ctrl+S saves it and closes
                        0x53 if ctrl => should_close = sess.finish(OverlayAction::Save),
                        // Ctrl+Z and Ctrl+Y undo and redo a mark
                        0x5A if ctrl => {
                            sess.marks.undo(&sess.capture.image);
                            need_redraw = true;
                        }
                        0x59 if ctrl => {
                            sess.marks.redo(&sess.capture.image);
                            need_redraw = true;
                        }
                        0x1B => {
                            // Esc key cancels capture
                            sess.state.cancelled = true;
                            should_close = true;
                        }
                        // Enter confirms: for a screenshot, in the full editor
                        0x0D => should_close = sess.confirm(),
                        // S / s
                        0x53 => {
                            sess.state.capture_mode = CaptureMode::Screenshot;
                            LAST_USED_MODE.store(0, Ordering::Release);
                            need_redraw = true;
                        }
                        // R / r
                        0x52 => {
                            sess.state.capture_mode = CaptureMode::RecordVideo;
                            LAST_USED_MODE.store(1, Ordering::Release);
                            need_redraw = true;
                        }
                        // L / l
                        0x4C => {
                            sess.state.capture_mode = CaptureMode::ScrollCapture;
                            LAST_USED_MODE.store(2, Ordering::Release);
                            need_redraw = true;
                        }
                        // T / t
                        0x54 => {
                            sess.state.capture_mode = CaptureMode::TextExtract;
                            LAST_USED_MODE.store(3, Ordering::Release);
                            need_redraw = true;
                        }
                        _ => {}
                    }
                }
            });
            if should_close {
                PostQuitMessage(0);
            } else if need_redraw {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            paint_overlay(hwnd, hdc);

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn paint_overlay(hwnd: HWND, hdc: HDC) {
    SESSION.with(|s| {
        let borrowed = s.borrow();
        let Some(sess) = borrowed.as_ref() else {
            return;
        };

        let w = sess.capture.bounds.width() as u32;
        let h = sess.capture.bounds.height() as u32;
        if w == 0 || h == 0 {
            return;
        }

        let scale = if sess.state.dpi_scale > 0.1 {
            sess.state.dpi_scale
        } else {
            get_window_dpi_scale(hwnd)
        };

        let draft = sess.marks.draft();
        let display_img = render_frame(
            sess.marks.picture(&sess.capture.image),
            sess.state.rect,
            sess.state.current_cursor,
            scale,
            sess.state.is_scrolling,
            sess.state.is_recording,
            sess.state.capture_mode,
            sess.state.delay_timer,
            sess.state.include_cursor,
            sess.state.destination,
            draft.as_ref(),
            Some(sess),
        );

        // Present display_img to window DC via DIB Section
        unsafe {
            let mem_dc = CreateCompatibleDC(Some(hdc));
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

            let mut bits: *mut std::ffi::c_void = null_mut();
            if let Ok(dib) =
                CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                if !dib.is_invalid() && !bits.is_null() {
                    let total_pixels = (w * h) as usize;
                    let total_bytes = total_pixels * 4;
                    let dst = std::slice::from_raw_parts_mut(bits as *mut u8, total_bytes);
                    for i in 0..total_pixels {
                        let src_idx = i * 4;
                        dst[src_idx] = display_img.pixels[src_idx + 2]; // B
                        dst[src_idx + 1] = display_img.pixels[src_idx + 1]; // G
                        dst[src_idx + 2] = display_img.pixels[src_idx]; // R
                        dst[src_idx + 3] = 255;
                    }

                    let old = SelectObject(mem_dc, dib.into());
                    let _ = BitBlt(hdc, 0, 0, w as i32, h as i32, Some(mem_dc), 0, 0, SRCCOPY);
                    let _ = SelectObject(mem_dc, old);
                    let _ = DeleteObject(dib.into());
                }
            }
            let _ = DeleteDC(mem_dc);
        }
    });
}

/// Executes a unified capture selection session across all monitors.
/// Returns the chosen action, screen rectangle, cropped image, DRM flag, and options.
pub fn select_capture_session() -> Option<CaptureOverlayOutcome> {
    enable_per_monitor_dpi_v2();

    // The picture is frozen now, before the overlay is on screen.
    let capture_res = capture_virtual_desktop().ok()?;
    let bounds = capture_res.bounds;
    let monitors = monitors_in(bounds);

    SESSION.with(|s| {
        *s.borrow_mut() = Some(OverlaySession {
            capture: capture_res,
            state: SelectionState {
                dpi_scale: 1.0,
                ..Default::default()
            },
            marks: Marks::default(),
            monitors,
        });
    });

    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let class_name = w!("LookoutCaptureOverlay");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(overlay_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class_name,
            w!("Lookout Capture"),
            WS_POPUP,
            bounds.left,
            bounds.top,
            bounds.width(),
            bounds.height(),
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?;

        let scale = get_window_dpi_scale(hwnd);
        SESSION.with(|s| {
            if let Some(sess) = s.borrow_mut().as_mut() {
                sess.state.dpi_scale = scale;
            }
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

    let final_session = SESSION.with(|s| s.borrow_mut().take());
    let sess = final_session?;

    if sess.state.confirmed && !sess.state.cancelled {
        if let Some(sel) = sess.state.rect {
            if sel.width() >= 4 && sel.height() >= 4 {
                let action = sess
                    .state
                    .chosen_action
                    .unwrap_or(default_action(sess.state.capture_mode));
                let raw = sess.capture.image.crop(sel).ok()?;
                let is_drm_black = raw.is_black(0.999);
                // Marks go with the picture, not into a recording or a text read.
                let cropped = match action {
                    OverlayAction::Copy | OverlayAction::Save | OverlayAction::Edit => {
                        sess.marks.picture(&sess.capture.image).crop(sel).ok()?
                    }
                    _ => raw,
                };
                let screen_sel = RectI::new(
                    bounds.left + sel.left,
                    bounds.top + sel.top,
                    bounds.left + sel.right,
                    bounds.top + sel.bottom,
                );
                return Some(CaptureOverlayOutcome {
                    action,
                    screen_rect: screen_sel,
                    cropped_image: cropped,
                    is_drm_black,
                    delay_seconds: sess.state.delay_timer.seconds(),
                    include_cursor: sess.state.include_cursor,
                    destination: sess.state.destination,
                });
            }
        }
    }

    None
}

/// Executes an area selection across all monitors.
/// Returns the cropped image and a boolean indicating whether DRM black capture was detected.
pub fn select_area() -> Option<(ImageBuffer, bool)> {
    let outcome = select_capture_session()?;
    Some((outcome.cropped_image, outcome.is_drm_black))
}

/// Executes an area selection across all monitors for scrolling capture.
/// Returns the chosen physical screen rectangle in virtual desktop coordinates.
pub fn select_scrolling_area() -> Option<RectI> {
    let outcome = select_capture_session()?;
    Some(outcome.screen_rect)
}

/// Executes an area selection across all monitors for screen recording.
/// Returns the chosen physical screen rectangle in virtual desktop coordinates.
pub fn select_recording_area() -> Option<RectI> {
    let outcome = select_capture_session()?;
    Some(outcome.screen_rect)
}

/// Creates and shows a real full-screen selection overlay window in an active state.
/// Useful for capturing the real window during reviews or live tests.
#[cfg(feature = "review")]
pub fn open_real_overlay_for_demo(
    drag_start: PointI,
    drag_current: PointI,
) -> Result<HWND, String> {
    open_real_overlay_for_demo_scaled(drag_start, drag_current, 1.0)
}

#[cfg(feature = "review")]
pub fn open_real_overlay_for_demo_scaled(
    drag_start: PointI,
    drag_current: PointI,
    scale: f32,
) -> Result<HWND, String> {
    enable_per_monitor_dpi_v2();

    let capture_res = capture_virtual_desktop().unwrap_or_else(|_| {
        let w = 1920;
        let h = 1080;
        let mut img = ImageBuffer::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, [24, 26, 32, 255]);
            }
        }
        CaptureResult {
            image: img,
            bounds: RectI::new(0, 0, w as i32, h as i32),
            origin: PointI::default(),
            is_drm_black: false,
        }
    });

    let bounds = capture_res.bounds;

    let state = SelectionState {
        rect: Some(RectI::from_points(drag_start, drag_current)),
        mode: OverlayMode::Idle,
        drag_start,
        current_cursor: drag_current,
        dpi_scale: scale,
        ..Default::default()
    };

    SESSION.with(|s| {
        *s.borrow_mut() = Some(OverlaySession {
            capture: capture_res,
            state,
            marks: Marks::default(),
            monitors: Vec::new(),
        });
    });

    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class_name = w!("LookoutCaptureOverlay");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(overlay_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class_name,
            w!("Lookout Capture"),
            WS_POPUP,
            bounds.left,
            bounds.top,
            bounds.width(),
            bounds.height(),
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| e.to_string())?;

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        Ok(hwnd)
    }
}

pub fn draw_top_bar(
    img: &mut ImageBuffer,
    scale: f32,
    mode: CaptureMode,
    delay: DelayTimer,
    include_cursor: bool,
    dest: Destination,
) {
    let scr_w = img.width as i32;
    let top_bar = get_top_bar_rect(scr_w, scale);

    let radius = (8.0 * scale).round() as i32;
    fill_rounded_rect_rgba(img, top_bar, radius, Color::rgb(20, 23, 28));
    stroke_rounded_rect_rgba(img, top_bar, radius, Color::rgb(38, 43, 51));

    let bx = top_bar.left;
    let by = top_bar.top;
    let btn_radius = (5.0 * scale).round() as i32;
    let font_size = (11.0 * scale).round().max(10.0) as u32;

    let modes = [
        (CaptureMode::Screenshot, "Shot (S)", 6.0, 80.0),
        (CaptureMode::RecordVideo, "Record (R)", 84.0, 166.0),
        (CaptureMode::ScrollCapture, "Scroll (L)", 170.0, 248.0),
        (CaptureMode::TextExtract, "Text (T)", 252.0, 324.0),
    ];

    for (m, label, l_off, r_off) in modes {
        let is_active = mode == m;
        let btn_rect = RectI::new(
            bx + (l_off * scale).round() as i32,
            by + (6.0 * scale).round() as i32,
            bx + (r_off * scale).round() as i32,
            by + (36.0 * scale).round() as i32,
        );
        let bg = if is_active {
            Color::AMBER // Lookout amber accent #f5a524
        } else {
            Color::rgb(28, 32, 38)
        };
        fill_rounded_rect_rgba(img, btn_rect, btn_radius, bg);
        if is_active {
            stroke_rounded_rect_rgba(img, btn_rect, btn_radius, Color::rgb(251, 191, 36));
        }

        let text_color = if is_active {
            Color::rgb(20, 23, 28) // Dark high-contrast text on amber
        } else {
            Color::rgb(180, 185, 195)
        };
        let (tw, th) = measure_system_text(label, font_size, true);
        let tx = btn_rect.left + (btn_rect.width() - tw) / 2;
        let ty = btn_rect.top + (btn_rect.height() - th) / 2;
        draw_system_text_rgba(img, PointI::new(tx, ty), label, text_color, font_size, true);
    }

    // Divider
    let div_x = bx + (330.0 * scale).round() as i32;
    for y in (by + (8.0 * scale).round() as i32)..(by + (34.0 * scale).round() as i32) {
        if div_x >= 0 && div_x < img.width as i32 && y >= 0 && y < img.height as i32 {
            img.set_pixel(div_x as u32, y as u32, [55, 65, 81, 255]);
        }
    }

    // Delay timer
    let delay_label = match delay {
        DelayTimer::Off => "⏱ 0s",
        DelayTimer::Sec3 => "⏱ 3s",
        DelayTimer::Sec5 => "⏱ 5s",
    };
    let btn_timer = RectI::new(
        bx + (336.0 * scale).round() as i32,
        by + (6.0 * scale).round() as i32,
        bx + (384.0 * scale).round() as i32,
        by + (36.0 * scale).round() as i32,
    );
    fill_rounded_rect_rgba(img, btn_timer, btn_radius, Color::rgb(28, 32, 38));
    let (tw, th) = measure_system_text(delay_label, font_size, false);
    draw_system_text_rgba(
        img,
        PointI::new(
            btn_timer.left + (btn_timer.width() - tw) / 2,
            btn_timer.top + (btn_timer.height() - th) / 2,
        ),
        delay_label,
        Color::rgb(210, 215, 225),
        font_size,
        false,
    );

    // Cursor
    let cursor_label = if include_cursor {
        "🖱 On"
    } else {
        "🖱 Off"
    };
    let btn_cursor = RectI::new(
        bx + (388.0 * scale).round() as i32,
        by + (6.0 * scale).round() as i32,
        bx + (442.0 * scale).round() as i32,
        by + (36.0 * scale).round() as i32,
    );
    fill_rounded_rect_rgba(img, btn_cursor, btn_radius, Color::rgb(28, 32, 38));
    let (tw, th) = measure_system_text(cursor_label, font_size, false);
    draw_system_text_rgba(
        img,
        PointI::new(
            btn_cursor.left + (btn_cursor.width() - tw) / 2,
            btn_cursor.top + (btn_cursor.height() - th) / 2,
        ),
        cursor_label,
        Color::rgb(210, 215, 225),
        font_size,
        false,
    );

    // Destination: unclipped at 100%, 150%, and 200%
    let dest_label = match dest {
        Destination::Clipboard => "Clipboard",
        Destination::File => "File",
        Destination::Both => "Both",
    };
    let btn_dest = RectI::new(
        bx + (446.0 * scale).round() as i32,
        by + (6.0 * scale).round() as i32,
        bx + (534.0 * scale).round() as i32,
        by + (36.0 * scale).round() as i32,
    );
    fill_rounded_rect_rgba(img, btn_dest, btn_radius, Color::rgb(28, 32, 38));
    let (tw, th) = measure_system_text(dest_label, font_size, false);
    draw_system_text_rgba(
        img,
        PointI::new(
            btn_dest.left + (btn_dest.width() - tw) / 2,
            btn_dest.top + (btn_dest.height() - th) / 2,
        ),
        dest_label,
        Color::rgb(210, 215, 225),
        font_size,
        false,
    );

    // Close
    let btn_close = RectI::new(
        bx + (538.0 * scale).round() as i32,
        by + (6.0 * scale).round() as i32,
        bx + (562.0 * scale).round() as i32,
        by + (36.0 * scale).round() as i32,
    );
    fill_rounded_rect_rgba(img, btn_close, btn_radius, Color::rgb(28, 32, 38));
    draw_system_text_rgba(
        img,
        PointI::new(
            btn_close.left + (6.0 * scale).round() as i32,
            btn_close.top + (7.0 * scale).round() as i32,
        ),
        "✕",
        Color::rgb(180, 185, 195),
        font_size,
        true,
    );
}

pub fn draw_selection_toolbar(img: &mut ImageBuffer, r: RectI, scale: f32) {
    let toolbar = get_selection_toolbar_rect(r, img.width as i32, img.height as i32, scale);
    draw_toolbar(img, toolbar, true, &Marks::default());
}

fn draw_toolbar(img: &mut ImageBuffer, toolbar: RectI, draws: bool, held: &Marks) {
    let scale = toolbar.width() as f32 / 448.0;
    let radius = (6.0 * scale).round() as i32;
    fill_rounded_rect_rgba(img, toolbar, radius, Color::rgb(20, 23, 28));
    stroke_rounded_rect_rgba(img, toolbar, radius, Color::rgb(45, 52, 64));

    let btn_radius = (4.0 * scale).round() as i32;
    let font_size = (11.0 * scale).round().max(10.0) as u32;
    for (button, label, rect) in marks::buttons(toolbar, draws) {
        let active = matches!(button, Button::Tool(t) if held.tool == Some(t));
        let dim = (button == Button::Undo && !held.model.can_undo())
            || (button == Button::Redo && !held.model.can_redo());
        let (back, ink) = if active {
            (Color::AMBER, Color::rgb(20, 23, 28))
        } else if dim {
            (Color::rgb(24, 28, 34), Color::rgb(110, 116, 128))
        } else {
            (Color::rgb(30, 35, 43), Color::rgb(230, 233, 238))
        };
        fill_rounded_rect_rgba(img, rect, btn_radius, back);
        let (tw, th) = measure_system_text(label, font_size, true);
        let tx = rect.left + (rect.width() - tw) / 2;
        let ty = rect.top + (rect.height() - th) / 2;
        draw_system_text_rgba(img, PointI::new(tx, ty), label, ink, font_size, true);
    }
}

#[cfg(feature = "review")]
pub fn render_top_bar_demo(scale: f32) -> ImageBuffer {
    let w = (588.0 * scale).round() as u32;
    let h = (64.0 * scale).round() as u32;
    let mut img = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(x, y, [12, 14, 18, 255]);
        }
    }
    draw_top_bar(
        &mut img,
        scale,
        CaptureMode::Screenshot,
        DelayTimer::Off,
        true,
        Destination::Clipboard,
    );
    img
}

#[cfg(feature = "review")]
pub fn render_selection_toolbar_demo(scale: f32) -> ImageBuffer {
    let w = (468.0 * scale).round() as u32;
    let h = (96.0 * scale).round() as u32;
    let mut img = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(x, y, [12, 14, 18, 255]);
        }
    }
    let dummy_r = RectI::new(10, -10, (w - 10) as i32, 10);
    draw_selection_toolbar(&mut img, dummy_r, scale);
    img
}

/// Pure software renderer for the overlay frame.
/// Applies the selection dimming mask, crosshairs, handles, selection border, size readout badge,
/// top bar, and selection toolbar.
pub fn render_overlay_frame_scaled(
    base_image: &ImageBuffer,
    sel_rect: Option<RectI>,
    cur: PointI,
    scale: f32,
) -> ImageBuffer {
    render_overlay_frame_scaled_opts(
        base_image,
        sel_rect,
        cur,
        scale,
        false,
        false,
        CaptureMode::Screenshot,
        DelayTimer::Off,
        true,
        Destination::Clipboard,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_overlay_frame_scaled_opts(
    base_image: &ImageBuffer,
    sel_rect: Option<RectI>,
    cur: PointI,
    scale: f32,
    is_scrolling: bool,
    is_recording: bool,
    capture_mode: CaptureMode,
    delay_timer: DelayTimer,
    include_cursor: bool,
    destination: Destination,
) -> ImageBuffer {
    render_frame(
        base_image,
        sel_rect,
        cur,
        scale,
        is_scrolling,
        is_recording,
        capture_mode,
        delay_timer,
        include_cursor,
        destination,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_frame(
    base_image: &ImageBuffer,
    sel_rect: Option<RectI>,
    cur: PointI,
    scale: f32,
    is_scrolling: bool,
    is_recording: bool,
    capture_mode: CaptureMode,
    delay_timer: DelayTimer,
    include_cursor: bool,
    destination: Destination,
    draft: Option<&Annotation>,
    session: Option<&OverlaySession>,
) -> ImageBuffer {
    let w = base_image.width;
    let h = base_image.height;
    let mut display_img = base_image.clone();

    // 1. Darken outside of the selection rectangle to 50% black
    let total_pixels = (w * h) as usize;
    for i in 0..total_pixels {
        let x = (i % (w as usize)) as i32;
        let y = (i / (w as usize)) as i32;

        let in_selection = sel_rect.is_some_and(|r| r.contains(PointI::new(x, y)));
        if !in_selection {
            let idx = i * 4;
            display_img.pixels[idx] = (display_img.pixels[idx] as u32 * 50 / 100) as u8;
            display_img.pixels[idx + 1] = (display_img.pixels[idx + 1] as u32 * 50 / 100) as u8;
            display_img.pixels[idx + 2] = (display_img.pixels[idx + 2] as u32 * 50 / 100) as u8;
        }
    }

    if let Some(mark) = draft {
        render_annotation(&mut display_img, mark);
    }

    // 2. Draw neutral thin white crosshair guidelines
    let crosshair_color = Color::rgba(255, 255, 255, 90);
    let cy = cur.y.clamp(0, h as i32 - 1) as usize;
    let inv_a = 255 - crosshair_color.a as u32;
    for x in 0..w {
        let idx = (cy * (w as usize) + (x as usize)) * 4;
        display_img.pixels[idx] = ((display_img.pixels[idx] as u32 * inv_a
            + crosshair_color.r as u32 * crosshair_color.a as u32)
            / 255) as u8;
        display_img.pixels[idx + 1] = ((display_img.pixels[idx + 1] as u32 * inv_a
            + crosshair_color.g as u32 * crosshair_color.a as u32)
            / 255) as u8;
        display_img.pixels[idx + 2] = ((display_img.pixels[idx + 2] as u32 * inv_a
            + crosshair_color.b as u32 * crosshair_color.a as u32)
            / 255) as u8;
    }
    let cx = cur.x.clamp(0, w as i32 - 1) as usize;
    for y in 0..h {
        let idx = ((y as usize) * (w as usize) + cx) * 4;
        display_img.pixels[idx] = ((display_img.pixels[idx] as u32 * inv_a
            + crosshair_color.r as u32 * crosshair_color.a as u32)
            / 255) as u8;
        display_img.pixels[idx + 1] = ((display_img.pixels[idx + 1] as u32 * inv_a
            + crosshair_color.g as u32 * crosshair_color.a as u32)
            / 255) as u8;
        display_img.pixels[idx + 2] = ((display_img.pixels[idx + 2] as u32 * inv_a
            + crosshair_color.b as u32 * crosshair_color.a as u32)
            / 255) as u8;
    }

    // 3. Draw Top Center Bar
    draw_top_bar(
        &mut display_img,
        scale,
        capture_mode,
        delay_timer,
        include_cursor,
        destination,
    );

    // 4. Selection border, handles, size badge, and toolbar
    if let Some(mut r) = sel_rect {
        if r.left > r.right {
            std::mem::swap(&mut r.left, &mut r.right);
        }
        if r.top > r.bottom {
            std::mem::swap(&mut r.top, &mut r.bottom);
        }

        if r.width() > 1 && r.height() > 1 {
            let border_color = Color::AMBER;
            // 2px amber border
            for x in r.left..r.right {
                if x >= 0 && x < w as i32 {
                    if r.top >= 0 && r.top < h as i32 {
                        display_img.set_pixel(x as u32, r.top as u32, border_color.to_array());
                    }
                    if r.top + 1 >= 0 && r.top + 1 < h as i32 {
                        display_img.set_pixel(
                            x as u32,
                            (r.top + 1) as u32,
                            border_color.to_array(),
                        );
                    }
                    if r.bottom > 0 && r.bottom - 1 < h as i32 {
                        display_img.set_pixel(
                            x as u32,
                            (r.bottom - 1) as u32,
                            border_color.to_array(),
                        );
                    }
                    if r.bottom > 1 && r.bottom - 2 < h as i32 {
                        display_img.set_pixel(
                            x as u32,
                            (r.bottom - 2) as u32,
                            border_color.to_array(),
                        );
                    }
                }
            }
            for y in r.top..r.bottom {
                if y >= 0 && y < h as i32 {
                    if r.left >= 0 && r.left < w as i32 {
                        display_img.set_pixel(r.left as u32, y as u32, border_color.to_array());
                    }
                    if r.left + 1 >= 0 && r.left + 1 < w as i32 {
                        display_img.set_pixel(
                            (r.left + 1) as u32,
                            y as u32,
                            border_color.to_array(),
                        );
                    }
                    if r.right > 0 && r.right - 1 < w as i32 {
                        display_img.set_pixel(
                            (r.right - 1) as u32,
                            y as u32,
                            border_color.to_array(),
                        );
                    }
                    if r.right > 1 && r.right - 2 < w as i32 {
                        display_img.set_pixel(
                            (r.right - 2) as u32,
                            y as u32,
                            border_color.to_array(),
                        );
                    }
                }
            }

            // 8 Corner and edge resize handles
            let handle_size = get_selection_handle_size(scale);
            let half = handle_size / 2;
            let handles = get_selection_handles(r);

            for hpt in &handles {
                let h_left = hpt.x - half;
                let h_top = hpt.y - half;
                let h_right = h_left + handle_size;
                let h_bottom = h_top + handle_size;

                for hy in h_top..h_bottom {
                    if hy < 0 || hy >= h as i32 {
                        continue;
                    }
                    for hx in h_left..h_right {
                        if hx < 0 || hx >= w as i32 {
                            continue;
                        }
                        let is_border =
                            hx == h_left || hx == h_right - 1 || hy == h_top || hy == h_bottom - 1;
                        if is_border {
                            display_img.set_pixel(hx as u32, hy as u32, [245, 165, 36, 255]);
                        } else {
                            display_img.set_pixel(hx as u32, hy as u32, [255, 255, 255, 255]);
                        }
                    }
                }
            }

            // Size readout badge
            let size_text = if is_recording {
                format!("{} × {}  (Enter to record)", r.width(), r.height())
            } else if is_scrolling {
                format!("{} × {}  (Enter to scroll)", r.width(), r.height())
            } else {
                match capture_mode {
                    CaptureMode::Screenshot => {
                        format!(
                            "{} × {}  (Enter for the editor, Ctrl+C to copy)",
                            r.width(),
                            r.height()
                        )
                    }
                    CaptureMode::RecordVideo => {
                        format!("{} × {}  (Enter to record)", r.width(), r.height())
                    }
                    CaptureMode::ScrollCapture => {
                        format!("{} × {}  (Enter to scroll)", r.width(), r.height())
                    }
                    CaptureMode::TextExtract => {
                        format!("{} × {}  (Enter to OCR)", r.width(), r.height())
                    }
                }
            };
            let font_size = (12.0 * scale).round().max(11.0) as u32;
            let (tw, th) = measure_system_text(&size_text, font_size, true);

            let pad_x = (8.0 * scale).round() as i32;
            let pad_y = (4.0 * scale).round() as i32;
            let badge_w = tw + pad_x * 2;
            let badge_h = th + pad_y * 2;

            let mut badge_x = (r.left + r.right - badge_w) / 2;
            badge_x = badge_x.clamp(8, w as i32 - badge_w - 8);
            let badge_y = (r.top - badge_h - (12.0 * scale).round() as i32).max(8);

            let badge_rect = RectI::new(badge_x, badge_y, badge_x + badge_w, badge_y + badge_h);
            let badge_radius = (5.0 * scale).round() as i32;

            fill_rounded_rect_rgba(
                &mut display_img,
                badge_rect,
                badge_radius,
                Color::rgb(20, 23, 28),
            );
            stroke_rounded_rect_rgba(
                &mut display_img,
                badge_rect,
                badge_radius,
                Color::rgb(38, 43, 51),
            );

            draw_system_text_rgba(
                &mut display_img,
                PointI::new(badge_x + pad_x, badge_y + pad_y),
                &size_text,
                Color::rgb(230, 233, 238),
                font_size,
                true,
            );

            // Selection Toolbar (when selection is at least 16x16)
            if r.width() >= 16 && r.height() >= 16 {
                match session {
                    Some(s) => {
                        let toolbar = marks::toolbar_rect(r, s.monitor(r), s.draws());
                        draw_toolbar(&mut display_img, toolbar, s.draws(), &s.marks);
                    }
                    None => draw_selection_toolbar(&mut display_img, r, scale),
                }
            }
        }
    }

    display_img
}

pub fn render_overlay_frame(
    base_image: &ImageBuffer,
    sel_rect: Option<RectI>,
    cur: PointI,
) -> ImageBuffer {
    render_overlay_frame_scaled(base_image, sel_rect, cur, 1.0)
}
