//! Names that are never indexed (brief section 9): system folders,
//! `node_modules`, `.git`, browser profiles and password-manager directories.
//! `AppData` is searchable by folder name; the contents of its cache-like
//! folders are not read (see [`APPDATA_SKIPPED`]).
//! Matching is by lowercase file or folder name at any depth below a root. A
//! root the user chose explicitly is never excluded itself.

use std::collections::HashSet;
use std::path::Path;

const DEFAULT_EXCLUDED: &[&str] = &[
    // System
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "$recycle.bin",
    "system volume information",
    "$windows.~bt",
    "recovery",
    // Developer noise
    "node_modules",
    ".git",
    // Browser profiles
    "user data",
    // Password managers
    "keepass",
    "1password",
    "bitwarden",
    "lastpass",
];

/// Folders inside `AppData` whose contents are never indexed: browser and
/// Electron caches, temp, crash dumps, logs and package caches. The folders'
/// own names stay searchable. Shown in Settings.
pub const APPDATA_SKIPPED: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "ShaderCache",
    "Service Worker",
    "IndexedDB",
    "blob_storage",
    "Temp",
    "CrashDumps",
    "Crashpad",
    "Crash Reports",
    "Logs",
    "npm-cache",
    "pip",
];

/// Package caches skipped wherever they are.
pub const ANYWHERE_SKIPPED: &[&str] = &[".npm", "_cacache"];

/// Every skip-list name, for the Settings screen.
pub fn skip_list() -> Vec<String> {
    APPDATA_SKIPPED
        .iter()
        .chain(ANYWHERE_SKIPPED)
        .map(|s| (*s).to_owned())
        .collect()
}

fn in_list(list: &[&str], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// True if a folder called `name` at `path` is listed by name but its contents
/// are not read: a skip-list name below an `AppData` folder, or a package cache.
pub fn skips_contents(path: &Path, name: &str) -> bool {
    in_list(ANYWHERE_SKIPPED, name)
        || (in_list(APPDATA_SKIPPED, name)
            && path.components().any(|c| {
                c.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("appdata")
            }))
}

#[derive(Debug, Clone)]
pub struct Excludes {
    names: HashSet<String>,
    /// Folders the user excluded, normalised (see [`folder_key`]).
    folders: Vec<String>,
}

/// Case-insensitive, separator-normalised form of a path with no trailing slash.
fn folder_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
    s.trim_end_matches('\\').to_owned()
}

impl Default for Excludes {
    fn default() -> Self {
        Self {
            names: DEFAULT_EXCLUDED.iter().map(|s| (*s).to_owned()).collect(),
            folders: Vec::new(),
        }
    }
}

impl Excludes {
    pub fn with_extra<I: IntoIterator<Item = String>>(mut self, extra: I) -> Self {
        self.names
            .extend(extra.into_iter().map(|n| n.to_lowercase()));
        self
    }

    /// Adds folders that are excluded together with everything below them.
    pub fn with_folders<I: IntoIterator<Item = std::path::PathBuf>>(mut self, folders: I) -> Self {
        self.folders.extend(
            folders
                .into_iter()
                .map(|f| folder_key(&f))
                .filter(|f| !f.is_empty()),
        );
        self
    }

    /// True if `path` is an excluded folder or lies below one.
    pub fn is_excluded_folder(&self, path: &Path) -> bool {
        if self.folders.is_empty() {
            return false;
        }
        let key = folder_key(path);
        self.folders.iter().any(|f| {
            key == *f
                || key
                    .strip_prefix(f.as_str())
                    .is_some_and(|r| r.starts_with('\\'))
        })
    }

    /// For readers of file contents (text, OCR, semantic): same rules, and
    /// `AppData` is not read at all.
    pub fn for_contents(&self) -> Self {
        let mut c = self.clone();
        c.names.insert("appdata".to_owned());
        c
    }

    pub fn is_excluded(&self, name: &str) -> bool {
        self.names.contains(&name.to_lowercase())
    }

