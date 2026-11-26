//! "Open" as an action with a risk level. Documents, folders and installed
//! apps are `safe`; file types that can run code need an explicit confirm.

use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use super::{Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target};
use crate::item::Kind;

pub const ID: &str = "open";

/// Extensions known to run code when opened (used for the card wording; the
/// safe list below is what decides the risk).
const EXECUTABLE: &[&str] = &[
    "exe",
    "bat",
    "cmd",
    "js",
    "vbs",
    "msi",
    "ps1",
    "scr",
    "hta", //
    "com",
    "pif",
    "lnk",
    "vbe",
    "jse",
    "wsf",
    "wsh",
    "reg",
    "jar",
    "cpl",
    "msp",
    "msc",
    "scf",
    "inf",
    "py",
    "pyw",
    "application",
    "gadget",
];

/// Starts a path with its default handler. The shell supplies the real one;
/// tests supply one that only records.
pub trait Launcher: Send + Sync {
    fn open(&self, path: &Path) -> std::io::Result<()>;
}

/// The only file types that open without confirmation: documents, text,
/// images, audio, video and archives. Anything else, including a file with no
/// extension, needs confirm. Deliberately left out: `svg`, `html`, `htm`
/// (script in a browser), macro documents (`docm xlsm pptm`), `iso`, `chm`,
/// `url`, `appx`, `msix`, and all code types.
const SAFE: &[&str] = &[
    // documents
    "pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "odp",
    "epub", // text
    "txt", "md", "log", "json", "xml", "yaml", "yml", "toml", "tsv", // images
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "ico", "heic", // audio
    "mp3", "wav", "flac", "ogg", "m4a", "aac", "wma", // video
    "mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", // archives
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz",
];

/// Risk of opening `path` as it is on disk right now.
pub fn classify(path: &Path, is_dir: bool) -> Risk {
    if is_dir {
        return Risk::Safe;
    }
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
        return Risk::Confirm;
    };
    // Windows ignores trailing dots and spaces ("setup.exe." is setup.exe).
    let name = name.trim_end_matches(['.', ' ']);
    // A colon in a file name is an NTFS alternate data stream ("a.txt:evil.exe").
    if name.contains(':') {
        return Risk::Confirm;
    }
    match name.rfind('.') {
        Some(i) if SAFE.contains(&&name[i + 1..]) => Risk::Safe,
        _ => Risk::Confirm,
    }
}

/// Opens nothing. For tests and tools that must never launch a program.
pub struct NoopLauncher;

