#![allow(clippy::all)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use lookout_screenshot::annotation::{draw_system_text_with_font, Color};
use lookout_screenshot::geometry::PointI;
use lookout_screenshot::image::ImageBuffer;
use lookout_screenshot::ocr::execute_ocr;

fn sha256_hex(data: &[u8]) -> String {
    fn rotr(x: u32, n: u32) -> u32 {
        (x >> n) | (x << (32 - n))
    }
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while (msg.len() + 8) % 64 != 0 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
            let s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut h_val = h[7];

        for i in 0..64 {
            let s1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h_val
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h_val = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(h_val);
    }

    let mut out = String::new();
    for val in h {
        out.push_str(&format!("{val:08x}"));
    }
    out
}

fn load_png(path: &Path) -> Result<ImageBuffer, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let decoder = png::Decoder::new(BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![
        0u8;
        reader
            .output_buffer_size()
            .ok_or_else(|| "size error".to_string())?
    ];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;

    let mut img = ImageBuffer::new(info.width, info.height);
    match info.color_type {
        png::ColorType::Rgba => {
            for (src, dst) in buf.chunks_exact(4).zip(img.pixels.chunks_exact_mut(4)) {
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
        }
        png::ColorType::Rgb => {
            for (src, dst) in buf.chunks_exact(3).zip(img.pixels.chunks_exact_mut(4)) {
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = 255;
            }
        }
        _ => return Err(format!("unsupported color type {:?}", info.color_type)),
    }
    Ok(img)
}

fn calculate_cer_and_wer(
    expected: &str,
    recognized: &str,
) -> (f64, f64, HashMap<(char, char), usize>) {
    let exp_chars: Vec<char> = expected.chars().collect();
    let rec_chars: Vec<char> = recognized.chars().collect();
    let n = exp_chars.len();
    let m = rec_chars.len();
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..=n {
        d[i][0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if exp_chars[i - 1] == rec_chars[j - 1] {
                0
            } else {
                1
            };
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
        }
    }
    let cer = if n > 0 {
        d[n][m] as f64 / n as f64
    } else {
        0.0
    };

    // Backtrack alignment to extract exact character substitution/insertion/deletion errors
    let mut substitutions = HashMap::new();
    let mut i = n;
    let mut j = m;
    while i > 0 || j > 0 {
        if i > 0
            && j > 0
            && d[i][j]
                == d[i - 1][j - 1]
                    + (if exp_chars[i - 1] == rec_chars[j - 1] {
                        0
                    } else {
                        1
                    })
        {
            if exp_chars[i - 1] != rec_chars[j - 1] {
                *substitutions
                    .entry((exp_chars[i - 1], rec_chars[j - 1]))
                    .or_insert(0) += 1;
            }
            i -= 1;
            j -= 1;
        } else if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            *substitutions.entry((exp_chars[i - 1], '∅')).or_insert(0) += 1;
            i -= 1;
        } else if j > 0 && d[i][j] == d[i][j - 1] + 1 {
            *substitutions.entry(('∅', rec_chars[j - 1])).or_insert(0) += 1;
            j -= 1;
        } else {
            break;
        }
    }

    let exp_words: Vec<&str> = expected.split_whitespace().collect();
    let rec_words: Vec<&str> = recognized.split_whitespace().collect();
    let nw = exp_words.len();
    let mw = rec_words.len();
    let mut dw = vec![vec![0usize; mw + 1]; nw + 1];
    for i in 0..=nw {
        dw[i][0] = i;
    }
    for j in 0..=mw {
        dw[0][j] = j;
    }
    for i in 1..=nw {
        for j in 1..=mw {
            let cost = if exp_words[i - 1] == rec_words[j - 1] {
                0
            } else {
                1
            };
            dw[i][j] = (dw[i - 1][j] + 1)
                .min(dw[i][j - 1] + 1)
                .min(dw[i - 1][j - 1] + cost);
        }
    }
    let wer = if nw > 0 {
        dw[nw][mw] as f64 / nw as f64
    } else {
        0.0
    };
    (cer, wer, substitutions)
}

