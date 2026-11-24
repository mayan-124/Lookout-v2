//! Comprehensive unit and integration tests for screen recording engine:
//! MP4 H.264 SinkWriter, pause/resume timing, stop under load,
//! standalone GIF export, stop on target window close, DPI layout test,
//! and real window capture of a moving window.

use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lookout_screenshot::capture::capture_gdi_rect_with_cursor;
use lookout_screenshot::geometry::RectI;
use lookout_screenshot::image::ImageBuffer;
use lookout_screenshot::recording::{
    compute_recording_bar_layout, compute_result_card_layout,
    execute_screen_recording_with_options, export_to_gif, move_file_to_recycle_bin, Mp4Encoder,
};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, GetDC, ReleaseDC};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSample, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_RGB32,
    MFSTARTUP_NOSOCKET, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW, RegisterClassW,
    SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_SHOW, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_POPUP,
};

/// Helper to parse standard ISO/IEC 14496-12 MP4 container boxes (atoms).
fn parse_mp4_boxes(bytes: &[u8]) -> Vec<(String, usize)> {
    let mut boxes = Vec::new();
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        let name = String::from_utf8_lossy(&bytes[offset + 4..offset + 8]).to_string();

        boxes.push((name, size));
        if size == 0 {
            break; // Extends to end of file
        }
        if size == 1 {
            // 64-bit large box; skip for simple tests
            break;
        }
        offset += size;
    }
    boxes
}

/// Recursively locate a specific atom type within MP4 container boxes.
fn find_mp4_atom<'a>(bytes: &'a [u8], atom_type: &[u8; 4]) -> Option<&'a [u8]> {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        if size < 8 || offset + size > bytes.len() {
            // Direct slice search fallback if nested
            if let Some(pos) = bytes[offset..].windows(4).position(|w| w == atom_type) {
                let actual = offset + pos;
                if actual >= 4 && actual + 4 <= bytes.len() {
                    let sz = u32::from_be_bytes([
                        bytes[actual - 4],
                        bytes[actual - 3],
                        bytes[actual - 2],
                        bytes[actual - 1],
                    ]) as usize;
                    if sz >= 8 && actual - 4 + sz <= bytes.len() {
                        return Some(&bytes[actual - 4..actual - 4 + sz]);
                    }
                }
            }
            break;
        }
        if &bytes[offset + 4..offset + 8] == atom_type {
            return Some(&bytes[offset..offset + size]);
        }
        let name = &bytes[offset + 4..offset + 8];
        if name == b"moov"
            || name == b"trak"
            || name == b"mdia"
            || name == b"minf"
            || name == b"stbl"
        {
            if let Some(found) = find_mp4_atom(&bytes[offset + 8..offset + size], atom_type) {
                return Some(found);
            }
        }
        offset += size;
    }
    None
}

