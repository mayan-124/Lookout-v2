//! Real-screen OCR accuracy benchmark on actual captured application windows:
//! 1. Article Light theme in Edge (15px Segoe UI)
//! 2. Article Dark theme in Edge (15px Segoe UI)
//! 3. Structured Table in Edge (columns separated by tabs)
//! 4. Code file in Notepad (Consolas with indentation)
//! 5. Code file in VS Code (Consolas with indentation)
//! 6. Small Typography in Edge (11px Arial)
//! 7. Bullet Lists & 2 Columns in Edge (gap 80px)
//! 8. Accented and Non-English text in Edge (café, naïve, 東京)
//!
//! Run with: cargo run --manifest-path plugins/screenshot/Cargo.toml --example real_screen_ocr_test --features review

#[cfg(feature = "review")]
fn main() {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    use lookout_screenshot::capture::{capture_gdi_rect, enable_per_monitor_dpi_v2};
    use lookout_screenshot::ocr::execute_ocr;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClientRect, GetWindowRect, GetWindowTextW, IsWindowVisible, PostMessageW,
        SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_SHOWWINDOW, SW_SHOWNORMAL,
        WM_CLOSE,
    };

    #[link(name = "user32")]
    extern "system" {
        fn GetClassNameW(hwnd: HWND, lpClassName: *mut u16, nMaxCount: i32) -> i32;
    }

    enable_per_monitor_dpi_v2();

    let out_dir = PathBuf::from(std::env::var("LOOKOUT_REVIEW_DIR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("lookout-review")
            .display()
            .to_string()
    }));
    let _ = std::fs::create_dir_all(&out_dir);

    let pages_dir = PathBuf::from(format!(
        "{}/ocr_test_pages",
        std::env::var("LOOKOUT_SCRATCH_DIR").unwrap_or_else(|_| std::env::temp_dir()
            .join("lookout-scratch")
            .display()
            .to_string())
    ));

    println!("==================================================================");
    println!("  LOOKOUT REAL-SCREEN OCR BENCHMARK ON RUNNING WINDOWS");
    println!("  Saving captured images & diffs to: {}", out_dir.display());
    println!("==================================================================");

    #[derive(Debug, Clone)]
    struct TestCase {
        name: &'static str,
        app_type: &'static str,
        file_path: PathBuf,
        title_keyword: &'static str,
        ground_truth: &'static str,
        is_code: bool,
        is_table: bool,
        is_two_col: bool,
        has_bullets: bool,
    }

    let cases = vec![
        TestCase {
            name: "article_light",
            app_type: "edge",
            file_path: pages_dir.join("article_light.html"),
            title_keyword: "Article Light",
            ground_truth: "Architectural Principles of Modern Distributed Systems\nDistributed systems require careful decoupling between storage and computation to achieve high throughput and predictable latency.\nEventual consistency allows decentralized nodes to process reads and writes locally without blocking for global consensus across network partitions.\nObservability platforms must aggregate telemetry metrics, distributed traces, and structured logs to maintain operational reliability.",
            is_code: false,
            is_table: false,
            is_two_col: false,
            has_bullets: false,
        },
        TestCase {
            name: "article_dark",
            app_type: "edge",
            file_path: pages_dir.join("article_dark.html"),
            title_keyword: "Article Dark",
            ground_truth: "Core Runtime Architecture and Execution Model\nThe runtime schedules lightweight tasks across a pool of worker threads using lock-free work-stealing queues.\nMemory safety is enforced through static lifetime analysis and ownership semantics without requiring garbage collection pauses.\nAsynchronous input and output operations leverage native platform event loops to minimize operating system context switches.",
            is_code: false,
            is_table: false,
            is_two_col: false,
            has_bullets: false,
        },
        TestCase {
            name: "table_page",
            app_type: "edge",
            file_path: pages_dir.join("table_page.html"),
            title_keyword: "Table Page",
            ground_truth: "Service\tRegion\tLatency\tStatus\napi-gateway\tus-east-1\t12ms\tHealthy\nauth-server\teu-west-1\t48ms\tHealthy\ndata-store\tap-northeast-1\t115ms\tDegraded\ncache-node\tsa-east-1\t85ms\tHealthy",
            is_code: false,
            is_table: true,
            is_two_col: false,
            has_bullets: false,
        },
        TestCase {
            name: "code_notepad",
            app_type: "notepad",
            file_path: pages_dir.join("code_sample.rs"),
            title_keyword: "code_sample",
            ground_truth: "fn process_batch(items: &[u64]) -> Result<usize, Error> {\n    let mut processed = 0;\n    for &id in items {\n        if validate_token(id)? {\n            dispatch_job(id);\n            processed += 1;\n        }\n    }\n    Ok(processed)\n}",
            is_code: true,
            is_table: false,
            is_two_col: false,
            has_bullets: false,
        },
        TestCase {
            name: "code_editor",
            app_type: "vscode_or_browser",
            file_path: pages_dir.join("code_browser.html"),
            title_keyword: "code_editor_block",
            ground_truth: "fn process_batch(items: &[u64]) -> Result<usize, Error> {\n    let mut processed = 0;\n    for &id in items {\n        if validate_token(id)? {\n            dispatch_job(id);\n            processed += 1;\n        }\n    }\n    Ok(processed)\n}",
            is_code: true,
            is_table: false,
            is_two_col: false,
            has_bullets: false,
        },
        TestCase {
            name: "small_text",
            app_type: "edge",
            file_path: pages_dir.join("small_text.html"),
            title_keyword: "Small Typography",
            ground_truth: "Garden notes: water the tomato beds every second morning (about 12 litres).\nThe town library opens at 09:00 and closes at 18:00 on weekdays. Bring your card.\nBread recipe: mix 500 g flour, 350 ml water and 7 g yeast. Bake for 35 minutes.",
            is_code: false,
            is_table: false,
            is_two_col: false,
            has_bullets: false,
        },
        TestCase {
            name: "bullets_columns",
            app_type: "edge",
            file_path: pages_dir.join("bullets_columns.html"),
            title_keyword: "Bullet Lists",
            ground_truth: "Primary Features\n• Fast desktop screen capture\n• High fidelity video encoding\n• Automatic scrolling stitcher\n\nSystem Requirements\n• Windows 10 build 19041 or higher\n• DirectX 11 compatible graphics\n• Minimum 4 GB available system RAM",
            is_code: false,
            is_table: false,
            is_two_col: true,
            has_bullets: true,
        },
        TestCase {
            name: "accents_page",
            app_type: "edge",
            file_path: pages_dir.join("accents_page.html"),
            title_keyword: "Accented",
            ground_truth: "The café serves fresh croissants to naïve travellers visiting Zürich and São Paulo.\nInternational offices located in 東京, 서울, and München report quarterly earnings.\nMathematical symbols include ∑, ∏, √, and standard ASCII punctuation.",
            is_code: false,
            is_table: false,
            is_two_col: false,
            has_bullets: false,
        },
    ];

    unsafe fn get_all_visible_hwnds() -> HashSet<isize> {
        struct Collector {
            set: HashSet<isize>,
        }
        unsafe extern "system" fn enum_proc(hwnd: HWND, lp: LPARAM) -> BOOL {
            let col = &mut *(lp.0 as *mut Collector);
            if IsWindowVisible(hwnd).as_bool() {
                col.set.insert(hwnd.0 as isize);
            }
            BOOL(1)
        }
        let mut c = Collector {
            set: HashSet::new(),
        };
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut c as *mut _ as isize));
        c.set
    }

    fn find_window(
        before: &HashSet<isize>,
        title_needle: &str,
        class_needle: &str,
        timeout_ms: u64,
    ) -> Option<HWND> {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            let current = unsafe { get_all_visible_hwnds() };
            // First check newly appeared windows
            for &h_val in &current {
                if !before.contains(&h_val) {
                    let hwnd = HWND(h_val as *mut std::ffi::c_void);
                    let mut title = [0u16; 512];
                    let len = unsafe { GetWindowTextW(hwnd, &mut title) };
                    let title_str = if len > 0 {
                        String::from_utf16_lossy(&title[..len as usize])
                    } else {
                        String::new()
                    };

                    let mut class_buf = [0u16; 256];
                    let clen = unsafe { GetClassNameW(hwnd, class_buf.as_mut_ptr(), 256) };
                    let class_str = if clen > 0 {
                        String::from_utf16_lossy(&class_buf[..clen as usize])
                    } else {
                        String::new()
                    };

                    let mut rc = RECT::default();
                    unsafe {
                        let _ = GetWindowRect(hwnd, &mut rc);
                    }
                    let w = rc.right - rc.left;
                    let h = rc.bottom - rc.top;

                    if w > 200 && h > 200 {
                        if !title_needle.is_empty() {
                            if title_str
                                .to_lowercase()
                                .contains(&title_needle.to_lowercase())
                            {
                                println!(
                                    "    Found matching window: '{}' (class: '{}', hwnd: {:?})",
                                    title_str, class_str, hwnd
                                );
                                return Some(hwnd);
                            }
                        } else if !class_needle.is_empty() && class_str.contains(class_needle) {
                            return Some(hwnd);
                        }
                    }
                }
            }

            // Also search all visible windows by title if new window set didn't catch it
            for &h_val in &current {
                let hwnd = HWND(h_val as *mut std::ffi::c_void);
                let mut title = [0u16; 512];
                let len = unsafe { GetWindowTextW(hwnd, &mut title) };
                if len > 0 {
                    let title_str = String::from_utf16_lossy(&title[..len as usize]);
                    if !title_needle.is_empty()
                        && title_str
                            .to_lowercase()
                            .contains(&title_needle.to_lowercase())
                    {
                        let mut rc = RECT::default();
                        unsafe {
                            let _ = GetWindowRect(hwnd, &mut rc);
                        }
                        if (rc.right - rc.left) > 200 && (rc.bottom - rc.top) > 200 {
                            println!(
                                "    Found visible window by title: '{}' (hwnd: {:?})",
                                title_str, hwnd
                            );
                            return Some(hwnd);
                        }
                    }
                }
            }

            thread::sleep(Duration::from_millis(200));
        }
        None
    }

    #[allow(clippy::needless_range_loop)]
    fn calculate_cer_and_wer(truth: &str, recognized: &str) -> (f32, f32, Vec<(char, char)>) {
        // Character level Levenshtein
        let s1: Vec<char> = truth.chars().collect();
        let s2: Vec<char> = recognized.chars().collect();
        let n = s1.len();
        let m = s2.len();

        let mut dp = vec![vec![0usize; m + 1]; n + 1];
        for i in 0..=n {
            dp[i][0] = i;
        }
        for j in 0..=m {
            dp[0][j] = j;
        }

        for i in 1..=n {
            for j in 1..=m {
                let cost = if s1[i - 1] == s2[j - 1] { 0 } else { 1 };
                dp[i][j] = (dp[i - 1][j] + 1)
                    .min(dp[i][j - 1] + 1)
                    .min(dp[i - 1][j - 1] + cost);
            }
        }

        let cer = if n > 0 {
            dp[n][m] as f32 / n as f32
        } else {
            0.0
        };

        // Backtrack for substitutions
        let mut substitutions = Vec::new();
        let mut i = n;
        let mut j = m;
        while i > 0 && j > 0 {
            if s1[i - 1] == s2[j - 1] {
                i -= 1;
                j -= 1;
            } else if dp[i][j] == dp[i - 1][j - 1] + 1 {
                substitutions.push((s1[i - 1], s2[j - 1]));
                i -= 1;
                j -= 1;
            } else if dp[i][j] == dp[i - 1][j] + 1 {
                // Deletion
                substitutions.push((s1[i - 1], '∅'));
                i -= 1;
            } else {
                // Insertion
                substitutions.push(('∅', s2[j - 1]));
                j -= 1;
            }
        }
        substitutions.reverse();

        // Word level Levenshtein
        let w1: Vec<&str> = truth.split_whitespace().collect();
        let w2: Vec<&str> = recognized.split_whitespace().collect();
        let wn = w1.len();
        let wm = w2.len();
        let mut wdp = vec![vec![0usize; wm + 1]; wn + 1];
        for i in 0..=wn {
            wdp[i][0] = i;
        }
        for j in 0..=wm {
            wdp[0][j] = j;
        }
        for i in 1..=wn {
            for j in 1..=wm {
                let cost = if w1[i - 1].eq_ignore_ascii_case(w2[j - 1]) {
                    0
                } else {
                    1
                };
                wdp[i][j] = (wdp[i - 1][j] + 1)
                    .min(wdp[i][j - 1] + 1)
                    .min(wdp[i - 1][j - 1] + cost);
            }
        }
        let wer = if wn > 0 {
            wdp[wn][wm] as f32 / wn as f32
        } else {
            0.0
        };

        (cer, wer, substitutions)
    }

    struct BenchmarkResult {
        name: &'static str,
        app: &'static str,
        cer: f32,
        wer: f32,
        line_breaks_kept: bool,
        indentation_kept: bool,
        bullets_kept: bool,
        columns_kept: bool,
        table_tabs_kept: bool,
        _errors: Vec<(char, char)>,
    }

    let mut results = Vec::new();

    for case in &cases {
        println!(
            "\n>>> Running Real Screen Capture Benchmark: {} ({})",
            case.name, case.app_type
        );
        assert!(
            case.file_path.exists(),
            "File must exist: {:?}",
            case.file_path
        );

        let clean_path = case.file_path.display().to_string();

        let before_hwnds = unsafe { get_all_visible_hwnds() };
        let mut child_opt = None;
        let mut target_hwnd = None;

        match case.app_type {
            "edge" => {
                let _ = Command::new("taskkill")
                    .args(["/F", "/IM", "msedge.exe"])
                    .output();
                thread::sleep(Duration::from_millis(300));

                let edge_exe =
                    Path::new(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe");
                let unique_id = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let profile = std::env::temp_dir().join(format!("lookout_edge_prof_{unique_id}"));
                let _ = std::fs::create_dir_all(&profile);
                let file_url = format!("file:///{}", clean_path.replace('\\', "/"));
                let child = Command::new(edge_exe)
                    .arg("--no-sandbox")
                    .arg("--disable-gpu")
                    .arg("--test-type")
                    .arg(format!("--user-data-dir={}", profile.display()))
                    .arg("--no-first-run")
                    .arg("--no-default-browser-check")
                    .arg("--window-size=960,720")
                    .arg(&file_url)
                    .spawn()
                    .expect("launch Edge");
                child_opt = Some(child);
                target_hwnd = find_window(
                    &before_hwnds,
                    case.title_keyword,
                    "Chrome_WidgetWin_1",
                    10000,
                );

                if target_hwnd.is_none() {
                    let chrome_exe =
                        Path::new(r"C:\Program Files\Google\Chrome\Application\chrome.exe");
                    if chrome_exe.exists() {
                        let c_profile =
                            std::env::temp_dir().join(format!("lookout_chrome_prof_{unique_id}"));
                        let _ = std::fs::create_dir_all(&c_profile);
                        let c_child = Command::new(chrome_exe)
                            .arg("--no-sandbox")
                            .arg("--disable-gpu")
                            .arg("--test-type")
                            .arg(format!("--user-data-dir={}", c_profile.display()))
                            .arg("--no-first-run")
                            .arg("--no-default-browser-check")
                            .arg("--window-size=960,720")
                            .arg(&file_url)
                            .spawn();
                        if let Ok(c) = c_child {
                            child_opt = Some(c);
                            target_hwnd = find_window(
                                &before_hwnds,
                                case.title_keyword,
                                "Chrome_WidgetWin_1",
                                8000,
                            );
                        }
                    }
                }
            }
            "notepad" => {
                let child = Command::new("notepad.exe")
                    .arg(&case.file_path)
                    .spawn()
                    .expect("launch Notepad");
                child_opt = Some(child);
                target_hwnd = find_window(&before_hwnds, case.title_keyword, "Notepad", 8000);
            }
            "vscode_or_browser" => {
                // Try launching VS Code if possible
                let code_exe = Path::new(r"C:\Program Files\Microsoft VS Code\Code.exe");
                if code_exe.exists() {
                    let child = Command::new(code_exe)
                        .arg("--no-sandbox")
                        .arg("-n")
                        .arg(&case.file_path)
                        .spawn();
                    if let Ok(c) = child {
                        child_opt = Some(c);
                        target_hwnd = find_window(
                            &before_hwnds,
                            case.title_keyword,
                            "Chrome_WidgetWin_1",
                            4000,
                        );
                    }
                }
                if target_hwnd.is_none() {
                    println!("    [VS Code cannot be launched in sandbox (chdir EPERM); using real browser code editor block]");
                    let _ = Command::new("taskkill")
                        .args(["/F", "/IM", "msedge.exe"])
                        .output();
                    thread::sleep(Duration::from_millis(300));

                    let edge_exe =
                        Path::new(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe");
                    let unique_id = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos();
                    let profile =
                        std::env::temp_dir().join(format!("lookout_edge_prof_{unique_id}"));
                    let _ = std::fs::create_dir_all(&profile);
                    let file_url = format!("file:///{}", clean_path.replace('\\', "/"));
                    let child = Command::new(edge_exe)
                        .arg("--no-sandbox")
                        .arg("--disable-gpu")
                        .arg("--test-type")
                        .arg(format!("--user-data-dir={}", profile.display()))
                        .arg("--no-first-run")
                        .arg("--no-default-browser-check")
                        .arg("--window-size=960,720")
                        .arg(&file_url)
                        .spawn()
                        .expect("launch Edge code block");
                    child_opt = Some(child);
                    target_hwnd = find_window(
                        &before_hwnds,
                        case.title_keyword,
                        "Chrome_WidgetWin_1",
                        10000,
                    );
                }
            }
            _ => {}
        }

        let hwnd = match target_hwnd {
            Some(h) => h,
            None => {
                eprintln!("    FAILED to find window for {}", case.name);
                continue;
            }
        };

        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 50, 50, 960, 720, SWP_SHOWWINDOW);
            let _ = SetForegroundWindow(hwnd);
        }
        thread::sleep(Duration::from_millis(2500));

        let mut client_rc = RECT::default();
        let mut pt = POINT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut client_rc);
            pt.x = client_rc.left;
            pt.y = client_rc.top;
            let _ = ClientToScreen(hwnd, &mut pt);
        }

        let is_notepad = case.app_type == "notepad";
        let top_offset = if is_notepad { 14 } else { 85 };
        let capture_x = if is_notepad { pt.x + 2 } else { pt.x + 15 };
        let capture_y = pt.y + top_offset;
        let capture_w = if is_notepad {
            (client_rc.right - client_rc.left)
                .saturating_sub(10)
                .max(100) as u32
        } else {
            (client_rc.right - client_rc.left)
                .saturating_sub(30)
                .max(100) as u32
        };
        let capture_h = if case.name == "small_text" {
            220
        } else {
            (client_rc.bottom - client_rc.top)
                .saturating_sub(top_offset + 20)
                .max(100) as u32
        };

        println!(
            "    Capturing real screen area: ({capture_x}, {capture_y}, {capture_w}x{capture_h})"
        );
        let captured = match capture_gdi_rect(capture_x, capture_y, capture_w, capture_h) {
            Ok(img) => img,
            Err(e) => {
                eprintln!("    Capture error: {e}");
                continue;
            }
        };

        // Save real screen image into review-screenshots
        let img_out = out_dir.join(format!("real_ocr_{}.png", case.name));
        let _ = captured.save_png(&img_out);
        println!("    Saved screenshot: {}", img_out.display());

        // Execute real OCR Text Mode
        let ocr_result = match execute_ocr(&captured) {
            Ok(res) => res,
            Err(e) => {
                eprintln!("    OCR failed: {e}");
                continue;
            }
        };

        let recognized = ocr_result.formatted_text.trim();
        let expected = case.ground_truth.trim();

        let (cer, wer, substitutions) = calculate_cer_and_wer(expected, recognized);

        // Analyze formatting preservation
        let expected_line_count = expected.lines().count();
        let rec_line_count = recognized.lines().count();
        let line_breaks_kept = (expected_line_count as i32 - rec_line_count as i32).abs() <= 2;

        let indentation_kept = if case.is_code {
            recognized
                .lines()
                .any(|l| l.starts_with("    ") || l.starts_with("  ") || l.starts_with('\t'))
        } else {
            true
        };

        let bullets_kept = if case.has_bullets {
            recognized
                .lines()
                .filter(|l| l.contains('•') || l.contains('-') || l.contains('*'))
                .count()
                >= 3
        } else {
            true
        };

        let columns_kept = if case.is_two_col {
            let pf_pos = recognized.find("Primary Features");
            let sr_pos = recognized.find("System Requirements");
            match (pf_pos, sr_pos) {
                (Some(p), Some(s)) => p < s,
                _ => false,
            }
        } else {
            true
        };

        let table_tabs_kept = if case.is_table {
            recognized.contains('\t')
        } else {
            true
        };

        // Unique character substitution pairs (excluding whitespace-whitespace matches)
        let mut distinct_errors = Vec::new();
        let mut seen = HashSet::new();
        for (exp_ch, act_ch) in substitutions {
            if exp_ch != act_ch && !exp_ch.is_whitespace() && seen.insert((exp_ch, act_ch)) {
                distinct_errors.push((exp_ch, act_ch));
            }
        }

        // Save diff file into review-screenshots
        let diff_path = out_dir.join(format!("real_ocr_diff_{}.txt", case.name));
        let diff_content = format!(
            "Case: {}\nApp: {}\nCER: {:.4} ({:.2}%)\nWER: {:.4} ({:.2}%)\nLine breaks kept: {}\nIndentation kept: {}\nBullets kept: {}\nColumns kept: {}\nTable tabs kept: {}\n\n=== EXACT CHARACTER ERRORS (Expected -> Recognized) ===\n{:?}\n\n=== EXPECTED GROUND TRUTH ===\n{}\n\n=== COPIED RECOGNIZED TEXT ===\n{}\n",
            case.name, case.app_type, cer, cer * 100.0, wer, wer * 100.0,
            line_breaks_kept, indentation_kept, bullets_kept, columns_kept, table_tabs_kept,
            distinct_errors, expected, recognized
        );
        let _ = std::fs::write(&diff_path, diff_content);
        println!("    Saved diff: {}", diff_path.display());

        println!(
            "    Results: CER={:.2}%, WER={:.2}%, Lines={}, Indent={}, Bullets={}, Cols={}, Tabs={}",
            cer * 100.0, wer * 100.0, line_breaks_kept, indentation_kept, bullets_kept, columns_kept, table_tabs_kept
        );

        results.push(BenchmarkResult {
            name: case.name,
            app: case.app_type,
            cer,
            wer,
            line_breaks_kept,
            indentation_kept,
            bullets_kept,
            columns_kept,
            table_tabs_kept,
            _errors: distinct_errors,
        });

        // Clean close of window
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        thread::sleep(Duration::from_millis(400));
        if let Some(mut child) = child_opt {
            let _ = child.kill();
            let _ = child.wait();
        }
        if case.app_type == "edge" {
            let _ = Command::new("taskkill")
                .args(["/F", "/IM", "msedge.exe"])
                .output();
        }
        thread::sleep(Duration::from_millis(500));
    }

    println!("\n==================================================================");
    println!("  REAL-SCREEN OCR BENCHMARK SUMMARY TABLE");
    println!("==================================================================");
    println!(
        "{:<18} {:<10} {:<10} {:<10} {:<6} {:<6} {:<7} {:<6} {:<6}",
        "Test Case", "App", "CER", "WER", "Lines", "Indent", "Bullets", "Cols", "Tabs"
    );
    println!(
        "-----------------------------------------------------------------------------------------"
    );
    for r in &results {
        println!(
            "{:<18} {:<10} {:<10} {:<10} {:<6} {:<6} {:<7} {:<6} {:<6}",
            r.name,
            r.app,
            format!("{:.2}%", r.cer * 100.0),
            format!("{:.2}%", r.wer * 100.0),
            if r.line_breaks_kept { "YES" } else { "NO" },
            if r.indentation_kept { "YES" } else { "NO" },
            if r.bullets_kept { "YES" } else { "NO" },
            if r.columns_kept { "YES" } else { "NO" },
            if r.table_tabs_kept { "YES" } else { "NO" }
        );
    }
    println!(
        "-----------------------------------------------------------------------------------------"
    );
}

#[cfg(not(feature = "review"))]
fn main() {
    eprintln!("This example requires the 'review' feature.");
    std::process::exit(1);
}
