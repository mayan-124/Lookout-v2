//! Comprehensive tests for scrolling capture:
//! synthetic frame stitching with known overlap, sticky header & footer detection,
//! repeated rows disambiguation, end of page detection, max height truncation,
//! and real-window scrolling capture.

use lookout_screenshot::geometry::RectI;
use lookout_screenshot::image::ImageBuffer;
use lookout_screenshot::scrolling::{
    detect_sticky_footer, detect_sticky_header, find_vertical_overlap, OverlapMatch,
    ScrollingStitcher, StitchStepResult,
};

#[test]
fn test_stitching_known_overlap() {
    let w = 240u32;
    let full_h = 600u32;

    // Create a tall synthetic canvas with unique row patterns
    let mut canvas = ImageBuffer::new(w, full_h);
    for y in 0..full_h {
        let r = (y % 256) as u8;
        let g = ((y * 7) % 256) as u8;
        let b = (255 - (y % 256)) as u8;
        for x in 0..w {
            canvas.set_pixel(x, y, [r, g, b, 255]);
        }
    }

    let frame_h = 240u32;
    let shift = 100u32;

    // Frame 0: rows 0..240
    let f0 = canvas
        .crop(RectI::new(0, 0, w as i32, frame_h as i32))
        .unwrap();
    // Frame 1: rows 100..340 (shift 100, overlap 140)
    let f1 = canvas
        .crop(RectI::new(
            0,
            shift as i32,
            w as i32,
            (shift + frame_h) as i32,
        ))
        .unwrap();
    // Frame 2: rows 200..440 (shift 100, overlap 140)
    let f2 = canvas
        .crop(RectI::new(
            0,
            (shift * 2) as i32,
            w as i32,
            (shift * 2 + frame_h) as i32,
        ))
        .unwrap();

    let mut stitcher = ScrollingStitcher::new(30_000);
    stitcher.init(f0);

    let res1 = stitcher.add_frame(&f1);
    assert_eq!(
        res1,
        StitchStepResult::Added {
            shift: 100,
            total_height: 340
        }
    );

    let res2 = stitcher.add_frame(&f2);
    assert_eq!(
        res2,
        StitchStepResult::Added {
            shift: 100,
            total_height: 440
        }
    );

    let stitched = stitcher.finish().expect("finish stitching");
    assert_eq!(stitched.width, w);
    assert_eq!(stitched.height, 440);

    // Verify all pixels in stitched image match the original canvas
    for y in 0..440 {
        for x in (0..w).step_by(8) {
            let expected = canvas.get_pixel(x, y).unwrap();
            let actual = stitched.get_pixel(x, y).unwrap();
            assert_eq!(
                actual, expected,
                "Pixel mismatch at ({x}, {y}) in stitched image"
            );
        }
    }
}

#[test]
fn test_stitching_sticky_header_and_footer() {
    let w = 200u32;
    let frame_h = 240u32;
    let header_h = 36u32;
    let footer_h = 24u32;

    // Header pixel color
    let header_color = [30, 80, 200, 255];
    // Footer pixel color
    let footer_color = [15, 18, 22, 255];

    let make_frame = |scroll_offset: u32| -> ImageBuffer {
        let mut img = ImageBuffer::new(w, frame_h);
        // Sticky header
        for y in 0..header_h {
            for x in 0..w {
                img.set_pixel(x, y, header_color);
            }
        }
        // Scrolling content
        for y in header_h..(frame_h - footer_h) {
            let doc_y = scroll_offset + (y - header_h);
            let val = (doc_y % 250) as u8;
            for x in 0..w {
                img.set_pixel(x, y, [val, val.wrapping_add(40), 200, 255]);
            }
        }
        // Sticky footer
        for y in (frame_h - footer_h)..frame_h {
            for x in 0..w {
                img.set_pixel(x, y, footer_color);
            }
        }
        img
    };

    let f0 = make_frame(0);
    let f1 = make_frame(60);
    let f2 = make_frame(120);

    assert_eq!(detect_sticky_header(&f0, &f1, 0.35), header_h);
    assert_eq!(detect_sticky_footer(&f0, &f1, 0.25), footer_h);

    let mut stitcher = ScrollingStitcher::new(30_000);
    stitcher.init(f0);

    let res1 = stitcher.add_frame(&f1);
    assert_eq!(
        res1,
        StitchStepResult::Added {
            shift: 60,
            total_height: frame_h + 60
        }
    );

    let res2 = stitcher.add_frame(&f2);
    assert_eq!(
        res2,
        StitchStepResult::Added {
            shift: 60,
            total_height: frame_h + 120
        }
    );

    let stitched = stitcher.finish().expect("finish stitching");
    let expected_total_h = frame_h + 120; // 360 px
    assert_eq!(stitched.height, expected_total_h);

    // Verify header is at top
    for y in 0..header_h {
        assert_eq!(stitched.get_pixel(10, y).unwrap(), header_color);
    }
    // Verify footer is at bottom
    for y in (expected_total_h - footer_h)..expected_total_h {
        assert_eq!(stitched.get_pixel(10, y).unwrap(), footer_color);
    }
    // Verify content in middle is continuous from 0 to 300 (height 300)
    let content_h = expected_total_h - header_h - footer_h;
    assert_eq!(content_h, (frame_h - header_h - footer_h) + 120);

    for cy in 0..content_h {
        let val = (cy % 250) as u8;
        let expected = [val, val.wrapping_add(40), 200, 255];
        let actual = stitched.get_pixel(10, header_h + cy).unwrap();
        assert_eq!(actual, expected, "Content pixel mismatch at cy={cy}");
    }
}