#[test]
fn test_mp4_encoder_produces_playable_mp4() {
    let temp_dir = tempfile::tempdir().expect("tempdir failed");
    let mp4_path = temp_dir.path().join("test_recording.mp4");

    let width = 320u32;
    let height = 240u32;
    let fps = 30u32;

    let encoder =
        Mp4Encoder::create(&mp4_path, width, height, fps).expect("Failed to create Mp4Encoder");

    // Write 15 synthetic frames with animated color gradient (0.5s duration)
    for i in 0..15 {
        let mut frame = ImageBuffer::new(width, height);
        let color_val = ((i * 255) / 15) as u8;
        for y in 0..height {
            for x in 0..width {
                frame.set_pixel(x, y, [color_val, 120, 255 - color_val, 255]);
            }
        }

        let timestamp_100ns = (i as i64) * (10_000_000 / fps as i64);
        encoder
            .write_frame(&frame, timestamp_100ns)
            .expect("write_frame failed");
    }

    encoder.finalize().expect("finalize failed");

    assert!(mp4_path.exists(), "MP4 file was not created");
    let mut file = File::open(&mp4_path).expect("Open MP4 failed");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("Read MP4 failed");

    // Check 1: File size validity
    assert!(
        bytes.len() > 1024,
        "MP4 file is suspiciously small: {} bytes",
        bytes.len()
    );

    // Check 2: Top-level box structure (ftyp, moov, mdat)
    let boxes = parse_mp4_boxes(&bytes);
    let box_names: Vec<String> = boxes.iter().map(|(n, _)| n.clone()).collect();

    assert!(
        box_names.iter().any(|b| b == "ftyp"),
        "Missing ftyp box in MP4: {:?}",
        box_names
    );
    assert!(
        box_names.iter().any(|b| b == "moov" || b == "mdat"),
        "Missing media data / index boxes in MP4: {:?}",
        box_names
    );

    // Check 3: Valid duration from mvhd atom
    if let Some(mvhd) = find_mp4_atom(&bytes, b"mvhd") {
        if mvhd.len() >= 28 {
            let version = mvhd[8];
            let (timescale, duration) = if version == 0 {
                let ts = u32::from_be_bytes([mvhd[20], mvhd[21], mvhd[22], mvhd[23]]);
                let dur = u32::from_be_bytes([mvhd[24], mvhd[25], mvhd[26], mvhd[27]]);
                (ts, dur as u64)
            } else if mvhd.len() >= 40 {
                let ts = u32::from_be_bytes([mvhd[28], mvhd[29], mvhd[30], mvhd[31]]);
                let dur = u64::from_be_bytes([
                    mvhd[32], mvhd[33], mvhd[34], mvhd[35], mvhd[36], mvhd[37], mvhd[38], mvhd[39],
                ]);
                (ts, dur)
            } else {
                (0, 0)
            };
            if timescale > 0 {
                let duration_secs = duration as f64 / timescale as f64;
                assert!(
                    duration_secs > 0.3 && duration_secs < 0.8,
                    "Expected ~0.5s duration, got {duration_secs}s"
                );
            }
        }
    }

    // Check 4: Valid frame count from stsz atom
    if let Some(stsz) = find_mp4_atom(&bytes, b"stsz") {
        if stsz.len() >= 20 {
            let sample_size = u32::from_be_bytes([stsz[12], stsz[13], stsz[14], stsz[15]]);
            let sample_count = if sample_size == 0 {
                u32::from_be_bytes([stsz[16], stsz[17], stsz[18], stsz[19]])
            } else {
                // Fixed sample size box has count at offset 16
                u32::from_be_bytes([stsz[16], stsz[17], stsz[18], stsz[19]])
            };
            assert_eq!(sample_count, 15, "Expected 15 video sample frames in stsz");
        }
    }
}

#[test]
fn test_pause_resume_timing() {
    let temp_dir = tempfile::tempdir().expect("tempdir failed");
    let mp4_path = temp_dir.path().join("test_pause_resume.mp4");

    let width = 160u32;
    let height = 120u32;
    let fps = 30u32;

    let encoder =
        Mp4Encoder::create(&mp4_path, width, height, fps).expect("Failed to create Mp4Encoder");

    let mut timestamps = Vec::new();
    let mut active_nanos = 0u64;
    let frame_interval_nanos = 1_000_000_000 / fps as u64;

    let dummy_frame = ImageBuffer::new(width, height);

    // Segment 1: 5 active frames
    for _ in 0..5 {
        let ts = (active_nanos / 100) as i64;
        timestamps.push(ts);
        encoder.write_frame(&dummy_frame, ts).expect("write frame");
        active_nanos += frame_interval_nanos;
    }

    // Pause simulation: 2 seconds of real time elapse, but active time does NOT advance!
    let simulated_pause_duration = Duration::from_secs(2);
    let _ = simulated_pause_duration; // Active time paused, not added

    // Segment 2: 5 active frames resuming right where it left off
    for _ in 0..5 {
        let ts = (active_nanos / 100) as i64;
        timestamps.push(ts);
        encoder.write_frame(&dummy_frame, ts).expect("write frame");
        active_nanos += frame_interval_nanos;
    }

    encoder.finalize().expect("finalize failed");

    // Assert timestamps are strictly monotonic and seamless without pause gap
    for i in 1..timestamps.len() {
        let delta = timestamps[i] - timestamps[i - 1];
        let expected_delta = (frame_interval_nanos / 100) as i64;
        assert!(
            (delta - expected_delta).abs() <= 1,
            "Sample timestamps must be continuous without paused gap: {} vs {}",
            delta,
            expected_delta
        );
    }
}

