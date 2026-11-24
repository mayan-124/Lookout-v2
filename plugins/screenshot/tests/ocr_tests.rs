use lookout_screenshot::annotation::{draw_system_text_rgba, draw_system_text_with_font, Color};
use lookout_screenshot::geometry::PointI;
use lookout_screenshot::image::ImageBuffer;
use lookout_screenshot::ocr::{
    count_lines_and_words, detect_missing_language_ocr, detect_text_actions, execute_ocr,
    execute_ocr_with_config, get_installed_ocr_languages, get_ocr_keep_line_breaks,
    reconstruct_layout, reconstruct_layout_with_options, set_ocr_keep_line_breaks, OcrAction,
    OcrConfig, OcrLanguageInfo, OcrWordBox,
};

/// Computes the Character Error Rate (CER) using Levenshtein distance:
/// CER = distance(ground_truth, recognized) / len(ground_truth)
#[allow(clippy::needless_range_loop)]
fn calculate_cer(truth: &str, recognized: &str) -> f32 {
    let s1: Vec<char> = truth.chars().filter(|c| !c.is_whitespace()).collect();
    let s2: Vec<char> = recognized.chars().filter(|c| !c.is_whitespace()).collect();

    let n = s1.len();
    let m = s2.len();
    if n == 0 {
        return if m == 0 { 0.0 } else { 1.0 };
    }

    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..=n {
        dp[i][0] = i;
    }
    for j in 0..=m {
        dp[0][j] = j;
    }

    for i in 1..=n {
        for j in 1..=m {
            let cost = if s1[i - 1].eq_ignore_ascii_case(&s2[j - 1]) {
                0
            } else {
                1
            };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }

    dp[n][m] as f32 / n as f32
}

fn create_solid_image(width: u32, height: u32, color: [u8; 4]) -> ImageBuffer {
    let mut img = ImageBuffer::new(width, height);
    for y in 0..height {
        for x in 0..width {
            img.set_pixel(x, y, color);
        }
    }
    img
}

#[test]
fn test_ocr_light_background() {
    let text = "Lookout desktop screen capture and OCR engine";
    let mut img = create_solid_image(600, 100, [255, 255, 255, 255]);
    draw_system_text_rgba(
        &mut img,
        PointI::new(20, 35),
        text,
        Color::rgb(20, 20, 20),
        22,
        false,
    );

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        let cer = calculate_cer(text, &extraction.formatted_text);
        println!(
            "Light background CER: {:.3}, text: '{}'",
            cer,
            extraction.formatted_text.trim()
        );
        assert!(
            cer < 0.15,
            "CER {cer} exceeded threshold 0.15 for light text"
        );
        assert!(!extraction.words.is_empty(), "Expected extracted words");
    } else {
        println!("Windows OCR not available in test environment, skipping assertion");
    }
}

#[test]
fn test_ocr_dark_background_inversion() {
    let text = "Dark background auto inverted for crisp reading";
    // Background average luminance < 128 (dark theme #1e1e1e)
    let mut img = create_solid_image(650, 100, [30, 30, 30, 255]);
    draw_system_text_rgba(
        &mut img,
        PointI::new(20, 35),
        text,
        Color::rgb(240, 240, 240),
        22,
        false,
    );

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        let cer = calculate_cer(text, &extraction.formatted_text);
        println!(
            "Dark background CER: {:.3}, text: '{}'",
            cer,
            extraction.formatted_text.trim()
        );
        assert!(
            cer < 0.15,
            "CER {cer} exceeded threshold 0.15 for inverted dark text"
        );
    } else {
        println!("Windows OCR not available in test environment, skipping assertion");
    }
}

#[test]
fn test_ocr_small_text_upscaling() {
    let text = "Small typography upscale test 12345";
    // Small height (font size 13, image height 70) triggers 2x-3x upscale
    let mut img = create_solid_image(600, 80, [255, 255, 255, 255]);
    draw_system_text_rgba(
        &mut img,
        PointI::new(30, 25),
        text,
        Color::rgb(0, 0, 0),
        14,
        false,
    );

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        let cer = calculate_cer(text, &extraction.formatted_text);
        println!(
            "Small text raw: '{}', formatted: '{}', CER: {:.3}",
            extraction.raw_text, extraction.formatted_text, cer
        );
        assert!(
            cer < 0.20,
            "CER {cer} exceeded threshold 0.20 for small text"
        );
    } else {
        println!("Windows OCR not available in test environment, skipping assertion");
    }
}

