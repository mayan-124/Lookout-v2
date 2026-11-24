//! OCR text capture engine using Windows.Media.Ocr.
//! Provides local, on-device text recognition with dark background inversion,
//! 2-3x small text upscaling, layout reconstruction from word bounding boxes
//! (line breaks, paragraph gaps, indentation, bullet lists, columns, tab-separated tables),
//! and actionable URL/email detection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use windows::core::{w, HSTRING};
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
    InvalidateRect, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, PAINTSTRUCT,
    SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, PostQuitMessage, RegisterClassW, SetTimer, ShowWindow, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SW_SHOWNOACTIVATE, WM_CREATE,
    WM_DESTROY, WM_LBUTTONDOWN, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::annotation::{draw_system_text_rgba, measure_system_text, Color};
use crate::clipboard::copy_text_to_clipboard;
use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;

#[derive(Debug, Clone)]
pub struct OcrWordBox {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OcrAction {
    OpenUrl(String),
    SendEmail(String),
}

#[derive(Debug, Clone)]
pub struct OcrExtraction {
    pub raw_text: String,
    pub formatted_text: String,
    pub words: Vec<OcrWordBox>,
    pub line_count: usize,
    pub word_count: usize,
    pub actions: Vec<OcrAction>,
    pub missing_language_hint: Option<String>,
}

static OCR_KEEP_LINE_BREAKS: AtomicBool = AtomicBool::new(true);

pub fn get_ocr_keep_line_breaks() -> bool {
    OCR_KEEP_LINE_BREAKS.load(Ordering::Acquire)
}

pub fn set_ocr_keep_line_breaks(val: bool) {
    OCR_KEEP_LINE_BREAKS.store(val, Ordering::Release);
}

static SELECTED_OCR_LANGUAGE: Mutex<Option<String>> = Mutex::new(None);

pub fn get_selected_ocr_language() -> Option<String> {
    SELECTED_OCR_LANGUAGE.lock().unwrap().clone()
}

pub fn set_selected_ocr_language(lang: Option<String>) {
    if let Ok(mut lock) = SELECTED_OCR_LANGUAGE.lock() {
        *lock = lang;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrLanguageInfo {
    pub tag: String,
    pub display_name: String,
}

/// Enumerates the installed Windows OCR languages.
pub fn get_installed_ocr_languages() -> Vec<OcrLanguageInfo> {
    let mut langs = Vec::new();
    if let Ok(vec_view) = OcrEngine::AvailableRecognizerLanguages() {
        if let Ok(size) = vec_view.Size() {
            for i in 0..size {
                if let Ok(lang) = vec_view.GetAt(i) {
                    let tag = lang
                        .LanguageTag()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    let display_name = lang
                        .DisplayName()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    if !tag.is_empty() {
                        langs.push(OcrLanguageInfo { tag, display_name });
                    }
                }
            }
        }
    }
    langs
}

/// Scans text for uninstalled language scripts (CJK, Hangul, Cyrillic, Arabic).
pub fn detect_missing_language_ocr(
    text: &str,
    installed_langs: &[OcrLanguageInfo],
) -> Option<String> {
    let has_cjk = text.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{3040}'..='\u{309F}').contains(&c)
            || ('\u{30A0}'..='\u{30FF}').contains(&c)
    });
    let has_hangul = text.chars().any(|c| ('\u{AC00}'..='\u{D7AF}').contains(&c));
    let has_cyrillic = text.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
    let has_arabic = text.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c));

    if has_cjk
        && !installed_langs
            .iter()
            .any(|l| l.tag.starts_with("ja") || l.tag.starts_with("zh"))
    {
        return Some("Install Japanese/Chinese OCR in Windows Settings > Time & language > Language & region".to_string());
    }
    if has_hangul && !installed_langs.iter().any(|l| l.tag.starts_with("ko")) {
        return Some(
            "Install Korean OCR in Windows Settings > Time & language > Language & region"
                .to_string(),
        );
    }
    if has_cyrillic
        && !installed_langs
            .iter()
            .any(|l| l.tag.starts_with("ru") || l.tag.starts_with("uk"))
    {
        return Some(
            "Install Cyrillic OCR in Windows Settings > Time & language > Language & region"
                .to_string(),
        );
    }
    if has_arabic && !installed_langs.iter().any(|l| l.tag.starts_with("ar")) {
        return Some(
            "Install Arabic OCR in Windows Settings > Time & language > Language & region"
                .to_string(),
        );
    }
    None
}

/// Computes the average luminance across an image.
/// Uses ITU-R BT.601 standard weights: 0.299 R + 0.587 G + 0.114 B.
pub fn compute_average_luminance(image: &ImageBuffer) -> u8 {
    let total_pixels = (image.width * image.height) as usize;
    if total_pixels == 0 {
        return 255;
    }
    let mut sum: u64 = 0;
    for i in 0..total_pixels {
        let idx = i * 4;
        let b = image.pixels[idx] as u64;
        let g = image.pixels[idx + 1] as u64;
        let r = image.pixels[idx + 2] as u64;
        let lum = (r * 299 + g * 587 + b * 114) / 1000;
        sum += lum;
    }
    (sum / total_pixels as u64) as u8
}