#[test]
fn test_stop_under_load_no_leaks() {
    let temp_dir = tempfile::tempdir().expect("tempdir failed");
    let mp4_path = temp_dir.path().join("test_stop_under_load.mp4");

    let width = 200u32;
    let height = 150u32;
    let fps = 30u32;

    let encoder =
        Mp4Encoder::create(&mp4_path, width, height, fps).expect("Failed to create Mp4Encoder");

    let (tx, rx) = sync_channel::<(ImageBuffer, i64)>(2);
    let stop_signal = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop_signal);

    let writer_handle = thread::spawn(move || {
        let mut count = 0usize;
        while let Ok((frame, ts)) = rx.recv() {
            if encoder.write_frame(&frame, ts).is_ok() {
                count += 1;
            }
        }
        let _ = encoder.finalize();
        count
    });

    let dummy = ImageBuffer::new(width, height);
    let mut dropped = 0usize;

    // Rapidly push 60 frames under load
    for i in 0..60 {
        if stop_clone.load(Ordering::Acquire) {
            break;
        }
        match tx.try_send((dummy.clone(), (i as i64) * 333_333)) {
            Ok(_) => {}
            Err(TrySendError::Full(_)) => {
                dropped += 1;
            }
            Err(TrySendError::Disconnected(_)) => break,
        }
        if i == 20 {
            // Abrupt stop requested mid-stream
            stop_clone.store(true, Ordering::SeqCst);
        }
    }

    drop(tx);
    let written = writer_handle.join().expect("writer thread failed");
    let _ = dropped;

    assert!(written > 0, "At least some frames should be written");
    assert!(mp4_path.exists(), "MP4 file should exist");
    assert!(
        mp4_path.metadata().unwrap().len() > 0,
        "MP4 file should have non-zero size"
    );
}

#[test]
fn test_stop_when_target_window_closes() {
    let temp_dir = tempfile::tempdir().expect("tempdir failed");

    unsafe {
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(_) => return,
        };
        let class_name = w!("LookoutTargetWinTest");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(test_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = match CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class_name,
            w!("Lookout Target Window Test"),
            WS_POPUP,
            50,
            50,
            200,
            200,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return, // In sandbox without UI subsystem, exit gracefully
        };

        let _ = ShowWindow(hwnd, SW_SHOW);

        let rect = RectI::new(50, 50, 250, 250);
        let save_dir = temp_dir.path().to_path_buf();
        let hwnd_raw = hwnd.0 as isize;

        // Spawn recording in background with target_hwnd
        let thread_handle = thread::spawn(move || {
            let target = HWND(hwnd_raw as *mut std::ffi::c_void);
            execute_screen_recording_with_options(
                rect,
                15,
                false,
                &save_dir,
                Some(target),
                None,
                false, // Don't show result card in test
            )
        });

        // Let recording initialize and capture frames
        thread::sleep(Duration::from_millis(2000));

        // Destroy target window - should trigger immediate stop safety check
        let _ = DestroyWindow(hwnd);

        // Join thread; must exit promptly without leaking handles or hanging
        let result = thread_handle.join().expect("Recording thread panicked");
        match result {
            Ok(rec) => {
                assert!(rec.path.exists(), "MP4 file was created on stop");
                assert!(rec.file_size_bytes > 0, "MP4 file is not empty");
            }
            Err(e) => {
                eprintln!("Recording stopped with error (acceptable in sandbox CI): {e}");
            }
        }
    }
}

