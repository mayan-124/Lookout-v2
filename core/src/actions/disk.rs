//! Actions the disk usage view offers on a folder or file: reveal in Explorer,
//! copy its path and Move to Recycle Bin. They run through the same gate as
//! every other action; the path always comes from the scan held by the backend.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target};
use crate::diskusage::Hints;
use crate::item::Kind;
use crate::share::Rules;

pub const REVEAL: &str = "reveal";
pub const COPY_PATH: &str = "copy_path";
pub const COPY_FILE: &str = "copy_file";
pub const RECYCLE: &str = "recycle";
/// Said by every Disk confirmation that moves something to the Recycle Bin.
const RECYCLE_NOTE: &str = "What you move goes to the Windows Recycle Bin and can be restored from there. Lookout has no in-app undo.";

fn existing_path(target: &Target) -> Result<PathBuf, ActionError> {
    let uri = target
        .uri
        .as_deref()
        .ok_or_else(|| ActionError::Refused("that row is not a path".into()))?;
    let path = PathBuf::from(uri);
    if !path.is_absolute() {
        return Err(ActionError::Refused(format!("{uri} is not a full path")));
    }
    if std::fs::symlink_metadata(&path).is_err() {
        return Err(ActionError::NothingToDo(format!("{uri} no longer exists")));
    }
    Ok(path)
}