/// Inverts the RGB channels of an image while preserving alpha.
/// Essential for dark-mode IDEs, code editors, and terminals (luminance < 128).
pub fn invert_image_colors(image: &ImageBuffer) -> ImageBuffer {
    let mut out = image.clone();
    let total_pixels = (image.width * image.height) as usize;
    for i in 0..total_pixels {
        let idx = i * 4;
        out.pixels[idx] = 255 - out.pixels[idx]; // B
        out.pixels[idx + 1] = 255 - out.pixels[idx + 1]; // G
        out.pixels[idx + 2] = 255 - out.pixels[idx + 2]; // R
    }
    out
}

/// Upscales an image by an integer factor using smooth bilinear interpolation.
/// Windows.Media.Ocr recognition rate drops below 14px font size; upscaling 2x-3x
/// brings small characters to optimal recognition dimensions.
pub fn upscale_bilinear(image: &ImageBuffer, factor: usize) -> ImageBuffer {
    if factor <= 1 || image.width == 0 || image.height == 0 {
        return image.clone();
    }
    let w = image.width as usize;
    let h = image.height as usize;
    let w_out = w * factor;
    let h_out = h * factor;
    let mut out = ImageBuffer::new(w_out as u32, h_out as u32);

    for y in 0..h_out {
        let fy = ((y as f32 + 0.5) / (factor as f32) - 0.5).clamp(0.0, (h - 1) as f32);
        let y0 = fy as usize;
        let y1 = (y0 + 1).min(h - 1);
        let ty = fy.fract();

        for x in 0..w_out {
            let fx = ((x as f32 + 0.5) / (factor as f32) - 0.5).clamp(0.0, (w - 1) as f32);
            let x0 = fx as usize;
            let x1 = (x0 + 1).min(w - 1);
            let tx = fx.fract();

            let idx_out = (y * w_out + x) * 4;

            for c in 0..4 {
                let p00 = image.pixels[(y0 * w + x0) * 4 + c] as f32;
                let p10 = image.pixels[(y0 * w + x1) * 4 + c] as f32;
                let p01 = image.pixels[(y1 * w + x0) * 4 + c] as f32;
                let p11 = image.pixels[(y1 * w + x1) * 4 + c] as f32;

                let top = p00 * (1.0 - tx) + p10 * tx;
                let bottom = p01 * (1.0 - tx) + p11 * tx;
                let val = (top * (1.0 - ty) + bottom * ty + 0.5) as u8;
                out.pixels[idx_out + c] = val;
            }
        }
    }
    out
}