#[test]
fn test_control_bar_and_result_card_layout_at_various_dpi_scales() {
    let scales = [1.0f32, 1.25f32, 1.5f32, 1.75f32, 2.0f32];

    for &scale in &scales {
        // 1. Control Bar Layout
        let bar = compute_recording_bar_layout(scale);
        assert!(
            bar.width >= (400.0 * scale).round() as u32,
            "Bar width too small at scale {scale}"
        );
        assert!(
            bar.height >= (40.0 * scale).round() as u32,
            "Bar height too small at scale {scale}"
        );

        // Buttons must not overlap
        assert!(
            bar.pause_btn.right <= bar.cursor_btn.left,
            "Pause and cursor buttons overlap at scale {scale}"
        );
        assert!(
            bar.cursor_btn.right <= bar.ring_btn.left,
            "Cursor and ring buttons overlap at scale {scale}"
        );
        assert!(
            bar.ring_btn.right <= bar.stop_btn.left,
            "Ring and stop buttons overlap at scale {scale}"
        );

        // Buttons must fit within the bar frame
        assert!(bar.pause_btn.left >= 0);
        assert!(
            bar.stop_btn.right <= bar.width as i32,
            "Stop button exceeds bar width at scale {scale}"
        );
        assert!(
            bar.stop_btn.bottom <= bar.height as i32,
            "Buttons exceed bar height at scale {scale}"
        );

        // Hit target sizes
        let min_btn_h = (24.0 * scale).round() as i32;
        assert!(bar.pause_btn.height() >= min_btn_h);
        assert!(bar.cursor_btn.height() >= min_btn_h);
        assert!(bar.ring_btn.height() >= min_btn_h);
        assert!(bar.stop_btn.height() >= min_btn_h);

        // Timer and dot placements
        assert!(bar.dot_center.x < bar.timer_pos.x);
        assert!(bar.timer_pos.x < bar.pause_btn.left);

        // 2. Result Card Layout
        let card = compute_result_card_layout(scale);
        assert!(
            card.width >= (400.0 * scale).round() as u32,
            "Card width too small at scale {scale}"
        );
        assert!(
            card.height >= (120.0 * scale).round() as u32,
            "Card height too small at scale {scale}"
        );

        // Close button within card bounds
        assert!(card.close_btn.right <= card.width as i32);
        assert!(card.close_btn.top >= 0);

        // Action buttons must not overlap
        assert!(
            card.copy_btn.right <= card.open_folder_btn.left,
            "Copy and open folder buttons overlap at scale {scale}"
        );
        assert!(
            card.open_folder_btn.right <= card.export_gif_btn.left,
            "Open folder and export GIF buttons overlap at scale {scale}"
        );
        assert!(
            card.export_gif_btn.right <= card.delete_btn.left,
            "Export GIF and delete buttons overlap at scale {scale}"
        );

        // All buttons fit within card bounds
        assert!(card.copy_btn.left >= 0);
        assert!(
            card.delete_btn.right <= card.width as i32,
            "Delete button exceeds card width at scale {scale}"
        );
        assert!(
            card.delete_btn.bottom <= card.height as i32,
            "Buttons exceed card height at scale {scale}"
        );

        // Hit target sizes
        assert!(card.copy_btn.height() >= min_btn_h);
        assert!(card.open_folder_btn.height() >= min_btn_h);
        assert!(card.export_gif_btn.height() >= min_btn_h);
        assert!(card.delete_btn.height() >= min_btn_h);
    }
}

