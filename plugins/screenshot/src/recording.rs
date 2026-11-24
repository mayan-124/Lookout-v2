//! Screen recording engine: Windows Graphics Capture / GDI BitBlt acquisition,
//! Media Foundation H.264 MP4 SinkWriter (hardware encoding with software fallback),
//! floating control bar excluded from capture, click highlight ring,
//! post-recording interactive result card, hard caps, and pure-Rust GIF export.

use std::fs::File;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaBuffer, IMFMediaType, IMFSample, IMFSinkWriter, MFCreateAttributes,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL,
    MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_H264, MFVideoFormat_RGB32,
    MFVideoInterlace_Progressive, MFSTARTUP_NOSOCKET, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, DESKTOP_CONTROL_FLAGS,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, VK_ESCAPE, VK_LBUTTON,
};
use windows::Win32::UI::Shell::{
    SHFileOperationW, ShellExecuteW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FO_DELETE,
    SHFILEOPSTRUCTW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, IsWindow, PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW,
    SetTimer, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, HTCAPTION, MSG,
    SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOWNOACTIVATE,
    WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_LBUTTONDOWN, WM_NCLBUTTONDOWN,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::annotation::{
    draw_system_text_rgba, fill_rounded_rect_rgba, stroke_rounded_rect_rgba, Color,
};
use crate::capture::capture_gdi_rect_with_cursor;
use crate::clipboard::copy_text_to_clipboard;
use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;
use crate::save::{current_timestamp_recording_string, next_available_named_path};

pub const MAX_RECORDING_DURATION_SECS: u64 = 3600; // 60 minutes hard cap
pub const WARN_RECORDING_DURATION_SECS: u64 = 3300; // 55 minutes warning
pub const MAX_RECORDING_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GB hard cap
pub const WARN_RECORDING_FILE_BYTES: u64 = 1_900_000_000; // 1.9 GB warning

#[inline]
pub fn mf_set_attribute_size(
    target: &IMFMediaType,
    guid: &windows::core::GUID,
    width: u32,
    height: u32,
) -> windows::core::Result<()> {
    unsafe { target.SetUINT64(guid, ((width as u64) << 32) | (height as u64)) }
}

#[inline]
pub fn mf_set_attribute_ratio(
    target: &IMFMediaType,
    guid: &windows::core::GUID,
    numerator: u32,
    denominator: u32,
) -> windows::core::Result<()> {
    unsafe { target.SetUINT64(guid, ((numerator as u64) << 32) | (denominator as u64)) }
}

/// Checks whether the Windows workstation is currently locked.
pub fn is_workstation_locked() -> bool {
    unsafe {
        // First check if current thread can access default desktop
        let default_desk = windows::Win32::System::StationsAndDesktops::OpenDesktopW(
            w!("Default"),
            DESKTOP_CONTROL_FLAGS(0),
            false,
            windows::Win32::System::StationsAndDesktops::DESKTOP_READOBJECTS.0,
        );
        match default_desk {
            Ok(def_h) => {
                let _ = CloseDesktop(def_h);
                // When we have desktop access, test opening the current input desktop
                let desktop = OpenInputDesktop(
                    DESKTOP_CONTROL_FLAGS(0),
                    false,
                    windows::Win32::System::StationsAndDesktops::DESKTOP_READOBJECTS,
                );
                match desktop {
                    Ok(h) => {
                        let _ = CloseDesktop(h);
                        false
                    }
                    Err(_) => true,
                }
            }
            Err(_) => false,
        }
    }
}

/// Statistics and metadata from a completed screen recording session.
#[derive(Debug, Clone)]
pub struct RecordingResult {
    pub path: PathBuf,
    pub duration_secs: f64,
    pub width: u32,
    pub height: u32,
    pub file_size_bytes: u64,
    pub frame_count: usize,
    pub dropped_frames: usize,
    pub fps: u32,
}

/// Windows Media Foundation H.264 MP4 SinkWriter wrapper.
/// Supports hardware-accelerated transforms with automatic software fallback.
pub struct Mp4Encoder {
    writer: Option<IMFSinkWriter>,
    stream_index: u32,
    width: u32,
    height: u32,
    fps: u32,
}

unsafe impl Send for Mp4Encoder {}

impl Mp4Encoder {
    pub fn create(out_path: &Path, width: u32, height: u32, fps: u32) -> Result<Self, String> {
        // Round dimensions up to even numbers required by H.264
        let w = (width + 1) & !1;
        let h = (height + 1) & !1;

        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)
                .map_err(|e| format!("MFStartup failed: {e}"))?;

            let wide_path: Vec<u16> = out_path
                .to_str()
                .ok_or_else(|| "Invalid output path".to_string())?
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            // Attempt hardware encoder first; fallback to software on any setup failure
            let (writer, stream_index) = match Self::init_sink_writer(&wide_path, w, h, fps, true) {
                Ok(res) => res,
                Err(_hw_err) => {
                    Self::init_sink_writer(&wide_path, w, h, fps, false).map_err(|sw_err| {
                        format!("Encoder initialization failed (SW fallback failed: {sw_err})")
                    })?
                }
            };

            Ok(Self {
                writer: Some(writer),
                stream_index,
                width: w,
                height: h,
                fps,
            })
        }
    }

    unsafe fn init_sink_writer(
        wide_path: &[u16],
        w: u32,
        h: u32,
        fps: u32,
        hardware_transforms: bool,
    ) -> Result<(IMFSinkWriter, u32), String> {
        let mut attrs: Option<IMFAttributes> = None;
        if hardware_transforms && MFCreateAttributes(&mut attrs, 1).is_ok() {
            if let Some(ref a) = attrs {
                let _ = a.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1);
            }
        }

        let writer = MFCreateSinkWriterFromURL(PCWSTR(wide_path.as_ptr()), None, attrs.as_ref())
            .map_err(|e| format!("MFCreateSinkWriterFromURL failed: {e}"))?;

        // 1. Output Media Type (H.264 MP4 Video)
        let out_type: IMFMediaType =
            MFCreateMediaType().map_err(|e| format!("Create out_type failed: {e}"))?;
        out_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        out_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(|e| e.to_string())?;

        let bitrate = ((w as u64 * h as u64 * fps as u64 * 2) / 10).clamp(2_000_000, 16_000_000);
        out_type
            .SetUINT32(&MF_MT_AVG_BITRATE, bitrate as u32)
            .map_err(|e| e.to_string())?;
        out_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| e.to_string())?;
        mf_set_attribute_size(&out_type, &MF_MT_FRAME_SIZE, w, h).map_err(|e| e.to_string())?;
        mf_set_attribute_ratio(&out_type, &MF_MT_FRAME_RATE, fps, 1).map_err(|e| e.to_string())?;
        mf_set_attribute_ratio(&out_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)
            .map_err(|e| e.to_string())?;

        let stream_index = writer
            .AddStream(&out_type)
            .map_err(|e| format!("AddStream failed: {e}"))?;

        // 2. Input Media Type (RGB32 / BGRA uncompressed buffer)
        let in_type: IMFMediaType =
            MFCreateMediaType().map_err(|e| format!("Create in_type failed: {e}"))?;
        in_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        in_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| e.to_string())?;
        in_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| e.to_string())?;
        mf_set_attribute_size(&in_type, &MF_MT_FRAME_SIZE, w, h).map_err(|e| e.to_string())?;
        mf_set_attribute_ratio(&in_type, &MF_MT_FRAME_RATE, fps, 1).map_err(|e| e.to_string())?;
        mf_set_attribute_ratio(&in_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)
            .map_err(|e| e.to_string())?;

        writer
            .SetInputMediaType(stream_index, &in_type, None)
            .map_err(|e| format!("SetInputMediaType failed: {e}"))?;

        writer
            .BeginWriting()
            .map_err(|e| format!("BeginWriting failed: {e}"))?;

        Ok((writer, stream_index))
    }

    /// Writes a single frame at the given timestamp (in 100-nanosecond units) with frame duration.
    pub fn write_frame(&self, frame: &ImageBuffer, timestamp_100ns: i64) -> Result<(), String> {
        let byte_len = self.width * self.height * 4;
        let duration_100ns = (10_000_000 / self.fps as i64).max(1);

        unsafe {
            let buffer: IMFMediaBuffer = MFCreateMemoryBuffer(byte_len)
                .map_err(|e| format!("MFCreateMemoryBuffer failed: {e}"))?;

            let mut dst_ptr: *mut u8 = std::ptr::null_mut();
            buffer
                .Lock(&mut dst_ptr, None, None)
                .map_err(|e| format!("Buffer Lock failed: {e}"))?;

            if dst_ptr.is_null() {
                return Err("Buffer lock returned null pointer".to_string());
            }

            // Copy pixels into buffer row by row to match encoder dimensions,
            // swapping RGBA -> BGRA for Media Foundation RGB32.
            // Note: Media Foundation RGB32 SinkWriter expects bottom-up row ordering (standard DIB format).
            // Map frame row (copy_h - 1 - y) to buffer row y so that the output MP4 video is right-side up.
            let dst_slice = std::slice::from_raw_parts_mut(dst_ptr, byte_len as usize);
            let copy_w = self.width.min(frame.width) as usize;
            let copy_h = self.height.min(frame.height) as usize;

            for y in 0..copy_h {
                let src_y = (copy_h - 1 - y) as u32;
                let src_row = frame.row(src_y);
                let dst_start = y * (self.width as usize) * 4;

                for x in 0..copy_w {
                    let s = x * 4;
                    let d = dst_start + s;
                    dst_slice[d] = src_row[s + 2]; // B
                    dst_slice[d + 1] = src_row[s + 1]; // G
                    dst_slice[d + 2] = src_row[s]; // R
                    dst_slice[d + 3] = 255; // A
                }
            }

            let _ = buffer.Unlock();
            buffer
                .SetCurrentLength(byte_len)
                .map_err(|e| format!("SetCurrentLength failed: {e}"))?;

            let sample: IMFSample =
                MFCreateSample().map_err(|e| format!("MFCreateSample failed: {e}"))?;
            sample
                .AddBuffer(&buffer)
                .map_err(|e| format!("AddBuffer failed: {e}"))?;
            sample
                .SetSampleTime(timestamp_100ns)
                .map_err(|e| format!("SetSampleTime failed: {e}"))?;
            sample
                .SetSampleDuration(duration_100ns)
                .map_err(|e| format!("SetSampleDuration failed: {e}"))?;

            if let Some(ref w) = self.writer {
                w.WriteSample(self.stream_index, &sample)
                    .map_err(|e| format!("WriteSample failed: {e}"))?;
            }
        }

        Ok(())
    }

    /// Finalizes the MP4 container and shuts down the SinkWriter.
    pub fn finalize(mut self) -> Result<(), String> {
        unsafe {
            if let Some(w) = self.writer.take() {
                w.Finalize().map_err(|e| format!("Finalize failed: {e}"))?;
                drop(w);
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
        Ok(())
    }
}

impl Drop for Mp4Encoder {
    fn drop(&mut self) {
        if let Some(w) = self.writer.take() {
            unsafe {
                let _ = w.Finalize();
                drop(w);
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }
}

/// Blends a single translucent RGBA pixel over an existing pixel in ImageBuffer.
#[inline]
pub fn blend_pixel_rgba(img: &mut ImageBuffer, x: u32, y: u32, color: Color) {
    if x >= img.width || y >= img.height || color.a == 0 {
        return;
    }
    let idx = ((y as usize) * (img.width as usize) + (x as usize)) * 4;
    let inv_a = 255 - color.a as u32;
    let a = color.a as u32;

    img.pixels[idx] = ((img.pixels[idx] as u32 * inv_a + color.r as u32 * a) / 255) as u8;
    img.pixels[idx + 1] = ((img.pixels[idx + 1] as u32 * inv_a + color.g as u32 * a) / 255) as u8;
    img.pixels[idx + 2] = ((img.pixels[idx + 2] as u32 * inv_a + color.b as u32 * a) / 255) as u8;
}

/// Draws an expanding soft translucent ring around cursor clicks.
pub fn draw_click_ring(img: &mut ImageBuffer, center: PointI, radius: f32, color: Color) {
    let r_int = (radius + 2.0).ceil() as i32;
    let w = img.width as i32;
    let h = img.height as i32;

    for dy in -r_int..=r_int {
        let py = center.y + dy;
        if py < 0 || py >= h {
            continue;
        }
        for dx in -r_int..=r_int {
            let px = center.x + dx;
            if px < 0 || px >= w {
                continue;
            }
            let dist = ((dx * dx + dy * dy) as f32).sqrt();
            let dist_diff = (dist - radius).abs();
            if dist_diff <= 1.75 {
                let edge_alpha = (1.0 - (dist_diff / 1.75)) * (color.a as f32 / 255.0);
                let alpha_u8 = (edge_alpha * 255.0).clamp(0.0, 255.0) as u8;
                let ring_col = Color::rgba(color.r, color.g, color.b, alpha_u8);
                blend_pixel_rgba(img, px as u32, py as u32, ring_col);
            }
        }
    }
}

/// DPI layout calculation for the floating recording control bar.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingBarLayout {
    pub width: u32,
    pub height: u32,
    pub timer_pos: PointI,
    pub timer_font_size: u32,
    pub dot_center: PointI,
    pub dot_radius: i32,
    pub pause_btn: RectI,
    pub cursor_btn: RectI,
    pub ring_btn: RectI,
    pub stop_btn: RectI,
}

pub fn compute_recording_bar_layout(scale: f32) -> RecordingBarLayout {
    let s = scale.max(0.5);
    let width = (480.0 * s).round() as u32;
    let height = (48.0 * s).round() as u32;

    let dot_center = PointI::new((18.0 * s).round() as i32, (24.0 * s).round() as i32);
    let dot_radius = (5.0 * s).round() as i32;

    let timer_pos = PointI::new((30.0 * s).round() as i32, (15.0 * s).round() as i32);
    let timer_font_size = (13.0 * s).round() as u32;

    let btn_y1 = (8.0 * s).round() as i32;
    let btn_y2 = (40.0 * s).round() as i32;

    let pause_btn = RectI::new(
        (104.0 * s).round() as i32,
        btn_y1,
        (188.0 * s).round() as i32,
        btn_y2,
    );
    let cursor_btn = RectI::new(
        (194.0 * s).round() as i32,
        btn_y1,
        (278.0 * s).round() as i32,
        btn_y2,
    );
    let ring_btn = RectI::new(
        (284.0 * s).round() as i32,
        btn_y1,
        (396.0 * s).round() as i32,
        btn_y2,
    );
    let stop_btn = RectI::new(
        (402.0 * s).round() as i32,
        btn_y1,
        (470.0 * s).round() as i32,
        btn_y2,
    );

    RecordingBarLayout {
        width,
        height,
        timer_pos,
        timer_font_size,
        dot_center,
        dot_radius,
        pause_btn,
        cursor_btn,
        ring_btn,
        stop_btn,
    }
}

/// DPI layout calculation for the post-recording interactive result card.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultCardLayout {
    pub width: u32,
    pub height: u32,
    pub header_pos: PointI,
    pub title_pos: PointI,
    pub subtitle_pos: PointI,
    pub close_btn: RectI,
    pub copy_btn: RectI,
    pub open_folder_btn: RectI,
    pub export_gif_btn: RectI,
    pub delete_btn: RectI,
}

pub fn compute_result_card_layout(scale: f32) -> ResultCardLayout {
    let s = scale.max(0.5);
    let width = (440.0 * s).round() as u32;
    let height = (142.0 * s).round() as u32;

    let header_pos = PointI::new((16.0 * s).round() as i32, (14.0 * s).round() as i32);
    let title_pos = PointI::new((16.0 * s).round() as i32, (34.0 * s).round() as i32);
    let subtitle_pos = PointI::new((16.0 * s).round() as i32, (58.0 * s).round() as i32);

    let close_btn = RectI::new(
        (406.0 * s).round() as i32,
        (10.0 * s).round() as i32,
        (430.0 * s).round() as i32,
        (34.0 * s).round() as i32,
    );

    let btn_y1 = (94.0 * s).round() as i32;
    let btn_y2 = (128.0 * s).round() as i32;

    let copy_btn = RectI::new(
        (16.0 * s).round() as i32,
        btn_y1,
        (114.0 * s).round() as i32,
        btn_y2,
    );
    let open_folder_btn = RectI::new(
        (120.0 * s).round() as i32,
        btn_y1,
        (220.0 * s).round() as i32,
        btn_y2,
    );
    let export_gif_btn = RectI::new(
        (226.0 * s).round() as i32,
        btn_y1,
        (326.0 * s).round() as i32,
        btn_y2,
    );
    let delete_btn = RectI::new(
        (332.0 * s).round() as i32,
        btn_y1,
        (424.0 * s).round() as i32,
        btn_y2,
    );

    ResultCardLayout {
        width,
        height,
        header_pos,
        title_pos,
        subtitle_pos,
        close_btn,
        copy_btn,
        open_folder_btn,
        export_gif_btn,
        delete_btn,
    }
}

/// Shared atomic state connecting the recording capture loop and the floating control bar.
#[derive(Debug)]
pub struct RecordingSharedState {
    pub is_paused: AtomicBool,
    pub stop_requested: AtomicBool,
    pub record_cursor: AtomicBool,
    pub click_highlight: AtomicBool,
    pub active_nanos: AtomicU64,
    pub dropped_frames: AtomicUsize,
    pub total_frames: AtomicUsize,
    pub file_bytes: AtomicU64,
    pub warn_limit_shown: AtomicBool,
    pub bar_hwnd: std::sync::atomic::AtomicIsize,
    pub fps: u32,
    pub custom_scale: Option<f32>,
}

impl RecordingSharedState {
    pub fn new(fps: u32, record_cursor: bool) -> Self {
        Self {
            is_paused: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            record_cursor: AtomicBool::new(record_cursor),
            click_highlight: AtomicBool::new(true),
            active_nanos: AtomicU64::new(0),
            dropped_frames: AtomicUsize::new(0),
            total_frames: AtomicUsize::new(0),
            file_bytes: AtomicU64::new(0),
            warn_limit_shown: AtomicBool::new(false),
            bar_hwnd: std::sync::atomic::AtomicIsize::new(0),
            fps,
            custom_scale: None,
        }
    }
}

thread_local! {
    static CONTROL_BAR_STATE: std::cell::RefCell<Option<Arc<RecordingSharedState>>> = const { std::cell::RefCell::new(None) };
}

fn get_control_bar_scale(hwnd: HWND) -> f32 {
    let custom = CONTROL_BAR_STATE.with(|s| s.borrow().as_ref().and_then(|st| st.custom_scale));
    if let Some(s) = custom {
        return s;
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0f32
    } else {
        dpi as f32 / 96.0
    }
}

unsafe extern "system" fn recording_bar_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let scale = get_control_bar_scale(hwnd);
            let layout = compute_recording_bar_layout(scale);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                layout.width as i32,
                layout.height as i32,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = SetTimer(Some(hwnd), 1, 100, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if let Some(state) = CONTROL_BAR_STATE.with(|s| s.borrow().clone()) {
                if state.stop_requested.load(Ordering::Acquire) {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            let scale = get_control_bar_scale(hwnd);
            let layout = compute_recording_bar_layout(scale);

            let state_opt = CONTROL_BAR_STATE.with(|s| s.borrow().clone());
            if let Some(state) = state_opt {
                if layout.pause_btn.contains(PointI::new(x, y)) {
                    let _ = state.is_paused.fetch_xor(true, Ordering::SeqCst);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                } else if layout.cursor_btn.contains(PointI::new(x, y)) {
                    state.record_cursor.fetch_xor(true, Ordering::SeqCst);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                } else if layout.ring_btn.contains(PointI::new(x, y)) {
                    state.click_highlight.fetch_xor(true, Ordering::SeqCst);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                } else if layout.stop_btn.contains(PointI::new(x, y)) {
                    state.stop_requested.store(true, Ordering::SeqCst);
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }

                // If clicked outside buttons, enable dragging the floating bar
                let _ = ReleaseCapture();
                let _ = SendMessageW(
                    hwnd,
                    WM_NCLBUTTONDOWN,
                    Some(WPARAM(HTCAPTION as usize)),
                    Some(LPARAM(0)),
                );
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let scale = get_control_bar_scale(hwnd);
            let layout = compute_recording_bar_layout(scale);

            let w = layout.width;
            let h = layout.height;
            let mut img = ImageBuffer::new(w, h);

            let state_opt = CONTROL_BAR_STATE.with(|s| s.borrow().clone());
            let (is_paused, record_cur, click_ring, active_secs) = if let Some(ref st) = state_opt {
                (
                    st.is_paused.load(Ordering::Acquire),
                    st.record_cursor.load(Ordering::Acquire),
                    st.click_highlight.load(Ordering::Acquire),
                    st.active_nanos.load(Ordering::Acquire) / 1_000_000_000,
                )
            } else {
                (false, true, true, 0)
            };

            // 1. Dark bar background #14171c, rounded corners
            let bar_rect = RectI::new(0, 0, w as i32, h as i32);
            let radius = (10.0 * scale).round() as i32;
            fill_rounded_rect_rgba(&mut img, bar_rect, radius, Color::rgb(20, 23, 28));
            stroke_rounded_rect_rgba(&mut img, bar_rect, radius, Color::rgb(38, 43, 51));

            // 2. Status pulsing dot
            let dot_x = layout.dot_center.x;
            let dot_y = layout.dot_center.y;
            let dot_r = layout.dot_radius;
            let dot_color = if is_paused {
                Color::rgb(245, 165, 36)
            } else {
                Color::rgb(239, 68, 68)
            };

            for dy in -dot_r..=dot_r {
                for dx in -dot_r..=dot_r {
                    if dx * dx + dy * dy <= dot_r * dot_r {
                        let px = dot_x + dx;
                        let py = dot_y + dy;
                        if px >= 0 && px < w as i32 && py >= 0 && py < h as i32 {
                            img.set_pixel(px as u32, py as u32, dot_color.to_array());
                        }
                    }
                }
            }

            // 3. Timer readout
            let time_str = format!("{:02}:{:02}", active_secs / 60, active_secs % 60);
            draw_system_text_rgba(
                &mut img,
                layout.timer_pos,
                &time_str,
                Color::rgb(230, 233, 238),
                layout.timer_font_size,
                true,
            );

            // 4. Pause / Resume button
            fill_rounded_rect_rgba(
                &mut img,
                layout.pause_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(30, 35, 43),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                layout.pause_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(50, 57, 69),
            );
            let pause_label = if is_paused { "▶ Resume" } else { "⏸ Pause" };
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.pause_btn.left + (12.0 * scale).round() as i32,
                    layout.pause_btn.top + (7.0 * scale).round() as i32,
                ),
                pause_label,
                Color::rgb(230, 233, 238),
                (11.0 * scale).round() as u32,
                true,
            );

            // 5. Cursor toggle button
            fill_rounded_rect_rgba(
                &mut img,
                layout.cursor_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(30, 35, 43),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                layout.cursor_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(50, 57, 69),
            );
            let cur_label = if record_cur {
                "Cursor: ON"
            } else {
                "Cursor: OFF"
            };
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.cursor_btn.left + (8.0 * scale).round() as i32,
                    layout.cursor_btn.top + (7.0 * scale).round() as i32,
                ),
                cur_label,
                Color::rgb(230, 233, 238),
                (10.0 * scale).round() as u32,
                true,
            );

            // 6. Click Ring toggle button
            fill_rounded_rect_rgba(
                &mut img,
                layout.ring_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(30, 35, 43),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                layout.ring_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(50, 57, 69),
            );
            let ring_label = if click_ring {
                "Click ring: ON"
            } else {
                "Click ring: OFF"
            };
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.ring_btn.left + (8.0 * scale).round() as i32,
                    layout.ring_btn.top + (7.0 * scale).round() as i32,
                ),
                ring_label,
                Color::rgb(230, 233, 238),
                (10.0 * scale).round() as u32,
                true,
            );

            // 7. Stop button
            fill_rounded_rect_rgba(
                &mut img,
                layout.stop_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(217, 72, 82),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                layout.stop_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(239, 68, 68),
            );
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.stop_btn.left + (10.0 * scale).round() as i32,
                    layout.stop_btn.top + (7.0 * scale).round() as i32,
                ),
                "⏹ Stop",
                Color::WHITE,
                (11.0 * scale).round() as u32,
                true,
            );

            // Present to window DC
            unsafe {
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
                            for (i, p) in img.pixels.as_chunks::<4>().0.iter().enumerate() {
                                let offset = i * 4;
                                dst[offset] = p[2]; // B
                                dst[offset + 1] = p[1]; // G
                                dst[offset + 2] = p[0]; // R
                                dst[offset + 3] = p[3]; // A
                            }
                            let old_bmp = SelectObject(mem_dc, dib.into());
                            let _ =
                                BitBlt(hdc, 0, 0, w as i32, h as i32, Some(mem_dc), 0, 0, SRCCOPY);
                            let _ = SelectObject(mem_dc, old_bmp);
                            let _ = DeleteObject(dib.into());
                        }
                    }
                    let _ = DeleteDC(mem_dc);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Spawns the floating control bar on a dedicated UI thread, excluded from capture.