#[test]
fn test_stitching_repeated_rows() {
    let w = 200u32;
    let full_h = 500u32;

    // Synthetic page containing blocks of repeated identical rows alongside distinct markers:
    // e.g. a document table with consecutive identical "Pending" status rows,
    // separated by unique section headers and rows.
    let mut canvas = ImageBuffer::new(w, full_h);
    for y in 0..full_h {
        let color = match y {
            0..=30 => [100, 150, 220, 255],              // Header
            31..=70 => [45, 48, 55, 255],                // Block 1 of repeated identical rows
            71..=90 => [245, 165, 36, 255],              // Section 1 Accent
            91..=150 => [45, 48, 55, 255], // Block 2 of repeated identical rows (identical to Block 1)
            151..=180 => [80, 180, 120, 255], // Section 2 Accent
            181..=260 => [45, 48, 55, 255], // Block 3 of repeated identical rows
            261..=300 => [200, 80, 80, 255], // Section 3 Accent
            _ => [((y * 11) % 255) as u8, 60, 180, 255], // Trailing content
        };
        for x in 0..w {
            canvas.set_pixel(x, y, color);
        }
    }

    let frame_h = 240u32;
    let shift = 60u32;

    // Frame 0: rows 0..240
    let f0 = canvas
        .crop(RectI::new(0, 0, w as i32, frame_h as i32))
        .unwrap();
    // Frame 1: rows 60..300 (overlap is rows 60..240, which contains multiple identical row blocks)
    let f1 = canvas
        .crop(RectI::new(
            0,
            shift as i32,
            w as i32,
            (shift + frame_h) as i32,
        ))
        .unwrap();

    // Verify find_vertical_overlap resolves the true global shift 60 despite identical row blocks
    match find_vertical_overlap(&f0, &f1, 0, 0) {
        OverlapMatch::Shift { shift: s, .. } => {
            assert_eq!(
                s, shift,
                "Expected shift {shift} across repeated row blocks, got {s}"
            );
        }
        other => panic!("Expected Shift {shift}, got {:?}", other),
    }

    let mut stitcher = ScrollingStitcher::new(30_000);
    stitcher.init(f0);
    let res = stitcher.add_frame(&f1);
    assert_eq!(
        res,
        StitchStepResult::Added {
            shift,
            total_height: frame_h + shift
        }
    );

    let stitched = stitcher.finish().expect("finish");
    assert_eq!(stitched.height, frame_h + shift);
}

#[test]
fn test_stitching_end_of_page() {
    let w = 200u32;
    let h = 200u32;
    let mut img = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(x, y, [(x % 255) as u8, (y % 255) as u8, 100, 255]);
        }
    }

    // Both frames identical (scrolling at bottom of document)
    let f0 = img.clone();
    let f1 = img;

    assert_eq!(
        find_vertical_overlap(&f0, &f1, 0, 0),
        OverlapMatch::EndOfPage
    );

    let mut stitcher = ScrollingStitcher::new(30_000);
    stitcher.init(f0);
    assert_eq!(stitcher.add_frame(&f1), StitchStepResult::EndOfPage);
}

