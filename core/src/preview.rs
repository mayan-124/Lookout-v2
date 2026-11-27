//! Read-only file preview for the search results and the disk pages.
//!
//! Nothing is cached or stored here. Files of sensitive types, files in places
//! the indexer skips, online-only placeholders and files over [`MAX_FILE`] are
//! never opened; text is cut at [`MAX_TEXT`] bytes; images over [`MAX_IMAGE`]
//! are not read at all; secrets are masked in any text shown with the same
//! masking used before indexing. Other types may get a picture from the Windows
//! shell ([`crate::thumbnail`]).

use crate::content::extract::{self, FileClass};
use crate::content::secrets;
use crate::share::Rules;
use serde::Serialize;
use std::io::Read;
use std::path::Path;

pub const MAX_TEXT: u64 = 64 * 1024;
pub const MAX_IMAGE: u64 = 15 * 1024 * 1024;
/// Larger files are not previewed in any way.
pub const MAX_FILE: u64 = 200 * 1024 * 1024;
/// Most names listed from a zip file.
pub const ZIP_ENTRIES: usize = 50;

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
    /// "text", "image", "folder", "archive" or "other".
    pub kind: &'static str,
    /// "ok", "hidden", "skipped", "online_only", "too_large", "missing", "denied",
    /// "locked" or "error".
    pub state: &'static str,
    pub message: String,
    pub text: Option<String>,
    pub truncated: bool,
    /// `data:` URL of the image.
    pub image: Option<String>,
    /// For folders: (items, bytes) when the scan knows them. For zip files: how
    /// many entries there are.
    pub items: Option<u64>,
    /// The first [`ZIP_ENTRIES`] entries of a zip file.
    pub entries: Option<Vec<ZipEntry>>,
    /// The shell may be asked for a thumbnail or icon.
    pub thumbnail: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    /// Unpacked size in bytes.
    pub size: u64,
}

/// Why a file must not be previewed, as (state, message). Decided from its name
/// and metadata; the file is not opened.
fn refusal(
    path: &Path,
    meta: &std::fs::Metadata,
    rules: &Rules,
) -> Option<(&'static str, &'static str)> {
    let link = || std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if extract::is_sensitive(path) {
        Some(("hidden", "Preview hidden: sensitive file"))
    } else if rules.hides(path) {
        Some(("skipped", "Preview hidden: in a place Lookout skips"))
    } else if link() {
        Some(("skipped", "Links are not previewed"))
    } else if is_online_only(attributes(meta)) {
        Some((
            "online_only",
            "Online-only file: not previewed (avoids downloading it)",
        ))
    } else if meta.len() > MAX_FILE {
        Some(("too_large", "Too large to preview"))
    } else {
        None
    }
}