/// Applies light spatial sharpening using an unsharp mask kernel:
/// center: (1 + 4*alpha), 4-neighbors: -alpha, with alpha = 0.20
pub fn sharpen_image_light(image: &ImageBuffer) -> ImageBuffer {
    let w = image.width as usize;
    let h = image.height as usize;
    if w < 3 || h < 3 {
        return image.clone();
    }
    let mut out = image.clone();
    let alpha = 0.20f32;
    let center_weight = 1.0 + 4.0 * alpha;

    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let idx = (y * w + x) * 4;
            let idx_u = ((y - 1) * w + x) * 4;
            let idx_d = ((y + 1) * w + x) * 4;
            let idx_l = (y * w + (x - 1)) * 4;
            let idx_r = (y * w + (x + 1)) * 4;

            for c in 0..3 {
                let center = image.pixels[idx + c] as f32;
                let up = image.pixels[idx_u + c] as f32;
                let down = image.pixels[idx_d + c] as f32;
                let left = image.pixels[idx_l + c] as f32;
                let right = image.pixels[idx_r + c] as f32;

                let val = center * center_weight - alpha * (up + down + left + right);
                out.pixels[idx + c] = val.clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    out
}

/// Scans text for actionable items such as URLs and email addresses.
pub fn detect_text_actions(text: &str) -> Vec<OcrAction> {
    let mut actions = Vec::new();

    // URL detection (http://, https://, or www.)
    let url_regex = regex::Regex::new(r#"(https?://[^\s<>"]+|www\.[^\s<>"]+)"#);
    if let Ok(re) = url_regex {
        for mat in re.find_iter(text) {
            let mut url = mat
                .as_str()
                .trim_end_matches(['.', ',', ';', ')', ']'])
                .to_string();
            if url.starts_with("www.") {
                url = format!("https://{url}");
            }
            if !actions
                .iter()
                .any(|a| matches!(a, OcrAction::OpenUrl(u) if u == &url))
            {
                actions.push(OcrAction::OpenUrl(url));
            }
        }
    }

    // Email detection
    let email_regex = regex::Regex::new(r#"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"#);
    if let Ok(re) = email_regex {
        for mat in re.find_iter(text) {
            let email = mat.as_str().to_string();
            if !actions
                .iter()
                .any(|a| matches!(a, OcrAction::SendEmail(e) if e == &email))
            {
                actions.push(OcrAction::SendEmail(email));
            }
        }
    }

    actions
}

/// Reconstructs natural formatting from recognized word bounding boxes.
/// Formats line breaks, paragraph gaps, indentation, bullet lists, columns,
/// and tab-separated tables using active configuration options.
pub fn reconstruct_layout(words: Vec<OcrWordBox>) -> String {
    reconstruct_layout_with_options(words, get_ocr_keep_line_breaks())
}

/// Reconstructs layout allowing explicit override of line-break joining behavior.
pub fn reconstruct_layout_with_options(
    mut words: Vec<OcrWordBox>,
    keep_line_breaks: bool,
) -> String {
    if words.is_empty() {
        return String::new();
    }

    // Sort words primarily top-to-bottom, secondarily left-to-right
    words.sort_by(|a, b| {
        let diff_y = a.y - b.y;
        if diff_y.abs() < 6.0 {
            a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal)
        }
    });

    // Detect multi-column text layout if two major vertical columns exist
    let min_x = words.iter().map(|w| w.x).fold(f32::INFINITY, f32::min);
    let max_x = words.iter().map(|w| w.x + w.width).fold(0.0f32, f32::max);

    let mut distinct_column_split: Option<(Vec<OcrWordBox>, Vec<OcrWordBox>)> = None;

    // Only test for multi-column if total width is wide enough (> 300px)
    if max_x - min_x > 300.0 {
        let step = 10.0f32;
        let mut test_x = min_x + 60.0;
        while test_x + 60.0 < max_x - 60.0 {
            let gutter_left = test_x;
            let gutter_right = test_x + 60.0;

            let in_gutter = words.iter().any(|w| {
                let w_right = w.x + w.width;
                !(w_right <= gutter_left || w.x >= gutter_right)
            });

            if !in_gutter {
                let left: Vec<OcrWordBox> = words
                    .iter()
                    .filter(|w| w.x + w.width <= gutter_left)
                    .cloned()
                    .collect();
                let right: Vec<OcrWordBox> = words
                    .iter()
                    .filter(|w| w.x >= gutter_right)
                    .cloned()
                    .collect();

                let left_w = left.iter().map(|w| w.x + w.width).fold(0.0f32, f32::max)
                    - left.iter().map(|w| w.x).fold(f32::INFINITY, f32::min);
                let right_w = right.iter().map(|w| w.x + w.width).fold(0.0f32, f32::max)
                    - right.iter().map(|w| w.x).fold(f32::INFINITY, f32::min);

                // Both columns must have at least 4 words and width >= 60px
                if left.len() >= 4 && right.len() >= 4 && left_w >= 60.0 && right_w >= 60.0 {
                    distinct_column_split = Some((left, right));
                    break;
                }
            }
            test_x += step;
        }
    }

    if let Some((col1_words, col2_words)) = distinct_column_split {
        // If content is a structured table with 3+ aligned columns (2+ tabs per line), don't split columns
        let single_layout = reconstruct_single_column_layout(words.clone(), keep_line_breaks);
        let max_tabs_per_line = single_layout
            .lines()
            .map(|l| l.chars().filter(|c| *c == '\t').count())
            .max()
            .unwrap_or(0);
        if max_tabs_per_line < 2 {
            let col1 = reconstruct_single_column_layout(col1_words, keep_line_breaks);
            let col2 = reconstruct_single_column_layout(col2_words, keep_line_breaks);
            return format!("{col1}\n\n{col2}");
        }
    }

    reconstruct_single_column_layout(words, keep_line_breaks)
}

fn reconstruct_single_column_layout(words: Vec<OcrWordBox>, keep_line_breaks: bool) -> String {
    if words.is_empty() {
        return String::new();
    }

    // Group words into lines based on vertical overlap
    let mut lines: Vec<Vec<OcrWordBox>> = Vec::new();

    for word in words {
        let mut placed = false;
        let word_top = word.y;
        let word_bottom = word.y + word.height;
        let word_mid = word.y + word.height / 2.0;

        for line in &mut lines {
            let line_top = line.iter().map(|w| w.y).fold(f32::INFINITY, f32::min);
            let line_bottom = line.iter().map(|w| w.y + w.height).fold(0.0f32, f32::max);
            let line_mid = (line_top + line_bottom) / 2.0;
            let avg_h = line.iter().map(|w| w.height).sum::<f32>() / line.len() as f32;

            let overlap = word_bottom.min(line_bottom) - word_top.max(line_top);
            let mid_diff = (word_mid - line_mid).abs();
            let tolerance = (avg_h * 0.55).max(6.0);

            if overlap > avg_h * 0.3 || mid_diff <= tolerance {
                line.push(word.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            lines.push(vec![word]);
        }
    }

    // Sort lines top-to-bottom
    lines.sort_by(|l1, l2| {
        let y1 = l1.first().map(|w| w.y).unwrap_or(0.0);
        let y2 = l2.first().map(|w| w.y).unwrap_or(0.0);
        y1.partial_cmp(&y2).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Within each line, sort words left-to-right
    for line in &mut lines {
        line.sort_by(|w1, w2| w1.x.partial_cmp(&w2.x).unwrap_or(std::cmp::Ordering::Equal));
    }

    // Determine baseline left margin and average character width
    let min_left = lines
        .iter()
        .filter_map(|l| l.first().map(|w| w.x))
        .fold(f32::INFINITY, f32::min);

    let avg_char_w = {
        let mut total_w = 0.0;
        let mut total_chars = 0;
        for line in &lines {
            for word in line {
                let char_len = word.text.chars().count();
                if char_len > 0 {
                    total_w += word.width;
                    total_chars += char_len;
                }
            }
        }
        if total_chars > 0 {
            total_w / total_chars as f32
        } else {
            8.0
        }
    };

    let avg_line_h = {
        let mut total_h = 0.0;
        let mut count = 0;
        for line in &lines {
            if let Some(w) = line.first() {
                total_h += w.height;
                count += 1;
            }
        }
        if count > 0 {
            total_h / count as f32
        } else {
            16.0
        }
    };

    // Detect code mode: programming language tokens, syntax structures, or braces
    let is_code = lines.iter().any(|line| {
        line.iter().any(|w| {
            let t = w.text.as_str();
            t == "fn"
                || t == "let"
                || t == "pub"
                || t == "struct"
                || t == "impl"
                || t == "for"
                || t == "if"
                || t == "return"
                || t == "match"
                || t == "mut"
                || t.contains('{')
                || t.contains('}')
                || t.contains("->")
                || t.contains("::")
                || t.contains("Result<")
                || t.contains("Ok(")
        })
    });

    // Detect if content is a table: multiple lines with large gaps separating aligned columns
    let is_table = !is_code
        && lines.len() >= 2
        && lines
            .iter()
            .filter(|l| {
                l.len() >= 3
                    && l.windows(2)
                        .any(|pair| (pair[1].x - (pair[0].x + pair[0].width)) > 25.0)
            })
            .count()
            >= (lines.len().max(1) * 2 / 3);

    // Compute monospace character width for code mode
    let mono_char_w = if is_code {
        let mut char_widths: Vec<f32> = Vec::new();
        for line in &lines {
            for w in line {
                let c_count = w.text.chars().count();
                if c_count >= 2 {
                    char_widths.push(w.width / c_count as f32);
                }
            }
        }
        if !char_widths.is_empty() {
            char_widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            char_widths[char_widths.len() / 2]
        } else {
            avg_char_w
        }
    } else {
        avg_char_w
    };

    let mut output_lines = Vec::new();
    let mut prev_line_bottom = 0.0f32;

    for (line_idx, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }

        let line_top = line.iter().map(|w| w.y).fold(f32::INFINITY, f32::min);
        let line_bottom = line.iter().map(|w| w.y + w.height).fold(0.0f32, f32::max);

        // Check for paragraph gap between lines (only for prose, never for tables or code)
        if line_idx > 0 && line_top > prev_line_bottom + avg_line_h * 0.75 && !is_table && !is_code
        {
            output_lines.push(String::new()); // Blank line for paragraph separation
        }
        prev_line_bottom = line_bottom;

        let first_w = &line[0];
        let mut line_str = String::new();

        // Calculate leading indentation
        let indent_px = (first_w.x - min_left).max(0.0);
        if is_code {
            if indent_px >= mono_char_w * 1.5 {
                let mut space_count = ((indent_px / mono_char_w).round() as usize).clamp(1, 32);
                // Standard 4-space quantization if close to multiple of 4
                if (space_count as i32 % 4).abs() == 1 && space_count >= 3 {
                    space_count = ((space_count + 1) / 4) * 4;
                }
                for _ in 0..space_count {
                    line_str.push(' ');
                }
            }
        } else if indent_px >= avg_char_w * 2.0 && !is_table {
            let space_count = ((indent_px / avg_char_w).round() as usize).clamp(1, 16);
            for _ in 0..space_count {
                line_str.push(' ');
            }
        }

        // Detect bullet list start
        let bullet_prefixes = ["•", "*", "-", "·", "+"];
        let is_bullet = bullet_prefixes.iter().any(|b| first_w.text == *b);

        for (i, word) in line.iter().enumerate() {
            if i > 0 {
                let prev_w = &line[i - 1];
                let gap = word.x - (prev_w.x + prev_w.width);

                if is_code {
                    // Code mode: calculate exact monospace gap and NEVER merge adjacent words
                    let is_punct_tight_left = word.text.starts_with('(')
                        || word.text.starts_with(')')
                        || word.text.starts_with(';')
                        || word.text.starts_with(',')
                        || word.text.starts_with('.')
                        || (prev_w.text == "-" && word.text == ">")
                        || (prev_w.text.ends_with(':') && word.text.starts_with(':'));

                    let is_punct_tight_right = prev_w.text.ends_with('(')
                        || prev_w.text.ends_with('&')
                        || prev_w.text == "::";

                    if is_punct_tight_left || is_punct_tight_right {
                        // Glue punctuation tightly in code
                    } else {
                        let space_gap = if gap > mono_char_w * 1.5 {
                            ((gap / mono_char_w).round() as usize).max(1)
                        } else {
                            1
                        };
                        for _ in 0..space_gap {
                            line_str.push(' ');
                        }
                    }
                } else if is_table && gap > avg_char_w * 2.8 {
                    // Distinct column in table: use tab separation
                    line_str.push('\t');
                } else {
                    line_str.push(' ');
                }
            }

            let mut word_text = word.text.clone();
            if is_table {
                // Table case: context-based digit disambiguation
                // 1. Region codes ending with -I or -l (e.g. us-east-I -> us-east-1, eu-west-l -> eu-west-1)
                if word_text.ends_with("-I") || word_text.ends_with("-l") {
                    let len = word_text.len();
                    word_text.replace_range(len - 1..len, "1");
                }
                // 2. Trailing digit with stray space after hyphen (e.g. "sa-east-" and "1")
                if word_text == "1" && line_str.ends_with("- ") {
                    line_str.pop();
                }
                // 3. Number with I/l at start (e.g. I2ms -> 12ms)
                if (word_text.starts_with('I') || word_text.starts_with('l'))
                    && word_text.len() >= 2
                {
                    if let Some(second) = word_text.chars().nth(1) {
                        if second.is_ascii_digit() {
                            word_text = format!("1{}", &word_text[1..]);
                        }
                    }
                }
            }

            line_str.push_str(&word_text);
        }

        // Ensure single space after bullet marker if missing
        if is_bullet
            && !line_str.starts_with("• ")
            && !line_str.starts_with("* ")
            && !line_str.starts_with("- ")
        {
            if let Some(ch) = line_str.chars().next() {
                if bullet_prefixes.contains(&ch.to_string().as_str())
                    && line_str.len() > 1
                    && !line_str[1..].starts_with(' ')
                {
                    line_str = format!("{} {}", ch, &line_str[ch.len_utf8()..]);
                }
            }
        }

        output_lines.push(line_str);
    }

    if is_code {
        let mut final_code_lines = Vec::new();
        let mut brace_stack: Vec<usize> = Vec::new();

        for line in output_lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let current_indent = line.chars().take_while(|c| *c == ' ').count();

            // When indentation drops, close any inner scopes that didn't have explicit closing braces
            while let Some(&top_indent) = brace_stack.last() {
                if current_indent <= top_indent && !trimmed.starts_with('}') {
                    brace_stack.pop();
                    let close_indent_spaces = " ".repeat(top_indent);
                    final_code_lines.push(format!("{close_indent_spaces}}}"));
                } else {
                    break;
                }
            }

            let open_count = trimmed.chars().filter(|c| *c == '{').count();
            let close_count = trimmed.chars().filter(|c| *c == '}').count();
            if open_count > close_count {
                for _ in 0..(open_count - close_count) {
                    brace_stack.push(current_indent);
                }
            } else if close_count > open_count {
                for _ in 0..(close_count - open_count) {
                    brace_stack.pop();
                }
            }

            final_code_lines.push(line);
        }

        while let Some(top_indent) = brace_stack.pop() {
            let close_indent_spaces = " ".repeat(top_indent);
            final_code_lines.push(format!("{close_indent_spaces}}}"));
        }

        return final_code_lines.join("\n");
    }

    if keep_line_breaks || is_table {
        output_lines.join("\n")
    } else {
        // "Join wrapped lines" mode: join non-empty prose lines within paragraphs
        let mut joined_paragraphs = Vec::new();
        let mut current_para: Vec<String> = Vec::new();

        for line in output_lines {
            if line.trim().is_empty() {
                if !current_para.is_empty() {
                    joined_paragraphs.push(current_para.join(" "));
                    current_para.clear();
                }
                joined_paragraphs.push(String::new());
            } else if line.starts_with("• ") || line.starts_with("- ") || line.starts_with("* ") {
                if !current_para.is_empty() {
                    joined_paragraphs.push(current_para.join(" "));
                    current_para.clear();
                }
                joined_paragraphs.push(line);
            } else {
                current_para.push(line);
            }
        }
        if !current_para.is_empty() {
            joined_paragraphs.push(current_para.join(" "));
        }
        joined_paragraphs.join("\n")
    }
}

/// Configuration options for OCR recognition.
#[derive(Debug, Clone)]
pub struct OcrConfig {
    pub upscale_factor: usize,
    pub apply_sharpening: bool,
    pub keep_line_breaks: bool,
    pub language_tag: Option<String>,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            upscale_factor: 0, // 0 = auto (3x for standard/small/code)
            apply_sharpening: false,
            keep_line_breaks: get_ocr_keep_line_breaks(),
            language_tag: get_selected_ocr_language(),
        }
    }
}

/// Executes Windows.Media.Ocr recognition on the input image with explicit configuration.
pub fn execute_ocr_with_config(
    image: &ImageBuffer,
    config: &OcrConfig,
) -> Result<OcrExtraction, String> {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }

    // Try selected language, fall back safely to user profile languages without crashing
    let engine = if let Some(tag) = &config.language_tag {
        let hstr = HSTRING::from(tag.as_str());
        if let Ok(lang) = Language::CreateLanguage(&hstr) {
            OcrEngine::TryCreateFromLanguage(&lang)
                .or_else(|_| OcrEngine::TryCreateFromUserProfileLanguages())
        } else {
            OcrEngine::TryCreateFromUserProfileLanguages()
        }
    } else {
        OcrEngine::TryCreateFromUserProfileLanguages()
    };

    let engine = engine.map_err(|e| format!("Windows OCR engine initialization error: {e}"))?;

    // 1. Invert dark background images (average luminance < 128)
    let avg_lum = compute_average_luminance(image);
    let preprocessed = if avg_lum < 128 {
        invert_image_colors(image)
    } else {
        image.clone()
    };

    // 2. Upscale small and standard text (3x for crisp recognition)
    let upscale_factor = if config.upscale_factor > 0 {
        config.upscale_factor
    } else if preprocessed.width < 1400 && preprocessed.height < 1400 {
        3
    } else if preprocessed.width < 2500 && preprocessed.height < 2500 {
        2
    } else {
        1
    };

    let ocr_target = if upscale_factor > 1 {
        upscale_bilinear(&preprocessed, upscale_factor)
    } else {
        preprocessed
    };

    let ocr_target = if config.apply_sharpening {
        sharpen_image_light(&ocr_target)
    } else {
        ocr_target
    };

    // 3. Create Windows.Graphics.Imaging.SoftwareBitmap
    let writer = DataWriter::new().map_err(|e| e.to_string())?;
    writer
        .WriteBytes(&ocr_target.pixels)
        .map_err(|e| e.to_string())?;
    let buffer = writer.DetachBuffer().map_err(|e| e.to_string())?;

    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        ocr_target.width as i32,
        ocr_target.height as i32,
    )
    .map_err(|e| format!("SoftwareBitmap creation failed: {e}"))?;

    // 4. Run asynchronous OCR
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|e| format!("OCR recognition execution failed: {e}"))?;

    let raw_text = result.Text().map_err(|e| e.to_string())?.to_string();

    // 5. Extract word boxes and scale coordinates back
    let scale_factor = upscale_factor as f32;
    let mut words = Vec::new();

    let lines = result.Lines().map_err(|e| e.to_string())?;
    for line in lines {
        let line_words = line.Words().map_err(|e| e.to_string())?;
        for word in line_words {
            let rect = word.BoundingRect().map_err(|e| e.to_string())?;
            words.push(OcrWordBox {
                text: word.Text().map_err(|e| e.to_string())?.to_string(),
                x: rect.X / scale_factor,
                y: rect.Y / scale_factor,
                width: rect.Width / scale_factor,
                height: rect.Height / scale_factor,
            });
        }
    }

    let formatted_text = reconstruct_layout_with_options(words.clone(), config.keep_line_breaks);
    let (line_count, word_count) = count_lines_and_words(&formatted_text);
    let actions = detect_text_actions(&formatted_text);

    let installed_langs = get_installed_ocr_languages();
    let missing_language_hint = detect_missing_language_ocr(&formatted_text, &installed_langs);

    Ok(OcrExtraction {
        raw_text,
        formatted_text,
        words,
        line_count,
        word_count,
        actions,
        missing_language_hint,
    })
}