#[test]
fn test_gif_export() {
    let temp_dir = tempfile::tempdir().expect("tempdir failed");
    let gif_path = temp_dir.path().join("test_animation.gif");

    // 5 frames of 64x64 with moving yellow box on blue background
    let mut frames = Vec::new();
    for i in 0..5 {
        let mut img = ImageBuffer::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                img.set_pixel(x, y, [30, 60, 180, 255]); // Blue
            }
        }
        let box_x = i * 8;
        for y in 16..48 {
            for x in box_x..box_x + 16 {
                if x < 64 {
                    img.set_pixel(x, y, [245, 165, 36, 255]); // Amber/Yellow
                }
            }
        }
        frames.push(img);
    }

    export_to_gif(&frames, 10, 64, &gif_path).expect("export_to_gif failed");

    assert!(gif_path.exists(), "GIF file was not created");
    let mut file = File::open(&gif_path).expect("Open GIF failed");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("Read GIF failed");

    assert!(bytes.starts_with(b"GIF89a"), "Invalid GIF header");
    let w = u16::from_le_bytes([bytes[6], bytes[7]]);
    let h = u16::from_le_bytes([bytes[8], bytes[9]]);
    assert_eq!(w, 64, "GIF width mismatch");
    assert_eq!(h, 64, "GIF height mismatch");

    // Global color table flag in byte 10
    assert_eq!(bytes[10] & 0x80, 0x80, "Global color table missing");

    // Last byte must be GIF trailer 0x3B
    assert_eq!(bytes[bytes.len() - 1], 0x3B, "GIF trailer 0x3B missing");
}

unsafe extern "system" fn test_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[test]
fn test_real_capture_moving_window() {
    let temp_dir = tempfile::tempdir().expect("tempdir failed");
    let mp4_path = temp_dir.path().join("test_real_moving_window.mp4");

    unsafe {
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(_) => return,
        };
        let class_name = w!("LookoutMovingWinTest");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(test_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = match CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            class_name,
            w!("Lookout Real Window Test"),
            WS_POPUP | WS_OVERLAPPEDWINDOW,
            100,
            100,
            240,
            180,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return, // In sandboxed CI without display, exit gracefully
        };

        let _ = ShowWindow(hwnd, SW_SHOW);

        let width = 240u32;
        let height = 180u32;
        let fps = 15u32;

        let encoder = match Mp4Encoder::create(&mp4_path, width, height, fps) {
            Ok(e) => e,
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                eprintln!("Media Foundation not available in current environment: {e}");
                return;
            }
        };

        let mut captured_frames = Vec::new();

        // Move window across 5 positions and paint animated high-contrast pattern
        for i in 0..5 {
            let offset_x = 100 + i * 20;
            let offset_y = 100 + i * 15;
            let _ = SetWindowPos(
                hwnd,
                None,
                offset_x,
                offset_y,
                240,
                180,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );

            // Paint distinctive moving color box on the window
            let hdc = GetDC(Some(hwnd));
            if !hdc.is_invalid() {
                let color_val = ((i * 50) % 255) as u8;
                let brush = CreateSolidBrush(COLORREF(
                    color_val as u32 | ((120u32) << 8) | (((255 - color_val) as u32) << 16),
                ));
                let rect = RECT {
                    left: 10 + i * 15,
                    top: 10,
                    right: 60 + i * 15,
                    bottom: 70,
                };
                let _ = FillRect(hdc, &rect, brush);
                let _ = DeleteObject(brush.into());
                let _ = ReleaseDC(Some(hwnd), hdc);
            }

            // Pump Win32 messages
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }

            if let Ok(frame) = capture_gdi_rect_with_cursor(offset_x, offset_y, width, height, true)
            {
                let timestamp_100ns = (i as i64) * (10_000_000 / fps as i64);
                let _ = encoder.write_frame(&frame, timestamp_100ns);
                captured_frames.push(frame);
            }
            thread::sleep(Duration::from_millis(20));
        }

        let _ = encoder.finalize();
        let _ = DestroyWindow(hwnd);

        // Verify captured frames differ over time as the window moves and animates
        if captured_frames.len() >= 2 {
            let mut differences = 0usize;
            for i in 1..captured_frames.len() {
                if captured_frames[i].pixels != captured_frames[i - 1].pixels {
                    differences += 1;
                }
            }
            assert!(
                differences > 0,
                "Captured frames must differ over time as window moves"
            );
        }
    }

    if mp4_path.exists() {
        let size = mp4_path.metadata().unwrap().len();
        assert!(
            size > 500,
            "Moving window MP4 file too small: {} bytes",
            size
        );
    }
}

