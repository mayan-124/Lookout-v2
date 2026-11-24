//! Read-only file preview for the disk pages.
//!
//! Nothing is cached or stored. Files of sensitive types are never opened; text
//! is cut at [`MAX_TEXT`] bytes and has secrets masked with the same masking
//! used before indexing; images over [`MAX_IMAGE`] are not read at all.

use crate::content::extract::{self, FileClass};
use crate::content::secrets;
use serde::Serialize;
use std::io::Read;
use std::path::Path;

pub const MAX_TEXT: u64 = 64 * 1024;
pub const MAX_IMAGE: u64 = 15 * 1024 * 1024;

const IMAGE_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
];

#[derive(Debug, Serialize, Default)]
pub struct Preview {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Seconds since the Unix epoch; 0 when unknown.
    pub modified: u64,
    /// "text", "image", "folder" or "other".
    pub kind: &'static str,
    /// "ok", "hidden", "online_only", "too_large", "missing", "denied", "locked" or "error".
    pub state: &'static str,
    pub message: String,
    pub text: Option<String>,
    pub truncated: bool,
    /// `data:` URL of the image.
    pub image: Option<String>,
    /// For folders: (items, bytes) when the scan knows them.
    pub items: Option<u64>,
}

/// Builds the preview of `path`. `folder` is what the scan knows about a
/// folder: (items, bytes).
pub fn build(path: &Path, folder: Option<(u64, u64)>) -> Preview {
    let mut p = Preview {
        name: path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        ),
        path: path.to_string_lossy().into_owned(),
        kind: "other",
        state: "ok",
        ..Preview::default()
    };
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            fail(&mut p, &e);
            return p;
        }
    };
    p.is_dir = meta.is_dir();
    p.size = meta.len();
    p.modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    if p.is_dir {
        p.kind = "folder";
        if let Some((items, bytes)) = folder {
            p.items = Some(items);
            p.size = bytes;
        }
        return p;
    }
    // Checked before anything is opened.
    if extract::is_sensitive(path) {
        p.state = "hidden";
        p.message = "Preview hidden: sensitive file".into();
        return p;
    }
    // Attributes come from the metadata above; the file itself is not opened.
    if is_online_only(attributes(&meta)) {
        p.state = "online_only";
        p.message = "Online-only file: not previewed (avoids downloading it)".into();
        return p;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if let Some((_, mime)) = IMAGE_TYPES.iter().find(|(e, _)| *e == ext) {
        p.kind = "image";
        if p.size > MAX_IMAGE {
            p.state = "too_large";
            p.message = "Too large to preview".into();
            return p;
        }
        match read_capped(path, MAX_IMAGE) {
            Ok(bytes) => p.image = Some(format!("data:{mime};base64,{}", base64(&bytes))),
            Err(e) => fail(&mut p, &e),
        }
    } else if extract::classify(path) == FileClass::Text {
        p.kind = "text";
        match read_capped(path, MAX_TEXT) {
            Ok(bytes) => {
                p.truncated = p.size > MAX_TEXT;
                match extract::decode_text(&bytes) {
                    Some(mut text) => {
                        if p.truncated {
                            // Cut at a line end so a secret is never split by the cap.
                            if let Some(i) = text.rfind('\n') {
                                text.truncate(i);
                            }
                        }
                        p.text = Some(secrets::mask(&text).0.into_owned());
                    }
                    None => p.kind = "other",
                }
            }
            Err(e) => fail(&mut p, &e),
        }
    }
    p
}

const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// True for cloud placeholders: opening them would download the file.
pub fn is_online_only(attrs: u32) -> bool {
    attrs & (ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(windows)]
fn attributes(meta: &std::fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
fn attributes(_: &std::fs::Metadata) -> u32 {
    0
}

fn fail(p: &mut Preview, e: &std::io::Error) {
    use std::io::ErrorKind;
    // 32 and 33: the file is open elsewhere without sharing.
    let locked = matches!(e.raw_os_error(), Some(32 | 33));
    let (state, message) = match e.kind() {
        ErrorKind::NotFound => ("missing", "This file is gone (moved or deleted)"),
        ErrorKind::PermissionDenied => ("denied", "Not readable without admin"),
        _ if locked => ("locked", "This file is in use by another program"),
        _ => ("error", "This file could not be read"),
    };
    p.state = state;
    p.message = message.into();
}

fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path)?.take(cap).read_to_end(&mut buf)?;
    Ok(buf)
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        p
    }

    #[test]
    fn sensitive_files_are_never_opened() {
        let d = tempfile::tempdir().unwrap();
        for name in [
            "id_rsa",
            "server.pem",
            "vault.kdbx",
            ".env",
            ".env.local",
            "a.key",
        ] {
            let p = file(d.path(), name, b"-----BEGIN PRIVATE KEY----- hunter2");
            let v = build(&p, None);
            assert_eq!(v.state, "hidden", "{name}");
            assert!(v.text.is_none() && v.image.is_none(), "{name}");
            assert_eq!(v.message, "Preview hidden: sensitive file");
        }
    }

    #[test]
    fn text_is_capped_and_marked_truncated() {
        let d = tempfile::tempdir().unwrap();
        let line = "hello world\n";
        let big = line.repeat(10_000);
        let p = file(d.path(), "a.txt", big.as_bytes());
        let v = build(&p, None);
        assert!(v.truncated);
        assert!(v.text.unwrap().len() as u64 <= MAX_TEXT);
        let small = file(d.path(), "b.txt", b"short");
        let v = build(&small, None);
        assert!(!v.truncated);
        assert_eq!(v.text.as_deref(), Some("short"));
    }

    #[test]
    fn text_secrets_are_masked() {
        let d = tempfile::tempdir().unwrap();
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let p = file(
            d.path(),
            "notes.md",
            format!("key = {secret}\nok\n").as_bytes(),
        );
        let text = build(&p, None).text.unwrap();
        assert!(!text.contains(secret), "{text}");
        assert!(text.contains(secrets::MASK));
    }

    #[test]
    fn images_over_the_cap_are_not_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("huge.png");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_IMAGE + 1).unwrap();
        let v = build(&p, None);
        assert_eq!(v.state, "too_large");
        assert!(v.image.is_none());
        let ok = file(d.path(), "ok.png", &[1, 2, 3, 4]);
        let v = build(&ok, None);
        assert_eq!(v.image.as_deref(), Some("data:image/png;base64,AQIDBA=="));
    }

    #[test]
    fn cloud_placeholders_are_recognised_by_attribute_bits() {
        assert!(is_online_only(0x40_0000)); // recall on data access
        assert!(is_online_only(0x4_0000 | 0x20)); // recall on open
        assert!(is_online_only(0x1000)); // offline
        assert!(!is_online_only(0x20 | 0x1)); // archive, read-only
        assert!(!is_online_only(0));
    }

    #[test]
    fn missing_files_and_folders_are_reported() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(build(&d.path().join("gone.txt"), None).state, "missing");
        let v = build(d.path(), Some((7, 99)));
        assert_eq!((v.kind, v.items, v.size), ("folder", Some(7), 99));
    }
}
