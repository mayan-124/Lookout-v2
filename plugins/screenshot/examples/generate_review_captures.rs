//! Generates review screenshots of real Win32 windows.
//! Run with: cargo run --example generate_review_captures --features review

#[cfg(feature = "review")]
fn main() {
    use lookout_screenshot::annotation::{
        draw_system_text_bgra, Annotation, AnnotationModel, Color,
    };
    use lookout_screenshot::capture::{
        capture_virtual_desktop, capture_window_hwnd, enable_per_monitor_dpi_v2, CaptureResult,
    };
    use lookout_screenshot::editor::{
        get_button_rect_scaled, get_toolbar_rect_scaled, open_real_editor_for_demo_scaled,
        EDITOR_SESSION,
    };
    use lookout_screenshot::geometry::{PointI, RectI};
    use lookout_screenshot::image::ImageBuffer;
    use lookout_screenshot::overlay::{
        open_real_overlay_for_demo, open_real_overlay_for_demo_scaled,
    };
    use std::path::PathBuf;
    use windows::Win32::Graphics::Gdi::{InvalidateRect, UpdateWindow};
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyWindow, DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };

    unsafe fn pump_messages_briefly(ms: u64) {
        let start = std::time::Instant::now();
        let mut msg = MSG::default();
        while start.elapsed().as_millis() < ms as u128 {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    enable_per_monitor_dpi_v2();

    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // Default to artifacts directory if available, else temp dir
            let artifact_dir =
                PathBuf::from(std::env::var("LOOKOUT_REVIEW_DIR").unwrap_or_else(|_| {
                    std::env::temp_dir()
                        .join("lookout-review")
                        .display()
                        .to_string()
                }));
            if artifact_dir.parent().map(|p| p.exists()).unwrap_or(false) {
                artifact_dir
            } else {
                std::env::temp_dir().join("lookout-capture-review-screenshots")
            }
        });

    let _ = std::fs::create_dir_all(&out_dir);
    println!("Capturing real windows to: {}", out_dir.display());

    // 1. Selection overlay during a drag over a real window (100% and 150% scaling)
    println!("Capturing selection overlay (100% and 150%)...");
    let hwnd_ov = open_real_overlay_for_demo(PointI::new(260, 160), PointI::new(1180, 680))
        .expect("Failed to open overlay 100%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ov_img = capture_window_hwnd(hwnd_ov).expect("Failed to capture overlay 100%");
    ov_img
        .save_png(&out_dir.join("selection_overlay.png"))
        .expect("Failed to save selection_overlay.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ov);
        pump_messages_briefly(50);
    }

    let hwnd_ov_125 =
        open_real_overlay_for_demo_scaled(PointI::new(260, 160), PointI::new(1180, 680), 1.25)
            .expect("Failed to open overlay 125%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ov_img_125 = capture_window_hwnd(hwnd_ov_125).expect("Failed to capture overlay 125%");
    ov_img_125
        .save_png(&out_dir.join("selection_overlay_125.png"))
        .expect("Failed to save selection_overlay_125.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ov_125);
        pump_messages_briefly(50);
    }

    let hwnd_ov_150 =
        open_real_overlay_for_demo_scaled(PointI::new(260, 160), PointI::new(1180, 680), 1.5)
            .expect("Failed to open overlay 150%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ov_img_150 = capture_window_hwnd(hwnd_ov_150).expect("Failed to capture overlay 150%");
    ov_img_150
        .save_png(&out_dir.join("selection_overlay_150.png"))
        .expect("Failed to save selection_overlay_150.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ov_150);
        pump_messages_briefly(50);
    }

    let hwnd_ov_175 =
        open_real_overlay_for_demo_scaled(PointI::new(260, 160), PointI::new(1180, 680), 1.75)
            .expect("Failed to open overlay 175%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ov_img_175 = capture_window_hwnd(hwnd_ov_175).expect("Failed to capture overlay 175%");
    ov_img_175
        .save_png(&out_dir.join("selection_overlay_175.png"))
        .expect("Failed to save selection_overlay_175.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ov_175);
        pump_messages_briefly(50);
    }

    // 2. Editor open on a real capture with each tool used once
    println!("Preparing capture image and annotations...");
    let capture_res = capture_virtual_desktop().unwrap_or_else(|_| {
        let w = 1280;
        let h = 720;
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

    let mut base_img = capture_res.image;
    if base_img.width > 960 || base_img.height > 540 {
        let cw = 960.min(base_img.width);
        let ch = 540.min(base_img.height);
        if let Ok(cropped) = base_img.crop(RectI::new(0, 0, cw as i32, ch as i32)) {
            base_img = cropped;
        }
    }

    let mut model = AnnotationModel::new();
    // 1. Arrow
    model.push(Annotation::Arrow {
        start: PointI::new(140, 80),
        end: PointI::new(280, 130),
        color: Color::AMBER,
        thickness: 3.0,
    });
    // 2. Line
    model.push(Annotation::Line {
        start: PointI::new(100, 180),
        end: PointI::new(340, 180),
        color: Color::BLUE,
        thickness: 2.0,
    });
    // 3. Rectangle
    model.push(Annotation::Rectangle {
        rect: RectI::new(260, 140, 620, 240),
        color: Color::GREEN,
        thickness: 2.0,
        fill: None,
    });
    // 4. Ellipse
    model.push(Annotation::Ellipse {
        rect: RectI::new(660, 140, 880, 250),
        color: Color::RED,
        thickness: 2.0,
        fill: None,
    });
    // 5. Highlighter (semi-transparent ~38% multiply blend)
    let mut highlight_pts = Vec::new();
    for x in (100..460).step_by(10) {
        highlight_pts.push(PointI::new(x, 280));
    }
    model.push(Annotation::Highlighter {
        points: highlight_pts,
        color: Color::rgba(245, 165, 36, 120),
        thickness: 20.0,
    });
    // 6. Text tool in two sizes, mixed case & Unicode (No "CleanShot" text)
    model.push(Annotation::Text {
        pos: PointI::new(420, 260),
        text: "Text Tool • Annotation & Café".to_string(),
        color: Color::WHITE,
        size: 22,
        bold: true,
    });
    model.push(Annotation::Text {
        pos: PointI::new(420, 295),
        text: "Unicode: नमस्ते / 東京 / 한국어".to_string(),
        color: Color::AMBER,
        size: 16,
        bold: false,
    });
    // 7. Step Markers (1, 2, 3)
    model.push(Annotation::StepMarker {
        pos: PointI::new(120, 360),
        number: 1,
        color: Color::AMBER,
        radius: 12.0,
    });
    model.push(Annotation::StepMarker {
        pos: PointI::new(240, 360),
        number: 2,
        color: Color::AMBER,
        radius: 12.0,
    });
    model.push(Annotation::StepMarker {
        pos: PointI::new(360, 360),
        number: 3,
        color: Color::AMBER,
        radius: 12.0,
    });
    // 8. Blur
    let blur_r = RectI::new(480, 340, 780, 390);
    let saved_px = base_img.get_rect_pixels(blur_r);
    model.push(Annotation::Blur {
        rect: blur_r,
        cell_size: 10,
        saved_pixels: saved_px,
    });

    // 2a. Editor at 100% scaling (WHOLE window)
    println!("Capturing editor at 100% scaling...");
    let hwnd_ed = open_real_editor_for_demo_scaled(base_img.clone(), model.clone(), 1.0, None)
        .expect("Failed to open editor 100%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ed_img = capture_window_hwnd(hwnd_ed).expect("Failed to capture editor 100%");
    ed_img
        .save_png(&out_dir.join("editor_tools.png"))
        .expect("Failed to save editor_tools.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ed);
        pump_messages_briefly(50);
    }

    // 2b. Editor at 125% scaling (WHOLE window)
    println!("Capturing editor at 125% scaling...");
    let hwnd_ed_125 = open_real_editor_for_demo_scaled(base_img.clone(), model.clone(), 1.25, None)
        .expect("Failed to open editor 125%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ed_img_125 = capture_window_hwnd(hwnd_ed_125).expect("Failed to capture editor 125%");
    ed_img_125
        .save_png(&out_dir.join("editor_tools_125.png"))
        .expect("Failed to save editor_tools_125.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ed_125);
        pump_messages_briefly(50);
    }

    // 2c. Editor at 150% scaling (WHOLE window)
    println!("Capturing editor at 150% scaling...");
    let hwnd_ed_150 = open_real_editor_for_demo_scaled(base_img.clone(), model.clone(), 1.5, None)
        .expect("Failed to open editor 150%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ed_img_150 = capture_window_hwnd(hwnd_ed_150).expect("Failed to capture editor 150%");
    ed_img_150
        .save_png(&out_dir.join("editor_tools_150.png"))
        .expect("Failed to save editor_tools_150.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ed_150);
        pump_messages_briefly(50);
    }

    // 2d. Editor at 175% scaling (WHOLE window)
    println!("Capturing editor at 175% scaling...");
    let hwnd_ed_175 = open_real_editor_for_demo_scaled(base_img.clone(), model.clone(), 1.75, None)
        .expect("Failed to open editor 175%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ed_img_175 = capture_window_hwnd(hwnd_ed_175).expect("Failed to capture editor 175%");
    ed_img_175
        .save_png(&out_dir.join("editor_tools_175.png"))
        .expect("Failed to save editor_tools_175.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ed_175);
        pump_messages_briefly(50);
    }

    // 2e. Editor at 200% scaling (WHOLE window)
    println!("Capturing editor at 200% scaling...");
    let hwnd_ed_200 = open_real_editor_for_demo_scaled(base_img.clone(), model.clone(), 2.0, None)
        .expect("Failed to open editor 200%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ed_img_200 = capture_window_hwnd(hwnd_ed_200).expect("Failed to capture editor 200%");
    ed_img_200
        .save_png(&out_dir.join("editor_tools_200.png"))
        .expect("Failed to save editor_tools_200.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ed_200);
        pump_messages_briefly(50);
    }

    // 2d. Editor at narrow window (~700 px) demonstrating multi-row wrapping (WHOLE window)
    println!("Capturing editor at narrow window (~700 px, 100% and 150%)...");
    let hwnd_ed_narrow =
        open_real_editor_for_demo_scaled(base_img.clone(), model.clone(), 1.0, Some((700, 620)))
            .expect("Failed to open editor narrow 100%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ed_img_narrow =
        capture_window_hwnd(hwnd_ed_narrow).expect("Failed to capture editor narrow 100%");
    ed_img_narrow
        .save_png(&out_dir.join("editor_narrow_700.png"))
        .expect("Failed to save editor_narrow_700.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ed_narrow);
        pump_messages_briefly(50);
    }

    let hwnd_ed_narrow_150 =
        open_real_editor_for_demo_scaled(base_img.clone(), model.clone(), 1.5, Some((700, 620)))
            .expect("Failed to open editor narrow 150%");
    unsafe {
        pump_messages_briefly(200);
    }
    let ed_img_narrow_150 =
        capture_window_hwnd(hwnd_ed_narrow_150).expect("Failed to capture editor narrow 150%");
    ed_img_narrow_150
        .save_png(&out_dir.join("editor_narrow_700_150.png"))
        .expect("Failed to save editor_narrow_700_150.png");
    unsafe {
        let _ = DestroyWindow(hwnd_ed_narrow_150);
        pump_messages_briefly(50);
    }

    // 3. Blur result as saved PNG zoomed in (pure pixels without synthetic border)
    println!("Saving blurred capture and zoomed blur result...");
    let mut annotated_for_save = base_img;
    model.render_onto(&mut annotated_for_save);
    annotated_for_save
        .save_png(&out_dir.join("saved_capture_blurred.png"))
        .expect("Failed to save saved_capture_blurred.png");

    let crop_x = (blur_r.left - 20).max(0);
    let crop_y = (blur_r.top - 20).max(0);
    let crop_w = (blur_r.width() + 40).min(annotated_for_save.width as i32 - crop_x) as u32;
    let crop_h = (blur_r.height() + 40).min(annotated_for_save.height as i32 - crop_y) as u32;

    if let Ok(cropped_blur) = annotated_for_save.crop(RectI::new(
        crop_x,
        crop_y,
        crop_x + crop_w as i32,
        crop_y + crop_h as i32,
    )) {
        let zoom = 3u32;
        let zw = crop_w * zoom;
        let zh = crop_h * zoom;
        let mut zoomed = ImageBuffer::new(zw, zh);
        for zy in 0..zh {
            for zx in 0..zw {
                if let Some(p) = cropped_blur.get_pixel(zx / zoom, zy / zoom) {
                    zoomed.set_pixel(zx, zy, p);
                }
            }
        }
        zoomed
            .save_png(&out_dir.join("blur_result.png"))
            .expect("Failed to save blur_result.png");
    }

    // 3b. One clear before/after of a blurred word
    println!("Generating before/after of a blurred word...");
    let word_w = 400u32;
    let word_h = 120u32;
    let mut word_img = ImageBuffer::new(word_w, word_h);
    for y in 0..word_h {
        for x in 0..word_w {
            word_img.set_pixel(x, y, [30, 34, 42, 255]);
        }
    }
    for y in 15..105 {
        for x in 20..380 {
            word_img.set_pixel(x, y, [38, 43, 51, 255]);
        }
    }
    draw_system_text_bgra(
        &mut word_img.pixels,
        word_w as i32,
        word_h as i32,
        PointI::new(35, 24),
        "Secret Key:",
        Color::rgb(160, 170, 185),
        13,
        false,
    );
    draw_system_text_bgra(
        &mut word_img.pixels,
        word_w as i32,
        word_h as i32,
        PointI::new(35, 48),
        "CONFIDENTIAL-KEY-9842",
        Color::AMBER,
        22,
        true,
    );

    word_img
        .save_png(&out_dir.join("word_before_blur.png"))
        .expect("Failed to save word_before_blur.png");

    let mut blurred_word_img = word_img.clone();
    blurred_word_img.pixelate(RectI::new(32, 45, 368, 80), 8);
    blurred_word_img
        .save_png(&out_dir.join("word_after_blur.png"))
        .expect("Failed to save word_after_blur.png");

    let side_w = word_w * 2 + 30;
    let side_h = word_h + 36;
    let mut side_img = ImageBuffer::new(side_w, side_h);
    for y in 0..side_h {
        for x in 0..side_w {
            side_img.set_pixel(x, y, [20, 23, 28, 255]);
        }
    }
    draw_system_text_bgra(
        &mut side_img.pixels,
        side_w as i32,
        side_h as i32,
        PointI::new(15, 8),
        "BEFORE BLUR",
        Color::rgb(200, 205, 215),
        14,
        true,
    );
    draw_system_text_bgra(
        &mut side_img.pixels,
        side_w as i32,
        side_h as i32,
        PointI::new((word_w + 30) as i32, 8),
        "AFTER BLUR (REDACTED)",
        Color::AMBER,
        14,
        true,
    );
    for y in 0..word_h {
        for x in 0..word_w {
            if let Some(p) = word_img.get_pixel(x, y) {
                side_img.set_pixel(x + 10, y + 28, p);
            }
            if let Some(p) = blurred_word_img.get_pixel(x, y) {
                side_img.set_pixel(x + word_w + 20, y + 28, p);
            }
        }
    }
    side_img
        .save_png(&out_dir.join("word_blur_before_after.png"))
        .expect("Failed to save word_blur_before_after.png");

    // 4. Toolbar tooltips (100% and 150%)
    println!("Capturing toolbar tooltips (100% and 150%)...");
    let hwnd_tt =
        open_real_editor_for_demo_scaled(annotated_for_save.clone(), model.clone(), 1.0, None)
            .expect("Failed to open editor for tooltip 100%");
    EDITOR_SESSION.with(|s| {
        if let Some(sess) = s.borrow_mut().as_mut() {
            sess.hovered_btn = Some(6); // Step Marker
            let tb = get_toolbar_rect_scaled(1060, 1.0);
            let b_rect = get_button_rect_scaled(tb, 6, 1.0);
            sess.tooltip = Some((
                "Step Marker (S)".to_string(),
                PointI::new(b_rect.center_x(), b_rect.bottom + 8),
            ));
        }
    });
    unsafe {
        let _ = InvalidateRect(Some(hwnd_tt), None, false);
        let _ = UpdateWindow(hwnd_tt);
        pump_messages_briefly(150);
    }
    let tt_img = capture_window_hwnd(hwnd_tt).expect("Failed to capture tooltip 100%");
    tt_img
        .save_png(&out_dir.join("toolbar_tooltips.png"))
        .expect("Failed to save toolbar_tooltips.png");
    unsafe {
        let _ = DestroyWindow(hwnd_tt);
        pump_messages_briefly(50);
    }

    let hwnd_tt_150 =
        open_real_editor_for_demo_scaled(annotated_for_save.clone(), model.clone(), 1.5, None)
            .expect("Failed to open editor for tooltip 150%");
    EDITOR_SESSION.with(|s| {
        if let Some(sess) = s.borrow_mut().as_mut() {
            sess.hovered_btn = Some(6); // Step Marker
            let tb = get_toolbar_rect_scaled(1590, 1.5);
            let b_rect = get_button_rect_scaled(tb, 6, 1.5);
            sess.tooltip = Some((
                "Step Marker (S)".to_string(),
                PointI::new(b_rect.center_x(), b_rect.bottom + 12),
            ));
        }
    });
    unsafe {
        let _ = InvalidateRect(Some(hwnd_tt_150), None, false);
        let _ = UpdateWindow(hwnd_tt_150);
        pump_messages_briefly(150);
    }
    let tt_img_150 = capture_window_hwnd(hwnd_tt_150).expect("Failed to capture tooltip 150%");
    tt_img_150
        .save_png(&out_dir.join("toolbar_tooltips_150.png"))
        .expect("Failed to save toolbar_tooltips_150.png");
    unsafe {
        let _ = DestroyWindow(hwnd_tt_150);
        pump_messages_briefly(50);
    }

    // 6. Scrolling capture: floating stop pill and stitched document in editor
    println!("Capturing scrolling stop pill and stitched editor result...");
    let stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pill_rect = RectI::new(200, 200, 800, 700);
    if let Some(pill) = lookout_screenshot::scrolling::FloatingStopPill::show(pill_rect, stop_flag)
    {
        unsafe {
            pump_messages_briefly(150);
        }
        let pill_img = capture_window_hwnd(pill.hwnd).expect("capture pill");
        pill_img
            .save_png(&out_dir.join("scrolling_stop_pill.png"))
            .expect("save scrolling_stop_pill.png");
        pill.close();
        unsafe {
            pump_messages_briefly(50);
        }
    }

    // Create a realistic stitched document: header + multiple content sections + footer
    let doc_w = 900u32;
    let doc_h = 2400u32;
    let mut stitched_doc = ImageBuffer::new(doc_w, doc_h);
    // Dark document background
    for y in 0..doc_h {
        for x in 0..doc_w {
            stitched_doc.set_pixel(x, y, [24, 27, 34, 255]);
        }
    }
    // Sticky header
    for y in 0..60 {
        for x in 0..doc_w {
            stitched_doc.set_pixel(x, y, [18, 20, 24, 255]);
        }
    }
    lookout_screenshot::annotation::draw_system_text_rgba(
        &mut stitched_doc,
        PointI::new(28, 18),
        "LOOKOUT DOCUMENTATION - SCROLLING CAPTURE RESULT (2,400 PX)",
        Color::rgb(245, 165, 36),
        15,
        true,
    );

    // Content cards
    for sec in 0..6 {
        let top = 80 + sec * 370;
        let card = RectI::new(30, top, (doc_w as i32) - 30, top + 340);
        lookout_screenshot::annotation::fill_rounded_rect_rgba(
            &mut stitched_doc,
            card,
            10,
            Color::rgb(30, 34, 42),
        );
        lookout_screenshot::annotation::stroke_rounded_rect_rgba(
            &mut stitched_doc,
            card,
            10,
            Color::rgb(50, 56, 68),
        );
        lookout_screenshot::annotation::draw_system_text_rgba(
            &mut stitched_doc,
            PointI::new(50, top + 24),
            &format!("Section #{}: Stitched Page Flow & Architecture", sec + 1),
            Color::rgb(240, 243, 248),
            16,
            true,
        );
        lookout_screenshot::annotation::draw_system_text_rgba(
            &mut stitched_doc,
            PointI::new(50, top + 64),
            "Automated vertical stitching aligns incoming frames seamlessly using pixel mean absolute difference.",
            Color::rgb(180, 186, 196),
            13,
            false,
        );
    }

    let hwnd_scroll_editor = open_real_editor_for_demo_scaled(
        stitched_doc,
        AnnotationModel::default(),
        1.0,
        Some((1100, 750)),
    )
    .expect("open scrolling editor");

    unsafe {
        pump_messages_briefly(200);
    }
    let scroll_editor_img =
        capture_window_hwnd(hwnd_scroll_editor).expect("capture scrolling editor");
    scroll_editor_img
        .save_png(&out_dir.join("scrolling_editor_result.png"))
        .expect("save scrolling_editor_result.png");
    unsafe {
        let _ = DestroyWindow(hwnd_scroll_editor);
        pump_messages_briefly(50);
    }

    // 7. Screen recording: floating control bar and interactive result card at 100%, 150%, and 200%
    println!("Capturing screen recording control bar and result card (100%, 150%, 200%)...");

    // 7a. Control bar at 100%
    let hwnd_bar_100 = lookout_screenshot::recording::open_real_control_bar_for_demo(1.0)
        .expect("open control bar 100%");
    unsafe {
        pump_messages_briefly(200);
    }
    let bar_img_100 = capture_window_hwnd(hwnd_bar_100).expect("capture control bar 100%");
    bar_img_100
        .save_png(&out_dir.join("recording_control_bar_100.png"))
        .expect("save recording_control_bar_100.png");
    unsafe {
        let _ = DestroyWindow(hwnd_bar_100);
        pump_messages_briefly(50);
    }

    // 7b. Control bar at 150%
    let hwnd_bar_150 = lookout_screenshot::recording::open_real_control_bar_for_demo(1.5)
        .expect("open control bar 150%");
    unsafe {
        pump_messages_briefly(200);
    }
    let bar_img_150 = capture_window_hwnd(hwnd_bar_150).expect("capture control bar 150%");
    bar_img_150
        .save_png(&out_dir.join("recording_control_bar_150.png"))
        .expect("save recording_control_bar_150.png");
    unsafe {
        let _ = DestroyWindow(hwnd_bar_150);
        pump_messages_briefly(50);
    }

    // 7c. Control bar at 200%
    let hwnd_bar_200 = lookout_screenshot::recording::open_real_control_bar_for_demo(2.0)
        .expect("open control bar 200%");
    unsafe {
        pump_messages_briefly(200);
    }
    let bar_img_200 = capture_window_hwnd(hwnd_bar_200).expect("capture control bar 200%");
    bar_img_200
        .save_png(&out_dir.join("recording_control_bar_200.png"))
        .expect("save recording_control_bar_200.png");
    unsafe {
        let _ = DestroyWindow(hwnd_bar_200);
        pump_messages_briefly(50);
    }

    // 7d. Result card at 100%
    let sample_mp4_path = out_dir.join("Recording 2026-11-21 23-45-00.mp4");
    let hwnd_card_100 =
        lookout_screenshot::recording::open_real_result_card_for_demo(1.0, sample_mp4_path.clone())
            .expect("open result card 100%");
    unsafe {
        pump_messages_briefly(200);
    }
    let card_img_100 = capture_window_hwnd(hwnd_card_100).expect("capture result card 100%");
    card_img_100
        .save_png(&out_dir.join("recording_result_card_100.png"))
        .expect("save recording_result_card_100.png");
    unsafe {
        let _ = DestroyWindow(hwnd_card_100);
        pump_messages_briefly(50);
    }

    // 7e. Result card at 150%
    let hwnd_card_150 =
        lookout_screenshot::recording::open_real_result_card_for_demo(1.5, sample_mp4_path.clone())
            .expect("open result card 150%");
    unsafe {
        pump_messages_briefly(200);
    }
    let card_img_150 = capture_window_hwnd(hwnd_card_150).expect("capture result card 150%");
    card_img_150
        .save_png(&out_dir.join("recording_result_card_150.png"))
        .expect("save recording_result_card_150.png");
    unsafe {
        let _ = DestroyWindow(hwnd_card_150);
        pump_messages_briefly(50);
    }

    // 7f. Result card at 200%
    let hwnd_card_200 =
        lookout_screenshot::recording::open_real_result_card_for_demo(2.0, sample_mp4_path)
            .expect("open result card 200%");
    unsafe {
        pump_messages_briefly(200);
    }
    let card_img_200 = capture_window_hwnd(hwnd_card_200).expect("capture result card 200%");
    card_img_200
        .save_png(&out_dir.join("recording_result_card_200.png"))
        .expect("save recording_result_card_200.png");
    unsafe {
        let _ = DestroyWindow(hwnd_card_200);
        pump_messages_briefly(50);
    }

    // 7e. Short (~5 second) sample MP4 saved into review-screenshots
    println!("Generating ~5-second sample MP4 recording...");
    let sample_mp4_out = out_dir.join("sample_recording_5s.mp4");
    let rec_w = 640u32;
    let rec_h = 360u32;
    let rec_fps = 30u32;
    if let Ok(encoder) =
        lookout_screenshot::recording::Mp4Encoder::create(&sample_mp4_out, rec_w, rec_h, rec_fps)
    {
        let mut gif_frames = Vec::new();
        for frame_idx in 0..150 {
            let mut frame = ImageBuffer::new(rec_w, rec_h);
            // Dark sleek canvas
            for y in 0..rec_h {
                for x in 0..rec_w {
                    frame.set_pixel(x, y, [20, 23, 28, 255]);
                }
            }

            // Animated moving card
            let card_x = 50 + ((frame_idx * 3) % (rec_w as usize - 220)) as i32;
            let card_y = 100 + ((frame_idx * 2) % (rec_h as usize - 160)) as i32;
            let moving_card = RectI::new(card_x, card_y, card_x + 180, card_y + 90);
            lookout_screenshot::annotation::fill_rounded_rect_rgba(
                &mut frame,
                moving_card,
                8,
                Color::rgb(30, 35, 43),
            );
            lookout_screenshot::annotation::stroke_rounded_rect_rgba(
                &mut frame,
                moving_card,
                8,
                Color::rgb(50, 57, 69),
            );

            lookout_screenshot::annotation::draw_system_text_rgba(
                &mut frame,
                PointI::new(card_x + 16, card_y + 16),
                "LOOKOUT RECORDING",
                Color::rgb(245, 165, 36),
                11,
                true,
            );
            lookout_screenshot::annotation::draw_system_text_rgba(
                &mut frame,
                PointI::new(card_x + 16, card_y + 42),
                &format!("Frame #{:03} / 150", frame_idx + 1),
                Color::rgb(230, 233, 238),
                13,
                true,
            );

            // Timer display on top bar
            let cur_sec = frame_idx / 30;
            let cur_frac = (frame_idx % 30) * 10 / 3;
            let timer_str = format!("00:{:02}.{:02} (5s sample)", cur_sec, cur_frac);
            lookout_screenshot::annotation::draw_system_text_rgba(
                &mut frame,
                PointI::new(24, 24),
                &timer_str,
                Color::rgb(240, 243, 248),
                14,
                true,
            );

            // Click ring effect around frame 60-85
            if (60..=85).contains(&frame_idx) {
                let ring_ms = (frame_idx - 60) as f32 / 25.0;
                let radius = 6.0 + ring_ms * 24.0;
                let alpha = (220.0 * (1.0 - ring_ms)) as u8;
                lookout_screenshot::recording::draw_click_ring(
                    &mut frame,
                    PointI::new(card_x + 60, card_y + 40),
                    radius,
                    Color::rgba(245, 165, 36, alpha),
                );
            }

            if frame_idx % 3 == 0 {
                gif_frames.push(frame.clone());
            }

            let timestamp_100ns = (frame_idx as i64) * (10_000_000 / rec_fps as i64);
            let _ = encoder.write_frame(&frame, timestamp_100ns);
        }
        let _ = encoder.finalize();
        println!("Sample MP4 created: {}", sample_mp4_out.display());

        // 7f. Short sample animated GIF saved into review-screenshots
        let sample_gif_out = out_dir.join("sample_recording.gif");
        if let Err(e) =
            lookout_screenshot::recording::export_to_gif(&gif_frames, 10, 480, &sample_gif_out)
        {
            eprintln!("Failed to export sample GIF: {e}");
        } else {
            println!("Sample GIF created: {}", sample_gif_out.display());
        }
    }

    // 8a. Top bar at 100% and 150%
    println!("Capturing top bar (100% and 150%)...");
    let top_bar_100 = lookout_screenshot::overlay::render_top_bar_demo(1.0);
    top_bar_100
        .save_png(&out_dir.join("top_bar_100.png"))
        .expect("save top_bar_100.png");

    let top_bar_150 = lookout_screenshot::overlay::render_top_bar_demo(1.5);
    top_bar_150
        .save_png(&out_dir.join("top_bar_150.png"))
        .expect("save top_bar_150.png");

    // 8b. Selection toolbar at 100% and 150%
    println!("Capturing selection toolbar (100% and 150%)...");
    let sel_tb_100 = lookout_screenshot::overlay::render_selection_toolbar_demo(1.0);
    sel_tb_100
        .save_png(&out_dir.join("selection_toolbar_100.png"))
        .expect("save selection_toolbar_100.png");

    let sel_tb_150 = lookout_screenshot::overlay::render_selection_toolbar_demo(1.5);
    sel_tb_150
        .save_png(&out_dir.join("selection_toolbar_150.png"))
        .expect("save selection_toolbar_150.png");

    // 8c. Updated scrolling stop pill at 100% and 150%
    println!("Capturing scrolling stop pill (100% and 150%)...");
    if let Some(pill_100) =
        lookout_screenshot::scrolling::FloatingStopPill::open_real_scrolling_pill_for_demo(1.0)
    {
        unsafe {
            pump_messages_briefly(200);
        }
        if let Ok(img) = capture_window_hwnd(pill_100.hwnd) {
            img.save_png(&out_dir.join("scrolling_stop_pill_100.png"))
                .expect("save scrolling_stop_pill_100.png");
        }
        unsafe {
            let _ = DestroyWindow(pill_100.hwnd);
            pump_messages_briefly(50);
        }
    }

    if let Some(pill_150) =
        lookout_screenshot::scrolling::FloatingStopPill::open_real_scrolling_pill_for_demo(1.5)
    {
        unsafe {
            pump_messages_briefly(200);
        }
        if let Ok(img) = capture_window_hwnd(pill_150.hwnd) {
            img.save_png(&out_dir.join("scrolling_stop_pill_150.png"))
                .expect("save scrolling_stop_pill_150.png");
        }
        unsafe {
            let _ = DestroyWindow(pill_150.hwnd);
            pump_messages_briefly(50);
        }
    }

    // 8d. OCR preview card at 100% and 150%
    println!("Capturing OCR preview card (100% and 150%)...");
    if let Ok(hwnd_ocr_100) = lookout_screenshot::ocr::open_real_ocr_card_for_demo(1.0) {
        unsafe {
            pump_messages_briefly(200);
        }
        if let Ok(img) = capture_window_hwnd(hwnd_ocr_100) {
            img.save_png(&out_dir.join("ocr_preview_card_100.png"))
                .expect("save ocr_preview_card_100.png");
        }
        unsafe {
            let _ = DestroyWindow(hwnd_ocr_100);
            pump_messages_briefly(50);
        }
    }

    if let Ok(hwnd_ocr_150) = lookout_screenshot::ocr::open_real_ocr_card_for_demo(1.5) {
        unsafe {
            pump_messages_briefly(200);
        }
        if let Ok(img) = capture_window_hwnd(hwnd_ocr_150) {
            img.save_png(&out_dir.join("ocr_preview_card_150.png"))
                .expect("save ocr_preview_card_150.png");
        }
        unsafe {
            let _ = DestroyWindow(hwnd_ocr_150);
            pump_messages_briefly(50);
        }
    }

    println!(
        "All review screenshots captured successfully to: {}",
        out_dir.display()
    );
}

#[cfg(not(feature = "review"))]
fn main() {
    eprintln!("This example requires the 'review' feature.");
    eprintln!("Run with: cargo run --example generate_review_captures --features review");
    std::process::exit(1);
}