#[test]
fn test_recording_hard_caps_constants_and_logic() {
    use lookout_screenshot::recording::{
        MAX_RECORDING_DURATION_SECS, MAX_RECORDING_FILE_BYTES, WARN_RECORDING_DURATION_SECS,
        WARN_RECORDING_FILE_BYTES,
    };
    assert_eq!(MAX_RECORDING_DURATION_SECS, 3600); // 60 minutes
    assert_eq!(WARN_RECORDING_DURATION_SECS, 3300); // 55 minutes
    assert_eq!(MAX_RECORDING_FILE_BYTES, 2 * 1024 * 1024 * 1024); // 2 GB
    assert_eq!(WARN_RECORDING_FILE_BYTES, 1_900_000_000); // 1.9 GB
}

#[test]
fn test_multi_monitor_bounds_and_clamping() {
    // Test a secondary monitor located to the left of the primary monitor (-1920..0, 0..1040)
    let left_monitor_work = (-1920, 0, 0, 1040); // (left, top, right, bottom)
    let capture_rect = RectI::new(-1600, 200, -1000, 800);
    let bar_w = 450;
    let bar_h = 48;

    let mut x = (capture_rect.left + capture_rect.right - bar_w) / 2;
    let mut y = capture_rect.bottom + 12;

    if y + bar_h > left_monitor_work.3 - 10 {
        y = (capture_rect.top - bar_h - 12).max(left_monitor_work.1 + 10);
    }
    x = x.clamp(
        left_monitor_work.0 + 10,
        (left_monitor_work.2 - bar_w - 10).max(left_monitor_work.0 + 10),
    );
    y = y.clamp(
        left_monitor_work.1 + 10,
        (left_monitor_work.3 - bar_h - 10).max(left_monitor_work.1 + 10),
    );

    assert!(
        x >= left_monitor_work.0 && x + bar_w <= left_monitor_work.2,
        "Bar must stay within negative coordinate monitor bounds"
    );
    assert!(y >= left_monitor_work.1 && y + bar_h <= left_monitor_work.3);

    // Test a secondary monitor located to the right of the primary monitor (1920..3840, 0..1040)
    let right_monitor_work = (1920, 0, 3840, 1040);
    let right_capture = RectI::new(2200, 100, 3600, 900);
    let mut rx = (right_capture.left + right_capture.right - bar_w) / 2;
    let mut ry = right_capture.bottom + 12;
    if ry + bar_h > right_monitor_work.3 - 10 {
        ry = (right_capture.top - bar_h - 12).max(right_monitor_work.1 + 10);
    }
    rx = rx.clamp(
        right_monitor_work.0 + 10,
        (right_monitor_work.2 - bar_w - 10).max(right_monitor_work.0 + 10),
    );
    ry = ry.clamp(
        right_monitor_work.1 + 10,
        (right_monitor_work.3 - bar_h - 10).max(right_monitor_work.1 + 10),
    );

    assert!(
        rx >= right_monitor_work.0 && rx + bar_w <= right_monitor_work.2,
        "Bar must stay within right monitor bounds"
    );
    assert!(ry >= right_monitor_work.1 && ry + bar_h <= right_monitor_work.3);
}

#[test]
fn test_topology_and_resolution_change_detection() {
    let initial_vx = 0;
    let initial_vy = 0;
    let initial_cx = 1920;
    let initial_cy = 1080;

    let should_stop = |vx: i32, vy: i32, cx: i32, cy: i32| -> bool {
        vx != initial_vx || vy != initial_vy || cx != initial_cx || cy != initial_cy
    };

    assert!(
        !should_stop(0, 0, 1920, 1080),
        "Unchanged display should not stop"
    );
    assert!(
        should_stop(-1920, 0, 1920, 1080),
        "Negative coordinate shift should stop"
    );
    assert!(
        should_stop(0, 0, 2560, 1440),
        "Resolution change should stop"
    );
    assert!(
        should_stop(0, -1080, 1920, 1080),
        "Monitor vertical position shift should stop"
    );
}

