//! Screen region selection overlay across all monitors.
//! Features dimming mask, thin selection frame, resize handles, crosshair
//! guides, size tag and one floating bar.

use std::cell::RefCell;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU8, Ordering};

use windows::core::w;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
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
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    LoadCursorW, PostQuitMessage, RegisterClassW, SetCursor, SetForegroundWindow, ShowWindow,
    TranslateMessage, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, IDC_ARROW, IDC_CROSS, IDC_SIZEALL,
    IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, MSG, SW_SHOW, WM_CHAR, WM_DESTROY,
    WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WM_SETCURSOR, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::annotation::{render_annotation, Annotation, Color};
use crate::bar::{self, Button, Panel, Popup};
use crate::capture::{capture_virtual_desktop, enable_per_monitor_dpi_v2, CaptureResult};
use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;
use crate::marks::{self, Marks, Monitor};
use crate::skin::{self, Skin};

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
    /// The list or popover open beside the bar.
    pub popup: Option<Popup>,
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

    /// The bar as it stands now.
    fn bar(&self) -> Option<Panel> {
        let p = self.state.current_cursor;
        let under = self.monitor(RectI::new(p.x, p.y, p.x + 1, p.y + 1));
        bar_for(&self.state, &self.monitors, under)
    }

    fn open_popup(&self) -> Option<Panel> {
        bar::popup(&self.bar()?, self.popup?)
    }

    /// Is `p` on the bar or on what is open beside it?
    fn on_controls(&self, p: PointI) -> bool {
        self.bar().is_some_and(|b| b.rect.contains(p))
            || self.open_popup().is_some_and(|l| l.rect.contains(p))
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

    /// Runs a button of the bar or of the list `open` beside it. True if the
    /// overlay is done.
    fn press_button(&mut self, button: Button, open: Option<Popup>) -> bool {
        self.marks.finish_text(&self.capture.image);
        // Its own button closes a list again.
        let toggle = |popup| (open != Some(popup)).then_some(popup);
        match button {
            Button::Modes => self.popup = toggle(Popup::Modes),
            Button::More => self.popup = toggle(Popup::More),
            // The tool in hand opens its colours; one without any is put down.
            Button::Tool(tool) => {
                if self.marks.tool == Some(tool) && !marks::colors(tool).is_empty() {
                    self.popup = toggle(Popup::Style(tool));
                } else {
                    self.marks.pick(tool);
                }
            }
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
            Button::Mode(mode) => self.set_mode(mode),
            // The rest change a setting and leave the list open.
            Button::Timer => {
                self.state.delay_timer = self.state.delay_timer.next();
                self.popup = open;
            }
            Button::Pointer => {
                self.state.include_cursor = !self.state.include_cursor;
                self.popup = open;
            }
            Button::Target => {
                self.state.destination = self.state.destination.next();
                self.popup = open;
            }
            Button::Color(index) => {
                if let Some(Popup::Style(tool)) = open {
                    self.marks.set_color(tool, index);
                }
                self.popup = open;
            }
            Button::Size(level) => {
                if let Some(Popup::Style(tool)) = open {
                    self.marks.set_level(tool, level);
                }
                self.popup = open;
            }
            Button::PutDown => self.marks.tool = None,
        }
        false
    }

    fn set_mode(&mut self, mode: CaptureMode) {
        self.state.capture_mode = mode;
        let stored = match mode {
            CaptureMode::Screenshot => 0,
            CaptureMode::RecordVideo => 1,
            CaptureMode::ScrollCapture => 2,
            CaptureMode::TextExtract => 3,
        };
        LAST_USED_MODE.store(stored, Ordering::Release);
    }
}

/// With a box big enough, the bar beside it; while one is being dragged out,
/// nothing; otherwise the bar at the top of the `idle` monitor.
fn bar_for(state: &SelectionState, monitors: &[Monitor], idle: Monitor) -> Option<Panel> {
    if state.mode == OverlayMode::Selecting {
        return None;
    }
    let sel = state
        .rect
        .map(normalized)
        .filter(|r| r.width() >= 16 && r.height() >= 16);
    let monitor = sel.map_or(idle, |sel| marks::monitor_for(monitors, sel, idle));
    Some(bar::layout(sel, monitor, state.capture_mode))
}