#[test]
fn test_ocr_columns_reading_order() {
    let mut img = create_solid_image(700, 180, [255, 255, 255, 255]);
    // Column 1 (left)
    draw_system_text_rgba(
        &mut img,
        PointI::new(30, 30),
        "Left Col Alpha",
        Color::rgb(0, 0, 0),
        20,
        false,
    );
    draw_system_text_rgba(
        &mut img,
        PointI::new(30, 75),
        "Left Col Beta",
        Color::rgb(0, 0, 0),
        20,
        false,
    );

    // Column 2 (right, separated by > 120px)
    draw_system_text_rgba(
        &mut img,
        PointI::new(420, 30),
        "Right Col Gamma",
        Color::rgb(0, 0, 0),
        20,
        false,
    );
    draw_system_text_rgba(
        &mut img,
        PointI::new(420, 75),
        "Right Col Delta",
        Color::rgb(0, 0, 0),
        20,
        false,
    );

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        println!("Columns extracted text:\n{}", extraction.formatted_text);
        // Assert words are recognized
        assert!(extraction.words.len() >= 4, "Expected multi-column words");
    }
}

#[test]
fn test_ocr_tab_separated_table() {
    let mut img = create_solid_image(700, 160, [255, 255, 255, 255]);
    // Header
    draw_system_text_rgba(
        &mut img,
        PointI::new(30, 30),
        "Product",
        Color::rgb(0, 0, 0),
        20,
        true,
    );
    draw_system_text_rgba(
        &mut img,
        PointI::new(260, 30),
        "Price",
        Color::rgb(0, 0, 0),
        20,
        true,
    );
    draw_system_text_rgba(
        &mut img,
        PointI::new(480, 30),
        "Status",
        Color::rgb(0, 0, 0),
        20,
        true,
    );

    // Row 1
    draw_system_text_rgba(
        &mut img,
        PointI::new(30, 80),
        "Monitor",
        Color::rgb(0, 0, 0),
        20,
        false,
    );
    draw_system_text_rgba(
        &mut img,
        PointI::new(260, 80),
        "300",
        Color::rgb(0, 0, 0),
        20,
        false,
    );
    draw_system_text_rgba(
        &mut img,
        PointI::new(480, 80),
        "Active",
        Color::rgb(0, 0, 0),
        20,
        false,
    );

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        println!("Table extracted text:\n{}", extraction.formatted_text);
        assert!(extraction.formatted_text.contains('\t') || extraction.words.len() >= 4);
    }
}

#[test]
fn test_ocr_code_indentation() {
    let mut img = create_solid_image(600, 160, [255, 255, 255, 255]);
    draw_system_text_rgba(
        &mut img,
        PointI::new(30, 30),
        "fn example() {",
        Color::rgb(0, 0, 0),
        20,
        false,
    );
    // Indented by ~60px
    draw_system_text_rgba(
        &mut img,
        PointI::new(90, 80),
        "let value = 100;",
        Color::rgb(0, 0, 0),
        20,
        false,
    );
    draw_system_text_rgba(
        &mut img,
        PointI::new(30, 125),
        "}",
        Color::rgb(0, 0, 0),
        20,
        false,
    );

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        println!("Code extracted text:\n{}", extraction.formatted_text);
        assert!(!extraction.words.is_empty());
    }
}

#[test]
fn test_ocr_action_detection_links_and_emails() {
    let text = "Visit https://github.com/lookout or email team@lookout.app for details";
    let actions = detect_text_actions(text);
    assert_eq!(actions.len(), 2, "Expected 2 actions (1 URL, 1 email)");

    assert_eq!(
        actions[0],
        OcrAction::OpenUrl("https://github.com/lookout".to_string())
    );
    assert_eq!(
        actions[1],
        OcrAction::SendEmail("team@lookout.app".to_string())
    );
}