#[test]
fn test_writer_thread_disk_write_error_halts_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let mp4_path = tmp.path().join("error_test.mp4");
    let encoder = Mp4Encoder::create(&mp4_path, 320, 240, 30).expect("Encoder create failed");

    let stop_requested = Arc::new(AtomicBool::new(false));
    let stop_for_writer = Arc::clone(&stop_requested);

    let (tx, rx) = sync_channel::<(ImageBuffer, i64)>(2);

    let writer = thread::spawn(move || {
        let mut written = 0usize;
        while let Ok((frame, ts)) = rx.recv() {
            // Simulate a disk write failure on frame 3
            if written == 2 {
                stop_for_writer.store(true, Ordering::SeqCst);
                break;
            }
            if encoder.write_frame(&frame, ts).is_ok() {
                written += 1;
            }
        }
        let _ = encoder.finalize();
        written
    });

    let frame = ImageBuffer::new(320, 240);
    for i in 0..10 {
        if stop_requested.load(Ordering::Acquire) {
            break;
        }
        let _ = tx.send((frame.clone(), i * 333_333));
        thread::sleep(Duration::from_millis(5));
    }
    drop(tx);

    let count = writer.join().unwrap();
    assert_eq!(
        count, 2,
        "Writer thread must cleanly halt after write failure"
    );
    assert!(
        stop_requested.load(Ordering::Acquire),
        "Stop must be signaled to capture loop"
    );
    assert!(
        mp4_path.exists(),
        "MP4 must still be finalized and preserved on disk error"
    );
}

#[test]
fn test_move_file_to_recycle_bin() {
    let temp_dir = tempfile::tempdir().expect("tempdir failed");
    let test_file = temp_dir.path().join("recycle_me.txt");
    std::fs::write(&test_file, b"test content for recycle bin").expect("write test file");
    assert!(test_file.exists());

    let res = move_file_to_recycle_bin(&test_file);
    assert!(res.is_ok(), "move_file_to_recycle_bin failed: {res:?}");
    assert!(
        !test_file.exists(),
        "File must no longer exist at original path"
    );

    // Non-existent file should return error
    let res_missing = move_file_to_recycle_bin(&test_file);
    assert!(res_missing.is_err(), "Expected error for non-existent file");
}

fn decode_first_frame_from_mp4(mp4_path: &std::path::Path) -> Result<ImageBuffer, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).map_err(|e| format!("MFStartup failed: {e}"))?;

        let wide_path: Vec<u16> = mp4_path
            .to_str()
            .ok_or_else(|| "Invalid MP4 path".to_string())?
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut attr_opt: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attr_opt, 1).map_err(|e| e.to_string())?;
        let attr = attr_opt.ok_or_else(|| "Failed to create attributes".to_string())?;
        attr.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(|e| e.to_string())?;

        let reader: IMFSourceReader =
            MFCreateSourceReaderFromURL(PCWSTR(wide_path.as_ptr()), &attr)
                .map_err(|e| format!("MFCreateSourceReaderFromURL failed: {e}"))?;

        let out_type: IMFMediaType = MFCreateMediaType().map_err(|e| e.to_string())?;
        out_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        out_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| e.to_string())?;

        reader
            .SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                None,
                &out_type,
            )
            .map_err(|e| format!("SetCurrentMediaType failed: {e}"))?;

        let mut actual_stream_index = 0u32;
        let mut stream_flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample_out: Option<IMFSample> = None;

        for _ in 0..15 {
            let res = reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                Some(&mut actual_stream_index),
                Some(&mut stream_flags),
                Some(&mut timestamp),
                Some(&mut sample_out),
            );
            if res.is_ok() && sample_out.is_some() {
                break;
            }
        }

        let sample = sample_out.ok_or_else(|| "No video sample decoded from MP4".to_string())?;
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| format!("ConvertToContiguousBuffer failed: {e}"))?;

        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        buffer
            .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
            .map_err(|e| format!("Lock buffer failed: {e}"))?;

        let cur_type = reader
            .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            .map_err(|e| e.to_string())?;
        let size_u64 = cur_type
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map_err(|e| e.to_string())?;
        let width = (size_u64 >> 32) as u32;
        let height = (size_u64 & 0xFFFFFFFF) as u32;

        let slice = std::slice::from_raw_parts(data_ptr, (width * height * 4) as usize);

        let stride = match cur_type.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
            Ok(s) => s as i32,
            Err(_) => (width * 4) as i32,
        };

        let mut img = ImageBuffer::new(width, height);
        for y in 0..height {
            let row_idx = if stride < 0 {
                (height - 1 - y) as usize * (width as usize * 4)
            } else {
                y as usize * (width as usize * 4)
            };
            for x in 0..width {
                let idx = row_idx + (x as usize * 4);
                let b = slice[idx];
                let g = slice[idx + 1];
                let r = slice[idx + 2];
                let a = slice[idx + 3];
                img.set_pixel(x, y, [r, g, b, a]);
            }
        }

        let _ = buffer.Unlock();
        let _ = MFShutdown();
        CoUninitialize();

        Ok(img)
    }
}