fn create_solid(w: u32, h: u32, bg: [u8; 4]) -> ImageBuffer {
    let mut img = ImageBuffer::new(w, h);
    for chunk in img.pixels.chunks_exact_mut(4) {
        chunk[0] = bg[2];
        chunk[1] = bg[1];
        chunk[2] = bg[0];
        chunk[3] = bg[3];
    }
    img
}

fn main() {
    let root = PathBuf::from(format!("{}/../..", env!("CARGO_MANIFEST_DIR")));
    let review_dir = PathBuf::from(std::env::var("LOOKOUT_REVIEW_DIR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("lookout-review")
            .display()
            .to_string()
    }));
    let frozen_dir = root.join("plugins/screenshot/fixtures/ocr/frozen");
    let held_out_dir = root.join("plugins/screenshot/fixtures/ocr/held_out");

    let _ = std::fs::create_dir_all(&frozen_dir);
    let _ = std::fs::create_dir_all(&held_out_dir);

    // =========================================================================
    // 1. GENERATE THE 6 HELD-OUT TEST CASES (Real Windows GDI Font Rasterization)
    // =========================================================================
    println!(">>> Generating 6 Held-Out Test Cases...");

    // Case 1: Terminal window (Cascadia/Consolas on #0c0c0c)
    let term_path = held_out_dir.join("held_out_terminal.png");
    if !term_path.exists() {
        let mut img = create_solid(720, 240, [12, 12, 12, 255]);
        let lines = [
            ("PS C:\\Lookout> cargo --version", 30, 30),
            ("cargo 1.82.0 (8f40fc59f 2026-10-15)", 30, 65),
            ("PS C:\\Lookout> lookout-cli status", 30, 110),
            ("Status: ACTIVE [PID 4092]", 30, 145),
            ("Worker pool: 4 threads initialized", 30, 180),
        ];
        for (txt, x, y) in lines {
            draw_system_text_with_font(
                &mut img,
                PointI::new(x, y),
                txt,
                Color::rgb(204, 204, 204),
                16,
                false,
                true,
                "Consolas",
            );
        }
        let _ = img.save_png(&term_path);
        let _ = img.save_png(&review_dir.join("held_out_terminal.png"));
    }

    // Case 2: Code editor with different theme (Solarized Light #fdf6e3)
    let sol_path = held_out_dir.join("held_out_code_solarized.png");
    if !sol_path.exists() {
        let mut img = create_solid(640, 220, [253, 246, 227, 255]);
        let lines = [
            ("struct WorkerConfig {", 30, 30, Color::rgb(181, 137, 0)),
            (
                "    pub max_retries: u32,",
                30,
                70,
                Color::rgb(101, 123, 131),
            ),
            (
                "    pub timeout_ms: u64,",
                30,
                110,
                Color::rgb(101, 123, 131),
            ),
            ("}", 30, 150, Color::rgb(181, 137, 0)),
        ];
        for (txt, x, y, col) in lines {
            draw_system_text_with_font(
                &mut img,
                PointI::new(x, y),
                txt,
                col,
                16,
                false,
                true,
                "Courier New",
            );
        }
        let _ = img.save_png(&sol_path);
        let _ = img.save_png(&review_dir.join("held_out_code_solarized.png"));
    }

    // Case 3: Dark Settings Page (#1a1a1a)
    let set_path = held_out_dir.join("held_out_settings_dark.png");
    if !set_path.exists() {
        let mut img = create_solid(760, 260, [26, 26, 26, 255]);
        let lines = [
            ("Lookout System Settings", 30, 25, 18, true),
            (
                "Capture Engine: DirectX 11 Desktop Duplication",
                30,
                65,
                15,
                false,
            ),
            (
                "Audio Loopback: Enabled (Default Audio Device)",
                30,
                105,
                15,
                false,
            ),
            (
                "Hardware Acceleration: Auto (NVIDIA GeForce RTX)",
                30,
                145,
                15,
                false,
            ),
            (
                "Telemetry Reporting: Anonymous Diagnostics Only",
                30,
                185,
                15,
                false,
            ),
        ];
        for (txt, x, y, sz, bold) in lines {
            draw_system_text_with_font(
                &mut img,
                PointI::new(x, y),
                txt,
                Color::rgb(230, 230, 230),
                sz,
                bold,
                true,
                "Segoe UI",
            );
        }
        let _ = img.save_png(&set_path);
        let _ = img.save_png(&review_dir.join("held_out_settings_dark.png"));
    }

    // Case 4: PDF Viewer (Paper document style, Georgia/Times New Roman serif font)
    let pdf_path = held_out_dir.join("held_out_pdf_viewer.png");
    if !pdf_path.exists() {
        let mut img = create_solid(880, 260, [248, 248, 248, 255]);
        let lines = [
            ("Abstract", 40, 25, 16, true),
            ("We present a high-performance visual capture pipeline designed for low-latency desktop streaming.", 40, 65, 14, false),
            ("By integrating hardware-accelerated color conversion directly into texture memory, host memory bus saturation is eliminated.", 40, 105, 14, false),
            ("Experimental benchmarks demonstrate continuous 60 FPS recording with minimal jitter across heterogeneous display configurations.", 40, 145, 14, false),
        ];
        for (txt, x, y, sz, bold) in lines {
            draw_system_text_with_font(
                &mut img,
                PointI::new(x, y),
                txt,
                Color::rgb(17, 17, 17),
                sz,
                bold,
                true,
                "Georgia",
            );
        }
        let _ = img.save_png(&pdf_path);
        let _ = img.save_png(&review_dir.join("held_out_pdf_viewer.png"));
    }

    // Case 5: Small Gray Text (10px Tahoma #767676 on #ffffff)
    let gray_path = held_out_dir.join("held_out_small_gray.png");
    if !gray_path.exists() {
        let mut img = create_solid(860, 180, [255, 255, 255, 255]);
        let lines = [
            (
                "Copyright (c) 2026 Lookout Technologies Inc. All rights reserved.",
                30,
                30,
            ),
            (
                "Confidential build 0.9.4-rc2. Not for public redistribution.",
                30,
                65,
            ),
            (
                "Hardware fingerprint: 4B82-99F1-0A3E. Timestamp: 2026-11-24 22:15:00 UTC.",
                30,
                100,
            ),
        ];
        for (txt, x, y) in lines {
            draw_system_text_with_font(
                &mut img,
                PointI::new(x, y),
                txt,
                Color::rgb(118, 118, 118),
                10,
                false,
                true,
                "Tahoma",
            );
        }
        let _ = img.save_png(&gray_path);
        let _ = img.save_png(&review_dir.join("held_out_small_gray.png"));
    }

    // Case 6: Numbers Table (Arial 14px financial metrics table)
    let num_path = held_out_dir.join("held_out_numbers_table.png");
    if !num_path.exists() {
        let mut img = create_solid(720, 220, [255, 255, 255, 255]);
        let table_rows = [
            ("Metric", "P50", "P95", "P99", "Loss%"),
            ("Latency", "1.42ms", "4.88ms", "12.05ms", "0.02%"),
            ("Throughput", "840MB/s", "792MB/s", "710MB/s", "0.00%"),
            ("Memory", "128.4MB", "142.1MB", "156.8MB", "0.00%"),
        ];
        let xs = [30, 170, 310, 450, 590];
        let ys = [30, 70, 110, 150];
        for (r_idx, row) in table_rows.iter().enumerate() {
            let y = ys[r_idx];
            let cells = [row.0, row.1, row.2, row.3, row.4];
            for (c_idx, cell) in cells.iter().enumerate() {
                let x = xs[c_idx];
                draw_system_text_with_font(
                    &mut img,
                    PointI::new(x, y),
                    cell,
                    Color::rgb(20, 20, 20),
                    14,
                    r_idx == 0,
                    true,
                    "Arial",
                );
            }
        }
        let _ = img.save_png(&num_path);
        let _ = img.save_png(&review_dir.join("held_out_numbers_table.png"));
    }

    // =========================================================================
    // 2. DEFINE DATASETS & GROUND TRUTH
    // =========================================================================
    struct BenchmarkCase {
        name: &'static str,
        file_path: PathBuf,
        ground_truth: &'static str,
        before_cer: f64,
        before_wer: f64,
    }

    let frozen_cases = vec![
        BenchmarkCase {
            name: "article_light",
            file_path: frozen_dir.join("real_ocr_article_light.png"),
            ground_truth: "Architectural Principles of Modern Distributed Systems\nDistributed systems require careful decoupling between storage and computation to achieve high throughput and predictable latency.\nEventual consistency allows decentralized nodes to process reads and writes locally without blocking for global consensus across network partitions.\nObservability platforms must aggregate telemetry metrics, distributed traces, and structured logs to maintain operational reliability.",
            before_cer: 0.0128,
            before_wer: 0.0000,
        },
        BenchmarkCase {
            name: "article_dark",
            file_path: frozen_dir.join("real_ocr_article_dark.png"),
            ground_truth: "Core Runtime Architecture and Execution Model\nThe runtime schedules lightweight tasks across a pool of worker threads using lock-free work-stealing queues.\nMemory safety is enforced through static lifetime analysis and ownership semantics without requiring garbage collection pauses.\nAsynchronous input and output operations leverage native platform event loops to minimize operating system context switches.",
            before_cer: 0.0172,
            before_wer: 0.0000,
        },
        BenchmarkCase {
            name: "table_page",
            file_path: frozen_dir.join("real_ocr_table_page.png"),
            ground_truth: "Service\tRegion\tLatency\tStatus\napi-gateway\tus-east-1\t12ms\tHealthy\nauth-server\teu-west-1\t48ms\tHealthy\ndata-store\tap-northeast-1\t115ms\tDegraded\ncache-node\tsa-east-1\t85ms\tHealthy",
            before_cer: 0.0460,
            before_wer: 0.2500,
        },
        BenchmarkCase {
            name: "code_notepad",
            file_path: frozen_dir.join("real_ocr_code_notepad.png"),
            ground_truth: "fn process_batch(items: &[u64]) -> Result<usize, Error> {\n    let mut processed = 0;\n    for &id in items {\n        if validate_token(id)? {\n            dispatch_job(id);\n            processed += 1;\n        }\n    }\n    Ok(processed)\n}",
            before_cer: 0.3376,
            before_wer: 0.2857,
        },
        BenchmarkCase {
            name: "code_editor",
            file_path: frozen_dir.join("real_ocr_code_editor.png"),
            ground_truth: "fn process_batch(items: &[u64]) -> Result<usize, Error> {\n    let mut processed = 0;\n    for &id in items {\n        if validate_token(id)? {\n            dispatch_job(id);\n            processed += 1;\n        }\n    }\n    Ok(processed)\n}",
            before_cer: 0.3034,
            before_wer: 0.1786,
        },
        BenchmarkCase {
            name: "small_text",
            file_path: frozen_dir.join("real_ocr_small_text.png"),
            ground_truth: "Lookout Capture Agent version 2.4.0 (x86_64-pc-windows-msvc build release).\nAll telemetry endpoints are operational. Configuration loaded from C:\\ProgramData\\Lookout.\nLicense identifier: MIT-Commercial-Dual-2026. Process priority: Normal. Thread count: 8.",
            before_cer: 0.1020,
            before_wer: 0.0769,
        },
        BenchmarkCase {
            name: "bullets_columns",
            file_path: frozen_dir.join("real_ocr_bullets_columns.png"),
            ground_truth: "Primary Features\n• Fast desktop screen capture\n• High fidelity video encoding\n• Automatic scrolling stitcher\n\nSystem Requirements\n• Windows 10 build 19041 or higher\n• DirectX 11 compatible graphics\n• Minimum 4 GB available system RAM",
            before_cer: 0.0258,
            before_wer: 0.0000,
        },
        BenchmarkCase {
            name: "accents_page",
            file_path: frozen_dir.join("real_ocr_accents_page.png"),
            ground_truth: "The café serves fresh croissants to naïve travellers visiting Zürich and São Paulo.\nInternational offices located in 東京, 서울, and München report quarterly earnings.\nMathematical symbols include ∑, ∏, √, and standard ASCII punctuation.",
            before_cer: 0.0515,
            before_wer: 0.2059,
        },
    ];

    let held_out_cases = vec![
        BenchmarkCase {
            name: "held_out_terminal",
            file_path: term_path,
            ground_truth: "PS C:\\Lookout> cargo --version\ncargo 1.82.0 (8f40fc59f 2026-10-15)\nPS C:\\Lookout> lookout-cli status\nStatus: ACTIVE [PID 4092]\nWorker pool: 4 threads initialized",
            before_cer: 0.0,
            before_wer: 0.0,
        },
        BenchmarkCase {
            name: "held_out_code_solarized",
            file_path: sol_path,
            ground_truth: "struct WorkerConfig {\n    pub max_retries: u32,\n    pub timeout_ms: u64,\n}",
            before_cer: 0.0,
            before_wer: 0.0,
        },
        BenchmarkCase {
            name: "held_out_settings_dark",
            file_path: set_path,
            ground_truth: "Lookout System Settings\nCapture Engine: DirectX 11 Desktop Duplication\nAudio Loopback: Enabled (Default Audio Device)\nHardware Acceleration: Auto (NVIDIA GeForce RTX)\nTelemetry Reporting: Anonymous Diagnostics Only",
            before_cer: 0.0,
            before_wer: 0.0,
        },
        BenchmarkCase {
            name: "held_out_pdf_viewer",
            file_path: pdf_path,
            ground_truth: "Abstract\nWe present a high-performance visual capture pipeline designed for low-latency desktop streaming.\nBy integrating hardware-accelerated color conversion directly into texture memory, host memory bus saturation is eliminated.\nExperimental benchmarks demonstrate continuous 60 FPS recording with minimal jitter across heterogeneous display configurations.",
            before_cer: 0.0,
            before_wer: 0.0,
        },
        BenchmarkCase {
            name: "held_out_small_gray",
            file_path: gray_path,
            ground_truth: "Copyright (c) 2026 Lookout Technologies Inc. All rights reserved.\nConfidential build 0.9.4-rc2. Not for public redistribution.\nHardware fingerprint: 4B82-99F1-0A3E. Timestamp: 2026-11-24 22:15:00 UTC.",
            before_cer: 0.0,
            before_wer: 0.0,
        },
        BenchmarkCase {
            name: "held_out_numbers_table",
            file_path: num_path,
            ground_truth: "Metric\tP50\tP95\tP99\tLoss%\nLatency\t1.42ms\t4.88ms\t12.05ms\t0.02%\nThroughput\t840MB/s\t792MB/s\t710MB/s\t0.00%\nMemory\t128.4MB\t142.1MB\t156.8MB\t0.00%",
            before_cer: 0.0,
            before_wer: 0.0,
        },
    ];

    // =========================================================================
    // 3. GENERATE MANIFESTS WITH SHA-256 HASHES
    // =========================================================================
    let mut frozen_manifest = Vec::new();
    for c in &frozen_cases {
        let bytes = std::fs::read(&c.file_path).expect(&format!("read {}", c.file_path.display()));
        let hash = sha256_hex(&bytes);
        frozen_manifest.push(serde_json::json!({
            "name": c.name,
            "file": c.file_path.file_name().unwrap().to_string_lossy(),
            "sha256": hash,
            "ground_truth": c.ground_truth,
        }));
    }
    let frozen_json = serde_json::to_string_pretty(&frozen_manifest).unwrap();
    let _ = File::create(frozen_dir.join("manifest.json"))
        .and_then(|mut f| f.write_all(frozen_json.as_bytes()));

    let mut held_out_manifest = Vec::new();
    for c in &held_out_cases {
        let bytes = std::fs::read(&c.file_path).expect(&format!("read {}", c.file_path.display()));
        let hash = sha256_hex(&bytes);
        held_out_manifest.push(serde_json::json!({
            "name": c.name,
            "file": c.file_path.file_name().unwrap().to_string_lossy(),
            "sha256": hash,
            "ground_truth": c.ground_truth,
        }));
    }
    let held_out_json = serde_json::to_string_pretty(&held_out_manifest).unwrap();
    let _ = File::create(held_out_dir.join("manifest.json"))
        .and_then(|mut f| f.write_all(held_out_json.as_bytes()));

    println!(">>> Manifests generated:");
    println!(
        "    Frozen:   {}",
        frozen_dir.join("manifest.json").display()
    );
    println!(
        "    Held-Out: {}",
        held_out_dir.join("manifest.json").display()
    );

    // =========================================================================
    // 4. RUN CURRENT OCR EVALUATION ON BOTH SETS
    // =========================================================================
    let mut all_wrong_chars: HashMap<(char, char), usize> = HashMap::new();

    println!("\n==================================================================");
    println!("  SET 1: FROZEN TEST SET (BEFORE [6bdec4c] -> AFTER [3763275])");
    println!("==================================================================");
    println!(
        "{:<18} {:<15} {:<15} {:<15} {:<15}",
        "Case", "Before CER", "After CER", "Before WER", "After WER"
    );
    println!("------------------------------------------------------------------");

    for c in &frozen_cases {
        let img = load_png(&c.file_path).expect(&format!("load {}", c.name));
        let ocr_res = execute_ocr(&img).expect("ocr extraction");
        let recognized = ocr_res.formatted_text.trim();
        let expected = c.ground_truth.trim();
        let (after_cer, after_wer, errors) = calculate_cer_and_wer(expected, recognized);

        for (pair, count) in errors {
            *all_wrong_chars.entry(pair).or_insert(0) += count;
        }

        println!(
            "{:<18} {:<15} {:<15} {:<15} {:<15}",
            c.name,
            format!("{:.2}%", c.before_cer * 100.0),
            format!("{:.2}%", after_cer * 100.0),
            format!("{:.2}%", c.before_wer * 100.0),
            format!("{:.2}%", after_wer * 100.0),
        );
    }

    println!("\n==================================================================");
    println!("  SET 2: HELD-OUT TEST SET (NEVER TUNED, UNTOUCHED BENCHMARK)");
    println!("==================================================================");
    println!("{:<25} {:<12} {:<12}", "Case", "CER", "WER");
    println!("------------------------------------------------------------------");

    for c in &held_out_cases {
        let img = load_png(&c.file_path).expect(&format!("load {}", c.name));
        let ocr_res = execute_ocr(&img).expect("ocr extraction");
        let recognized = ocr_res.formatted_text.trim();
        let expected = c.ground_truth.trim();
        let (cer, wer, errors) = calculate_cer_and_wer(expected, recognized);

        for (pair, count) in errors {
            *all_wrong_chars.entry(pair).or_insert(0) += count;
        }

        println!(
            "{:<25} {:<12} {:<12}",
            c.name,
            format!("{:.2}%", cer * 100.0),
            format!("{:.2}%", wer * 100.0)
        );
    }

    // Sort most common wrong characters
    let mut sorted_errors: Vec<((char, char), usize)> = all_wrong_chars.into_iter().collect();
    sorted_errors.sort_by(|a, b| b.1.cmp(&a.1));

    println!("\n==================================================================");
    println!("  TOP 5 MOST COMMON WRONG CHARACTERS ACROSS ALL EVALUATED CASES");
    println!("==================================================================");
    for (i, ((expected, recognized), count)) in sorted_errors.iter().take(5).enumerate() {
        let exp_disp = if *expected == '\n' {
            "\\n".to_string()
        } else if *expected == ' ' {
            "space".to_string()
        } else {
            expected.to_string()
        };
        let rec_disp = if *recognized == '\n' {
            "\\n".to_string()
        } else if *recognized == ' ' {
            "space".to_string()
        } else if *recognized == '∅' {
            "[omitted]".to_string()
        } else {
            recognized.to_string()
        };
        println!(
            "  {}. '{}' -> '{}' (occurrences: {})",
            i + 1,
            exp_disp,
            rec_disp,
            count
        );
    }
}