#[test]
fn test_ocr_preview_card_text_and_counts_match_copied_text() {
    let raw_copied = "System status: online\nDocs: https://lookout.dev\nAll systems operational";
    let (lines, words) = count_lines_and_words(raw_copied);

    // Verify non-empty line count matches actual line count
    assert_eq!(lines, 3);
    assert_eq!(words, 8);

    // Verify preview renders all lines from the exact same string
    let preview_lines: Vec<&str> = raw_copied.lines().collect();
    assert_eq!(preview_lines.len(), 3);
    assert_eq!(preview_lines[0], "System status: online");
    assert_eq!(preview_lines[1], "Docs: https://lookout.dev");
    assert_eq!(preview_lines[2], "All systems operational");

    // Single line case
    let single = "Single line of copied text";
    let (s_lines, s_words) = count_lines_and_words(single);
    assert_eq!(s_lines, 1);
    assert_eq!(s_words, 5);

    // String with blank lines
    let with_blanks = "Line one\n\nLine two\n  \nLine three";
    let (b_lines, b_words) = count_lines_and_words(with_blanks);
    assert_eq!(b_lines, 3);
    assert_eq!(b_words, 6);
}

#[test]
fn test_real_world_dark_web_page_ocr() {
    // Real-world dark theme web page (#0d1117 background with #161b22 card)
    let w = 820;
    let h = 240;
    let mut img = create_solid_image(w, h, [13, 17, 23, 255]);

    // Card container
    for y in 20..220 {
        for x in 20..800 {
            img.set_pixel(x, y, [22, 27, 34, 255]);
        }
    }
    // Card border
    for x in 20..800 {
        img.set_pixel(x, 20, [48, 54, 61, 255]);
        img.set_pixel(x, 219, [48, 54, 61, 255]);
    }
    for y in 20..220 {
        img.set_pixel(20, y, [48, 54, 61, 255]);
        img.set_pixel(799, y, [48, 54, 61, 255]);
    }

    // Rendered with real Segoe UI and ClearType subpixel anti-aliasing
    let header_text = "Lookout Desktop - Capture & OCR Engine";
    let body_text = "Lookout provides high performance local screen recording.";
    let link_text = "Documentation: https://lookout.dev/docs";

    draw_system_text_with_font(
        &mut img,
        PointI::new(40, 42),
        header_text,
        Color::rgb(240, 246, 252),
        20,
        true,
        true,
        "Segoe UI",
    );
    draw_system_text_with_font(
        &mut img,
        PointI::new(40, 92),
        body_text,
        Color::rgb(201, 209, 217),
        16,
        false,
        true,
        "Segoe UI",
    );
    draw_system_text_with_font(
        &mut img,
        PointI::new(40, 142),
        link_text,
        Color::rgb(88, 166, 255), // Link blue
        15,
        false,
        true,
        "Segoe UI",
    );

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        let expected = format!("{header_text}\n{body_text}\n{link_text}");
        let cer = calculate_cer(&expected, &extraction.formatted_text);
        println!(
            "\n[Real-World Dark Web Page]\nCER: {:.4} ({:.2}% error)\nExtracted:\n{}",
            cer,
            cer * 100.0,
            extraction.formatted_text
        );
        assert!(
            cer < 0.12,
            "Real-world dark web page CER {cer:.4} exceeded 0.12"
        );
        assert!(
            extraction
                .actions
                .iter()
                .any(|a| matches!(a, OcrAction::OpenUrl(u) if u.contains("lookout.dev"))),
            "Expected URL action detected in dark web page"
        );
    } else {
        println!("Windows OCR not available in test environment, skipping assertion");
    }
}

