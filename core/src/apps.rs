//! Installed applications: Start Menu shortcut folders, and the packaged
//! (Store) apps Windows lists in its Apps folder, which have no shortcut file.

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

/// How a Store app is named: this prefix followed by its app user model id.
const STORE_PREFIX: &str = r"shell:AppsFolder\";

/// A packaged app's id is `PackageFamilyName!AppName`. Anything else (a path,
/// a web address, other characters) is refused, so only an app can be started.
fn is_store_id(id: &str) -> bool {
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    id.split_once('!')
        .is_some_and(|(family, app)| plain(family) && plain(app))
}

/// Whether `uri` names a Store app (it is not a file on disk).
pub fn is_store_uri(uri: &str) -> bool {
    uri.strip_prefix(STORE_PREFIX).is_some_and(is_store_id)
}

/// Adds the Store apps among `listed` (name, app id) to `apps`. A Start Menu
/// shortcut with the same name wins.
pub fn add_store_apps(apps: &mut Vec<AppEntry>, listed: Vec<(String, String)>) {
    let mut seen: HashSet<String> = apps.iter().map(|a| a.lower.clone()).collect();
    for (name, id) in listed {
        let lower = crate::ranking::fold_lower(&name);
        if !is_store_id(&id)
            || lower.is_empty()
            || lower.contains("uninstall")
            || !seen.insert(lower.clone())
        {
            continue;
        }
        let path = PathBuf::from(format!("{STORE_PREFIX}{id}"));
        apps.push(AppEntry { name, lower, path });
    }
    apps.sort_by(|a, b| a.lower.cmp(&b.lower));
}

/// Name and id of every entry in the Windows Apps folder. Empty if Windows
/// cannot be asked.
#[cfg(windows)]
pub fn list_apps_folder() -> Vec<(String, String)> {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    // Its own thread: COM is set up per thread, and the caller's is unknown.
    std::thread::spawn(|| {
        // SAFETY: paired with CoUninitialize on this thread.
        if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
            return Vec::new();
        }
        let found = read_apps_folder().unwrap_or_default();
        // SAFETY: every COM object was dropped when `read_apps_folder` returned.
        unsafe { CoUninitialize() };
        found
    })
    .join()
    .unwrap_or_default()
}

#[cfg(not(windows))]
pub fn list_apps_folder() -> Vec<(String, String)> {
    Vec::new()
}

#[cfg(windows)]
fn read_apps_folder() -> windows::core::Result<Vec<(String, String)>> {
    use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, SHGetKnownFolderItem,
        KF_FLAG_DEFAULT, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
    };

    fn text(item: &IShellItem, form: SIGDN) -> Option<String> {
        // SAFETY: the returned string is ours to free, after it has been copied.
        unsafe {
            let p = item.GetDisplayName(form).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        }
    }

    // SAFETY: plain COM calls on an initialised thread.
    let items: IEnumShellItems = unsafe {
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
        folder.BindToHandler(None::<&IBindCtx>, &BHID_EnumItems)?
    };
    let mut out = Vec::new();
    loop {
        let mut next = [None];
        let mut got = 0;
        // SAFETY: one slot, and `got` says whether it was filled.
        unsafe { items.Next(&mut next, Some(&mut got))? };
        let Some(item) = next[0].take().filter(|_| got == 1) else {
            return Ok(out);
        };
        if let (Some(name), Some(id)) = (
            text(&item, SIGDN_NORMALDISPLAY),
            text(&item, SIGDN_PARENTRELATIVEPARSING),
        ) {
            out.push((name, id));
        }
    }
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
    fn store_apps_are_added_once_and_only_by_app_id() {
        let mut apps = vec![AppEntry {
            name: "Mail".into(),
            lower: "mail".into(),
            path: PathBuf::from("Mail.lnk"),
        }];
        let listed = [
            ("Mail", "Vendor.Mail_abc!App"),
            ("Calculator", "Vendor.Calc_abc!App"),
            ("A desktop program", r"C:\Tools\tool.exe"),
            ("A link", "https://example.com/!x"),
            ("Odd", "a!b c"),
        ];
        add_store_apps(
            &mut apps,
            listed.map(|(n, i)| (n.to_owned(), i.to_owned())).to_vec(),
        );
        let names: Vec<_> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Calculator", "Mail"]);
        assert_eq!(apps[1].path, PathBuf::from("Mail.lnk"));
        assert!(is_store_uri(&apps[0].path.to_string_lossy()));
        assert!(!is_store_uri(r"shell:AppsFolder\..\x!y"));
        assert!(!is_store_uri(r"C:\Apps\a!b"));
    }

    #[test]
    fn missing_folders_are_ignored() {
        assert!(scan_start_menu(&[PathBuf::from("/nope")]).is_empty());
    }
}