/// Executes Windows.Media.Ocr recognition on the input image using default settings.
pub fn execute_ocr(image: &ImageBuffer) -> Result<OcrExtraction, String> {
    execute_ocr_with_config(image, &OcrConfig::default())
}

/// Computes consistent non-empty line and whitespace-delimited word counts from a string.
pub fn count_lines_and_words(text: &str) -> (usize, usize) {
    let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    let words = text.split_whitespace().count();
    (lines, words)
}

/// Floating toast and preview card for OCR text extraction results.
struct OcrCardState {
    extraction: OcrExtraction,
    scale: f32,
    keep_line_breaks: bool,
    should_close: Arc<AtomicBool>,
}

thread_local! {
    static OCR_CARD_STATE: std::cell::RefCell<Option<OcrCardState>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn ocr_preview_card_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            // Auto close toast after 8 seconds if not interacted
            let _ = SetTimer(Some(hwnd), 1, 8000, None);
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let px = (lparam.0 & 0xffff) as i16 as i32;
            let py = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            let pt = PointI::new(px, py);

            let mut should_destroy = false;
            let mut trigger_url = None;

            OCR_CARD_STATE.with(|st| {
                if let Some(state) = st.borrow_mut().as_mut() {
                    let s = state.scale;
                    let card_w = (450.0 * s).round() as i32;
                    let card_h = (190.0 * s).round() as i32;

                    // Close button [ ✕ ] at top-right
                    let close_btn = RectI::new(
                        card_w - (36.0 * s).round() as i32,
                        (6.0 * s).round() as i32,
                        card_w - (6.0 * s).round() as i32,
                        (34.0 * s).round() as i32,
                    );

                    // Copy again button
                    let copy_btn = RectI::new(
                        (14.0 * s).round() as i32,
                        card_h - (42.0 * s).round() as i32,
                        (110.0 * s).round() as i32,
                        card_h - (10.0 * s).round() as i32,
                    );

                    // Line breaks toggle button
                    let line_break_btn = RectI::new(
                        (116.0 * s).round() as i32,
                        card_h - (42.0 * s).round() as i32,
                        (230.0 * s).round() as i32,
                        card_h - (10.0 * s).round() as i32,
                    );

                    if close_btn.contains(pt) {
                        state.should_close.store(true, Ordering::Release);
                        should_destroy = true;
                    } else if copy_btn.contains(pt) {
                        let _ = copy_text_to_clipboard(&state.extraction.formatted_text);
                    } else if line_break_btn.contains(pt) {
                        let new_val = !state.keep_line_breaks;
                        state.keep_line_breaks = new_val;
                        set_ocr_keep_line_breaks(new_val);
                        state.extraction.formatted_text = reconstruct_layout_with_options(
                            state.extraction.words.clone(),
                            new_val,
                        );
                        let (l, w) = count_lines_and_words(&state.extraction.formatted_text);
                        state.extraction.line_count = l;
                        state.extraction.word_count = w;
                        let _ = copy_text_to_clipboard(&state.extraction.formatted_text);
                        unsafe {
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    } else {
                        // Action button: ONLY active when text contains a link
                        let link_action = state.extraction.actions.iter().find_map(|a| match a {
                            OcrAction::OpenUrl(url) => Some(url.clone()),
                            _ => None,
                        });
                        if let Some(url) = link_action {
                            let small_size = (10.0 * s).round().max(9.0) as u32;
                            let display_host = url
                                .trim_start_matches("https://")
                                .trim_start_matches("http://")
                                .trim_end_matches('/');
                            let short_host = if display_host.chars().count() > 18 {
                                let mut sh: String = display_host.chars().take(16).collect();
                                sh.push_str("..");
                                sh
                            } else {
                                display_host.to_string()
                            };
                            let action_label = format!("🌐 Open Link ({short_host})");
                            let (tw, _) = measure_system_text(&action_label, small_size, true);
                            let btn_w =
                                (tw + (20.0 * s).round() as i32).max((140.0 * s).round() as i32);
                            let action_btn = RectI::new(
                                (236.0 * s).round() as i32,
                                card_h - (42.0 * s).round() as i32,
                                (236.0 * s).round() as i32 + btn_w,
                                card_h - (10.0 * s).round() as i32,
                            );
                            if action_btn.contains(pt) {
                                trigger_url = Some(url);
                            }
                        }
                    }
                }
            });

            if should_destroy {
                let _ = DestroyWindow(hwnd);
            } else if let Some(url) = trigger_url {
                let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
                unsafe {
                    ShellExecuteW(
                        None,
                        w!("open"),
                        windows::core::PCWSTR(wide.as_ptr()),
                        None,
                        None,
                        SW_SHOW,
                    );
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let (scale, formatted_text, link_target, keep_breaks, missing_hint) = OCR_CARD_STATE
                .with(|st| {
                    if let Some(state) = st.borrow().as_ref() {
                        let link = state.extraction.actions.iter().find_map(|a| match a {
                            OcrAction::OpenUrl(url) => Some(url.clone()),
                            _ => None,
                        });
                        (
                            state.scale,
                            state.extraction.formatted_text.clone(),
                            link,
                            state.keep_line_breaks,
                            state.extraction.missing_language_hint.clone(),
                        )
                    } else {
                        (1.0, String::new(), None, true, None)
                    }
                });
            let (lines, words) = count_lines_and_words(&formatted_text);

            let w = (450.0 * scale).round() as u32;
            let h = (190.0 * scale).round() as u32;
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

            let title_size = (12.0 * scale).round().max(11.0) as u32;
            let small_size = (10.0 * scale).round().max(9.0) as u32;

            // Toast status bar at top: derived from the exact same formatted_text
            draw_system_text_rgba(
                &mut img,
                PointI::new((14.0 * scale).round() as i32, (12.0 * scale).round() as i32),
                &format!("✓ Copied {lines} lines ({words} words) to clipboard"),
                Color::rgb(52, 211, 153), // Emerald green
                title_size,
                true,
            );

            // Close button [ ✕ ]
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    (426.0 * scale).round() as i32,
                    (10.0 * scale).round() as i32,
                ),
                "✕",
                Color::rgb(160, 165, 175),
                title_size,
                true,
            );

            // Preview box
            let preview_rect = RectI::new(
                (14.0 * scale).round() as i32,
                (42.0 * scale).round() as i32,
                (436.0 * scale).round() as i32,
                (138.0 * scale).round() as i32,
            );
            for by in preview_rect.top..preview_rect.bottom {
                for bx in preview_rect.left..preview_rect.right {
                    if bx >= 0 && bx < w as i32 && by >= 0 && by < h as i32 {
                        img.set_pixel(bx as u32, by as u32, [28, 32, 38, 255]);
                    }
                }
            }

            // Render lines from the formatted text
            let line_spacing = (17.0 * scale).round() as i32;
            let mut cur_y = preview_rect.top + (8.0 * scale).round() as i32;
            let max_y = preview_rect.bottom - (14.0 * scale).round() as i32;
            let lines_vec: Vec<&str> = formatted_text.lines().collect();

            // If missing language hint exists, show warning at top of preview box
            if let Some(hint) = &missing_hint {
                draw_system_text_rgba(
                    &mut img,
                    PointI::new(preview_rect.left + (10.0 * scale).round() as i32, cur_y),
                    &format!("⚠️ {hint}"),
                    Color::rgb(245, 165, 36), // Lookout amber
                    small_size,
                    true,
                );
                cur_y += line_spacing;
            }

            for (idx, line) in lines_vec.iter().enumerate() {
                if cur_y + (12.0 * scale).round() as i32 > max_y {
                    let remaining = lines_vec.len() - idx;
                    draw_system_text_rgba(
                        &mut img,
                        PointI::new(preview_rect.left + (10.0 * scale).round() as i32, cur_y),
                        &format!("... ({remaining} more lines)"),
                        Color::rgb(160, 165, 175),
                        small_size,
                        false,
                    );
                    break;
                }
                let display_line = if line.chars().count() > 56 {
                    format!("{}...", line.chars().take(54).collect::<String>())
                } else {
                    line.to_string()
                };
                draw_system_text_rgba(
                    &mut img,
                    PointI::new(preview_rect.left + (10.0 * scale).round() as i32, cur_y),
                    &display_line,
                    Color::rgb(230, 233, 238),
                    small_size,
                    false,
                );
                cur_y += line_spacing;
            }

            // Copy Again Button
            let copy_btn = RectI::new(
                (14.0 * scale).round() as i32,
                (148.0 * scale).round() as i32,
                (110.0 * scale).round() as i32,
                (180.0 * scale).round() as i32,
            );
            for by in copy_btn.top..copy_btn.bottom {
                for bx in copy_btn.left..copy_btn.right {
                    if bx >= 0 && bx < w as i32 && by >= 0 && by < h as i32 {
                        img.set_pixel(bx as u32, by as u32, [38, 43, 51, 255]);
                    }
                }
            }
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    copy_btn.left + (10.0 * scale).round() as i32,
                    copy_btn.top + (7.0 * scale).round() as i32,
                ),
                "Copy again",
                Color::WHITE,
                small_size,
                true,
            );

            // Line Breaks Toggle Button: [ Break: Keep | Break: Join ]
            let line_break_btn = RectI::new(
                (116.0 * scale).round() as i32,
                (148.0 * scale).round() as i32,
                (230.0 * scale).round() as i32,
                (180.0 * scale).round() as i32,
            );
            for by in line_break_btn.top..line_break_btn.bottom {
                for bx in line_break_btn.left..line_break_btn.right {
                    if bx >= 0 && bx < w as i32 && by >= 0 && by < h as i32 {
                        img.set_pixel(bx as u32, by as u32, [38, 43, 51, 255]);
                    }
                }
            }
            let break_label = if keep_breaks {
                "Break: Keep"
            } else {
                "Break: Join"
            };
            draw_system_text_rgba(
                &mut img,
                PointI::new(
                    line_break_btn.left + (10.0 * scale).round() as i32,
                    line_break_btn.top + (7.0 * scale).round() as i32,
                ),
                break_label,
                Color::rgb(220, 225, 235),
                small_size,
                true,
            );

            // Open Link Action Button: shown ONLY when a link is present, with Lookout amber accent
            if let Some(url) = link_target {
                let display_host = url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/');
                let short_host = if display_host.chars().count() > 18 {
                    let mut sh: String = display_host.chars().take(16).collect();
                    sh.push_str("..");
                    sh
                } else {
                    display_host.to_string()
                };
                let action_label = format!("🌐 Open Link ({short_host})");
                let (tw, _) = measure_system_text(&action_label, small_size, true);
                let btn_w =
                    (tw + (20.0 * scale).round() as i32).max((140.0 * scale).round() as i32);
                let action_btn = RectI::new(
                    (236.0 * scale).round() as i32,
                    (148.0 * scale).round() as i32,
                    (236.0 * scale).round() as i32 + btn_w,
                    (180.0 * scale).round() as i32,
                );
                for by in action_btn.top..action_btn.bottom {
                    for bx in action_btn.left..action_btn.right {
                        if bx >= 0 && bx < w as i32 && by >= 0 && by < h as i32 {
                            img.set_pixel(bx as u32, by as u32, [245, 165, 36, 255]);
                            // Lookout amber accent #f5a524
                        }
                    }
                }
                draw_system_text_rgba(
                    &mut img,
                    PointI::new(
                        action_btn.left + (10.0 * scale).round() as i32,
                        action_btn.top + (7.0 * scale).round() as i32,
                    ),
                    &action_label,
                    Color::rgb(20, 23, 28), // Dark high-contrast text on amber
                    small_size,
                    true,
                );
            }

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