fn file_or_folder(kind: &Kind) -> bool {
    matches!(kind, Kind::File | Kind::Folder)
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn snapshot_path(plan: &Plan) -> Result<PathBuf, ActionError> {
    plan.snapshot
        .get("path")
        .and_then(|p| p.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| ActionError::Failed("bad plan".into()))
}

// ---------------------------------------------------------------- reveal

pub trait Revealer: Send + Sync {
    fn reveal(&self, path: &Path) -> std::io::Result<()>;
}

/// Opens Explorer with the item selected.
pub struct ExplorerReveal;

impl Revealer for ExplorerReveal {
    fn reveal(&self, path: &Path) -> std::io::Result<()> {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
    }
}

pub struct RevealAction(pub Arc<dyn Revealer>);

impl Action for RevealAction {
    fn id(&self) -> &str {
        REVEAL
    }
    fn label(&self) -> &str {
        "Show in folder"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        file_or_folder(kind)
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let path = existing_path(target)?;
        Ok(Plan {
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: "Show in folder".into(),
                lines: vec![path.display().to_string()],
                warnings: vec![],
            },
            undoable: false,
            snapshot: serde_json::json!({ "path": path }),
            fingerprint: format!("reveal|{}", path.display()),
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        let path = snapshot_path(plan)?;
        self.0
            .reveal(&path)
            .map_err(|e| ActionError::Failed(format!("could not open Explorer: {e}")))?;
        Ok(Executed {
            message: "Shown in Explorer".into(),
            undo_token: None,
        })
    }
}

// ------------------------------------------------------------- copy path

pub trait TextClipboard: Send + Sync {
    fn set_text(&self, text: &str) -> Result<(), String>;
}

pub struct WindowsClipboardWriter;

impl TextClipboard for WindowsClipboardWriter {
    fn set_text(&self, text: &str) -> Result<(), String> {
        crate::screengrab::set_clipboard_text(text)
    }
}

pub struct CopyPathAction(pub Arc<dyn TextClipboard>);

impl Action for CopyPathAction {
    fn id(&self) -> &str {
        COPY_PATH
    }
    fn label(&self) -> &str {
        "Copy path"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        file_or_folder(kind)
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        // A path is text: copying it works even if the item has since gone.
        let uri = target
            .uri
            .clone()
            .ok_or_else(|| ActionError::Refused("that row is not a path".into()))?;
        Ok(Plan {
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: "Copy path".into(),
                lines: vec![uri.clone()],
                warnings: vec![],
            },
            undoable: false,
            snapshot: serde_json::json!({ "path": uri }),
            fingerprint: format!("copy_path|{uri}"),
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        let path = snapshot_path(plan)?;
        self.0
            .set_text(&path.to_string_lossy())
            .map_err(ActionError::Failed)?;
        Ok(Executed {
            message: "Path copied".into(),
            undo_token: None,
        })
    }
}

// ------------------------------------------------------------- copy file

pub trait FileClipboard: Send + Sync {
    fn set_files(&self, paths: &[PathBuf]) -> Result<(), String>;
}

impl FileClipboard for WindowsClipboardWriter {
    fn set_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        crate::share::copy_files(paths)
    }
}

/// Puts the file or folder itself on the clipboard, for the same results that
/// can be dragged.
pub struct CopyFileAction(pub Arc<dyn FileClipboard>, pub Arc<Rules>);

impl Action for CopyFileAction {
    fn id(&self) -> &str {
        COPY_FILE
    }
    fn label(&self) -> &str {
        "Copy file"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        file_or_folder(kind)
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let path = self
            .1
            .path(&target.kind, target.uri.as_deref(), target.origin.is_some())
            .map_err(|e| ActionError::Refused(e.to_string()))?;
        let text = path.to_string_lossy().into_owned();
        Ok(Plan {
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: "Copy file".into(),
                lines: vec![text.clone()],
                warnings: vec![],
            },
            undoable: false,
            snapshot: serde_json::json!({ "path": text }),
            fingerprint: format!("copy_file|{text}"),
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        let path = snapshot_path(plan)?;
        self.0
            .set_files(std::slice::from_ref(&path))
            .map_err(ActionError::Failed)?;
        Ok(Executed {
            message: "Copied. Paste it where you want the file.".into(),
            undo_token: None,
        })
    }
}

// --------------------------------------------------------------- recycle

pub trait Recycler: Send + Sync {
    /// Moves one file or folder to the Windows Recycle Bin.
    fn recycle(&self, path: &Path) -> Result<(), String>;
    /// Empties the Recycle Bin on every drive.
    fn empty_bin(&self) -> Result<(), String> {
        Err("emptying the Recycle Bin is not available here".to_owned())
    }
}

/// The shell's own "Move to Recycle Bin". `FOF_WANTNUKEWARNING` makes the shell
/// ask first when an item is too big for the bin or the bin is switched off,
/// instead of deleting it for good without a word.
pub struct ShellRecycler;

impl Recycler for ShellRecycler {
    fn empty_bin(&self) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::{
            SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND,
        };
        // SAFETY: no window and no root path (all drives); flags only.
        let code = unsafe {
            SHEmptyRecycleBinW(
                std::ptr::null_mut(),
                std::ptr::null(),
                SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
            )
        };
        if code < 0 {
            return Err(format!(
                "Windows could not empty the Recycle Bin ({code:#x})"
            ));
        }
        Ok(())
    }

    fn recycle(&self, path: &Path) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::{
            SHFileOperationW, FOF_ALLOWUNDO, FOF_NOERRORUI, FOF_WANTNUKEWARNING, FO_DELETE,
            SHFILEOPSTRUCTW,
        };
        let mut from: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
        from.extend([0, 0]); // the list ends with a double NUL
                             // SAFETY: a zeroed SHFILEOPSTRUCTW is valid; `from` outlives the call
                             // and is double-NUL-terminated as the API requires.
        let (code, aborted) = unsafe {
            let mut op: SHFILEOPSTRUCTW = std::mem::zeroed();
            op.wFunc = FO_DELETE;
            op.pFrom = from.as_ptr();
            op.fFlags = (FOF_ALLOWUNDO | FOF_NOERRORUI | FOF_WANTNUKEWARNING) as _;
            let code = SHFileOperationW(&mut op);
            (code, op.fAnyOperationsAborted != 0)
        };
        if aborted {
            return Err("cancelled in the Windows dialog; nothing was deleted".to_owned());
        }
        if code != 0 {
            return Err(format!(
                "Windows could not move it to the Recycle Bin (code {code:#x})"
            ));
        }
        Ok(())
    }
}

pub struct RecycleAction {
    recycler: Arc<dyn Recycler>,
    hints: Hints,
}

impl RecycleAction {
    pub fn new(recycler: Arc<dyn Recycler>, hints: Hints) -> Self {
        Self { recycler, hints }
    }
}

impl Action for RecycleAction {
    fn id(&self) -> &str {
        RECYCLE
    }
    fn label(&self) -> &str {
        "Move to Recycle Bin"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        file_or_folder(kind)
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let path = existing_path(target)?;
        if path.parent().is_none() {
            return Err(ActionError::Refused(
                "a whole drive cannot be moved to the Recycle Bin".into(),
            ));
        }
        let live = std::fs::symlink_metadata(&path).map_err(|_| {
            ActionError::NothingToDo(format!("{} no longer exists", path.display()))
        })?;
        let hint = self
            .hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&path)
            .copied();
        // Numbers come from the scan the user is looking at; a lone file is read live.
        let (items, size) = hint.unwrap_or((1, live.len()));
        let noun = if items == 1 { "item" } else { "items" };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Ok(Plan {
            risk: Risk::Confirm,
            preview: Preview {
                from: None,
                title: format!("Move {name} to the Recycle Bin"),
                lines: vec![
                    path.display().to_string(),
                    format!("{items} {noun}, {} on disk (as of the scan)", human(size)),
                ],
                warnings: vec![
                    RECYCLE_NOTE.to_owned(),
                    "If an item is too big for the Recycle Bin, Windows asks before deleting it permanently.".to_owned(),
                ],
            },
            undoable: false,
            snapshot: serde_json::json!({ "path": path, "items": items, "size": size }),
            fingerprint: format!("recycle|{}|{items}|{size}|{}", path.display(), live.len()),
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        let path = snapshot_path(plan)?;
        let items = plan.snapshot["items"].as_u64().unwrap_or(1);
        let size = plan.snapshot["size"].as_u64().unwrap_or(0);
        self.recycler.recycle(&path).map_err(ActionError::Failed)?;
        Ok(Executed {
            message: format!(
                "Moved {items} item{} ({}) to the Recycle Bin. Scan again to refresh the sizes.",
                if items == 1 { "" } else { "s" },
                human(size)
            ),
            undo_token: None,
        })
    }
}

// ----------------------------------------------------------- batch removal

pub const BATCH_REMOVE: &str = "batch_remove";

/// Removes a list the backend itself built: a Clean up item or the extra copies
/// of duplicate files. The list is looked up by id; no path ever comes from the
/// webview. Every path is checked against the protected-folder rule again here,
/// and a file to keep is never touched.
pub struct BatchRemoveAction {
    store: Arc<crate::cleanup::BatchStore>,
    recycler: Arc<dyn Recycler>,
}

impl BatchRemoveAction {
    pub fn new(store: Arc<crate::cleanup::BatchStore>, recycler: Arc<dyn Recycler>) -> Self {
        Self { store, recycler }
    }

    fn id_of(target: &Target) -> Result<String, ActionError> {
        target
            .uri
            .as_deref()
            .and_then(|u| u.strip_prefix("batch:"))
            .map(str::to_owned)
            .ok_or_else(|| ActionError::Refused("that is not a prepared list".into()))
    }

    /// The entries that still exist and are still what was found. Errors when
    /// anything protected is in the list or a file to keep is missing.
    fn live(
        &self,
        b: &crate::cleanup::Batch,
    ) -> Result<(Vec<crate::cleanup::Entry>, usize), ActionError> {
        use crate::cleanup::{is_protected, is_reparse, mtime_secs, Mode};
        if b.mode == Mode::EmptyBin {
            return Ok((Vec::new(), 0));
        }
        for k in &b.keepers {
            if std::fs::symlink_metadata(k).is_err() {
                return Err(ActionError::Stale(format!(
                    "{} (a file to keep) is missing; scan again",
                    k.display()
                )));
            }
        }
        let mut live = Vec::new();
        let mut gone = 0;
        for e in &b.entries {
            if is_protected(&self.store.env, &e.path) {
                return Err(ActionError::Refused(format!(
                    "{} is in a protected place and is never removed",
                    e.path.display()
                )));
            }
            if b.keepers.contains(&e.path) {
                return Err(ActionError::Refused(
                    "a file to keep was in the removal list; nothing was removed".into(),
                ));
            }
            let Ok(md) = std::fs::symlink_metadata(&e.path) else {
                gone += 1;
                continue;
            };
            if is_reparse(&md) {
                return Err(ActionError::Stale(format!(
                    "{} is now a link; scan again",
                    e.path.display()
                )));
            }
            // Duplicates must be byte-for-byte what was compared.
            let changed = md.is_file()
                && !b.keepers.is_empty()
                && (md.len() != e.size || mtime_secs(&md) != e.stamp);
            if changed {
                return Err(ActionError::Stale(format!(
                    "{} changed since the scan; scan again",
                    e.path.display()
                )));
            }
            live.push(e.clone());
        }
        Ok((live, gone))
    }
}

impl Action for BatchRemoveAction {
    fn id(&self) -> &str {
        BATCH_REMOVE
    }
    fn label(&self) -> &str {
        "Clean up"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::Folder)
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        use crate::cleanup::Mode;
        let id = Self::id_of(target)?;
        let b = self.store.get(&id).ok_or_else(|| {
            ActionError::Stale("that list is no longer available; scan again".into())
        })?;
        let (live, gone) = self.live(&b)?;
        let total: u64 = live.iter().map(|e| e.size).sum();
        let n = live.len();
        let mut lines = Vec::new();
        let (title, mut warnings) = match b.mode {
            Mode::EmptyBin => {
                lines.push(format!(
                    "Everything in the Recycle Bin on all drives: {} items, {}",
                    b.bin_items,
                    human(b.bin_bytes)
                ));
                (
                    "Empty the Recycle Bin".to_owned(),
                    vec!["This deletes the items for good. They cannot be restored.".to_owned()],
                )
            }
            Mode::Delete => (
                format!("Delete {n} items: {}", b.label),
                vec![
                    "These are deleted directly, not sent to the Recycle Bin, and cannot be restored. They are caches or leftovers that programs recreate.".to_owned(),
                    "Items that are in use are skipped.".to_owned(),
                ],
            ),
            Mode::Recycle => (
                format!("Move {n} items to the Recycle Bin: {}", b.label),
                vec![
                    RECYCLE_NOTE.to_owned(),
                    "Space is only freed once the Recycle Bin is emptied. If an item is too big for it, Windows asks before deleting it permanently.".to_owned(),
                ],
            ),
        };
        if b.mode != Mode::EmptyBin {
            if n == 0 {
                return Err(ActionError::NothingToDo("nothing left to remove".into()));
            }
            lines.push(format!("{n} items, {}", human(total)));
        }
        if !b.keepers.is_empty() {
            lines.push(format!(
                "Keeping {} file(s); only the other copies are removed.",
                b.keepers.len()
            ));
            for k in b.keepers.iter().take(50) {
                lines.push(format!("Keep: {}", k.display()));
            }
            if b.keepers.len() > 50 {
                lines.push(format!("… and {} more kept files", b.keepers.len() - 50));
            }
        }
        for e in live.iter().take(150) {
            lines.push(format!("{} ({})", e.path.display(), human(e.size)));
        }
        if n > 150 {
            lines.push(format!("… and {} more", n - 150));
        }
        if gone > 0 {
            warnings.push(format!("{gone} item(s) from the scan are already gone."));
        }
        Ok(Plan {
            risk: if b.mode == Mode::Recycle {
                Risk::Confirm
            } else {
                Risk::Dangerous
            },
            preview: Preview {
                from: None,
                title,
                lines,
                warnings,
            },
            undoable: false,
            snapshot: serde_json::json!({ "batch": id }),
            fingerprint: format!("batch|{id}|{n}|{total}"),
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        use crate::cleanup::Mode;
        let id = plan.snapshot["batch"]
            .as_str()
            .ok_or_else(|| ActionError::Failed("bad plan".into()))?
            .to_owned();
        let b = self.store.get(&id).ok_or_else(|| {
            ActionError::Stale("that list is no longer available; scan again".into())
        })?;
        if b.mode == Mode::EmptyBin {
            self.recycler.empty_bin().map_err(ActionError::Failed)?;
            self.store.add_reclaimed(b.bin_bytes);
            self.store.forget(&id);
            return Ok(Executed {
                message: format!("Emptied the Recycle Bin ({} freed).", human(b.bin_bytes)),
                undo_token: None,
            });
        }
        let (live, _) = self.live(&b)?;
        let (mut ok, mut failed, mut bytes) = (0usize, 0usize, 0u64);
        for e in &live {
            // Last line of defence, right before touching the disk.
            if b.keepers.contains(&e.path) || crate::cleanup::is_protected(&self.store.env, &e.path)
            {
                failed += 1;
                continue;
            }
            let r = match b.mode {
                Mode::Recycle => self.recycler.recycle(&e.path),
                _ => match std::fs::symlink_metadata(&e.path) {
                    Ok(m) if m.is_dir() => std::fs::remove_dir_all(&e.path),
                    Ok(_) => std::fs::remove_file(&e.path),
                    Err(err) => Err(err),
                }
                .map_err(|err| err.to_string()),
            };
            if r.is_ok() {
                ok += 1;
                bytes += e.size;
            } else {
                failed += 1;
            }
        }
        self.store.add_reclaimed(bytes);
        self.store.forget(&id);
        let how = if b.mode == Mode::Recycle {
            "moved to the Recycle Bin (empty it to free the space)"
        } else {
            "deleted"
        };
        let left = if failed > 0 {
            format!(" {failed} could not be removed (in use or changed); they were left alone.")
        } else {
            String::new()
        };
        Ok(Executed {
            message: format!(
                "{ok} item{} ({}) {how}.{left}",
                if ok == 1 { "" } else { "s" },
                human(bytes),
            ),
            undo_token: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Rec(Mutex<Vec<PathBuf>>);
    impl Recycler for Rec {
        fn recycle(&self, p: &Path) -> Result<(), String> {
            self.0.lock().unwrap().push(p.to_path_buf());
            Ok(())
        }
    }

    fn target(kind: Kind, p: &Path) -> Target {
        Target {
            item_id: "disk:1".into(),
            kind,
            uri: Some(p.to_string_lossy().into_owned()),
            port: None,
            origin: None,
        }
    }

    #[test]
    fn recycle_needs_confirm_shows_count_and_size_and_says_it_cannot_be_undone() {
        let dir = tempfile::tempdir().unwrap();
        let hints = Hints::default();
        hints
            .lock()
            .unwrap()
            .insert(dir.path().to_path_buf(), (1234, 5 * 1024 * 1024));
        let rec = Arc::new(Rec::default());
        let action = RecycleAction::new(rec.clone(), hints);
        let plan = action.plan(&target(Kind::Folder, dir.path())).unwrap();
        assert_eq!(plan.risk, Risk::Confirm);
        assert!(!plan.undoable);
        let text = format!("{:?}", plan.preview);
        assert!(
            text.contains("1234 items") && text.contains("5.0 MB"),
            "{text}"
        );
        assert!(text.contains(RECYCLE_NOTE), "{text}");
        assert!(rec.0.lock().unwrap().is_empty(), "planning deletes nothing");
        action.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(*rec.0.lock().unwrap(), [dir.path().to_path_buf()]);
    }

    #[test]
    fn recycle_refuses_drive_roots_relative_and_missing_paths() {
        let action = RecycleAction::new(Arc::new(Rec::default()), Hints::default());
        assert!(matches!(
            action.plan(&target(Kind::Folder, Path::new(r"C:\"))),
            Err(ActionError::Refused(_))
        ));
        assert!(action
            .plan(&target(Kind::File, Path::new("rel.txt")))
            .is_err());
        let gone = tempfile::tempdir().unwrap().path().join("gone");
        assert!(matches!(
            action.plan(&target(Kind::File, &gone)),
            Err(ActionError::NothingToDo(_))
        ));
    }

    #[test]
    fn copy_file_is_safe_and_follows_the_drag_rules() {
        #[derive(Default)]
        struct Clip(Mutex<Vec<PathBuf>>);
        impl FileClipboard for Clip {
            fn set_files(&self, p: &[PathBuf]) -> Result<(), String> {
                self.0.lock().unwrap().extend_from_slice(p);
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        let secret = dir.path().join(".env");
        std::fs::write(&file, "x").unwrap();
        std::fs::write(&secret, "x").unwrap();
        let rules = Rules {
            roots: vec![dir.path().to_path_buf()],
            ..Rules::default()
        };
        let clip = Arc::new(Clip::default());
        let copy = CopyFileAction(clip.clone(), Arc::new(rules));
        assert!(!copy.applies_to(&Kind::App));
        assert!(matches!(
            copy.plan(&target(Kind::File, &secret)),
            Err(ActionError::Refused(_))
        ));
        let plan = copy.plan(&target(Kind::File, &file)).unwrap();
        assert_eq!(plan.risk, Risk::Safe);
        copy.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(*clip.0.lock().unwrap(), [file]);
    }

    #[test]
    fn copy_path_and_reveal_are_safe_and_use_the_exact_path() {
        #[derive(Default)]
        struct Clip(Mutex<Vec<String>>);
        impl TextClipboard for Clip {
            fn set_text(&self, t: &str) -> Result<(), String> {
                self.0.lock().unwrap().push(t.to_owned());
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let clip = Arc::new(Clip::default());
        let copy = CopyPathAction(clip.clone());
        let plan = copy.plan(&target(Kind::Folder, dir.path())).unwrap();
        assert_eq!(plan.risk, Risk::Safe);
        copy.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(
            *clip.0.lock().unwrap(),
            [dir.path().to_string_lossy().into_owned()]
        );

        struct NoReveal;
        impl Revealer for NoReveal {
            fn reveal(&self, _: &Path) -> std::io::Result<()> {
                Ok(())
            }
        }
        let reveal = RevealAction(Arc::new(NoReveal));
        assert_eq!(
            reveal.plan(&target(Kind::Folder, dir.path())).unwrap().risk,
            Risk::Safe
        );
    }

    fn batch_env(root: &Path) -> crate::cleanup::Env {
        let profile = root.join("Users").join("me");
        std::fs::create_dir_all(&profile).unwrap();
        crate::cleanup::Env {
            local: profile.join("L"),
            roaming: profile.join("R"),
            temp: profile.join("T"),
            system_dirs: vec![root.join("Windows"), root.join("Program Files")],
            profile,
        }
    }

    fn entry(p: &Path) -> crate::cleanup::Entry {
        let md = std::fs::metadata(p).unwrap();
        crate::cleanup::Entry {
            path: p.to_path_buf(),
            size: md.len(),
            stamp: crate::cleanup::mtime_secs(&md),
        }
    }

    fn batch_target(id: &str) -> Target {
        target(Kind::Folder, Path::new(&format!("batch:{id}")))
    }

    fn mk_batch(
        entries: Vec<crate::cleanup::Entry>,
        keepers: Vec<PathBuf>,
    ) -> crate::cleanup::Batch {
        crate::cleanup::Batch {
            label: "test".into(),
            mode: crate::cleanup::Mode::Recycle,
            entries,
            keepers,
            bin_items: 0,
            bin_bytes: 0,
        }
    }

    #[test]
    fn batch_refuses_protected_paths_and_a_keeper_in_the_list() {
        let d = tempfile::tempdir().unwrap();
        let env = batch_env(d.path());
        let store = Arc::new(crate::cleanup::BatchStore::new(env.clone()));
        let rec = Arc::new(Rec::default());
        let action = BatchRemoveAction::new(store.clone(), rec.clone());
        let win = d.path().join("Windows");
        std::fs::create_dir_all(&win).unwrap();
        std::fs::write(win.join("a.dll"), "x").unwrap();
        let id = store.put(mk_batch(vec![entry(&win.join("a.dll"))], vec![]));
        assert!(matches!(
            action.plan(&batch_target(&id)),
            Err(ActionError::Refused(_))
        ));
        // The profile root itself.
        let root_entry = crate::cleanup::Entry {
            path: env.profile.clone(),
            size: 0,
            stamp: 0,
        };
        let id = store.put(mk_batch(vec![root_entry], vec![]));
        assert!(matches!(
            action.plan(&batch_target(&id)),
            Err(ActionError::Refused(_))
        ));
        // A keeper that is also in the removal list: nothing is removed.
        let f = env.profile.join("keep.bin");
        std::fs::write(&f, "data").unwrap();
        let id = store.put(mk_batch(vec![entry(&f)], vec![f.clone()]));
        assert!(matches!(
            action.plan(&batch_target(&id)),
            Err(ActionError::Refused(_))
        ));
        assert!(rec.0.lock().unwrap().is_empty());
        assert!(f.exists());
    }

    #[test]
    fn batch_removes_copies_but_never_the_keeper_and_stops_if_a_file_changed() {
        let d = tempfile::tempdir().unwrap();
        let env = batch_env(d.path());
        let store = Arc::new(crate::cleanup::BatchStore::new(env.clone()));
        let rec = Arc::new(Rec::default());
        let action = BatchRemoveAction::new(store.clone(), rec.clone());
        let keep = env.profile.join("keep.bin");
        let copy = env.profile.join("copy.bin");
        std::fs::write(&keep, "same").unwrap();
        std::fs::write(&copy, "same").unwrap();
        let id = store.put(mk_batch(vec![entry(&copy)], vec![keep.clone()]));
        let plan = action.plan(&batch_target(&id)).unwrap();
        assert_eq!(plan.risk, Risk::Confirm);
        assert!(format!("{:?}", plan.preview).contains("Keep:"));
        assert!(plan.preview.warnings.iter().any(|w| w == RECYCLE_NOTE));
        action.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(*rec.0.lock().unwrap(), std::slice::from_ref(&copy));
        // A missing keeper stops everything.
        let copy2 = env.profile.join("copy2.bin");
        std::fs::write(&copy2, "same").unwrap();
        let id = store.put(mk_batch(
            vec![entry(&copy2)],
            vec![env.profile.join("gone.bin")],
        ));
        assert!(matches!(
            action.plan(&batch_target(&id)),
            Err(ActionError::Stale(_))
        ));
        // A copy edited after the scan is not removed.
        let id = store.put(mk_batch(vec![entry(&copy2)], vec![keep.clone()]));
        let plan = action.plan(&batch_target(&id)).unwrap();
        std::fs::write(&copy2, "edited later and longer").unwrap();
        let before = rec.0.lock().unwrap().len();
        assert!(action.execute(&plan, &Authorized::for_tests()).is_err());
        assert_eq!(rec.0.lock().unwrap().len(), before);
    }
}