pub fn spawn_recording_control_bar(
    shared: Arc<RecordingSharedState>,
    initial_rect: RectI,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("lookout-recording-bar".to_string())
        .spawn(move || unsafe {
            CONTROL_BAR_STATE.with(|s| {
                *s.borrow_mut() = Some(Arc::clone(&shared));
            });

            let hinst = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(_) => return,
            };

            let class_name = w!("LookoutRecordingBar");
            let wc = WNDCLASSW {
                style: windows::Win32::UI::WindowsAndMessaging::CS_HREDRAW
                    | windows::Win32::UI::WindowsAndMessaging::CS_VREDRAW,
                lpfnWndProc: Some(recording_bar_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let center_pt = POINT {
                x: (initial_rect.left + initial_rect.right) / 2,
                y: (initial_rect.top + initial_rect.bottom) / 2,
            };
            let hmon = MonitorFromPoint(center_pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let (work_left, work_top, work_right, work_bottom) =
                if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                    (
                        mi.rcWork.left,
                        mi.rcWork.top,
                        mi.rcWork.right,
                        mi.rcWork.bottom,
                    )
                } else {
                    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                    let vcx = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                    let vcy = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                    (vx, vy, vx + vcx, vy + vcy)
                };

            let bar_w = 450;
            let bar_h = 48;

            let mut x = (initial_rect.left + initial_rect.right - bar_w) / 2;
            let mut y = initial_rect.bottom + 12;

            if y + bar_h > work_bottom - 10 {
                y = (initial_rect.top - bar_h - 12).max(work_top + 10);
            }
            x = x.clamp(
                work_left + 10,
                (work_right - bar_w - 10).max(work_left + 10),
            );
            y = y.clamp(work_top + 10, (work_bottom - bar_h - 10).max(work_top + 10));

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("Lookout Recording Bar"),
                WS_POPUP,
                x,
                y,
                bar_w,
                bar_h,
                None,
                None,
                Some(hinst.into()),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };

            // Explicitly exclude floating control bar from screen capture
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            shared.bar_hwnd.store(hwnd.0 as isize, Ordering::Release);

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if shared.stop_requested.load(Ordering::Acquire) {
                    let _ = DestroyWindow(hwnd);
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            CONTROL_BAR_STATE.with(|s| {
                *s.borrow_mut() = None;
            });
        })
        .expect("Failed to spawn recording control bar thread")
}

// ----------------------------------------------------------------------------
// Result Card UI: Shown after recording stops (Copy file, Open folder, Export GIF, Delete)
// ----------------------------------------------------------------------------

/// Moves a file to the Windows Recycle Bin (enables Undo in Windows Explorer).
pub fn move_file_to_recycle_bin(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("File does not exist".to_string());
    }
    let full_path_buf = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(abs) = std::fs::canonicalize(path) {
        abs
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let path_str = full_path_buf
        .to_str()
        .ok_or_else(|| "Invalid UTF-8 path".to_string())?;
    let clean_str = path_str.strip_prefix(r"\\?\").unwrap_or(path_str);
    let win_path = clean_str.replace('/', "\\");

    let mut wide: Vec<u16> = win_path.encode_utf16().collect();
    wide.push(0);
    wide.push(0);

    unsafe {
        let mut file_op = SHFILEOPSTRUCTW {
            hwnd: HWND(std::ptr::null_mut()),
            wFunc: FO_DELETE,
            pFrom: PCWSTR(wide.as_ptr()),
            pTo: PCWSTR::null(),
            fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT).0 as u16,
            fAnyOperationsAborted: Default::default(),
            hNameMappings: std::ptr::null_mut(),
            lpszProgressTitle: PCWSTR::null(),
        };

        let ret = SHFileOperationW(&mut file_op);
        if ret != 0 {
            return Err(format!("SHFileOperationW failed with code {ret}"));
        }
        if file_op.fAnyOperationsAborted.as_bool() {
            return Err("Operation aborted by user".to_string());
        }
        Ok(())
    }
}

struct ResultCardState {
    result: RecordingResult,
    save_folder: PathBuf,
    sample_frames: Option<Vec<ImageBuffer>>,
    copied_feedback: bool,
    exported_feedback: bool,
    recycled_feedback: bool,
    custom_scale: Option<f32>,
}

thread_local! {
    static RESULT_CARD_STATE: std::cell::RefCell<Option<Arc<std::sync::Mutex<ResultCardState>>>> = const { std::cell::RefCell::new(None) };
}

fn get_result_card_scale(hwnd: HWND) -> f32 {
    let custom = RESULT_CARD_STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|arc| arc.lock().unwrap().custom_scale)
    });
    if let Some(s) = custom {
        return s;
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0f32
    } else {
        dpi as f32 / 96.0
    }
}