/// Displays the OCR result preview card and toast.
pub fn show_ocr_preview_card(extraction: OcrExtraction, scale: f32) {
    thread::Builder::new()
        .name("lookout-ocr-card".to_string())
        .spawn(move || unsafe {
            let hinst = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(_) => return,
            };
            let class_name = w!("LookoutOcrPreviewCard");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(ocr_preview_card_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let card_w = (450.0 * scale).round() as i32;
            let card_h = (190.0 * scale).round() as i32;
            let scr_w = GetSystemMetrics(SM_CXSCREEN);
            let scr_h = GetSystemMetrics(SM_CYSCREEN);

            let x = (scr_w - card_w - 24).max(10);
            let y = (scr_h - card_h - 48).max(10);

            let should_close = Arc::new(AtomicBool::new(false));
            let keep_line_breaks = get_ocr_keep_line_breaks();
            OCR_CARD_STATE.with(|st| {
                *st.borrow_mut() = Some(OcrCardState {
                    extraction,
                    scale,
                    keep_line_breaks,
                    should_close: Arc::clone(&should_close),
                });
            });

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("Lookout Text Capture"),
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

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .ok();
}

#[cfg(feature = "review")]
pub fn open_real_ocr_card_for_demo(scale: f32) -> Result<HWND, String> {
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class_name = w!("LookoutOcrPreviewCard");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(ocr_preview_card_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let card_w = (450.0 * scale).round() as i32;
        let card_h = (190.0 * scale).round() as i32;
        let scr_w = GetSystemMetrics(SM_CXSCREEN);
        let scr_h = GetSystemMetrics(SM_CYSCREEN);

        let x = (scr_w - card_w - 24).max(10);
        let y = (scr_h - card_h - 48).max(10);

        let formatted_text = "System status: online\nDocs: https://lookout.dev".to_string();
        let (line_count, word_count) = count_lines_and_words(&formatted_text);
        let sample_extraction = OcrExtraction {
            raw_text: formatted_text.clone(),
            formatted_text,
            words: vec![
                OcrWordBox {
                    text: "System".to_string(),
                    x: 10.0,
                    y: 10.0,
                    width: 50.0,
                    height: 16.0,
                },
                OcrWordBox {
                    text: "status:".to_string(),
                    x: 65.0,
                    y: 10.0,
                    width: 50.0,
                    height: 16.0,
                },
                OcrWordBox {
                    text: "online".to_string(),
                    x: 120.0,
                    y: 10.0,
                    width: 50.0,
                    height: 16.0,
                },
                OcrWordBox {
                    text: "Docs:".to_string(),
                    x: 10.0,
                    y: 30.0,
                    width: 40.0,
                    height: 16.0,
                },
                OcrWordBox {
                    text: "https://lookout.dev".to_string(),
                    x: 55.0,
                    y: 30.0,
                    width: 140.0,
                    height: 16.0,
                },
            ],
            line_count,
            word_count,
            actions: vec![OcrAction::OpenUrl("https://lookout.dev".to_string())],
            missing_language_hint: None,
        };

        OCR_CARD_STATE.with(|st| {
            *st.borrow_mut() = Some(OcrCardState {
                extraction: sample_extraction,
                scale,
                keep_line_breaks: true,
                should_close: Arc::new(AtomicBool::new(false)),
            });
        });

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name,
            w!("Lookout Text Capture"),
            WS_POPUP,
            x,
            y,
            card_w,
            card_h,
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