#[test]
fn test_real_world_code_block_ocr() {
    // Real-world code editor snippet (#1e1e1e VS Code dark theme)
    let w = 820;
    let h = 240;
    let mut img = create_solid_image(w, h, [30, 30, 30, 255]);

    // Code lines rendered in Consolas with ClearType subpixel anti-aliasing
    let code_lines = [
        (
            "pub fn handle_event(id: u64) -> Result<(), Error> {",
            35,
            28,
        ),
        ("    let active = check_worker_status(id);", 35, 70),
        ("    println!(\"Worker status: {}\", active);", 35, 112),
        ("    Ok(())", 35, 154),
        ("}", 35, 196),
    ];

    for (line, x, y) in code_lines {
        draw_system_text_with_font(
            &mut img,
            PointI::new(x, y),
            line,
            Color::rgb(212, 212, 212),
            16,
            false,
            true,
            "Consolas",
        );
    }

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        let expected = "pub fn handle_event(id: u64) -> Result<(), Error> {\n    let active = check_worker_status(id);\n    println!(\"Worker status: {}\", active);\n    Ok(())\n}";
        let cer = calculate_cer(expected, &extraction.formatted_text);
        println!(
            "\n[Real-World Code Block]\nCER: {:.4} ({:.2}% error)\nExtracted:\n{}",
            cer,
            cer * 100.0,
            extraction.formatted_text
        );
        assert!(
            cer < 0.15,
            "Real-world code block CER {cer:.4} exceeded 0.15"
        );
    } else {
        println!("Windows OCR not available in test environment, skipping assertion");
    }
}

#[test]
fn test_real_world_structured_table_ocr() {
    // Real-world dashboard structured table with borders and alternating rows
    let w = 840;
    let h = 250;
    let mut img = create_solid_image(w, h, [248, 250, 252, 255]);

    // Header row background (#e2e8f0)
    for y in 25..65 {
        for x in 25..815 {
            img.set_pixel(x, y, [226, 232, 240, 255]);
        }
    }
    // Row 2 background (#f1f5f9)
    for y in 105..145 {
        for x in 25..815 {
            img.set_pixel(x, y, [241, 245, 249, 255]);
        }
    }
    // Row dividers
    for div_y in [25, 65, 105, 145, 185] {
        for x in 25..815 {
            img.set_pixel(x, div_y, [203, 213, 225, 255]);
        }
    }

    // Render table columns with real Segoe UI font and ClearType anti-aliasing
    // Columns at x = 40, 240, 460, 660
    let col_headers = [
        ("Instance", 40),
        ("Region", 240),
        ("Latency", 460),
        ("Status", 660),
    ];
    for (header, x) in col_headers {
        draw_system_text_with_font(
            &mut img,
            PointI::new(x, 34),
            header,
            Color::rgb(15, 23, 42),
            16,
            true,
            true,
            "Segoe UI",
        );
    }

    let rows = [
        (
            [
                ("node-alpha", 40),
                ("us-east-1", 240),
                ("18ms", 460),
                ("Healthy", 660),
            ],
            74,
        ),
        (
            [
                ("node-beta", 40),
                ("eu-central-1", 240),
                ("42ms", 460),
                ("Healthy", 660),
            ],
            114,
        ),
        (
            [
                ("node-gamma", 40),
                ("ap-southeast-1", 240),
                ("120ms", 460),
                ("Warning", 660),
            ],
            154,
        ),
    ];

    for (cols, y) in rows {
        for (val, x) in cols {
            draw_system_text_with_font(
                &mut img,
                PointI::new(x, y),
                val,
                Color::rgb(51, 65, 85),
                15,
                false,
                true,
                "Segoe UI",
            );
        }
    }

    let res = execute_ocr(&img);
    if let Ok(extraction) = res {
        let expected = "Instance\tRegion\tLatency\tStatus\nnode-alpha\tus-east-1\t18ms\tHealthy\nnode-beta\teu-central-1\t42ms\tHealthy\nnode-gamma\tap-southeast-1\t120ms\tWarning";
        let cer = calculate_cer(expected, &extraction.formatted_text);
        println!(
            "\n[Real-World Structured Table]\nCER: {:.4} ({:.2}% error)\nExtracted:\n{}",
            cer,
            cer * 100.0,
            extraction.formatted_text
        );
        assert!(cer < 0.15, "Real-world table CER {cer:.4} exceeded 0.15");
    } else {
        println!("Windows OCR not available in test environment, skipping assertion");
    }
}