/// Shortcuts are left alone: their picture is that of whatever they point to.
fn is_shortcut(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        ["lnk", "url", "pif"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}

/// Whether the shell may be asked for a picture of `path`.
pub fn may_thumbnail(path: &Path, rules: &Rules) -> bool {
    !is_shortcut(path)
        && std::fs::metadata(path).is_ok_and(|m| m.is_file() && refusal(path, &m, rules).is_none())
}

/// Builds the preview of `path`. `folder` is what the scan knows about a
/// folder: (items, bytes).
pub fn build(path: &Path, folder: Option<(u64, u64)>, rules: &Rules) -> Preview {
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
    if let Some((state, message)) = refusal(path, &meta, rules) {
        p.state = state;
        p.message = message.into();
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
    } else if ext == "zip" {
        // A damaged or unusual archive keeps the plain preview.
        if let Some((entries, total)) = zip_listing(path) {
            p.kind = "archive";
            p.entries = Some(entries);
            p.items = Some(total);
        }
    }
    p.thumbnail = p.state == "ok" && matches!(p.kind, "other" | "archive") && !is_shortcut(path);
    p
}

/// Names and sizes from the zip's directory; nothing is unpacked.
fn zip_listing(path: &Path) -> Option<(Vec<ZipEntry>, u64)> {
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let total = zip.len();
    let mut entries = Vec::new();
    for i in 0..total.min(ZIP_ENTRIES) {
        let entry = zip.by_index_raw(i).ok()?;
        let name = entry.name().ok()?;
        entries.push(ZipEntry {
            name: secrets::mask(&name).0.into_owned(),
            size: entry.size(),
        });
    }
    Some((entries, total as u64))
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

pub fn base64(bytes: &[u8]) -> String {
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

    /// The temp folder lies inside AppData, so it counts as an added folder.
    fn rules(dir: &Path) -> Rules {
        Rules {
            roots: vec![dir.to_path_buf()],
            ..Rules::default()
        }
    }

    fn view(dir: &Path, path: &Path) -> Preview {
        build(path, None, &rules(dir))
    }

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
            let v = view(d.path(), &p);
            assert_eq!(v.state, "hidden", "{name}");
            assert!(!v.thumbnail && !may_thumbnail(&p, &rules(d.path())));
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
        let v = view(d.path(), &p);
        assert!(v.truncated);
        assert!(v.text.unwrap().len() as u64 <= MAX_TEXT);
        let small = file(d.path(), "b.txt", b"short");
        let v = view(d.path(), &small);
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
        let text = view(d.path(), &p).text.unwrap();
        assert!(!text.contains(secret), "{text}");
        assert!(text.contains(secrets::MASK));
    }

    #[test]
    fn images_over_the_cap_are_not_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("huge.png");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_IMAGE + 1).unwrap();
        let v = view(d.path(), &p);
        assert_eq!(v.state, "too_large");
        assert!(v.image.is_none());
        let ok = file(d.path(), "ok.png", &[1, 2, 3, 4]);
        let v = view(d.path(), &ok);
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
        assert_eq!(view(d.path(), &d.path().join("gone.txt")).state, "missing");
        let v = build(d.path(), Some((7, 99)), &rules(d.path()));
        assert_eq!((v.kind, v.items, v.size), ("folder", Some(7), 99));
    }

    #[test]
    fn other_files_may_get_a_thumbnail_but_shortcuts_do_not() {
        let d = tempfile::tempdir().unwrap();
        for (name, yes) in [
            ("a.docx", true),
            ("a.pdf", true),
            ("a.mp4", true),
            ("a.bin", true),
            ("a.lnk", false),
            ("a.url", false),
        ] {
            let p = file(d.path(), name, b"x");
            let v = view(d.path(), &p);
            assert_eq!((v.kind, v.thumbnail), ("other", yes), "{name}");
            assert_eq!(may_thumbnail(&p, &rules(d.path())), yes, "{name}");
        }
        let text = file(d.path(), "a.txt", b"x");
        assert!(!view(d.path(), &text).thumbnail);
        assert!(!may_thumbnail(d.path(), &rules(d.path())), "a folder");
    }

    #[test]
    fn skipped_places_are_not_previewed() {
        let d = tempfile::tempdir().unwrap();
        for sub in ["node_modules", "AppData", ".git", "private"] {
            std::fs::create_dir(d.path().join(sub)).unwrap();
            let p = file(&d.path().join(sub), "a.docx", b"x");
            let mut r = rules(d.path());
            r.excludes = r.excludes.with_folders([d.path().join("private")]);
            let v = build(&p, None, &r);
            assert_eq!(v.state, "skipped", "{sub}");
            assert!(!v.thumbnail && !may_thumbnail(&p, &r), "{sub}");
        }
        // Lookout's own data stays hidden even when it is an indexed folder.
        let own = crate::exclude::own_data_folders().remove(0);
        assert!(rules(&own).hides(&own.join("settings.json")));
        // Outside every indexed folder, AppData is skipped.
        let p = file(d.path(), "b.docx", b"x");
        assert!(!may_thumbnail(&p, &Rules::default()));
    }

    #[test]
    fn files_over_the_size_cap_are_not_opened() {
        let d = tempfile::tempdir().unwrap();
        for name in ["big.docx", "big.txt", "big.zip"] {
            let p = d.path().join(name);
            let f = std::fs::File::create(&p).unwrap();
            f.set_len(MAX_FILE + 1).unwrap();
            let v = view(d.path(), &p);
            assert_eq!(v.state, "too_large", "{name}");
            assert!(v.text.is_none() && v.entries.is_none() && !v.thumbnail);
            assert!(!may_thumbnail(&p, &rules(d.path())));
        }
    }

    #[cfg(windows)]
    #[test]
    fn online_only_files_are_not_previewed_or_thumbnailed() {
        let d = tempfile::tempdir().unwrap();
        let p = file(d.path(), "cloud.docx", b"x");
        let marked = std::process::Command::new("attrib")
            .arg("+o")
            .arg(&p)
            .status()
            .unwrap()
            .success();
        assert!(marked);
        let v = view(d.path(), &p);
        assert_eq!(v.state, "online_only");
        assert!(!v.thumbnail && !may_thumbnail(&p, &rules(d.path())));
    }

    #[test]
    fn zip_listing_is_capped_masked_and_survives_damage() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        let secret = "AKIAIOSFODNN7EXAMPLE";
        zip.start_file(format!("key = {secret}.txt"), options)
            .unwrap();
        zip.write_all(b"hello").unwrap();
        for n in 0..60 {
            zip.start_file(format!("f{n:02}.txt"), options).unwrap();
        }
        zip.finish().unwrap();
        let v = view(d.path(), &p);
        assert_eq!((v.kind, v.items, v.thumbnail), ("archive", Some(61), true));
        let entries = v.entries.unwrap();
        assert_eq!(entries.len(), ZIP_ENTRIES);
        assert_eq!(entries[0].size, 5);
        assert!(!entries[0].name.contains(secret), "{}", entries[0].name);
        assert_eq!(entries[1].name, "f00.txt");
        let bad = file(d.path(), "bad.zip", b"not a zip");
        let v = view(d.path(), &bad);
        assert_eq!((v.kind, v.state), ("other", "ok"));
        assert!(v.entries.is_none() && v.thumbnail);
    }
}
