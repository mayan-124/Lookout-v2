//! Deciding which files have readable content, and reading it.
//!
//! Plain-text families are read directly (first 1 MiB). Images go to OCR.
//! Office and PDF files (docx, xlsx, pptx, pdf) go to the Office helper. Files that commonly
//! hold secrets (keys, certificates, password stores, `.env`) are never read.

use std::io::Read;
use std::path::Path;

/// At most this many bytes of a text file are indexed.
pub const MAX_TEXT_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    Text,
    Image,
    /// .docx, .xlsx, .pptx, .pdf: the text comes from the Office helper plugin.
    Office,
    Skip,
}

const TEXT_EXT: &[&str] = &[
    "txt", "md", "markdown", "log", "csv", "tsv", "json", "xml", "yaml", "yml", "toml", "ini",
    "cfg", "conf", "html", "htm", "rs", "py", "js", "ts", "tsx", "jsx", "java", "c", "h", "cpp",
    "hpp", "cs", "go", "rb", "php", "sh", "bat", "ps1", "sql", "tex", "rst", "org", "srt", "vtt",
];
const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"];
/// Never read: keys, certificates, password stores, environment files.
const SENSITIVE_EXT: &[&str] = &[
    "pem", "key", "pfx", "p12", "ppk", "kdbx", "kdb", "jks", "keystore", "gpg", "asc", "env",
];
const SENSITIVE_NAMES: &[&str] = &[
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "id_dsa",
    "credentials",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "secrets.json",
    "secret.json",
];

pub fn classify(path: &Path) -> FileClass {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if SENSITIVE_NAMES.contains(&name.as_str()) || name.starts_with(".env") {
        return FileClass::Skip;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if SENSITIVE_EXT.contains(&ext.as_str()) {
        FileClass::Skip
    } else if TEXT_EXT.contains(&ext.as_str()) {
        FileClass::Text
    } else if IMAGE_EXT.contains(&ext.as_str()) {
        FileClass::Image
    } else if super::office::is_office_ext(&ext) {
        FileClass::Office
    } else {
        FileClass::Skip
    }
}

/// Decodes UTF-8 (with or without BOM) or UTF-16 with BOM. `None` for binary data.
pub fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Some(utf16(rest, u16::from_le_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Some(utf16(rest, u16::from_be_bytes));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn utf16(bytes: &[u8], f: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| f(*c)).collect();
    String::from_utf16_lossy(&units)
}

/// Reads up to [`MAX_TEXT_BYTES`] of a text file.
pub fn read_text(path: &Path) -> std::io::Result<Option<String>> {
    Ok(decode_text(&read_bytes(path)?))
}

/// The raw first [`MAX_TEXT_BYTES`] of a file.
pub fn read_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_TEXT_BYTES)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

/// A one-line preview: whitespace collapsed, at most `max` characters.
pub fn preview(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut last_space = true;
    for c in text.chars() {
        if out.chars().count() >= max {
            break;
        }
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension_and_never_reads_sensitive_files() {
        let c = |p: &str| classify(Path::new(p));
        assert_eq!(c("notes.TXT"), FileClass::Text);
        assert_eq!(c("shot.PNG"), FileClass::Image);
        assert_eq!(c("report.pdf"), FileClass::Office);
        for p in [
            "server.pem",
            "id_rsa",
            ".env",
            ".env.local",
            "vault.kdbx",
            "a.key",
            "credentials",
        ] {
            assert_eq!(c(p), FileClass::Skip, "{p}");
        }
    }

    #[test]
    fn decodes_text_and_rejects_binary() {
        assert_eq!(decode_text(b"hello").as_deref(), Some("hello"));
        assert_eq!(decode_text(b"\xEF\xBB\xBFhi").as_deref(), Some("hi"));
        assert_eq!(
            decode_text(&[0xFF, 0xFE, b'h', 0, b'i', 0]).as_deref(),
            Some("hi")
        );
        assert!(decode_text(b"MZ\x00\x01binary").is_none());
    }

    #[test]
    fn preview_collapses_whitespace_and_truncates() {
        assert_eq!(preview("a \n\n  b\tc", 100), "a b c");
        assert_eq!(preview("abcdefgh", 3), "abc");
    }

    #[test]
    fn only_the_first_megabyte_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.txt");
        std::fs::write(&p, vec![b'a'; (MAX_TEXT_BYTES as usize) + 5000]).unwrap();
        assert_eq!(read_text(&p).unwrap().unwrap().len() as u64, MAX_TEXT_BYTES);
    }
}