unsafe extern "system" fn result_card_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let scale = get_result_card_scale(hwnd);
            let layout = compute_result_card_layout(scale);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                layout.width as i32,
                layout.height as i32,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            // Auto-dismiss after 20 seconds of inactivity
            let _ = SetTimer(Some(hwnd), 1, 20_000, None);
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            let scale = get_result_card_scale(hwnd);
            let layout = compute_result_card_layout(scale);

            let state_arc = RESULT_CARD_STATE.with(|s| s.borrow().clone());
            if let Some(arc) = state_arc {
                let mut state = arc.lock().unwrap();

                if layout.close_btn.contains(PointI::new(x, y)) {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                } else if layout.copy_btn.contains(PointI::new(x, y)) {
                    if let Some(p_str) = state.result.path.to_str() {
                        let _ = copy_text_to_clipboard(p_str);
                        state.copied_feedback = true;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    return LRESULT(0);
                } else if layout.open_folder_btn.contains(PointI::new(x, y)) {
                    let wide_folder: Vec<u16> = state
                        .save_folder
                        .as_os_str()
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect();
                    let _ = ShellExecuteW(
                        None,
                        w!("open"),
                        PCWSTR(wide_folder.as_ptr()),
                        None,
                        None,
                        windows::Win32::UI::WindowsAndMessaging::SW_SHOW,
                    );
                    return LRESULT(0);
                } else if layout.export_gif_btn.contains(PointI::new(x, y)) {
                    if let Some(ref frames) = state.sample_frames {
                        let gif_path = state.result.path.with_extension("gif");
                        let est_bytes =
                            (state.result.width * state.result.height * frames.len() as u32 * 3)
                                / 10;
                        if est_bytes > 25 * 1024 * 1024 {
                            show_recording_limit_toast(
                                "Warning: resulting GIF will be large. Using 800px width.",
                            );
                        }
                        if export_to_gif(frames, 15, 800, &gif_path).is_ok() {
                            state.exported_feedback = true;
                            if let Some(gs) = gif_path.to_str() {
                                let _ = copy_text_to_clipboard(gs);
                            }
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                    return LRESULT(0);
                } else if layout.delete_btn.contains(PointI::new(x, y)) {
                    if !state.recycled_feedback {
                        let _ = move_file_to_recycle_bin(&state.result.path).or_else(|_| {
                            std::fs::remove_file(&state.result.path).map_err(|e| e.to_string())
                        });
                        state.recycled_feedback = true;
                        show_recording_limit_toast(
                            "Moved to Recycle Bin (restore from Recycle Bin or Ctrl+Z in Explorer)",
                        );
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        // Dismiss after showing feedback
                        let _ = SetTimer(Some(hwnd), 1, 1500, None);
                    }
                    return LRESULT(0);
                }

                // Dragging card
                let _ = ReleaseCapture();
                let _ = SendMessageW(
                    hwnd,
                    WM_NCLBUTTONDOWN,
                    Some(WPARAM(HTCAPTION as usize)),
                    Some(LPARAM(0)),
                );
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let scale = get_result_card_scale(hwnd);
            let layout = compute_result_card_layout(scale);

            let w = layout.width;
            let h = layout.height;
            let mut img = ImageBuffer::new(w, h);

            let state_arc = RESULT_CARD_STATE.with(|s| s.borrow().clone());
            let (file_name, dur_secs, size_mb, fps, copied, exported, recycled) =
                if let Some(ref arc) = state_arc {
                    let st = arc.lock().unwrap();
                    let name = st
                        .result
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Recording.mp4")
                        .to_string();
                    let mb = st.result.file_size_bytes as f64 / (1024.0 * 1024.0);
                    (
                        name,
                        st.result.duration_secs as u64,
                        mb,
                        st.result.fps,
                        st.copied_feedback,
                        st.exported_feedback,
                        st.recycled_feedback,
                    )
                } else {
                    ("Recording.mp4".to_string(), 0, 0.0, 30, false, false, false)
                };

            // Background & border
            let card_rect = RectI::new(0, 0, w as i32, h as i32);
            fill_rounded_rect_rgba(
                &mut img,
                card_rect,
                (10.0 * scale).round() as i32,
                Color::rgb(20, 23, 28),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                card_rect,
                (10.0 * scale).round() as i32,
                Color::rgb(38, 43, 51),
            );

            // Header badge
            draw_system_text_rgba(
                &mut img,
                layout.header_pos,
                "SCREEN RECORDING SAVED",
                Color::rgb(245, 165, 36),
                (10.0 * scale).round() as u32,
                true,
            );

            // Close button [✕]
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.close_btn.left + (4.0 * scale).round() as i32,
                    layout.close_btn.top,
                ),
                "✕",
                Color::rgb(156, 163, 175),
                (12.0 * scale).round() as u32,
                true,
            );

            // File title
            draw_system_text_rgba(
                &mut img,
                layout.title_pos,
                &file_name,
                Color::rgb(240, 243, 248),
                (12.0 * scale).round() as u32,
                true,
            );

            // Subtitle stats or recycled notice
            let (stats, stats_color) = if recycled {
                (
                    "Moved to Recycle Bin (restore via Recycle Bin or Explorer)".to_string(),
                    Color::rgb(245, 165, 36),
                )
            } else {
                (
                    format!(
                        "Duration: {:02}:{:02}   Size: {:.1} MB   {} fps",
                        dur_secs / 60,
                        dur_secs % 60,
                        size_mb,
                        fps
                    ),
                    Color::rgb(156, 163, 175),
                )
            };
            draw_system_text_rgba(
                &mut img,
                layout.subtitle_pos,
                &stats,
                stats_color,
                (10.0 * scale).round() as u32,
                false,
            );

            // Button 1: Copy file
            fill_rounded_rect_rgba(
                &mut img,
                layout.copy_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(30, 35, 43),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                layout.copy_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(50, 57, 69),
            );
            let copy_lbl = if copied { "Copied!" } else { "Copy file" };
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.copy_btn.left + (16.0 * scale).round() as i32,
                    layout.copy_btn.top + (7.0 * scale).round() as i32,
                ),
                copy_lbl,
                if copied {
                    Color::rgb(245, 165, 36)
                } else {
                    Color::rgb(230, 233, 238)
                },
                (11.0 * scale).round() as u32,
                true,
            );

            // Button 2: Open folder
            fill_rounded_rect_rgba(
                &mut img,
                layout.open_folder_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(30, 35, 43),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                layout.open_folder_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(50, 57, 69),
            );
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.open_folder_btn.left + (14.0 * scale).round() as i32,
                    layout.open_folder_btn.top + (7.0 * scale).round() as i32,
                ),
                "Open folder",
                Color::rgb(230, 233, 238),
                (11.0 * scale).round() as u32,
                true,
            );

            // Button 3: Export GIF
            fill_rounded_rect_rgba(
                &mut img,
                layout.export_gif_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(30, 35, 43),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                layout.export_gif_btn,
                (6.0 * scale).round() as i32,
                Color::rgb(50, 57, 69),
            );
            let gif_lbl = if exported { "Exported!" } else { "Export GIF" };
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.export_gif_btn.left + (14.0 * scale).round() as i32,
                    layout.export_gif_btn.top + (7.0 * scale).round() as i32,
                ),
                gif_lbl,
                if exported {
                    Color::rgb(245, 165, 36)
                } else {
                    Color::rgb(230, 233, 238)
                },
                (11.0 * scale).round() as u32,
                true,
            );

            // Button 4: Delete
            let (del_bg, del_border, del_lbl, del_x_off) = if recycled {
                (
                    Color::rgb(70, 75, 85),
                    Color::rgb(85, 90, 100),
                    "Moved",
                    22.0,
                )
            } else {
                (
                    Color::rgb(217, 72, 82),
                    Color::rgb(239, 68, 68),
                    "Delete",
                    20.0,
                )
            };
            fill_rounded_rect_rgba(
                &mut img,
                layout.delete_btn,
                (6.0 * scale).round() as i32,
                del_bg,
            );
            stroke_rounded_rect_rgba(
                &mut img,
                layout.delete_btn,
                (6.0 * scale).round() as i32,
                del_border,
            );
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    layout.delete_btn.left + (del_x_off * scale).round() as i32,
                    layout.delete_btn.top + (7.0 * scale).round() as i32,
                ),
                del_lbl,
                Color::WHITE,
                (11.0 * scale).round() as u32,
                true,
            );

            // Present to DC
            unsafe {
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
                            for (i, p) in img.pixels.as_chunks::<4>().0.iter().enumerate() {
                                let offset = i * 4;
                                dst[offset] = p[2];
                                dst[offset + 1] = p[1];
                                dst[offset + 2] = p[0];
                                dst[offset + 3] = p[3];
                            }
                            let old_bmp = SelectObject(mem_dc, dib.into());
                            let _ =
                                BitBlt(hdc, 0, 0, w as i32, h as i32, Some(mem_dc), 0, 0, SRCCOPY);
                            let _ = SelectObject(mem_dc, old_bmp);
                            let _ = DeleteObject(dib.into());
                        }
                    }
                    let _ = DeleteDC(mem_dc);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Shows the post-recording interactive result card.