impl Launcher for NoopLauncher {
    fn open(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct OpenAction {
    launcher: Arc<dyn Launcher>,
}

impl OpenAction {
    pub fn new(launcher: Arc<dyn Launcher>) -> Self {
        Self { launcher }
    }
}

impl Action for OpenAction {
    fn id(&self) -> &'static str {
        ID
    }

    fn label(&self) -> &'static str {
        "Open"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::File | Kind::Folder | Kind::App)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let uri = target
            .uri
            .as_deref()
            .ok_or_else(|| ActionError::Refused("that result has nothing to open".into()))?;
        // A Store app is not a file: there is nothing on disk to look at.
        if target.kind == Kind::App && crate::apps::is_store_uri(uri) {
            return Ok(Plan {
                risk: Risk::Safe,
                preview: Preview {
                    from: None,
                    title: "Open app".into(),
                    lines: vec![uri.to_owned()],
                    warnings: Vec::new(),
                },
                undoable: false,
                snapshot: serde_json::json!({ "path": uri, "kind": target.kind }),
                fingerprint: format!("open|{uri}"),
            });
        }
        let path = Path::new(uri);
        if !path.is_absolute() {
            return Err(ActionError::Refused(format!("'{uri}' is not a full path")));
        }
        let meta = std::fs::metadata(path)
            .map_err(|_| ActionError::NothingToDo(format!("{uri} no longer exists")))?;
        let risk = match target.kind {
            // Start Menu entries are the user's installed apps.
            Kind::App => Risk::Safe,
            _ => classify(path, meta.is_dir()),
        };
        let name = path
            .file_name()
            .map_or_else(|| uri.to_owned(), |n| n.to_string_lossy().into_owned());
        let mut lines = vec![uri.to_owned()];
        let mut warnings = Vec::new();
        let title = if risk.needs_confirm() {
            warnings.push(
                "This type of file is not on the list of known-safe documents and may run programs on your computer. Only open it if you trust where it came from."
                    .to_owned(),
            );
            let runs = name.rsplit_once('.').is_some_and(|(_, e)| {
                let e = e.trim_end().to_lowercase();
                EXECUTABLE.contains(&e.as_str())
            });
            if runs {
                format!("Run {name}")
            } else {
                format!("Open {name} (unrecognised type)")
            }
        } else if meta.is_dir() {
            format!("Open folder {name}")
        } else {
            format!("Open {name}")
        };
        if let Some(ext) = path.extension() {
            lines.push(format!("Type: .{}", ext.to_string_lossy().to_lowercase()));
        }
        // For files that can run code, also pin the exact file that was previewed.
        let stamp = if risk.needs_confirm() {
            let modified = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("|{}|{modified}", meta.len())
        } else {
            String::new()
        };
        Ok(Plan {
            risk,
            preview: Preview {
                from: None,
                title,
                lines,
                warnings,
            },
            undoable: false,
            snapshot: serde_json::json!({ "path": uri, "kind": target.kind }),
            fingerprint: format!("open|{uri}|{risk:?}{stamp}"),
        })
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let path = plan
            .snapshot
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| ActionError::Failed("bad plan".into()))?;
        self.launcher
            .open(Path::new(path))
            .map_err(|e| ActionError::Failed(format!("could not open it: {e}")))?;
        let message = if crate::apps::is_store_uri(path) {
            "Opened the app".to_owned()
        } else {
            format!("Opened {path}")
        };
        Ok(Executed {
            message,
            undo_token: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn executable_types_need_confirm_and_documents_do_not() {
        let confirm = [
            "setup.exe",
            "run.bat",
            "run.cmd",
            "x.js",
            "x.vbs",
            "x.msi",
            "x.ps1",
            "x.scr",
            "x.hta",
            "SETUP.EXE",
            "Run.BaT",
            "invoice.pdf.exe",
            "setup.exe.",
            "setup.exe ",
            "setup.exe. .",
            ".exe",
            "a.txt:evil.exe",
            "a.exe:stream",
            "shortcut.lnk",
            "x.py",
            "link.url",
            "app.appx",
            "app.msix",
            "disc.iso",
            "help.chm",
            "macro.docm",
            "macro.xlsm",
            "noextension",
            "exe",
            "README",
            "page.html",
            "pic.svg",
            "x.jsx",
            "x.psd",
            "weird.zzz",
        ];
        for n in confirm {
            let p = Path::new(r"C:\files").join(n);
            assert_eq!(classify(&p, false), Risk::Confirm, "{n}");
        }
        let safe = [
            "report.pdf",
            "notes.txt",
            "exe.txt",
            "photo.jpg",
            "a.exe.txt",
            "song.MP3",
            "backup.7z",
            "clip.mp4",
            "sheet.xlsx",
        ];
        for n in safe {
            let p = Path::new(r"C:\files").join(n);
            assert_eq!(classify(&p, false), Risk::Safe, "{n}");
        }
        // A folder named like an executable is still a folder.
        assert_eq!(classify(Path::new(r"C:\files\tools.exe"), true), Risk::Safe);
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<std::path::PathBuf>>);
    impl Launcher for Recorder {
        fn open(&self, path: &Path) -> std::io::Result<()> {
            self.0.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }

    fn target(kind: Kind, path: &Path) -> Target {
        Target {
            item_id: format!("file:{}", path.display()),
            kind,
            uri: Some(path.to_string_lossy().into_owned()),
            port: None,
            origin: None,
        }
    }

    #[test]
    fn plan_reads_the_real_file_and_pins_executables() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("a.txt");
        let exe = dir.path().join("tool.exe");
        std::fs::write(&doc, b"x").unwrap();
        std::fs::write(&exe, b"x").unwrap();
        let rec = Arc::new(Recorder::default());
        let action = OpenAction::new(rec.clone());

        let p = action.plan(&target(Kind::File, &doc)).unwrap();
        assert_eq!(p.risk, Risk::Safe);
        let p = action.plan(&target(Kind::Folder, dir.path())).unwrap();
        assert_eq!(p.risk, Risk::Safe);
        assert!(p.preview.title.starts_with("Open folder"));

        let a = action.plan(&target(Kind::File, &exe)).unwrap();
        assert_eq!(a.risk, Risk::Confirm);
        assert!(a.preview.title.starts_with("Run tool.exe"));
        assert!(!a.preview.warnings.is_empty());
        // Replacing the file between preview and confirm changes the fingerprint.
        std::fs::write(&exe, b"something else entirely").unwrap();
        let b = action.plan(&target(Kind::File, &exe)).unwrap();
        assert_ne!(a.fingerprint, b.fingerprint);
        assert!(
            rec.0.lock().unwrap().is_empty(),
            "planning must not open anything"
        );
    }

    #[test]
    fn a_stale_kind_cannot_hide_an_executable() {
        // The index says "folder" but the path is now an .exe file: judged by the disk.
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("was_a_folder.exe");
        std::fs::write(&exe, b"x").unwrap();
        let action = OpenAction::new(Arc::new(Recorder::default()));
        assert_eq!(
            action.plan(&target(Kind::Folder, &exe)).unwrap().risk,
            Risk::Confirm
        );
    }

    #[test]
    fn missing_relative_and_empty_targets_are_refused() {
        let action = OpenAction::new(Arc::new(Recorder::default()));
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            action.plan(&target(Kind::File, &dir.path().join("gone.txt"))),
            Err(ActionError::NothingToDo(_))
        ));
        let mut t = target(Kind::File, dir.path());
        t.uri = Some("notes.txt".into());
        assert!(matches!(action.plan(&t), Err(ActionError::Refused(_))));
        t.uri = None;
        assert!(matches!(action.plan(&t), Err(ActionError::Refused(_))));
    }

    #[test]
    fn execute_hands_the_exact_path_to_the_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("R&D plan ^ 100%.txt");
        std::fs::write(&doc, b"x").unwrap();
        let rec = Arc::new(Recorder::default());
        let action = OpenAction::new(rec.clone());
        let plan = action.plan(&target(Kind::File, &doc)).unwrap();
        action.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(*rec.0.lock().unwrap(), [doc]);
    }
}
