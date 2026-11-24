//! Scrolling screenshot capture: mouse wheel automation, frame acquisition,
//! sticky header/footer detection, vertical overlap alignment, and image stitching.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
    InvalidateRect, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, PAINTSTRUCT,
    SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, VK_ESCAPE, VK_NEXT,
    VK_PRIOR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW, SetCursorPos,
    SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage, WindowFromPoint, CS_HREDRAW,
    CS_VREDRAW, MSG, PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, WHEEL_DELTA,
    WM_CREATE, WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_PAINT, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::annotation::{draw_system_text_rgba, Color};
use crate::capture::capture_gdi_rect;
use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;

pub const MAX_SCROLLING_HEIGHT: u32 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OverlapMatch {
    Shift {
        shift: u32,
        overlap: u32,
        error: f64,
    },
    UpwardShift {
        shift: u32,
        overlap: u32,
        error: f64,
    },
    EndOfPage,
    NoMatch,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StitchStepResult {
    Added { shift: u32, total_height: u32 },
    ScrolledUp { shift: u32, total_height: u32 },
    EndOfPage,
    ReachedMaxHeight,
    NoMatch,
}

/// Detects the height of a sticky top header between two consecutive frames.
/// Sticky headers maintain nearly identical pixel rows at the top while content below scrolls.
pub fn detect_sticky_header(frame_a: &ImageBuffer, frame_b: &ImageBuffer, max_ratio: f32) -> u32 {
    let w = frame_a.width.min(frame_b.width);
    let h = frame_a.height.min(frame_b.height);
    let limit = ((h as f32 * max_ratio).round() as u32).min(h / 2);
    // Ignore rightmost 20px where a vertical scrollbar thumb may shift
    let w_eff = (w.saturating_sub(20)).max(16);

    let mut header_h = 0;
    for y in 0..limit {
        let row_a = frame_a.row(y);
        let row_b = frame_b.row(y);
        let mut diff_sum = 0u64;

        // Sample across columns
        let step = (w_eff / 120).max(1) as usize;
        let mut count = 0usize;
        let mut x = 0;
        while x < w_eff as usize {
            let idx = x * 4;
            let dr = (row_a[idx] as i32 - row_b[idx] as i32).unsigned_abs() as u64;
            let dg = (row_a[idx + 1] as i32 - row_b[idx + 1] as i32).unsigned_abs() as u64;
            let db = (row_a[idx + 2] as i32 - row_b[idx + 2] as i32).unsigned_abs() as u64;
            diff_sum += dr + dg + db;
            count += 1;
            x += step;
        }

        let avg = if count > 0 {
            diff_sum as f64 / (count as f64 * 3.0)
        } else {
            0.0
        };

        if avg <= 2.5 {
            header_h = y + 1;
        } else {
            break;
        }
    }

    header_h
}

/// Detects the height of a sticky bottom footer between two consecutive frames.
/// Sticky footers maintain nearly identical pixel rows at the bottom while content above scrolls.
pub fn detect_sticky_footer(frame_a: &ImageBuffer, frame_b: &ImageBuffer, max_ratio: f32) -> u32 {
    let w = frame_a.width.min(frame_b.width);
    let h = frame_a.height.min(frame_b.height);
    let limit = ((h as f32 * max_ratio).round() as u32).min(h / 3);
    let w_eff = (w.saturating_sub(20)).max(16);

    let mut footer_h = 0;
    for offset in 0..limit {
        let y = h - 1 - offset;
        let row_a = frame_a.row(y);
        let row_b = frame_b.row(y);
        let mut diff_sum = 0u64;

        let step = (w_eff / 120).max(1) as usize;
        let mut count = 0usize;
        let mut x = 0;
        while x < w_eff as usize {
            let idx = x * 4;
            let dr = (row_a[idx] as i32 - row_b[idx] as i32).unsigned_abs() as u64;
            let dg = (row_a[idx + 1] as i32 - row_b[idx + 1] as i32).unsigned_abs() as u64;
            let db = (row_a[idx + 2] as i32 - row_b[idx + 2] as i32).unsigned_abs() as u64;
            diff_sum += dr + dg + db;
            count += 1;
            x += step;
        }

        let avg = if count > 0 {
            diff_sum as f64 / (count as f64 * 3.0)
        } else {
            0.0
        };

        if avg <= 2.5 {
            footer_h = offset + 1;
        } else {
            break;
        }
    }

    footer_h
}

/// Checks whether two frames are identical (end of page reached, scrolling did not move).
pub fn is_frames_identical(frame_a: &ImageBuffer, frame_b: &ImageBuffer) -> bool {
    if frame_a.width != frame_b.width || frame_a.height != frame_b.height {
        return false;
    }
    let w = frame_a.width as usize;
    let h = frame_a.height as usize;
    let w_eff = (w.saturating_sub(20)).max(16);
    let mut total_diff = 0u64;
    let mut count = 0usize;
    let mut changed_samples = 0usize;

    let y_step = (h / 80).max(1);
    let x_step = (w_eff / 80).max(1);

    for y in (0..h).step_by(y_step) {
        let row_a = frame_a.row(y as u32);
        let row_b = frame_b.row(y as u32);
        for x in (0..w_eff).step_by(x_step) {
            let idx = x * 4;
            let dr = (row_a[idx] as i32 - row_b[idx] as i32).unsigned_abs() as u64;
            let dg = (row_a[idx + 1] as i32 - row_b[idx + 1] as i32).unsigned_abs() as u64;
            let db = (row_a[idx + 2] as i32 - row_b[idx + 2] as i32).unsigned_abs() as u64;
            let diff = dr + dg + db;
            total_diff += diff;
            if diff > 18 {
                changed_samples += 1;
            }
            count += 1;
        }
    }

    if count == 0 {
        return true;
    }
    let avg = total_diff as f64 / (count as f64 * 3.0);
    avg <= 1.0 && changed_samples <= (count / 200).max(2)
}

/// Finds the vertical scroll shift between `frame_a` and `frame_b`.
/// Excludes sticky header and footer before searching for the optimal overlap.
pub fn find_vertical_overlap(
    frame_a: &ImageBuffer,
    frame_b: &ImageBuffer,
    header_h: u32,
    footer_h: u32,
) -> OverlapMatch {
    if is_frames_identical(frame_a, frame_b) {
        return OverlapMatch::EndOfPage;
    }

    let w = frame_a.width.min(frame_b.width);
    let h = frame_a.height.min(frame_b.height);

    if h <= header_h + footer_h + 16 {
        return OverlapMatch::NoMatch;
    }

    let content_h = h - header_h - footer_h;
    let min_overlap = 16u32.min(content_h / 4);
    let max_shift = content_h.saturating_sub(min_overlap);

    if max_shift < 1 {
        return OverlapMatch::NoMatch;
    }

    let w_eff = (w.saturating_sub(20)).max(16);

    // Step across columns for fast comparison
    let col_step = (w_eff / 160).max(1) as usize;

    // Check if content at shift 0 is essentially identical (no movement / end of page)
    let mut s0_diff = 0u64;
    let mut s0_count = 0usize;
    for i in (0..content_h).step_by(3) {
        let y = header_h + i;
        let row_a = frame_a.row(y);
        let row_b = frame_b.row(y);
        let mut x = 0usize;
        while x < w_eff as usize {
            let idx = x * 4;
            s0_diff += (row_a[idx] as i32 - row_b[idx] as i32).unsigned_abs() as u64;
            s0_diff += (row_a[idx + 1] as i32 - row_b[idx + 1] as i32).unsigned_abs() as u64;
            s0_diff += (row_a[idx + 2] as i32 - row_b[idx + 2] as i32).unsigned_abs() as u64;
            s0_count += 1;
            x += col_step;
        }
    }
    let s0_err = if s0_count > 0 {
        s0_diff as f64 / (s0_count as f64 * 3.0)
    } else {
        0.0
    };
    if s0_err <= 2.0 {
        return OverlapMatch::EndOfPage;
    }

    let mut best_shift = 0u32;
    let mut min_error = f64::MAX;

    // Search vertical shifts from 4 px up to max_shift
    let start_shift = 4u32.min(max_shift);
    for s in start_shift..=max_shift {
        let overlap = content_h - s;
        let row_step = 1u32;

        let mut diff_sum = 0u64;
        let mut sample_count = 0usize;

        let mut i = 0u32;
        while i < overlap {
            let y_a = header_h + s + i;
            let y_b = header_h + i;

            let row_a = frame_a.row(y_a);
            let row_b = frame_b.row(y_b);

            let mut x = 0usize;
            while x < w_eff as usize {
                let idx = x * 4;
                let dr = (row_a[idx] as i32 - row_b[idx] as i32).unsigned_abs() as u64;
                let dg = (row_a[idx + 1] as i32 - row_b[idx + 1] as i32).unsigned_abs() as u64;
                let db = (row_a[idx + 2] as i32 - row_b[idx + 2] as i32).unsigned_abs() as u64;
                diff_sum += dr + dg + db;
                sample_count += 1;
                x += col_step;
            }

            i += row_step;
        }

        if sample_count == 0 {
            continue;
        }

        let err = diff_sum as f64 / (sample_count as f64 * 3.0);
        if err < min_error {
            min_error = err;
            best_shift = s;
            if min_error < 0.05 && overlap > content_h / 2 {
                break;
            }
        }
    }

    // Check upward shift (small scroll back up)
    let mut best_up_shift = 0u32;
    let mut min_up_error = f64::MAX;
    for u in start_shift..=max_shift {
        let overlap = content_h - u;
        let mut diff_sum = 0u64;
        let mut sample_count = 0usize;
        let mut i = 0u32;

        while i < overlap {
            let y_a = header_h + i;
            let y_b = header_h + u + i;

            let row_a = frame_a.row(y_a);
            let row_b = frame_b.row(y_b);

            let mut x = 0usize;
            while x < w_eff as usize {
                let idx = x * 4;
                let dr = (row_a[idx] as i32 - row_b[idx] as i32).unsigned_abs() as u64;
                let dg = (row_a[idx + 1] as i32 - row_b[idx + 1] as i32).unsigned_abs() as u64;
                let db = (row_a[idx + 2] as i32 - row_b[idx + 2] as i32).unsigned_abs() as u64;
                diff_sum += dr + dg + db;
                sample_count += 1;
                x += col_step;
            }
            i += 1;
        }

        if sample_count == 0 {
            continue;
        }

        let err = diff_sum as f64 / (sample_count as f64 * 3.0);
        if err < min_up_error {
            min_up_error = err;
            best_up_shift = u;
            if min_up_error < 0.05 && overlap > content_h / 2 {
                break;
            }
        }
    }

    let down_valid = min_error <= 9.0 && best_shift >= start_shift && min_error < s0_err;
    let up_valid = min_up_error <= 9.0 && best_up_shift >= start_shift && min_up_error < s0_err;

    if down_valid && up_valid {
        let down_overlap = content_h - best_shift;
        let up_overlap = content_h - best_up_shift;

        if up_overlap >= down_overlap * 2 && min_up_error <= min_error + 2.0 {
            OverlapMatch::UpwardShift {
                shift: best_up_shift,
                overlap: up_overlap,
                error: min_up_error,
            }
        } else if down_overlap >= up_overlap * 2 && min_error <= min_up_error + 2.0 {
            OverlapMatch::Shift {
                shift: best_shift,
                overlap: down_overlap,
                error: min_error,
            }
        } else if min_up_error + 0.5 < min_error {
            OverlapMatch::UpwardShift {
                shift: best_up_shift,
                overlap: up_overlap,
                error: min_up_error,
            }
        } else {
            OverlapMatch::Shift {
                shift: best_shift,
                overlap: down_overlap,
                error: min_error,
            }
        }
    } else if down_valid {
        OverlapMatch::Shift {
            shift: best_shift,
            overlap: content_h - best_shift,
            error: min_error,
        }
    } else if up_valid {
        OverlapMatch::UpwardShift {
            shift: best_up_shift,
            overlap: content_h - best_up_shift,
            error: min_up_error,
        }
    } else if s0_err <= 3.5 {
        OverlapMatch::EndOfPage
    } else {
        OverlapMatch::NoMatch
    }
}

/// Accumulates scrolled slices into a growing composite image.
pub struct ScrollingStitcher {
    pub width: u32,
    pub header_h: u32,
    pub footer_h: u32,
    pub header_slice: Vec<u8>,
    pub content_buffer: Vec<u8>,
    pub content_height: u32,
    pub last_frame: Option<ImageBuffer>,
    pub max_height: u32,
    pub hit_limit: bool,
    pub frame_count: usize,
}

impl ScrollingStitcher {
    pub fn new(max_height: u32) -> Self {
        Self {
            width: 0,
            header_h: 0,
            footer_h: 0,
            header_slice: Vec::new(),
            content_buffer: Vec::new(),
            content_height: 0,
            last_frame: None,
            max_height,
            hit_limit: false,
            frame_count: 0,
        }
    }

    /// Initializes stitcher with the first frame.
    pub fn init(&mut self, first_frame: ImageBuffer) {
        self.width = first_frame.width;
        self.last_frame = Some(first_frame);
        self.frame_count = 1;
    }

    pub fn total_height(&self) -> u32 {
        self.header_h + self.content_height + self.footer_h
    }

    /// Adds a subsequent frame to the stitcher, aligning with vertical overlap.
    pub fn add_frame(&mut self, next_frame: &ImageBuffer) -> StitchStepResult {
        let prev = match self.last_frame.as_ref() {
            Some(f) => f,
            None => {
                self.init(next_frame.clone());
                return StitchStepResult::Added {
                    shift: next_frame.height,
                    total_height: next_frame.height,
                };
            }
        };

        // On the very first transition, detect sticky header and footer
        if self.frame_count == 1 {
            self.header_h = detect_sticky_header(prev, next_frame, 0.35);
            self.footer_h = detect_sticky_footer(prev, next_frame, 0.25);

            let row_stride = (self.width * 4) as usize;

            // Extract header slice from frame 0
            if self.header_h > 0 {
                let header_bytes = (self.header_h as usize) * row_stride;
                self.header_slice = prev.pixels[..header_bytes].to_vec();
            }

            // Extract content from frame 0 (excluding header and footer)
            let prev_h = prev.height;
            let c_start = (self.header_h as usize) * row_stride;
            let c_end = ((prev_h - self.footer_h) as usize) * row_stride;
            if c_end > c_start {
                self.content_buffer = prev.pixels[c_start..c_end].to_vec();
                self.content_height = prev_h - self.header_h - self.footer_h;
            }
        }

        match find_vertical_overlap(prev, next_frame, self.header_h, self.footer_h) {
            OverlapMatch::EndOfPage => StitchStepResult::EndOfPage,
            OverlapMatch::NoMatch => StitchStepResult::NoMatch,
            OverlapMatch::UpwardShift { shift, .. } => {
                let row_stride = (self.width * 4) as usize;
                let rollback_pixels = shift.min(self.content_height);
                let rollback_bytes = (rollback_pixels as usize) * row_stride;
                let current_len = self.content_buffer.len();
                if rollback_bytes <= current_len {
                    self.content_buffer.truncate(current_len - rollback_bytes);
                    self.content_height -= rollback_pixels;
                }
                self.last_frame = Some(next_frame.clone());
                self.frame_count += 1;
                StitchStepResult::ScrolledUp {
                    shift,
                    total_height: self.total_height(),
                }
            }
            OverlapMatch::Shift { shift, .. } => {
                let current_total = self.total_height();
                let next_h = next_frame.height;
                let row_stride = (self.width * 4) as usize;

                if current_total + shift >= self.max_height {
                    let allowed = self.max_height.saturating_sub(current_total);
                    if allowed > 0 {
                        let start_row = next_h - self.footer_h - shift;
                        let start_byte = (start_row as usize) * row_stride;
                        let end_byte = ((start_row + allowed) as usize) * row_stride;
                        if end_byte <= next_frame.pixels.len() {
                            self.content_buffer
                                .extend_from_slice(&next_frame.pixels[start_byte..end_byte]);
                            self.content_height += allowed;
                        }
                    }
                    self.hit_limit = true;
                    self.last_frame = Some(next_frame.clone());
                    self.frame_count += 1;
                    StitchStepResult::ReachedMaxHeight
                } else {
                    let start_row = next_h - self.footer_h - shift;
                    let end_row = next_h - self.footer_h;
                    let start_byte = (start_row as usize) * row_stride;
                    let end_byte = (end_row as usize) * row_stride;

                    if end_byte <= next_frame.pixels.len() {
                        self.content_buffer
                            .extend_from_slice(&next_frame.pixels[start_byte..end_byte]);
                        self.content_height += shift;
                    }

                    self.last_frame = Some(next_frame.clone());
                    self.frame_count += 1;

                    StitchStepResult::Added {
                        shift,
                        total_height: self.total_height(),
                    }
                }
            }
        }
    }

    /// Assembles the stitched result: header + continuous content + final footer.
    pub fn finish(mut self) -> Result<ImageBuffer, String> {
        let total_h = self.total_height();
        let last = self
            .last_frame
            .take()
            .ok_or_else(|| "No frames captured".to_string())?;

        if self.frame_count <= 1 {
            return Ok(last);
        }

        if total_h == 0 || self.width == 0 {
            return Ok(last);
        }

        let row_stride = (self.width * 4) as usize;
        let mut result = ImageBuffer::new(self.width, total_h);

        // 1. Copy sticky header (from frame 0)
        let mut dest_offset = 0usize;
        if self.header_h > 0 && !self.header_slice.is_empty() {
            let h_bytes = (self.header_h as usize) * row_stride;
            let copy_len = h_bytes.min(self.header_slice.len());
            result.pixels[dest_offset..dest_offset + copy_len]
                .copy_from_slice(&self.header_slice[..copy_len]);
            dest_offset += h_bytes;
        }

        // 2. Copy combined content
        let c_bytes = (self.content_height as usize) * row_stride;
        let copy_len = c_bytes.min(self.content_buffer.len());
        if copy_len > 0 && dest_offset + copy_len <= result.pixels.len() {
            result.pixels[dest_offset..dest_offset + copy_len]
                .copy_from_slice(&self.content_buffer[..copy_len]);
            dest_offset += c_bytes;
        }

        // 3. Copy sticky footer (from last frame)
        if self.footer_h > 0 {
            let f_bytes = (self.footer_h as usize) * row_stride;
            let last_h = last.height;
            let f_start = ((last_h - self.footer_h) as usize) * row_stride;
            if f_start + f_bytes <= last.pixels.len()
                && dest_offset + f_bytes <= result.pixels.len()
            {
                result.pixels[dest_offset..dest_offset + f_bytes]
                    .copy_from_slice(&last.pixels[f_start..f_start + f_bytes]);
            }
        }

        Ok(result)
    }
}

/// Injects a mouse wheel step at the current cursor position.
pub fn send_mouse_wheel(delta: i32) {
    unsafe {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: delta as u32,
                    dwFlags: MOUSEEVENTF_WHEEL,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Moves the cursor over (center_x, center_y), focuses the target window,
/// and dispatches a Page Down key event to trigger vertical scrolling.
pub fn send_page_down(center_x: i32, center_y: i32) {
    unsafe {
        let _ = SetCursorPos(center_x, center_y);
        let pt = POINT {
            x: center_x,
            y: center_y,
        };
        let target = WindowFromPoint(pt);
        if !target.is_invalid() {
            let _ = SetForegroundWindow(target);
            let _ = PostMessageW(
                Some(target),
                WM_KEYDOWN,
                WPARAM(VK_NEXT.0 as usize),
                LPARAM(0),
            );
            let _ = PostMessageW(
                Some(target),
                WM_KEYUP,
                WPARAM(VK_NEXT.0 as usize),
                LPARAM(0xC0000001),
            );
        }

        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_NEXT,
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let up = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_NEXT,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let _ = SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Dispatches a Page Up key event to restore scroll position after Page Down navigation.
pub fn send_page_up(center_x: i32, center_y: i32) {
    unsafe {
        let _ = SetCursorPos(center_x, center_y);
        let pt = POINT {
            x: center_x,
            y: center_y,
        };
        let target = WindowFromPoint(pt);
        if !target.is_invalid() {
            let _ = PostMessageW(
                Some(target),
                WM_KEYDOWN,
                WPARAM(VK_PRIOR.0 as usize),
                LPARAM(0),
            );
            let _ = PostMessageW(
                Some(target),
                WM_KEYUP,
                WPARAM(VK_PRIOR.0 as usize),
                LPARAM(0xC0000001),
            );
        }

        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_PRIOR,
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let up = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_PRIOR,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let _ = SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Checks whether the user has pressed Escape to cancel or stop.
pub fn is_escape_pressed() -> bool {
    unsafe { (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000) != 0 }
}

/// Floating status and Stop button UI during scrolling capture.
struct PillState {
    stop_flag: Arc<AtomicBool>,
    is_auto: Arc<AtomicBool>,
    current_height: Arc<AtomicU32>,
    scale: f32,
}

thread_local! {
    static PILL_STATE: std::cell::RefCell<Option<PillState>> = const { std::cell::RefCell::new(None) };
}

pub struct FloatingStopPill {
    pub hwnd: HWND,
    stop_flag: Arc<AtomicBool>,
    pub is_auto: Arc<AtomicBool>,
    pub current_height: Arc<AtomicU32>,
    pub scale: f32,
}

unsafe extern "system" fn pill_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let px = (lparam.0 & 0xffff) as i16 as i32;
            let py = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            let pt = PointI::new(px, py);

            let mut clicked_done = false;
            PILL_STATE.with(|st| {
                if let Some(state) = st.borrow().as_ref() {
                    let s = state.scale;
                    let btn_auto = RectI::new(
                        (130.0 * s).round() as i32,
                        (8.0 * s).round() as i32,
                        (220.0 * s).round() as i32,
                        (40.0 * s).round() as i32,
                    );
                    let btn_done = RectI::new(
                        (226.0 * s).round() as i32,
                        (8.0 * s).round() as i32,
                        (312.0 * s).round() as i32,
                        (40.0 * s).round() as i32,
                    );

                    if btn_auto.contains(pt) {
                        let cur = state.is_auto.load(Ordering::Acquire);
                        state.is_auto.store(!cur, Ordering::Release);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if btn_done.contains(pt) {
                        state.stop_flag.store(true, Ordering::Release);
                        clicked_done = true;
                    }
                }
            });
            if clicked_done {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let (scale, is_auto, height) = PILL_STATE.with(|st| {
                if let Some(state) = st.borrow().as_ref() {
                    (
                        state.scale,
                        state.is_auto.load(Ordering::Acquire),
                        state.current_height.load(Ordering::Acquire),
                    )
                } else {
                    (1.0, false, 0)
                }
            });

            let w = (320.0 * scale).round() as u32;
            let h = (48.0 * scale).round() as u32;
            let mut img = ImageBuffer::new(w, h);

            // Background #14171c
            for y in 0..h {
                for x in 0..w {
                    img.set_pixel(x, y, [20, 23, 28, 255]);
                }
            }
            // Border #262b33
            for x in 0..w {
                img.set_pixel(x, 0, [38, 43, 51, 255]);
                img.set_pixel(x, h - 1, [38, 43, 51, 255]);
            }
            for y in 0..h {
                img.set_pixel(0, y, [38, 43, 51, 255]);
                img.set_pixel(w - 1, y, [38, 43, 51, 255]);
            }

            // Status label and height readout
            let font_size = (11.0 * scale).round().max(10.0) as u32;
            let (status_color, status_text) = if is_auto {
                (Color::rgb(52, 211, 153), "● Auto")
            } else {
                (Color::rgb(245, 165, 36), "● Manual")
            };
            draw_system_text_rgba(
                &mut img,
                PointI::new((12.0 * scale).round() as i32, (10.0 * scale).round() as i32),
                status_text,
                status_color,
                font_size,
                true,
            );
            draw_system_text_rgba(
                &mut img,
                PointI::new((12.0 * scale).round() as i32, (26.0 * scale).round() as i32),
                &format!("{} px", height),
                Color::rgb(200, 205, 215),
                font_size,
                false,
            );

            // Auto button box
            let btn_auto = RectI::new(
                (130.0 * scale).round() as i32,
                (8.0 * scale).round() as i32,
                (220.0 * scale).round() as i32,
                (40.0 * scale).round() as i32,
            );
            for by in btn_auto.top..btn_auto.bottom {
                for bx in btn_auto.left..btn_auto.right {
                    if bx >= 0 && bx < w as i32 && by >= 0 && by < h as i32 {
                        let col = if is_auto {
                            [30, 41, 59, 255]
                        } else {
                            [38, 43, 51, 255]
                        };
                        img.set_pixel(bx as u32, by as u32, col);
                    }
                }
            }
            let auto_label = if is_auto {
                "⏸ Manual"
            } else {
                "▶ Auto scroll"
            };
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    btn_auto.left + (6.0 * scale).round() as i32,
                    btn_auto.top + (7.0 * scale).round() as i32,
                ),
                auto_label,
                if is_auto {
                    Color::rgb(52, 211, 153)
                } else {
                    Color::rgb(230, 233, 238)
                },
                font_size,
                true,
            );

            // Done button box [ ✓ Done (Esc) ]
            let btn_done = RectI::new(
                (226.0 * scale).round() as i32,
                (8.0 * scale).round() as i32,
                (312.0 * scale).round() as i32,
                (40.0 * scale).round() as i32,
            );
            for by in btn_done.top..btn_done.bottom {
                for bx in btn_done.left..btn_done.right {
                    if bx >= 0 && bx < w as i32 && by >= 0 && by < h as i32 {
                        img.set_pixel(bx as u32, by as u32, [16, 149, 106, 255]);
                        // Emerald green
                    }
                }
            }
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    btn_done.left + (8.0 * scale).round() as i32,
                    btn_done.top + (7.0 * scale).round() as i32,
                ),
                "✓ Done (Esc)",
                Color::WHITE,
                font_size,
                true,
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
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

impl FloatingStopPill {
    pub fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::Acquire)
    }

    pub fn show(rect: RectI, stop_flag: Arc<AtomicBool>) -> Option<Self> {
        Self::show_scaled(
            rect,
            stop_flag,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU32::new(rect.height() as u32)),
            1.0,
        )
    }

    pub fn show_scaled(
        rect: RectI,
        stop_flag: Arc<AtomicBool>,
        is_auto: Arc<AtomicBool>,
        current_height: Arc<AtomicU32>,
        scale: f32,
    ) -> Option<Self> {
        PILL_STATE.with(|st| {
            *st.borrow_mut() = Some(PillState {
                stop_flag: Arc::clone(&stop_flag),
                is_auto: Arc::clone(&is_auto),
                current_height: Arc::clone(&current_height),
                scale,
            });
        });

        unsafe {
            let hinst = GetModuleHandleW(None).ok()?;
            let class_name = w!("LookoutScrollingStopPill");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(pill_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let pill_w = (320.0 * scale).round() as i32;
            let pill_h = (48.0 * scale).round() as i32;
            let scr_w = GetSystemMetrics(SM_CXSCREEN);
            let scr_h = GetSystemMetrics(SM_CYSCREEN);

            // Position pill outside the capture rect so it never blocks captured pixels
            let x =
                ((rect.left + rect.right - pill_w) / 2).clamp(10, (scr_w - pill_w - 10).max(10));
            let y = if rect.bottom + pill_h + 16 < scr_h {
                rect.bottom + 10
            } else if rect.top - pill_h - 16 > 0 {
                rect.top - pill_h - 10
            } else {
                (scr_h - pill_h - 20).max(10)
            };

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("Lookout Scrolling Stop"),
                WS_POPUP,
                x,
                y,
                pill_w,
                pill_h,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .ok()?;

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            Some(Self {
                hwnd,
                stop_flag,
                is_auto,
                current_height,
                scale,
            })
        }
    }

    #[cfg(feature = "review")]
    pub fn open_real_scrolling_pill_for_demo(scale: f32) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let auto = Arc::new(AtomicBool::new(false));
        let height = Arc::new(AtomicU32::new(1420));
        let scr_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let scr_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        let center_rect = RectI::new(scr_w / 4, scr_h / 4, scr_w * 3 / 4, scr_h * 3 / 4);
        Self::show_scaled(center_rect, stop, auto, height, scale)
    }

    pub fn pump_messages(&self) {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, Some(self.hwnd), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn unhide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn close(self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Displays a toast when reaching the hard limit of 30,000 px.
pub fn show_scrolling_limit_toast() {
    thread::Builder::new()
        .name("lookout-scrolling-limit-toast".to_string())
        .spawn(move || unsafe {
            let hinst = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(_) => return,
            };
            let class_name = w!("LookoutScrollingLimitToast");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(limit_toast_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let scr_w = GetSystemMetrics(SM_CXSCREEN);
            let scr_h = GetSystemMetrics(SM_CYSCREEN);
            let toast_w = 460;
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

unsafe extern "system" fn limit_toast_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = SetTimer(Some(hwnd), 1, 4000, None);
            LRESULT(0)
        }
        WM_TIMER | WM_LBUTTONDOWN => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let w = 460u32;
            let h = 64u32;
            let mut img = ImageBuffer::new(w, h);

            for y in 0..h {
                for x in 0..w {
                    img.set_pixel(x, y, [20, 23, 28, 255]);
                }
            }
            for x in 0..w {
                img.set_pixel(x, 0, [38, 43, 51, 255]);
                img.set_pixel(x, h - 1, [38, 43, 51, 255]);
            }
            for y in 0..h {
                img.set_pixel(0, y, [38, 43, 51, 255]);
                img.set_pixel(w - 1, y, [38, 43, 51, 255]);
            }

            draw_system_text_rgba(
                &mut img,
                PointI::new(14, 12),
                "SCROLLING CAPTURE LIMIT",
                Color::rgb(245, 165, 36),
                11,
                true,
            );
            draw_system_text_rgba(
                &mut img,
                PointI::new(14, 32),
                "Reached maximum scrolling height limit (30,000 px).",
                Color::rgb(230, 233, 238),
                11,
                false,
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

/// Displays a toast when the target window does not scroll after wheel and Page Down fallback.
pub fn show_scrolling_not_scrolled_toast() {
    thread::Builder::new()
        .name("lookout-scrolling-notice-toast".to_string())
        .spawn(move || unsafe {
            let hinst = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(_) => return,
            };
            let class_name = w!("LookoutScrollingNoticeToast");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(not_scrolled_toast_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let scr_w = GetSystemMetrics(SM_CXSCREEN);
            let scr_h = GetSystemMetrics(SM_CYSCREEN);
            let toast_w = 540;
            let toast_h = 64;
            let x = (scr_w - toast_w - 24).max(0);
            let y = (scr_h - toast_h - 48).max(0);

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("Lookout Scrolling Notice"),
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

unsafe extern "system" fn not_scrolled_toast_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
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

            let w = 540u32;
            let h = 64u32;
            let mut img = ImageBuffer::new(w, h);

            for y in 0..h {
                for x in 0..w {
                    img.set_pixel(x, y, [20, 23, 28, 255]);
                }
            }
            for x in 0..w {
                img.set_pixel(x, 0, [38, 43, 51, 255]);
                img.set_pixel(x, h - 1, [38, 43, 51, 255]);
            }
            for y in 0..h {
                img.set_pixel(0, y, [38, 43, 51, 255]);
                img.set_pixel(w - 1, y, [38, 43, 51, 255]);
            }

            draw_system_text_rgba(
                &mut img,
                PointI::new(14, 12),
                "SCROLLING CAPTURE",
                Color::rgb(245, 165, 36),
                11,
                true,
            );
            draw_system_text_rgba(
                &mut img,
                PointI::new(14, 32),
                "This window did not scroll. Click inside it first, or try Page Down mode.",
                Color::rgb(230, 233, 238),
                11,
                false,
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

/// Waits for a scrolling window to settle until two consecutive probed frames are identical,
/// or until timeout expires. Handles smooth scrolling animation easing and inertial curves.
pub fn wait_for_scroll_settle(
    rect: RectI,
    timeout: Duration,
    pill: Option<&FloatingStopPill>,
) -> Result<ImageBuffer, String> {
    let start = std::time::Instant::now();
    let sample_interval = Duration::from_millis(35);

    // Initial pause to allow window to process WM_MOUSEWHEEL and initiate scrolling animation
    thread::sleep(Duration::from_millis(45));

    if let Some(p) = pill {
        p.hide();
    }
    let mut prev_frame = capture_gdi_rect(
        rect.left,
        rect.top,
        rect.width() as u32,
        rect.height() as u32,
    )?;
    if let Some(p) = pill {
        p.unhide();
        p.pump_messages();
    }

    while start.elapsed() < timeout {
        thread::sleep(sample_interval);
        if is_escape_pressed() {
            break;
        }
        if let Some(p) = pill {
            if p.is_stopped() {
                break;
            }
            p.hide();
        }
        let cur_frame = capture_gdi_rect(
            rect.left,
            rect.top,
            rect.width() as u32,
            rect.height() as u32,
        )?;
        if let Some(p) = pill {
            p.unhide();
            p.pump_messages();
        }

        if is_frames_identical(&prev_frame, &cur_frame) {
            // Window has finished moving and settled!
            return Ok(cur_frame);
        }
        prev_frame = cur_frame;
    }

    Ok(prev_frame)
}

/// Runs the interactive scrolling capture session over the specified screen rect.
/// Sends mouse wheel steps, stitches frames, checks stop conditions, and restores scroll position.
pub fn execute_scrolling_capture(rect: RectI) -> Result<ImageBuffer, String> {
    let w = rect.width() as u32;
    let h = rect.height() as u32;

    if w < 16 || h < 16 {
        return Err("Selected area is too small for scrolling capture".to_string());
    }

    // Save initial cursor position to restore later
    let mut orig_cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut orig_cursor);
    }

    let center_x = (rect.left + rect.right) / 2;
    let center_y = (rect.top + rect.bottom) / 2;

    // Position mouse inside the target scroll area
    unsafe {
        let _ = SetCursorPos(center_x, center_y);
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    let is_auto = Arc::new(AtomicBool::new(false));
    let current_height = Arc::new(AtomicU32::new(h));

    let dpi = unsafe {
        let pt = POINT {
            x: center_x,
            y: center_y,
        };
        let target = WindowFromPoint(pt);
        if !target.is_invalid() {
            windows::Win32::UI::HiDpi::GetDpiForWindow(target)
        } else {
            96
        }
    };
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };

    let pill = FloatingStopPill::show_scaled(
        rect,
        Arc::clone(&stop_flag),
        Arc::clone(&is_auto),
        Arc::clone(&current_height),
        scale,
    );

    let mut stitcher = ScrollingStitcher::new(MAX_SCROLLING_HEIGHT);

    // Initial frame acquisition
    if let Some(p) = pill.as_ref() {
        p.hide();
    }
    let first_frame = capture_gdi_rect(rect.left, rect.top, w, h)?;
    if let Some(p) = pill.as_ref() {
        p.unhide();
    }
    stitcher.init(first_frame);

    let mut total_wheel_steps = 0;
    let mut total_page_down_steps = 0;
    let mut use_page_down_mode = false;
    let mut is_first_step = true;
    let wheel_delta = -(WHEEL_DELTA as i32); // Wheel down

    while !stop_flag.load(Ordering::Acquire) {
        if is_escape_pressed() {
            break;
        }

        if let Some(p) = pill.as_ref() {
            p.pump_messages();
        }

        if is_auto.load(Ordering::Acquire) {
            // Optional automated wheel stepping
            if use_page_down_mode {
                send_page_down(center_x, center_y);
                total_page_down_steps += 1;
            } else {
                send_mouse_wheel(wheel_delta);
                total_wheel_steps += 1;
            }

            if is_escape_pressed() || stop_flag.load(Ordering::Acquire) {
                break;
            }

            // Settle check with 450ms timeout for smooth-scrolling animation
            let frame =
                match wait_for_scroll_settle(rect, Duration::from_millis(450), pill.as_ref()) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("Scrolling frame capture error: {e}");
                        break;
                    }
                };

            if is_escape_pressed() || stop_flag.load(Ordering::Acquire) {
                break;
            }

            // If the first scroll step produces an identical frame, attempt Page Down fallback automatically
            let is_identical_to_last = stitcher
                .last_frame
                .as_ref()
                .is_some_and(|last| is_frames_identical(last, &frame));

            if is_first_step && is_identical_to_last && !use_page_down_mode {
                is_first_step = false;
                send_page_down(center_x, center_y);
                total_page_down_steps += 1;

                if is_escape_pressed() || stop_flag.load(Ordering::Acquire) {
                    break;
                }

                let pgdn_frame =
                    match wait_for_scroll_settle(rect, Duration::from_millis(450), pill.as_ref()) {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("Scrolling frame capture error: {e}");
                            break;
                        }
                    };

                let pgdn_identical = stitcher
                    .last_frame
                    .as_ref()
                    .is_some_and(|last| is_frames_identical(last, &pgdn_frame));

                if pgdn_identical {
                    show_scrolling_not_scrolled_toast();
                    break;
                }

                use_page_down_mode = true;
                match stitcher.add_frame(&pgdn_frame) {
                    StitchStepResult::Added { total_height, .. }
                    | StitchStepResult::ScrolledUp { total_height, .. } => {
                        current_height.store(total_height, Ordering::Release);
                        if let Some(p) = pill.as_ref() {
                            let _ = unsafe { InvalidateRect(Some(p.hwnd), None, false) };
                        }
                    }
                    StitchStepResult::EndOfPage => break,
                    StitchStepResult::ReachedMaxHeight => {
                        show_scrolling_limit_toast();
                        break;
                    }
                    StitchStepResult::NoMatch => break,
                }
                continue;
            }
            is_first_step = false;

            match stitcher.add_frame(&frame) {
                StitchStepResult::Added { total_height, .. }
                | StitchStepResult::ScrolledUp { total_height, .. } => {
                    current_height.store(total_height, Ordering::Release);
                    if let Some(p) = pill.as_ref() {
                        let _ = unsafe { InvalidateRect(Some(p.hwnd), None, false) };
                    }
                }
                StitchStepResult::EndOfPage => {
                    break;
                }
                StitchStepResult::ReachedMaxHeight => {
                    show_scrolling_limit_toast();
                    break;
                }
                StitchStepResult::NoMatch => {
                    break;
                }
            }
        } else {
            // Manual scrolling mode (Default)
            // Poll at ~10 fps while user navigates naturally
            thread::sleep(Duration::from_millis(90));
            if is_escape_pressed() || stop_flag.load(Ordering::Acquire) {
                break;
            }

            if let Some(p) = pill.as_ref() {
                p.pump_messages();
            }

            if let Some(p) = pill.as_ref() {
                p.hide();
            }
            let probe_frame = match capture_gdi_rect(rect.left, rect.top, w, h) {
                Ok(f) => f,
                Err(_) => {
                    if let Some(p) = pill.as_ref() {
                        p.unhide();
                        p.pump_messages();
                    }
                    continue;
                }
            };
            if let Some(p) = pill.as_ref() {
                p.unhide();
                p.pump_messages();
            }

            let changed = stitcher
                .last_frame
                .as_ref()
                .is_some_and(|last| !is_frames_identical(last, &probe_frame));

            if changed {
                // Window moved! Wait for settle
                let settled_frame =
                    match wait_for_scroll_settle(rect, Duration::from_millis(350), pill.as_ref()) {
                        Ok(f) => f,
                        Err(_) => probe_frame,
                    };

                match stitcher.add_frame(&settled_frame) {
                    StitchStepResult::Added { total_height, .. }
                    | StitchStepResult::ScrolledUp { total_height, .. } => {
                        current_height.store(total_height, Ordering::Release);
                        if let Some(p) = pill.as_ref() {
                            let _ = unsafe { InvalidateRect(Some(p.hwnd), None, false) };
                        }
                    }
                    StitchStepResult::EndOfPage => break,
                    StitchStepResult::ReachedMaxHeight => {
                        show_scrolling_limit_toast();
                        break;
                    }
                    StitchStepResult::NoMatch => {}
                }
            }
        }
    }

    if let Some(p) = pill {
        p.close();
    }

    // Restore target window scroll position
    if use_page_down_mode && total_page_down_steps > 0 {
        for _ in 0..total_page_down_steps {
            send_page_up(center_x, center_y);
            thread::sleep(Duration::from_millis(15));
        }
    } else if total_wheel_steps > 0 {
        let reverse_delta = WHEEL_DELTA as i32; // Wheel up
        for _ in 0..total_wheel_steps {
            send_mouse_wheel(reverse_delta);
            thread::sleep(Duration::from_millis(10));
        }
    }

    // Restore original mouse cursor position
    unsafe {
        let _ = SetCursorPos(orig_cursor.x, orig_cursor.y);
    }

    stitcher.finish()
}
