//! Text of .pdf files, through lopdf (pure Rust).
//!
//! The parser is not trusted: it runs under `catch_unwind` (a panic skips the
//! file; the helper process is the second layer, and in a release build with
//! `panic = "abort"` it is the only one), encrypted files are skipped, and the
//! page count, time and text size are bounded. A file with no text on any page
//! (a scan) is reported as "no text layer"; there is no OCR here.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use lopdf::Document;

use crate::extract::{push_clean, Limits};

pub const NO_TEXT_LAYER: &str = "no text layer";

fn has_encrypt(bytes: &[u8]) -> bool {
    bytes.windows(8).any(|w| w == b"/Encrypt")
}

/// Text of one PDF, or the reason it is skipped.
pub fn extract_pdf(bytes: &[u8], limits: &Limits) -> Result<String, String> {
    if bytes.len() as u64 > limits.max_file {
        return Err("larger than 50 MB".into());
    }
    // lopdf copes with junk before the header; refuse only what has none near the start.
    if !bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
    {
        return Err("not a PDF".into());
    }
    if has_encrypt(bytes) {
        return Err("encrypted PDF".into());
    }
    let deadline = Instant::now() + limits.time;
    let (max_pages, max_text) = (limits.max_pages, limits.max_text);
    catch_unwind(AssertUnwindSafe(|| {
        read(bytes, deadline, max_pages, max_text)
    }))
    .unwrap_or_else(|_| Err("the PDF reader failed on this file".into()))
}

fn read(
    bytes: &[u8],
    deadline: Instant,
    max_pages: usize,
    max_text: usize,
) -> Result<String, String> {
    let doc = Document::load_mem(bytes).map_err(|_| "not a readable PDF (corrupt)".to_owned())?;
    if doc.is_encrypted() {
        return Err("encrypted PDF".into());
    }
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    if pages.len() > max_pages {
        return Err(format!("more than {max_pages} pages"));
    }
    let mut out = String::new();
    for page in pages {
        if Instant::now() > deadline {
            return Err("time limit exceeded".into());
        }
        // A page whose fonts cannot be decoded is left out, not fatal.
        if let Ok(text) = doc.extract_text(&[page]) {
            push_clean(&mut out, &text);
            out.push('\n');
        }
        if out.len() >= max_text {
            break;
        }
    }
    if out.trim().is_empty() {
        return Err(NO_TEXT_LAYER.into());
    }
    let mut end = out.len().min(max_text);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    Ok(out)
}