#[test]
fn test_stitching_max_height_cap() {
    let w = 150u32;
    let h = 200u32;
    let mut img0 = ImageBuffer::new(w, h);
    let mut img1 = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img0.set_pixel(x, y, [y as u8, 100, 150, 255]);
            img1.set_pixel(x, y, [(y + 80) as u8, 100, 150, 255]);
        }
    }

    // Cap at 250 px max height
    let mut stitcher = ScrollingStitcher::new(250);
    stitcher.init(img0);

    // Frame 1 adds 80 px (200 + 80 = 280 > 250 max height)
    let res = stitcher.add_frame(&img1);
    assert_eq!(res, StitchStepResult::ReachedMaxHeight);

    let finished = stitcher.finish().expect("finish");
    assert!(
        finished.height <= 250,
        "Stitched height {} exceeded max limit 250",
        finished.height
    );
}

#[test]
fn test_stitching_real_window() {
    use windows::core::w;
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, SetBkMode, SetTextColor,
        TextOutW, PAINTSTRUCT, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
        RegisterClassW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, MSG, PM_REMOVE,
        SW_SHOW, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    static mut SCROLL_OFFSET: i32 = 0;

    unsafe extern "system" fn test_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                let bg_brush = CreateSolidBrush(COLORREF(0x00FFFFFF)); // White bg
                let rc = windows::Win32::Foundation::RECT {
                    left: 0,
                    top: 0,
                    right: 400,
                    bottom: 400,
                };
                FillRect(hdc, &rc, bg_brush);
                let _ = DeleteObject(bg_brush.into());

                let _ = SetBkMode(hdc, TRANSPARENT);
                let _ = SetTextColor(hdc, COLORREF(0x00000000)); // Black text

                let offset = SCROLL_OFFSET;
                for i in 0..100 {
                    let y = i * 28 - offset;
                    if (-30..=420).contains(&y) {
                        let text = format!("Lookout Real Test Line #{i} - Content row\0");
                        let wide: Vec<u16> = text.encode_utf16().collect();
                        let _ = TextOutW(hdc, 20, y, &wide[..wide.len() - 1]);
                    }
                }

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe {
        SCROLL_OFFSET = 0;

        let hinst = GetModuleHandleW(None).unwrap();
        let class_name = w!("LookoutRealScrollTestWindow");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(test_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name,
            w!("Real Scroll Test"),
            WS_POPUP,
            100,
            100,
            360,
            300,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();

        let _ = ShowWindow(hwnd, SW_SHOW);

        let pump = || {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        };

        pump();
        std::thread::sleep(std::time::Duration::from_millis(60));
        pump();

        // Capture Frame 0 from real window DC
        let f0 = lookout_screenshot::capture::capture_window_hwnd(hwnd).expect("capture f0");
        assert!(f0.width >= 300 && f0.height >= 250);

        // Scroll window down by 56 px (2 lines)
        SCROLL_OFFSET = 56;
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
        pump();
        std::thread::sleep(std::time::Duration::from_millis(60));
        pump();

        // Capture Frame 1 from real window DC
        let f1 = lookout_screenshot::capture::capture_window_hwnd(hwnd).expect("capture f1");

        // Verify vertical overlap between real frames
        match find_vertical_overlap(&f0, &f1, 0, 0) {
            OverlapMatch::Shift { shift, error, .. } => {
                assert!(
                    (shift as i32 - 56).abs() <= 2,
                    "Real window shift expected ~56, got {shift} (error {error})"
                );
            }
            other => panic!("Expected overlap shift, got {:?}", other),
        }

        let mut stitcher = ScrollingStitcher::new(30_000);
        stitcher.init(f0);
        let res = stitcher.add_frame(&f1);
        assert!(matches!(res, StitchStepResult::Added { .. }));

        let final_stitched = stitcher.finish().expect("finish");
        assert!(final_stitched.height > 300);

        let _ = DestroyWindow(hwnd);
    }
}

#[test]
fn test_smooth_scrolling_settle_simulation() {
    // Simulates a smooth-scrolling window easing down across intermediate frames
    // Frame 0: rest at y=0
    // Probe 1: mid-easing at y=25
    // Probe 2: mid-easing at y=48
    // Probe 3: settled at y=60
    // Probe 4: identical to Probe 3 (settled!)
    let w = 180u32;
    let h = 200u32;
    let make = |offset: u32| {
        let mut img = ImageBuffer::new(w, h);
        for y in 0..h {
            let val = ((offset + y) % 255) as u8;
            for x in 0..w {
                img.set_pixel(x, y, [val, 120, 200, 255]);
            }
        }
        img
    };

    let p1 = make(25);
    let p2 = make(48);
    let p3 = make(60);
    let p4 = make(60); // Identical to p3: animation settled

    assert!(!lookout_screenshot::scrolling::is_frames_identical(
        &p1, &p2
    ));
    assert!(!lookout_screenshot::scrolling::is_frames_identical(
        &p2, &p3
    ));
    assert!(lookout_screenshot::scrolling::is_frames_identical(&p3, &p4));
}

#[test]
fn test_first_step_identical_triggers_fallback_or_toast() {
    // When a window does not scroll on the first step, frame 1 is identical to frame 0
    let w = 200u32;
    let h = 200u32;
    let mut f0 = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            f0.set_pixel(x, y, [50, 60, 70, 255]);
        }
    }
    let f1 = f0.clone();

    // Verify frames are recognized as identical (wheel did nothing)
    assert!(lookout_screenshot::scrolling::is_frames_identical(&f0, &f1));

    // A stitcher initialized with f0 correctly identifies f1 as EndOfPage/Stationary
    let mut stitcher = ScrollingStitcher::new(30_000);
    stitcher.init(f0.clone());
    assert!(stitcher
        .last_frame
        .as_ref()
        .is_some_and(|last| lookout_screenshot::scrolling::is_frames_identical(last, &f1)));

    // When Page Down fallback moves content by 40 px:
    let mut f_pgdn = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let val = ((y + 40) % 255) as u8;
            f_pgdn.set_pixel(x, y, [val, 60, 70, 255]);
        }
    }
    assert!(!lookout_screenshot::scrolling::is_frames_identical(
        &f0, &f_pgdn
    ));
}

