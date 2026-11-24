//! Installed applications, discovered from Start Menu shortcut folders.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub name: String,
    pub lower: String,
    pub path: PathBuf,
}

/// Start Menu program folders for all users and the current user.
pub fn default_start_menu_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["ProgramData", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            dirs.push(
                PathBuf::from(base)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs"),
            );
        }
    }
    dirs
}

/// Collects `.lnk` shortcuts below `dirs`, skipping uninstallers and
/// de-duplicating by name (the first folder listed wins).
pub fn scan_start_menu(dirs: &[PathBuf]) -> Vec<AppEntry> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in dirs {
        collect(dir, 0, &mut seen, &mut apps);
    }
    apps.sort_by(|a, b| a.lower.cmp(&b.lower));
    apps
}

fn collect(dir: &Path, depth: usize, seen: &mut HashSet<String>, out: &mut Vec<AppEntry>) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let Ok(ft) = ent.file_type() else { continue };
        if ft.is_dir() {
            collect(&path, depth + 1, seen, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        {
            let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let lower = crate::ranking::fold_lower(&name);
            if lower.contains("uninstall") || !seen.insert(lower.clone()) {
                continue;
            }
            out.push(AppEntry { name, lower, path });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scans_shortcuts_skips_uninstallers_and_dedupes() {
        let t = tempfile::tempdir().unwrap();
        let common = t.path().join("common");
        let user = t.path().join("user");
        fs::create_dir_all(common.join("Vendor Suite")).unwrap();
        fs::create_dir_all(&user).unwrap();
        fs::write(common.join("Notepad.lnk"), b"").unwrap();
        fs::write(common.join("Vendor Suite").join("Editor.LNK"), b"").unwrap();
        fs::write(
            common.join("Vendor Suite").join("Uninstall Editor.lnk"),
            b"",
        )
        .unwrap();
        fs::write(common.join("readme.txt"), b"").unwrap();
        fs::write(user.join("notepad.lnk"), b"").unwrap();
        fs::write(user.join("Mine.lnk"), b"").unwrap();

        let apps = scan_start_menu(&[common.clone(), user]);
        let names: Vec<_> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Editor", "Mine", "Notepad"]);
        let notepad = apps.iter().find(|a| a.name == "Notepad").unwrap();
        assert_eq!(notepad.path, common.join("Notepad.lnk"));
    }

    #[test]
    fn missing_folders_are_ignored() {
        assert!(scan_start_menu(&[PathBuf::from("/nope")]).is_empty());
    }
}