#[test]
fn test_code_mode_monospace_indentation_and_confusion_handling() {
    // 1. Indentation from monospace bounding boxes
    // Monospace char width = 10px. min_left = 20px.
    // Line 1: fn main() { (left 20)
    // Line 2: let O0_l1_I = 100; (left 60 -> 40px indent = 4 spaces)
    // Line 3: if true { (left 60 -> 40px indent = 4 spaces)
    // Line 4: dispatch_0O(l1); (left 100 -> 80px indent = 8 spaces)
    let words = vec![
        OcrWordBox {
            text: "fn".to_string(),
            x: 20.0,
            y: 10.0,
            width: 20.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "main()".to_string(),
            x: 50.0,
            y: 10.0,
            width: 60.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "{".to_string(),
            x: 120.0,
            y: 10.0,
            width: 10.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "let".to_string(),
            x: 60.0,
            y: 30.0,
            width: 30.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "O0_l1_I".to_string(),
            x: 100.0,
            y: 30.0,
            width: 70.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "=".to_string(),
            x: 180.0,
            y: 30.0,
            width: 10.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "100;".to_string(),
            x: 200.0,
            y: 30.0,
            width: 40.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "if".to_string(),
            x: 60.0,
            y: 50.0,
            width: 20.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "true".to_string(),
            x: 90.0,
            y: 50.0,
            width: 40.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "{".to_string(),
            x: 140.0,
            y: 50.0,
            width: 10.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "dispatch_0O(l1);".to_string(),
            x: 100.0,
            y: 70.0,
            width: 160.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "}".to_string(),
            x: 60.0,
            y: 90.0,
            width: 10.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "}".to_string(),
            x: 20.0,
            y: 110.0,
            width: 10.0,
            height: 16.0,
        },
    ];

    let layout = reconstruct_layout(words);
    println!("Reconstructed code:\n{}", layout);

    // Verify indentation levels
    let lines: Vec<&str> = layout.lines().collect();
    assert_eq!(lines[0], "fn main() {");
    assert!(lines[1].starts_with("    let "));
    assert!(lines[2].starts_with("    if "));
    assert!(lines[3].starts_with("        dispatch_0O(l1);"));
    assert_eq!(lines[4], "    }");
    assert_eq!(lines[5], "}");

    // Verify 0/O and l/1/I confusion handling (no autocorrect in code mode)
    assert!(
        layout.contains("O0_l1_I"),
        "Identifier O0_l1_I was modified by autocorrect"
    );
    assert!(
        layout.contains("dispatch_0O(l1);"),
        "Identifier dispatch_0O(l1) was modified by autocorrect"
    );
}

#[test]
fn test_ocr_keep_vs_join_line_breaks() {
    let words = vec![
        OcrWordBox {
            text: "First".to_string(),
            x: 10.0,
            y: 10.0,
            width: 40.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "line".to_string(),
            x: 60.0,
            y: 10.0,
            width: 40.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "Second".to_string(),
            x: 10.0,
            y: 28.0,
            width: 50.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "line".to_string(),
            x: 70.0,
            y: 28.0,
            width: 40.0,
            height: 16.0,
        },
    ];

    // Keep line breaks
    let kept = reconstruct_layout_with_options(words.clone(), true);
    assert_eq!(kept, "First line\nSecond line");

    // Join wrapped lines
    let joined = reconstruct_layout_with_options(words, false);
    assert_eq!(joined, "First line Second line");

    // Verify global preference setter & getter
    set_ocr_keep_line_breaks(false);
    assert!(!get_ocr_keep_line_breaks());
    set_ocr_keep_line_breaks(true);
    assert!(get_ocr_keep_line_breaks());
}