fn normalized(r: RectI) -> RectI {
    RectI::new(
        r.left.min(r.right),
        r.top.min(r.bottom),
        r.left.max(r.right),
        r.top.max(r.bottom),
    )
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

/// A press of the left button, on the bar, on the box or on the picture.
unsafe fn press(hwnd: HWND, p: PointI) -> LRESULT {
    let mut handled_ui = false;
    let mut should_close = false;

    SESSION.with(|s| {
        if let Some(sess) = s.borrow_mut().as_mut() {
            sess.state.current_cursor = p;
            let tol = get_selection_handle_size(sess.state.dpi_scale);

            let list = sess.open_popup();
            let bar = sess.bar();
            let open = sess.popup.take();

            // 1. What is open beside the bar, then the bar itself
            if let Some(list) = list.as_ref().filter(|l| l.rect.contains(p)) {
                handled_ui = true;
                match list.at(p) {
                    Some(button) => should_close = sess.press_button(button, open),
                    None => sess.popup = open,
                }
            } else if let Some(bar) = bar.filter(|b| b.rect.contains(p)) {
                handled_ui = true;
                if let Some(button) = bar.at(p) {
                    should_close = sess.press_button(button, open);
                }
            } else if list.is_some() {
                // A press elsewhere only closes it, unless it starts a mark.
                handled_ui = !(sess.draws()
                    && sess.marks.tool.is_some()
                    && sess
                        .state
                        .rect
                        .is_some_and(|r| r.contains(p) && !on_border(r, p, tol)));
            }

            if !handled_ui {
                sess.marks.finish_text(&sess.capture.image);
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

                    if sess.on_controls(cur) {
                        if let Ok(c) = LoadCursorW(None, IDC_ARROW) {
                            SetCursor(Some(c));
                            cursor_handled = true;
                        }
                    } else if let Some(r) = sess.state.rect {
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
                    again = (sess.draws() && sess.marks.tool.is_some())
                        || sess.popup.is_some()
                        || sess.on_controls(p);
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
                            sess.set_mode(CaptureMode::Screenshot);
                            need_redraw = true;
                        }
                        // R / r
                        0x52 => {
                            sess.set_mode(CaptureMode::RecordVideo);
                            need_redraw = true;
                        }
                        // L / l
                        0x4C => {
                            sess.set_mode(CaptureMode::ScrollCapture);
                            need_redraw = true;
                        }
                        // T / t
                        0x54 => {
                            sess.set_mode(CaptureMode::TextExtract);
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
            &SelectionState {
                dpi_scale: scale,
                ..sess.state
            },
            &sess.marks,
            sess.popup,
            &sess.monitors,
            draft.as_ref(),
            &skin::current(),
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
    // Where the pointer is says which screen the bar opens on.
    let mut at = POINT::default();
    let _ = unsafe { GetCursorPos(&mut at) };
    let pointer = PointI::new(at.x - bounds.left, at.y - bounds.top);

    SESSION.with(|s| {
        *s.borrow_mut() = Some(OverlaySession {
            capture: capture_res,
            state: SelectionState {
                dpi_scale: 1.0,
                current_cursor: pointer,
                ..Default::default()
            },
            marks: Marks::default(),
            monitors,
            popup: None,
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
            popup: None,
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

/// A bar alone on a dark ground, for review pictures.
#[cfg(feature = "review")]
fn bar_demo(sel: Option<RectI>, w: u32, h: u32, scale: f32) -> ImageBuffer {
    let mut img = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(x, y, [12, 14, 18, 255]);
        }
    }
    let screen = Monitor {
        rect: RectI::new(0, 0, w as i32, h as i32),
        scale,
    };
    let panel = bar::layout(sel, screen, CaptureMode::Screenshot);
    let state = SelectionState::default();
    let held = Marks::default();
    bar::draw_bar(&mut img, &panel, &state, &held, None, &skin::current());
    img
}

/// The bar as it is before there is a box.
#[cfg(feature = "review")]
pub fn render_top_bar_demo(scale: f32) -> ImageBuffer {
    let w = (460.0 * scale).round() as u32;
    let h = (64.0 * scale).round() as u32;
    bar_demo(None, w, h, scale)
}

/// The bar as it is beside a box.
#[cfg(feature = "review")]
pub fn render_selection_toolbar_demo(scale: f32) -> ImageBuffer {
    let w = (460.0 * scale).round() as u32;
    let h = (96.0 * scale).round() as u32;
    let sel = RectI::new(10, -20, w as i32 - 10, (10.0 * scale) as i32);
    bar_demo(Some(sel), w, h, scale)
}

/// A frame of the overlay in software, with a box or without one.
pub fn render_overlay_frame_scaled(
    base_image: &ImageBuffer,
    sel_rect: Option<RectI>,
    cur: PointI,
    scale: f32,
) -> ImageBuffer {
    let state = SelectionState {
        rect: sel_rect,
        current_cursor: cur,
        dpi_scale: scale,
        capture_mode: CaptureMode::Screenshot,
        ..Default::default()
    };
    let held = Marks::default();
    render_frame(base_image, &state, &held, None, &[], None, &skin::current())
}

/// The frame: the dimmed picture, crosshairs, the box with its handles and size
/// tag, then the bar and whatever is open beside it.
fn render_frame(
    base_image: &ImageBuffer,
    state: &SelectionState,
    held: &Marks,
    open: Option<Popup>,
    monitors: &[Monitor],
    draft: Option<&Annotation>,
    skin: &Skin,
) -> ImageBuffer {
    let w = base_image.width as i32;
    let h = base_image.height as i32;
    let cur = state.current_cursor;
    let sel_rect = state.rect.map(normalized);
    let mut display_img = base_image.clone();

    // 1. Dim what is outside the box
    for (i, pixel) in display_img
        .pixels
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let at = PointI::new(i as i32 % w, i as i32 / w);
        if !sel_rect.is_some_and(|r| r.contains(at)) {
            for c in &mut pixel[..3] {
                *c = (u32::from(*c) * 64 / 100) as u8;
            }
        }
    }

    if let Some(mark) = draft {
        render_annotation(&mut display_img, mark);
    }

    let whole = Monitor {
        rect: RectI::new(0, 0, w, h),
        scale: state.dpi_scale,
    };
    let point = RectI::new(cur.x, cur.y, cur.x + 1, cur.y + 1);
    let under = marks::monitor_for(monitors, point, whole);
    let bar = bar_for(state, monitors, under);
    let list = bar.as_ref().zip(open).and_then(|(b, o)| bar::popup(b, o));
    let on_list = list.as_ref().is_some_and(|l| l.rect.contains(cur));
    let on_bar = bar.as_ref().is_some_and(|b| b.rect.contains(cur));

    // 2. Thin white crosshair guidelines, away from the controls
    if !on_bar && !on_list {
        let line = Color::rgba(255, 255, 255, 90);
        for x in 0..w {
            skin::blend(&mut display_img, x, cur.y.clamp(0, h - 1), line, 1.0);
        }
        for y in 0..h {
            skin::blend(&mut display_img, cur.x.clamp(0, w - 1), y, line, 1.0);
        }
    }

    // 3. The box: frame, handles and size tag
    if let Some(r) = sel_rect.filter(|r| r.width() > 1 && r.height() > 1) {
        let monitor = marks::monitor_for(monitors, r, whole);
        draw_frame(&mut display_img, r, bar::scale_on(monitor), skin.accent);
        bar::draw_size_tag(&mut display_img, r, monitor, skin);
    }

    // 4. The bar, what is open beside it, and the name of what is pointed at
    if let Some(bar) = &bar {
        bar::draw_bar(&mut display_img, bar, state, held, open, skin);
        match list.as_ref().zip(open) {
            Some((list, open)) => {
                bar::draw_popup(&mut display_img, list, open, state, held, skin);
                // Lists name their own rows; the popover's buttons do not.
                if matches!(open, Popup::Style(_)) && on_list {
                    bar::draw_tooltip(&mut display_img, list, state, skin);
                }
            }
            None => bar::draw_tooltip(&mut display_img, bar, state, skin),
        }
    }

    display_img
}

/// A line of 1.5 px just outside the box, with eight small square handles.
fn draw_frame(img: &mut ImageBuffer, r: RectI, scale: f32, accent: Color) {
    let line = 1.5 * scale;
    for ring in 0..line.ceil() as i32 {
        let cover = line - ring as f32;
        let (left, top) = (r.left - 1 - ring, r.top - 1 - ring);
        let (right, bottom) = (r.right + ring, r.bottom + ring);
        for x in left..=right {
            skin::blend(img, x, top, accent, cover);
            skin::blend(img, x, bottom, accent, cover);
        }
        for y in top + 1..bottom {
            skin::blend(img, left, y, accent, cover);
            skin::blend(img, right, y, accent, cover);
        }
    }

    let size = (6.0 * scale).round() as i32;
    for at in get_selection_handles(r) {
        let (left, top) = (at.x - size / 2, at.y - size / 2);
        let square = RectI::new(left, top, left + size, top + size);
        // A dark rim keeps it visible on a picture of the same colour.
        let rim = RectI::new(left - 1, top - 1, left + size + 1, top + size + 1);
        skin::panel(img, rim, 1.0, Color::rgba(0, 0, 0, 110), None);
        skin::panel(img, square, 0.0, accent, None);
    }
}