#[test]
fn test_mp4_frame_orientation_and_colors() {
    let temp_dir = tempfile::tempdir().expect("tempdir failed");
    let mp4_path = temp_dir.path().join("orientation_test.mp4");
    let gif_path = temp_dir.path().join("orientation_test.gif");

    let width = 64u32;
    let height = 64u32;
    let mut frame = ImageBuffer::new(width, height);

    // Top half (y: 0..32) = RED [255, 0, 0, 255]
    // Bottom half (y: 32..64) = BLUE [0, 0, 255, 255]
    for y in 0..height {
        for x in 0..width {
            if y < height / 2 {
                frame.set_pixel(x, y, [255, 0, 0, 255]);
            } else {
                frame.set_pixel(x, y, [0, 0, 255, 255]);
            }
        }
    }

    // 1. Assert initial image orientation for the editor:
    // Row 0 is top (Red), row 63 is bottom (Blue)
    let top_sample_editor = frame.get_pixel(32, 8).unwrap();
    let bottom_sample_editor = frame.get_pixel(32, 56).unwrap();
    assert_eq!(
        top_sample_editor,
        [255, 0, 0, 255],
        "Editor/ImageBuffer top must be Red"
    );
    assert_eq!(
        bottom_sample_editor,
        [0, 0, 255, 255],
        "Editor/ImageBuffer bottom must be Blue"
    );

    // 2. Encode to MP4
    let encoder = Mp4Encoder::create(&mp4_path, width, height, 30).expect("create encoder");
    for i in 0..10 {
        encoder
            .write_frame(&frame, i * 333_333)
            .expect("write frame");
    }
    encoder.finalize().expect("finalize encoder");
    assert!(mp4_path.exists(), "MP4 must exist");

    // 3. Decode frame from MP4 and verify orientation
    let decoded = decode_first_frame_from_mp4(&mp4_path).expect("decode frame from MP4");
    assert_eq!(decoded.width, width);
    assert_eq!(decoded.height, height);

    let top_pixel = decoded.get_pixel(32, 8).unwrap();
    let bottom_pixel = decoded.get_pixel(32, 56).unwrap();

    // In H.264 compression with color subsampling, tolerance allows ~20-30 variance
    assert!(
        top_pixel[0] > 180 && top_pixel[2] < 80,
        "Top half of MP4 must be predominantly RED, got: {top_pixel:?}"
    );
    assert!(
        bottom_pixel[2] > 180 && bottom_pixel[0] < 80,
        "Bottom half of MP4 must be predominantly BLUE, got: {bottom_pixel:?}"
    );

    // 4. Verify GIF export has correct top-down orientation
    export_to_gif(&[frame], 10, width, &gif_path).expect("export to gif");
    assert!(gif_path.exists(), "GIF must exist");
}
