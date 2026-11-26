//! Home "Recent": the recently changed files a person would care about.
//! Skipped places, hidden and system files, and temporary or cache files never
//! appear; documents, pictures, media, code and the user's own folders come
//! before everything else.

use std::path::{Path, PathBuf};

use crate::exclude::Excludes;
use crate::index_service::{read, SharedIndex};
use crate::item::{Item, Kind};
use crate::name_index::SearchParams;
use crate::ranking::RankingWeights;

const WINDOW_SECS: u32 = 7 * 24 * 3600;
/// How many of the week's files are looked at; junk among them is dropped.
const CANDIDATES: usize = 500;

const JUNK_ENDINGS: &[&str] = &[
    ".wal",
    "-wal",
    ".shm",
    "-shm",
    ".tmp",
    ".lock",
    ".log-journal",
];
const JUNK_FOLDERS: &[&str] = &["temp", "tmp", "cache", "caches"];
const USER_FOLDERS: &[&str] = &[
    "documents",
    "downloads",
    "desktop",
    "pictures",
    "screenshots",
    "videos",
    "music",
];
const USER_EXTENSIONS: &[&str] = &[
    // Documents
    "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv", "ppt", "pptx",
    "odp", "epub", // Pictures
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "heic", "svg", "tif", "tiff",
    // Audio and video
    "mp3", "wav", "flac", "m4a", "ogg", "mp4", "mkv", "mov", "avi", "webm", // Code
    "rs", "py", "js", "ts", "jsx", "tsx", "svelte", "java", "kt", "c", "h", "cpp", "hpp", "cs",
    "go", "rb", "php", "swift", "html", "css", "sql", "sh", "ps1", "ipynb",
];

/// A temporary, lock or database side file, or one meant to be hidden.
pub fn is_junk_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with('.') || n.starts_with("~$") || JUNK_ENDINGS.iter().any(|e| n.ends_with(e))
}

fn named(list: &[&str], part: &std::ffi::OsStr) -> bool {
    let part = part.to_string_lossy();
    list.iter().any(|n| n.eq_ignore_ascii_case(&part))
}

fn is_user_file(path: &Path) -> bool {
    let known = path.extension().is_some_and(|e| named(USER_EXTENSIONS, e));
    known
        || path
            .parent()
            .is_some_and(|d| d.iter().any(|part| named(USER_FOLDERS, part)))
}

#[cfg(windows)]
fn hidden_or_system(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    m.file_attributes() & (HIDDEN | SYSTEM) != 0
}

#[cfg(not(windows))]
fn hidden_or_system(_: &std::fs::Metadata) -> bool {
    false
}

/// Whether the file may be listed. Folders are judged below the root only: a
/// root the user chose is theirs, wherever it is.
fn listable(path: &Path, roots: &[PathBuf], excludes: &Excludes) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy());
    if name.is_none_or(|n| is_junk_name(&n)) || excludes.excluded_under(path, roots) {
        return false;
    }
    // The deepest root that reaches the file.
    let Some(root) = roots
        .iter()
        .filter(|r| path.starts_with(r) && !excludes.excludes_path(path, r))
        .max_by_key(|r| r.as_os_str().len())
    else {
        return false;
    };
    path.ancestors()
        .take_while(|p| *p != root.as_path())
        .enumerate()
        .all(|(depth, p)| {
            let cache = depth > 0 && p.file_name().is_some_and(|n| named(JUNK_FOLDERS, n));
            // A file that is gone is not listed either.
            !cache && std::fs::metadata(p).is_ok_and(|m| !hidden_or_system(&m))
        })
}