pub fn show_result_card(
    result: RecordingResult,
    save_folder: PathBuf,
    sample_frames: Option<Vec<ImageBuffer>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("lookout-recording-result-card".to_string())
        .spawn(move || unsafe {
            let state = Arc::new(std::sync::Mutex::new(ResultCardState {
                result,
                save_folder,
                sample_frames,
                copied_feedback: false,
                exported_feedback: false,
                recycled_feedback: false,
                custom_scale: None,
            }));

            RESULT_CARD_STATE.with(|s| {
                *s.borrow_mut() = Some(Arc::clone(&state));
            });

            let hinst = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(_) => return,
            };

            let class_name = w!("LookoutRecordingResultCard");
            let wc = WNDCLASSW {
                style: windows::Win32::UI::WindowsAndMessaging::CS_HREDRAW
                    | windows::Win32::UI::WindowsAndMessaging::CS_VREDRAW,
                lpfnWndProc: Some(result_card_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let mut cur_pt = POINT::default();
            let _ = GetCursorPos(&mut cur_pt);
            let hmon = MonitorFromPoint(cur_pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let (work_left, work_top, work_right, work_bottom) =
                if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                    (
                        mi.rcWork.left,
                        mi.rcWork.top,
                        mi.rcWork.right,
                        mi.rcWork.bottom,
                    )
                } else {
                    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                    let vcx = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                    let vcy = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                    (vx, vy, vx + vcx, vy + vcy)
                };

            let card_w = 440;
            let card_h = 142;

            let x = (work_right - card_w - 24).max(work_left + 10);
            let y = (work_bottom - card_h - 48).max(work_top + 10);

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("Lookout Recording Result"),
                WS_POPUP,
                x,
                y,
                card_w,
                card_h,
                None,
                None,
                Some(hinst.into()),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };

            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            RESULT_CARD_STATE.with(|s| {
                *s.borrow_mut() = None;
            });
        })
        .expect("Failed to spawn result card thread")
}