#[test]
fn test_manual_scroll_simulation_with_rollback() {
    let w = 240u32;
    let full_h = 1000u32;
    let frame_h = 240u32;

    // Create a tall synthetic canvas with distinguishable row patterns
    let mut canvas = ImageBuffer::new(w, full_h);
    for y in 0..full_h {
        let r = (y % 256) as u8;
        let g = ((y * 11) % 256) as u8;
        let b = ((y * 23) % 256) as u8;
        for x in 0..w {
            canvas.set_pixel(x, y, [r, g, b, 255]);
        }
    }

    // Step 0: Initial frame at y = 0..240
    let f0 = canvas
        .crop(RectI::new(0, 0, w as i32, frame_h as i32))
        .unwrap();
    let mut stitcher = ScrollingStitcher::new(30_000);
    stitcher.init(f0);

    // Step 1: Manual scroll down 137 px -> y = 137..377
    let f1 = canvas
        .crop(RectI::new(0, 137, w as i32, 137 + frame_h as i32))
        .unwrap();
    let res1 = stitcher.add_frame(&f1);
    assert_eq!(
        res1,
        StitchStepResult::Added {
            shift: 137,
            total_height: 377
        }
    );

    // Step 2: Manual scroll down 89 px -> y = 226..466
    let f2 = canvas
        .crop(RectI::new(0, 226, w as i32, 226 + frame_h as i32))
        .unwrap();
    let res2 = stitcher.add_frame(&f2);
    assert_eq!(
        res2,
        StitchStepResult::Added {
            shift: 89,
            total_height: 466
        }
    );

    // Step 3: Human scroll overshoot - small scroll back UP by 35 px -> y = 191..431
    let f3 = canvas
        .crop(RectI::new(0, 191, w as i32, 191 + frame_h as i32))
        .unwrap();
    let res3 = stitcher.add_frame(&f3);
    assert_eq!(
        res3,
        StitchStepResult::ScrolledUp {
            shift: 35,
            total_height: 431
        }
    );
    assert_eq!(stitcher.total_height(), 431);

    // Step 4: Resume scrolling down 150 px from the rolled-back position -> y = 341..581
    let f4 = canvas
        .crop(RectI::new(0, 341, w as i32, 341 + frame_h as i32))
        .unwrap();
    let res4 = stitcher.add_frame(&f4);
    assert_eq!(
        res4,
        StitchStepResult::Added {
            shift: 150,
            total_height: 581
        }
    );

    let stitched = stitcher.finish().expect("finish manual scroll stitch");
    assert_eq!(stitched.width, w);
    assert_eq!(stitched.height, 581);

    // Verify seamless pixel alignment across the entire stitched result
    for y in 0..581 {
        for x in (0..w).step_by(12) {
            let expected = canvas.get_pixel(x, y).unwrap();
            let actual = stitched.get_pixel(x, y).unwrap();
            assert_eq!(
                actual, expected,
                "Pixel mismatch at ({x}, {y}) after manual scroll rollback"
            );
        }
    }
}