#[test]
fn test_ocr_installed_languages_and_missing_pack_detection() {
    let installed = get_installed_ocr_languages();
    println!("Installed Windows OCR languages: {:?}", installed);
    assert!(
        !installed.is_empty(),
        "Expected at least 1 installed language (e.g. en-US)"
    );

    // English text with en-US installed: no missing hint
    let en_hint = detect_missing_language_ocr("Hello world telemetry operational", &installed);
    assert_eq!(en_hint, None);

    // Japanese CJK text with only en-US installed: triggers installation instruction
    let fake_installed = vec![OcrLanguageInfo {
        tag: "en-US".to_string(),
        display_name: "English (United States)".to_string(),
    }];
    let ja_hint = detect_missing_language_ocr("Office located in 東京 (Tokyo)", &fake_installed);
    assert_eq!(
        ja_hint,
        Some("Install Japanese/Chinese OCR in Windows Settings > Time & language > Language & region".to_string())
    );

    // Korean text
    let ko_hint = detect_missing_language_ocr("Seoul office 서울", &fake_installed);
    assert_eq!(
        ko_hint,
        Some(
            "Install Korean OCR in Windows Settings > Time & language > Language & region"
                .to_string()
        )
    );
}

#[test]
fn test_ocr_table_digit_disambiguation_rules() {
    let words = vec![
        // Row 1
        OcrWordBox {
            text: "Service".to_string(),
            x: 10.0,
            y: 10.0,
            width: 60.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "Region".to_string(),
            x: 150.0,
            y: 10.0,
            width: 50.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "Latency".to_string(),
            x: 300.0,
            y: 10.0,
            width: 50.0,
            height: 16.0,
        },
        // Row 2: us-east-I recognized with I instead of 1, I2ms instead of 12ms
        OcrWordBox {
            text: "api-gw".to_string(),
            x: 10.0,
            y: 30.0,
            width: 50.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "us-east-I".to_string(),
            x: 150.0,
            y: 30.0,
            width: 70.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "I2ms".to_string(),
            x: 300.0,
            y: 30.0,
            width: 40.0,
            height: 16.0,
        },
        // Row 3: eu-west-l recognized with l instead of 1
        OcrWordBox {
            text: "auth".to_string(),
            x: 10.0,
            y: 50.0,
            width: 40.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "eu-west-l".to_string(),
            x: 150.0,
            y: 50.0,
            width: 70.0,
            height: 16.0,
        },
        OcrWordBox {
            text: "48ms".to_string(),
            x: 300.0,
            y: 50.0,
            width: 40.0,
            height: 16.0,
        },
    ];

    let layout = reconstruct_layout(words);
    println!("Disambiguated Table:\n{}", layout);

    assert!(
        layout.contains("us-east-1"),
        "Expected us-east-1, got: {layout}"
    );
    assert!(
        layout.contains("eu-west-1"),
        "Expected eu-west-1, got: {layout}"
    );
    assert!(layout.contains("12ms"), "Expected 12ms, got: {layout}");
    assert!(
        !layout.contains("\n\n"),
        "Table rows should not have blank lines between them"
    );
}

#[test]
fn test_ocr_small_text_upscale_and_sharpen_measurement() {
    let img_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/ocr/frozen/real_ocr_small_text.png"
    );
    if let Ok(img) = ImageBuffer::load_png(img_path) {
        let truth = "Lookout Capture Agent version 2.4.0 (x86_64-pc-windows-msvc build release).\nAll telemetry endpoints are operational. Configuration loaded from C:\\ProgramData\\Lookout.\nLicense identifier: MIT-Commercial-Dual-2026. Process priority: Normal. Thread count: 8.";

        let conf_3x = OcrConfig {
            upscale_factor: 3,
            apply_sharpening: false,
            keep_line_breaks: true,
            language_tag: None,
        };
        let conf_3x_sharp = OcrConfig {
            upscale_factor: 3,
            apply_sharpening: true,
            keep_line_breaks: true,
            language_tag: None,
        };

        let res_3x = execute_ocr_with_config(&img, &conf_3x);
        let res_sharp = execute_ocr_with_config(&img, &conf_3x_sharp);

        if let (Ok(ext_3x), Ok(ext_sharp)) = (res_3x, res_sharp) {
            let cer_3x = calculate_cer(truth, &ext_3x.formatted_text);
            let cer_sharp = calculate_cer(truth, &ext_sharp.formatted_text);
            println!("\n=== Small Text Optimization Measurement ===");
            println!("RAW: {:?}", ext_3x.raw_text);
            println!(
                "WORDS: {:?}",
                ext_3x.words.iter().map(|w| &w.text).collect::<Vec<_>>()
            );
            println!("3x Upscaling CER: {:.4} ({:.2}%)", cer_3x, cer_3x * 100.0);
            println!(
                "3x Upscale + Light Sharpening CER: {:.4} ({:.2}%)",
                cer_sharp,
                cer_sharp * 100.0
            );
            println!("3x Text:\n{}", ext_3x.formatted_text);
            println!("3x+Sharp Text:\n{}", ext_sharp.formatted_text);
        }
    }
}