/// Up to `limit` files changed in the last week, the user's own kinds first.
pub fn files(
    index: &SharedIndex,
    roots: &[PathBuf],
    excludes: &Excludes,
    now: u32,
    limit: usize,
) -> Vec<Item> {
    let mut found: Vec<(PathBuf, String, u32)> = {
        let idx = read(index);
        let params = SearchParams {
            tokens: &[],
            kind: Some(&Kind::File),
            limit: CANDIDATES,
            now,
            weights: RankingWeights::default(),
            time: Some((now.saturating_sub(WINDOW_SECS), u32::MAX)),
            exts: &[],
            filter: None,
        };
        idx.search(&params, &|| false)
            .iter()
            .filter_map(|h| idx.entry_info(h.entry))
            .filter(|i| !i.is_dir)
            .map(|i| (i.path, i.name, i.modified))
            .collect()
    };
    // The user's files first, then the rest; newest first in each.
    found.sort_by_key(|(path, _, modified)| (!is_user_file(path), std::cmp::Reverse(*modified)));
    found
        .into_iter()
        .filter(|(path, ..)| listable(path, roots, excludes))
        .take(limit)
        .map(|(path, name, modified)| {
            let path = path.to_string_lossy().into_owned();
            let mut item = Item::new(format!("file:{path}"), Kind::File, name, "files");
            item.subtitle = Some(path.clone());
            item.uri = Some(path);
            item.modified_at = Some(u64::from(modified));
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_service::{new_shared_index, unix_now, IndexConfig, IndexService};
    use crate::pause::PauseState;
    use std::fs;
    use std::time::Duration;

    fn listed(root: &Path, excludes: Excludes, limit: usize) -> Vec<String> {
        let index = new_shared_index();
        let mut cfg = IndexConfig::new(vec![root.to_path_buf()], excludes.clone());
        cfg.duty = 1.0;
        cfg.watch = false;
        let svc = IndexService::start(index.clone(), cfg, PauseState::new()).unwrap();
        assert!(svc.wait_for_crawl(Duration::from_secs(10)));
        svc.stop();
        files(&index, &[root.to_path_buf()], &excludes, unix_now(), limit)
            .into_iter()
            .map(|i| i.title)
            .collect()
    }

    fn put(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn junk_extensions_are_recognised() {
        for n in [
            "a.wal",
            "DIPS-wal",
            "b.SHM",
            "c.tmp",
            "d.lock",
            "000003.log-journal",
            "~$report.docx",
            ".env",
        ] {
            assert!(is_junk_name(n), "{n}");
        }
        for n in ["report.pdf", "lock.png", "wallet.txt", "my journal.md"] {
            assert!(!is_junk_name(n), "{n}");
        }
    }

    #[test]
    fn recent_leaves_out_junk_skipped_and_cache_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for rel in [
            "report.pdf",
            "photo.png",
            "code/main.rs",
            "state.wal",
            "DIPS-wal",
            "x.shm",
            "y.tmp",
            "z.lock",
            "000003.log-journal",
            "AppData/Local/BrowserMetrics",
            "temp/draft.txt",
            "app/Cache/page.html",
            "dev.lookout.app/content/meta.json",
        ] {
            put(root, rel);
        }
        let excludes = Excludes::default().with_folders([root.join("dev.lookout.app")]);
        let mut got = listed(root, excludes, 20);
        got.sort();
        assert_eq!(got, ["main.rs", "photo.png", "report.pdf"]);
    }

    #[cfg(windows)]
    #[test]
    fn recent_leaves_out_hidden_and_system_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "seen.txt");
        let hidden = put(root, "hidden.txt");
        let inside = put(root, "secret/inside.txt");
        for (path, flag) in [(hidden, "+h"), (inside.parent().unwrap().into(), "+s")] {
            let ok = std::process::Command::new("attrib")
                .arg(flag)
                .arg(&path)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
        assert_eq!(listed(root, Excludes::default(), 20), ["seen.txt"]);
    }

    #[test]
    fn the_users_own_files_come_before_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for rel in ["a.dat", "b.bin", "notes.txt", "Downloads/setup.bin"] {
            put(root, rel);
        }
        let mut got = listed(root, Excludes::default(), 2);
        got.sort();
        assert_eq!(got, ["notes.txt", "setup.bin"]);
    }
}