// ----------------------------------------------------------------------------
// Screen Recording Execution
// ----------------------------------------------------------------------------

/// Executes a complete screen recording session over the specified screen rectangle.
pub fn execute_screen_recording(
    rect: RectI,
    fps: u32,
    initial_record_cursor: bool,
    save_dir: &Path,
) -> Result<RecordingResult, String> {
    execute_screen_recording_with_options(
        rect,
        fps,
        initial_record_cursor,
        save_dir,
        None,
        None,
        true,
    )
}

/// Executes a complete screen recording session with safety controls:
/// monitoring target window existence, plugin paused flag, and workstation lock status.
pub fn execute_screen_recording_with_options(
    rect: RectI,
    fps: u32,
    initial_record_cursor: bool,
    save_dir: &Path,
    target_hwnd: Option<HWND>,
    paused_flag: Option<Arc<AtomicBool>>,
    show_result_card_ui: bool,
) -> Result<RecordingResult, String> {
    if rect.width() < 16 || rect.height() < 16 {
        return Err("Recorded area must be at least 16x16 pixels".to_string());
    }

    if let Err(e) = std::fs::create_dir_all(save_dir) {
        return Err(format!("Could not create save folder: {e}"));
    }

    let timestamp = current_timestamp_recording_string();
    let mp4_path = next_available_named_path(save_dir, "Recording", &timestamp, "mp4");

    let width = rect.width() as u32;
    let height = rect.height() as u32;

    let encoder = Mp4Encoder::create(&mp4_path, width, height, fps)?;

    let shared = Arc::new(RecordingSharedState::new(fps, initial_record_cursor));
    let bar_handle = spawn_recording_control_bar(Arc::clone(&shared), rect);

    // Bounded frame channel (capacity 2) to drop frames under load and prevent unbounded memory growth
    let (tx, rx) = sync_channel::<(ImageBuffer, i64)>(2);

    let shared_for_writer = Arc::clone(&shared);
    let writer_handle = thread::Builder::new()
        .name("lookout-recording-encoder".to_string())
        .spawn(move || {
            let mut written_count = 0usize;
            while let Ok((frame, ts)) = rx.recv() {
                match encoder.write_frame(&frame, ts) {
                    Ok(_) => {
                        written_count += 1;
                    }
                    Err(_e) => {
                        shared_for_writer
                            .stop_requested
                            .store(true, Ordering::SeqCst);
                        show_recording_limit_toast(
                            "Recording stopped: disk write error or disk full.",
                        );
                        break;
                    }
                }
            }
            let _ = encoder.finalize();
            written_count
        })
        .map_err(|e| format!("Failed to spawn encoder thread: {e}"))?;

    let frame_interval = Duration::from_nanos(1_000_000_000 / fps.max(1) as u64);
    let mut last_tick = Instant::now();
    let mut total_active_nanos = 0u64;

    // Track mouse clicks for soft expanding click highlight rings
    let mut recent_clicks: Vec<(PointI, Instant)> = Vec::new();
    let mut prev_lbutton_down = false;

    // Keep sample frames (up to 300) for optional GIF export
    let mut sample_frames: Vec<ImageBuffer> = Vec::new();

    // Initial desktop virtual coordinates & dimensions to detect display/topology changes
    let initial_vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let initial_vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let initial_cx = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let initial_cy = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    while !shared.stop_requested.load(Ordering::Acquire) {
        // Safety 1: Stop immediately on Escape key
        unsafe {
            if (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000) != 0 {
                shared.stop_requested.store(true, Ordering::SeqCst);
                break;
            }
        }

        // Safety 2: Stop if screen is locked
        if is_workstation_locked() {
            show_recording_limit_toast("Recording stopped: workstation was locked.");
            shared.stop_requested.store(true, Ordering::SeqCst);
            break;
        }

        // Safety 3: Stop if plugin is paused
        if let Some(ref p) = paused_flag {
            if p.load(Ordering::Acquire) {
                show_recording_limit_toast("Recording stopped: Lookout was paused.");
                shared.stop_requested.store(true, Ordering::SeqCst);
                break;
            }
        }

        // Safety 4: Stop if target window closed
        if let Some(hwnd) = target_hwnd {
            unsafe {
                if !IsWindow(Some(hwnd)).as_bool() {
                    show_recording_limit_toast("Recording stopped: target window was closed.");
                    shared.stop_requested.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }

        // Safety 5: Stop if display topology or resolution changes
        unsafe {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let cx = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let cy = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            if vx != initial_vx || vy != initial_vy || cx != initial_cx || cy != initial_cy {
                show_recording_limit_toast(
                    "Recording stopped: display resolution or topology changed.",
                );
                shared.stop_requested.store(true, Ordering::SeqCst);
                break;
            }
        }

        // Mouse click detection for click highlight ring
        unsafe {
            let lbutton_down = (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0;
            if lbutton_down && !prev_lbutton_down {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                if pt.x >= rect.left
                    && pt.x <= rect.right
                    && pt.y >= rect.top
                    && pt.y <= rect.bottom
                {
                    recent_clicks.push((
                        PointI::new(pt.x - rect.left, pt.y - rect.top),
                        Instant::now(),
                    ));
                }
            }
            prev_lbutton_down = lbutton_down;
        }
        recent_clicks.retain(|(_, t)| t.elapsed().as_millis() <= 300);

        let now = Instant::now();
        let elapsed = now.duration_since(last_tick);

        if !shared.is_paused.load(Ordering::Acquire) {
            total_active_nanos += elapsed.as_nanos() as u64;
            shared
                .active_nanos
                .store(total_active_nanos, Ordering::Release);

            // Hard caps check: 60 minutes or 2 GB
            let active_secs = total_active_nanos / 1_000_000_000;
            if active_secs >= MAX_RECORDING_DURATION_SECS {
                show_recording_limit_toast(
                    "Recording reached 60 min limit. Video saved successfully.",
                );
                shared.stop_requested.store(true, Ordering::SeqCst);
                break;
            } else if active_secs >= WARN_RECORDING_DURATION_SECS
                && !shared.warn_limit_shown.swap(true, Ordering::SeqCst)
            {
                show_recording_limit_toast("Recording approaching limit (55 min / 1.9 GB max).");
            }

            // Capture frame via GDI BitBlt
            let with_cur = shared.record_cursor.load(Ordering::Acquire);
            if let Ok(mut frame) =
                capture_gdi_rect_with_cursor(rect.left, rect.top, width, height, with_cur)
            {
                // Composite click highlight ring if active
                if shared.click_highlight.load(Ordering::Acquire) {
                    for &(pt, t) in &recent_clicks {
                        let ms = t.elapsed().as_millis().min(300) as f32;
                        let progress = ms / 300.0;
                        let radius = 6.0 + progress * 20.0;
                        let alpha = (220.0 * (1.0 - progress)) as u8;
                        draw_click_ring(&mut frame, pt, radius, Color::rgba(245, 165, 36, alpha));
                    }
                }

                // Sample frame for result card GIF export (up to 300 frames)
                if sample_frames.len() < 300 && sample_frames.len().is_multiple_of(2) {
                    sample_frames.push(frame.clone());
                }

                let count = shared.total_frames.fetch_add(1, Ordering::Relaxed);
                if count.is_multiple_of(30) {
                    if let Ok(meta) = std::fs::metadata(&mp4_path) {
                        let sz = meta.len();
                        shared.file_bytes.store(sz, Ordering::Release);
                        if sz >= MAX_RECORDING_FILE_BYTES {
                            show_recording_limit_toast(
                                "Recording reached 2 GB limit. Video saved successfully.",
                            );
                            shared.stop_requested.store(true, Ordering::SeqCst);
                            break;
                        } else if sz >= WARN_RECORDING_FILE_BYTES
                            && !shared.warn_limit_shown.swap(true, Ordering::SeqCst)
                        {
                            show_recording_limit_toast(
                                "Recording approaching 2 GB limit. Stopping soon.",
                            );
                        }
                    }
                }

                let timestamp_100ns = (total_active_nanos / 100) as i64;
                match tx.try_send((frame, timestamp_100ns)) {
                    Ok(_) => {}
                    Err(TrySendError::Full(_)) => {
                        shared.dropped_frames.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
        }

        last_tick = now;

        let work_duration = now.elapsed();
        if work_duration < frame_interval {
            thread::sleep(frame_interval - work_duration);
        }
    }

    // Signal control bar window to close and exit cleanly
    let raw_bar = shared.bar_hwnd.load(Ordering::Acquire);
    if raw_bar != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(raw_bar as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    // Drop sender so writer thread flushes and finishes
    drop(tx);
    let frame_count = writer_handle.join().unwrap_or(0);
    let _ = bar_handle.join();

    let file_size_bytes = std::fs::metadata(&mp4_path).map(|m| m.len()).unwrap_or(0);
    let duration_secs = total_active_nanos as f64 / 1_000_000_000.0;
    let dropped_frames = shared.dropped_frames.load(Ordering::Acquire);

    // Copy file path to clipboard
    if let Some(path_str) = mp4_path.to_str() {
        let _ = copy_text_to_clipboard(path_str);
    }

    let recording_res = RecordingResult {
        path: mp4_path,
        duration_secs,
        width,
        height,
        file_size_bytes,
        frame_count,
        dropped_frames,
        fps,
    };

    // Show interactive result card
    if show_result_card_ui {
        let frames_opt = if !sample_frames.is_empty() {
            Some(sample_frames)
        } else {
            None
        };
        let _ = show_result_card(recording_res.clone(), save_dir.to_path_buf(), frames_opt);
    }

    Ok(recording_res)
}

thread_local! {
    static LIMIT_TOAST_MESSAGE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn limit_toast_wndproc(
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
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER
        | windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let w = 460u32;
            let h = 64u32;
            let mut img = ImageBuffer::new(w, h);

            fill_rounded_rect_rgba(
                &mut img,
                RectI::new(0, 0, w as i32, h as i32),
                8,
                Color::rgb(20, 23, 28),
            );
            stroke_rounded_rect_rgba(
                &mut img,
                RectI::new(0, 0, w as i32, h as i32),
                8,
                Color::rgb(38, 43, 51),
            );

            draw_system_text_rgba(
                &mut img,
                PointI::new(14, 12),
                "LOOKOUT RECORDING",
                Color::rgb(245, 165, 36),
                11,
                true,
            );

            let message = LIMIT_TOAST_MESSAGE
                .with(|m| m.borrow().clone())
                .unwrap_or_else(|| "Recording limit approaching.".to_string());
            draw_system_text_rgba(
                &mut img,
                PointI::new(14, 32),
                &message,
                Color::rgb(230, 233, 238),
                12,
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
                        for (i, p) in img.pixels.as_chunks::<4>().0.iter().enumerate() {
                            let offset = i * 4;
                            dst[offset] = p[2];
                            dst[offset + 1] = p[1];
                            dst[offset + 2] = p[0];
                            dst[offset + 3] = p[3];
                        }
                        let old_bmp = SelectObject(mem_dc, dib.into());
                        let _ = BitBlt(hdc, 0, 0, w as i32, h as i32, Some(mem_dc), 0, 0, SRCCOPY);
                        let _ = SelectObject(mem_dc, old_bmp);
                        let _ = DeleteObject(dib.into());
                    }
                }
                let _ = DeleteDC(mem_dc);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Displays a toast notification when reaching or approaching limits.
pub fn show_recording_limit_toast(message: &str) {
    let msg_string = message.to_string();
    thread::Builder::new()
        .name("lookout-recording-limit-toast".to_string())
        .spawn(move || unsafe {
            let hinst = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(_) => return,
            };
            let class_name = w!("LookoutRecordingLimitToast");
            let wc = WNDCLASSW {
                style: windows::Win32::UI::WindowsAndMessaging::CS_HREDRAW
                    | windows::Win32::UI::WindowsAndMessaging::CS_VREDRAW,
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

            LIMIT_TOAST_MESSAGE.with(|m| {
                *m.borrow_mut() = Some(msg_string);
            });

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("Lookout Recording Notice"),
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

/// Executes a screen recording session directly capturing to an animated GIF.
pub fn execute_screen_recording_gif(
    rect: RectI,
    fps: u32,
    max_width: u32,
    initial_record_cursor: bool,
    save_dir: &Path,
) -> Result<RecordingResult, String> {
    if rect.width() < 16 || rect.height() < 16 {
        return Err("Recorded area must be at least 16x16 pixels".to_string());
    }

    if let Err(e) = std::fs::create_dir_all(save_dir) {
        return Err(format!("Could not create save folder: {e}"));
    }

    let timestamp = current_timestamp_recording_string();
    let gif_path = next_available_named_path(save_dir, "Recording", &timestamp, "gif");

    let width = rect.width() as u32;
    let height = rect.height() as u32;

    let shared = Arc::new(RecordingSharedState::new(fps, initial_record_cursor));
    let bar_handle = spawn_recording_control_bar(Arc::clone(&shared), rect);

    let mut frames = Vec::new();
    let frame_interval = Duration::from_nanos(1_000_000_000 / fps.max(1) as u64);
    let mut last_tick = Instant::now();
    let mut total_active_nanos = 0u64;

    while !shared.stop_requested.load(Ordering::Acquire) {
        unsafe {
            if (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000) != 0 {
                shared.stop_requested.store(true, Ordering::SeqCst);
                break;
            }
        }

        let now = Instant::now();
        let elapsed = now.duration_since(last_tick);

        if !shared.is_paused.load(Ordering::Acquire) {
            total_active_nanos += elapsed.as_nanos() as u64;
            shared
                .active_nanos
                .store(total_active_nanos, Ordering::Release);

            // Hard cap for GIF memory: 600 frames or 300 seconds (5 mins)
            if frames.len() >= 600 || (total_active_nanos / 1_000_000_000) >= 300 {
                show_recording_limit_toast("GIF length limit reached. Saving animated GIF.");
                shared.stop_requested.store(true, Ordering::SeqCst);
                break;
            }

            let with_cur = shared.record_cursor.load(Ordering::Acquire);
            if let Ok(frame) =
                capture_gdi_rect_with_cursor(rect.left, rect.top, width, height, with_cur)
            {
                frames.push(frame);
                shared.total_frames.fetch_add(1, Ordering::Relaxed);
            }
        }

        last_tick = now;
        let work_duration = now.elapsed();
        if work_duration < frame_interval {
            thread::sleep(frame_interval - work_duration);
        }
    }

    let _ = bar_handle.join();

    if frames.is_empty() {
        return Err("No frames recorded for GIF".to_string());
    }

    // Size estimate warning
    let est_bytes = (width * height * frames.len() as u32 * 3) / 10;
    if est_bytes > 25 * 1024 * 1024 {
        show_recording_limit_toast("Warning: resulting GIF exceeds 25 MB.");
    }

    export_to_gif(&frames, fps, max_width, &gif_path)?;

    let file_size_bytes = std::fs::metadata(&gif_path).map(|m| m.len()).unwrap_or(0);
    let duration_secs = total_active_nanos as f64 / 1_000_000_000.0;
    let frame_count = frames.len();

    if let Some(path_str) = gif_path.to_str() {
        let _ = copy_text_to_clipboard(path_str);
    }

    unsafe {
        let wide_folder: Vec<u16> = save_dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let _ = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide_folder.as_ptr()),
            None,
            None,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOW,
        );
    }

    Ok(RecordingResult {
        path: gif_path,
        duration_secs,
        width,
        height,
        file_size_bytes,
        frame_count,
        dropped_frames: 0,
        fps,
    })
}

/// Encodes an animated GIF from a sequence of frames in pure Rust without external tools.
pub fn export_to_gif(
    frames: &[ImageBuffer],
    fps: u32,
    max_width: u32,
    out_path: &Path,
) -> Result<(), String> {
    if frames.is_empty() {
        return Err("No frames to encode into GIF".to_string());
    }

    let orig_w = frames[0].width;
    let orig_h = frames[0].height;
    let scale = if orig_w > max_width && max_width > 0 {
        max_width as f32 / orig_w as f32
    } else {
        1.0f32
    };

    let target_w = ((orig_w as f32 * scale).round() as u32).max(1);
    let target_h = ((orig_h as f32 * scale).round() as u32).max(1);

    // Frame delay in 1/100ths of a second
    let delay_100ths = ((100.0 / fps.max(1) as f32).round() as u16).max(2);

    let mut file = File::create(out_path).map_err(|e| format!("Create GIF file failed: {e}"))?;

    // 1. Header & Logical Screen Descriptor
    file.write_all(b"GIF89a").map_err(|e| e.to_string())?;
    file.write_all(&target_w.to_le_bytes()[..2])
        .map_err(|e| e.to_string())?;
    file.write_all(&target_h.to_le_bytes()[..2])
        .map_err(|e| e.to_string())?;

    // Global Color Table flag: 1, Color Resolution: 7 (8 bits), Sort: 0, Size: 7 (256 colors) -> 0xF7
    file.write_all(&[0xF7, 0x00, 0x00])
        .map_err(|e| e.to_string())?;

    // 2. Global 256-color Low-Pass Uniform Palette (6x7x6 grid + grayscale ramp)
    let mut palette = [0u8; 768];
    let mut pal_idx = 0;
    for r in 0..6 {
        for g in 0..7 {
            for b in 0..6 {
                if pal_idx < 252 * 3 {
                    palette[pal_idx] = (r * 255 / 5) as u8;
                    palette[pal_idx + 1] = (g * 255 / 6) as u8;
                    palette[pal_idx + 2] = (b * 255 / 5) as u8;
                    pal_idx += 3;
                }
            }
        }
    }
    // Grayscale ramp for remaining 4 colors
    for i in 0..4 {
        let val = (i * 255 / 3) as u8;
        palette[pal_idx] = val;
        palette[pal_idx + 1] = val;
        palette[pal_idx + 2] = val;
        pal_idx += 3;
    }
    file.write_all(&palette).map_err(|e| e.to_string())?;

    // 3. Netscape 2.0 Application Extension (Infinite loop)
    file.write_all(&[
        0x21, 0xFF, 0x0B, b'N', b'E', b'T', b'S', b'C', b'A', b'P', b'E', b'2', b'.', b'0', 0x03,
        0x01, 0x00, 0x00, 0x00,
    ])
    .map_err(|e| e.to_string())?;

    // Palette lookup function
    let match_palette = |r: u8, g: u8, b: u8| -> u8 {
        let mut best_idx = 0u8;
        let mut min_dist = u32::MAX;
        for i in 0..256 {
            let pr = palette[i * 3] as i32;
            let pg = palette[i * 3 + 1] as i32;
            let pb = palette[i * 3 + 2] as i32;
            let dr = r as i32 - pr;
            let dg = g as i32 - pg;
            let db = b as i32 - pb;
            let dist = (dr * dr + dg * dg + db * db) as u32;
            if dist < min_dist {
                min_dist = dist;
                best_idx = i as u8;
                if dist == 0 {
                    break;
                }
            }
        }
        best_idx
    };

    // 4. Encode each frame
    for frame in frames {
        // Graphic Control Extension
        file.write_all(&[
            0x21,
            0xF9,
            0x04,
            0x04,
            (delay_100ths & 0xFF) as u8,
            ((delay_100ths >> 8) & 0xFF) as u8,
            0x00,
            0x00,
        ])
        .map_err(|e| e.to_string())?;

        // Image Descriptor
        file.write_all(&[
            0x2C,
            0x00,
            0x00,
            0x00,
            0x00,
            (target_w & 0xFF) as u8,
            ((target_w >> 8) & 0xFF) as u8,
            (target_h & 0xFF) as u8,
            ((target_h >> 8) & 0xFF) as u8,
            0x00, // No local color table
        ])
        .map_err(|e| e.to_string())?;

        // Quantize frame pixels to palette indices
        let mut indices = Vec::with_capacity((target_w * target_h) as usize);
        for ty in 0..target_h {
            let sy = (ty as f32 / scale).min((frame.height - 1) as f32) as u32;
            let row = frame.row(sy);
            for tx in 0..target_w {
                let sx = (tx as f32 / scale).min((frame.width - 1) as f32) as usize;
                let idx = sx * 4;
                let r = row[idx];
                let g = row[idx + 1];
                let b = row[idx + 2];
                indices.push(match_palette(r, g, b));
            }
        }

        // LZW Compression for 8-bit palette
        encode_lzw_subblocks(&indices, 8, &mut file)?;
    }

    // 5. Trailer
    file.write_all(&[0x3B]).map_err(|e| e.to_string())?;
    Ok(())
}

/// Standard GIF LZW encoder producing sub-blocks.
fn encode_lzw_subblocks(data: &[u8], min_code_size: u8, out: &mut File) -> Result<(), String> {
    out.write_all(&[min_code_size]).map_err(|e| e.to_string())?;

    let clear_code = 1u16 << min_code_size;
    let end_code = clear_code + 1;

    let mut code_size = min_code_size + 1;
    let mut next_code = clear_code + 2;

    let mut bit_buf = 0u32;
    let mut bit_count = 0u8;
    let mut sub_block = Vec::with_capacity(256);

    let mut emit_bits =
        |code: u16, size: u8, block: &mut Vec<u8>, f: &mut File| -> Result<(), String> {
            bit_buf |= (code as u32) << bit_count;
            bit_count += size;
            while bit_count >= 8 {
                block.push((bit_buf & 0xFF) as u8);
                bit_buf >>= 8;
                bit_count -= 8;
                if block.len() == 254 {
                    f.write_all(&[254]).map_err(|e| e.to_string())?;
                    f.write_all(block).map_err(|e| e.to_string())?;
                    block.clear();
                }
            }
            Ok(())
        };

    emit_bits(clear_code, code_size, &mut sub_block, out)?;

    // Trie-based dictionary lookup
    let mut dict = std::collections::HashMap::<(u16, u8), u16>::new();
    let mut current_prefix: Option<u16> = None;

    for &byte in data {
        if let Some(prefix) = current_prefix {
            if let Some(&code) = dict.get(&(prefix, byte)) {
                current_prefix = Some(code);
            } else {
                emit_bits(prefix, code_size, &mut sub_block, out)?;
                if next_code < 4096 {
                    dict.insert((prefix, byte), next_code);
                    next_code += 1;
                    if next_code > (1 << code_size) && code_size < 12 {
                        code_size += 1;
                    }
                } else {
                    emit_bits(clear_code, code_size, &mut sub_block, out)?;
                    dict.clear();
                    code_size = min_code_size + 1;
                    next_code = clear_code + 2;
                }
                current_prefix = Some(byte as u16);
            }
        } else {
            current_prefix = Some(byte as u16);
        }
    }

    if let Some(prefix) = current_prefix {
        emit_bits(prefix, code_size, &mut sub_block, out)?;
    }
    emit_bits(end_code, code_size, &mut sub_block, out)?;

    // Flush remaining bits
    if bit_count > 0 {
        sub_block.push((bit_buf & 0xFF) as u8);
    }
    if !sub_block.is_empty() {
        out.write_all(&[sub_block.len() as u8])
            .map_err(|e| e.to_string())?;
        out.write_all(&sub_block).map_err(|e| e.to_string())?;
    }
    // Block terminator
    out.write_all(&[0x00]).map_err(|e| e.to_string())?;

    Ok(())
}

// ----------------------------------------------------------------------------
// Real Demo Windows for Review Captures (only compiled under feature = "review")
// ----------------------------------------------------------------------------

#[cfg(feature = "review")]
pub fn open_real_control_bar_for_demo(scale: f32) -> Result<HWND, String> {
    unsafe {
        let mut shared_val = RecordingSharedState::new(30, true);
        shared_val.custom_scale = Some(scale);
        let shared = Arc::new(shared_val);
        shared.active_nanos.store(15_000_000_000, Ordering::Release); // 00:15
        CONTROL_BAR_STATE.with(|s| {
            *s.borrow_mut() = Some(Arc::clone(&shared));
        });

        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class_name = w!("LookoutRecordingBar");
        let wc = WNDCLASSW {
            style: windows::Win32::UI::WindowsAndMessaging::CS_HREDRAW
                | windows::Win32::UI::WindowsAndMessaging::CS_VREDRAW,
            lpfnWndProc: Some(recording_bar_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let layout = compute_recording_bar_layout(scale);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class_name,
            w!("Lookout Recording Bar Demo"),
            WS_POPUP,
            100,
            100,
            layout.width as i32,
            layout.height as i32,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| e.to_string())?;

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}

#[cfg(feature = "review")]
pub fn open_real_result_card_for_demo(scale: f32, sample_path: PathBuf) -> Result<HWND, String> {
    unsafe {
        let result = RecordingResult {
            path: sample_path.clone(),
            duration_secs: 15.0,
            width: 1920,
            height: 1080,
            file_size_bytes: 3_500_000,
            frame_count: 450,
            dropped_frames: 0,
            fps: 30,
        };
        let state = Arc::new(std::sync::Mutex::new(ResultCardState {
            result,
            save_folder: sample_path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            sample_frames: None,
            copied_feedback: false,
            exported_feedback: false,
            recycled_feedback: false,
            custom_scale: Some(scale),
        }));

        RESULT_CARD_STATE.with(|s| {
            *s.borrow_mut() = Some(Arc::clone(&state));
        });

        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class_name = w!("LookoutRecordingResultCard");
        let wc = WNDCLASSW {
            style: windows::Win32::UI::WindowsAndMessaging::CS_HREDRAW
                | windows::Win32::UI::WindowsAndMessaging::CS_VREDRAW,
            lpfnWndProc: Some(result_card_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let layout = compute_result_card_layout(scale);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class_name,
            w!("Lookout Recording Result Demo"),
            WS_POPUP,
            120,
            120,
            layout.width as i32,
            layout.height as i32,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| e.to_string())?;

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}