    /// True if any component of `path` below `root` is excluded.
    pub fn excludes_path(&self, path: &Path, root: &Path) -> bool {
        if self.is_excluded_folder(path) {
            return true;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            return true;
        };
        let names: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let mut dir = root.to_path_buf();
        for (i, name) in names.iter().enumerate() {
            if self.is_excluded(name) {
                return true;
            }
            dir.push(name);
            // Only what lies below a skipped folder is excluded, not the folder.
            if i + 1 < names.len() && skips_contents(&dir, name) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_the_brief() {
        let e = Excludes::default();
        for n in ["node_modules", ".GIT", "Windows", "KeePass", "User Data"] {
            assert!(e.is_excluded(n), "{n}");
        }
        assert!(!e.is_excluded("Documents"));
        assert!(!e.is_excluded("AppData"), "AppData is searchable by name");
        assert!(e.for_contents().is_excluded("AppData"));
    }

    #[test]
    fn skip_list_hides_cache_contents_but_not_the_folder_names() {
        let e = Excludes::default();
        let root = Path::new(r"C:\Users\u\AppData\Local");
        let chrome = root.join("Google").join("Chrome");
        for n in [
            "Cache",
            "code cache",
            "GPUCache",
            "Service Worker",
            "IndexedDB",
            "Temp",
            "Logs",
        ] {
            let dir = chrome.join(n);
            assert!(!e.excludes_path(&dir, root), "{n} itself stays searchable");
            assert!(e.excludes_path(&dir.join("f_000001"), root), "{n} contents");
            assert!(skips_contents(&dir, n));
        }
        assert!(!e.excludes_path(&root.join("Programs").join("app.exe"), root));
        assert!(!e.excludes_path(&root.join("Microsoft"), root));
        // Outside AppData, ordinary folders called Cache or Logs are indexed.
        assert!(!skips_contents(Path::new(r"D:\Work\Logs"), "Logs"));
        assert!(!e.excludes_path(Path::new(r"D:\Work\Logs\a.txt"), Path::new(r"D:\Work")));
        // Package caches are skipped anywhere.
        assert!(skips_contents(Path::new(r"C:\Users\u\.npm"), ".npm"));
        assert!(e.excludes_path(Path::new(r"C:\Users\u\.npm\x\y"), Path::new(r"C:\Users\u")));
        assert!(skip_list().contains(&"GPUCache".to_owned()));
    }

    #[test]
    fn excluded_folders_cover_everything_below_and_nothing_beside() {
        let e = Excludes::default().with_folders([std::path::PathBuf::from(r"C:\Docs\Private\")]);
        assert!(e.is_excluded_folder(Path::new(r"c:\docs\private")));
        assert!(e.is_excluded_folder(Path::new(r"C:\Docs\PRIVATE\taxes\2025.pdf")));
        assert!(!e.is_excluded_folder(Path::new(r"C:\Docs\Private2\a.txt")));
        assert!(!e.is_excluded_folder(Path::new(r"C:\Docs\a.txt")));
        assert!(e.excludes_path(Path::new(r"C:\Docs\Private\a.txt"), Path::new(r"C:\Docs")));
        assert!(!Excludes::default().is_excluded_folder(Path::new(r"C:\x")));
    }

    #[test]
    fn extra_names_are_case_insensitive() {
        let e = Excludes::default().with_extra(["Secret Stuff".to_owned()]);
        assert!(e.is_excluded("secret stuff"));
    }

    #[test]
    fn root_itself_is_never_excluded_but_children_are() {
        let e = Excludes::default();
        let root = Path::new("/home/u/AppData/Roaming/MyApp");
        assert!(!e.excludes_path(&root.join("notes.txt"), root));
        assert!(e.excludes_path(&root.join("node_modules").join("x.js"), root));
        assert!(e.excludes_path(Path::new("/elsewhere/x"), root));
    }
}