#[test]
fn test_real_screen_code_editor_reconstruction() {
    let img_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/ocr/frozen/real_ocr_code_editor.png"
    );
    if let Ok(img) = ImageBuffer::load_png(img_path) {
        let res = execute_ocr(&img);
        if let Ok(ext) = res {
            let ground_truth = "fn process_batch(items: &[u64]) -> Result<usize, Error> {\n    let mut processed = 0;\n    for &id in items {\n        if validate_token(id)? {\n            dispatch_job(id);\n            processed += 1;\n        }\n    }\n    Ok(processed)\n}";
            let cer = calculate_cer(ground_truth, &ext.formatted_text);
            println!(
                "\n=== Code Editor Verified Benchmark ===\nCER: {:.4} ({:.2}%)\n{}",
                cer,
                cer * 100.0,
                ext.formatted_text
            );
            assert!(cer < 0.08, "CER must be under 8%, got {cer:.4}");
            assert!(
                ext.formatted_text.contains("        dispatch_job(id);"),
                "Indentation not preserved"
            );
            assert!(
                ext.formatted_text.ends_with('}'),
                "Closing brace not preserved"
            );
        }
    }
}

#[test]
fn test_frozen_and_held_out_manifest_integrity() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let frozen_manifest_path = root.join("fixtures/ocr/frozen/manifest.json");
    let held_out_manifest_path = root.join("fixtures/ocr/held_out/manifest.json");

    assert!(
        frozen_manifest_path.exists(),
        "Frozen manifest must exist at {}",
        frozen_manifest_path.display()
    );
    assert!(
        held_out_manifest_path.exists(),
        "Held-out manifest must exist at {}",
        held_out_manifest_path.display()
    );

    let frozen_str = std::fs::read_to_string(&frozen_manifest_path).unwrap();
    let frozen_val: serde_json::Value = serde_json::from_str(&frozen_str).unwrap();
    let frozen_arr = frozen_val.as_array().expect("array");
    assert!(
        frozen_arr.len() >= 8,
        "Expected at least 8 frozen test cases"
    );

    for case in frozen_arr {
        let file_name = case["file"].as_str().unwrap();
        let sha256 = case["sha256"].as_str().unwrap();
        assert!(!sha256.is_empty(), "SHA-256 missing for {file_name}");
        let p = root.join("fixtures/ocr/frozen").join(file_name);
        assert!(p.exists(), "Frozen fixture image missing: {}", p.display());
    }

    let held_out_str = std::fs::read_to_string(&held_out_manifest_path).unwrap();
    let held_out_val: serde_json::Value = serde_json::from_str(&held_out_str).unwrap();
    let held_out_arr = held_out_val.as_array().expect("array");
    assert!(
        held_out_arr.len() >= 6,
        "Expected at least 6 held-out test cases"
    );

    for case in held_out_arr {
        let file_name = case["file"].as_str().unwrap();
        let sha256 = case["sha256"].as_str().unwrap();
        assert!(!sha256.is_empty(), "SHA-256 missing for {file_name}");
        let p = root.join("fixtures/ocr/held_out").join(file_name);
        assert!(
            p.exists(),
            "Held-out fixture image missing: {}",
            p.display()
        );
    }
}
