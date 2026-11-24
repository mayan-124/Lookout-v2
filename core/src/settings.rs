use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ranking::RankingWeights;

pub const DEFAULT_HOTKEY: &str = "Alt+Space";

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("settings file I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings file is not valid JSON: {0}")]
    Parse(#[from] serde_json::Error),
}

/// How fast file contents are indexed once the user is idle.
///
/// - Gentle: busy at most `crawl_duty` of the time (10% by default), index
///   saved every 50 files.
/// - Balanced: busy up to 25% of the time, saved every 200 files.
/// - Fast: busy up to 60% of the time (one worker, so under one core), saved every 500 files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexSpeed {
    Gentle,
    #[default]
    Balanced,
    Fast,
}

impl IndexSpeed {
    /// Busy fraction for content work; never below the user's own `crawl_duty`.
    pub fn duty(self, crawl_duty: f32) -> f32 {
        let floor = match self {
            IndexSpeed::Gentle => 0.0,
            IndexSpeed::Balanced => 0.25,
            IndexSpeed::Fast => 0.6,
        };
        crawl_duty.max(floor).min(1.0)
    }

    /// Files indexed between saves of the full-text index.
    pub fn commit_every(self) -> usize {
        match self {
            IndexSpeed::Gentle => 50,
            IndexSpeed::Balanced => 200,
            IndexSpeed::Fast => 500,
        }
    }
}

/// Colour theme of the window. Auto follows Windows (light or dark mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Global hotkey that toggles the bar, e.g. `Alt+Space` or `Ctrl+Shift+K`.
    pub hotkey: String,
    /// Folders whose file names and contents are indexed. Empty until the user
    /// chooses folders (first-run chooser): indexing is opt-in.
    pub index_roots: Vec<PathBuf>,
    /// Extra file or folder names to skip while indexing (case-insensitive),
    /// on top of the built-in list.
    pub excluded_names: Vec<String>,
    /// Fraction of time the background crawl may be busy, `0.01..=1.0`.
    pub crawl_duty: f32,
    /// Hours between full rescans that repair anything the file watcher
    /// missed. 0 (the default) means no scheduled rescan: a rescan then only
    /// runs after a watcher error. A rescan briefly holds two indexes in
    /// memory, which can exceed the idle memory budget; 24 is a sensible opt-in.
    pub rescan_hours: u32,
    pub ranking: RankingWeights,
    /// Set once the user has gone through the first-run folder choice (even
    /// if they chose no folders). Until then Lookout asks.
    pub first_run_done: bool,
    /// Folders (and everything below them) that are never indexed: not their
    /// names, not their contents, not OCR of their images.
    pub excluded_folders: Vec<PathBuf>,
    /// Global hotkey that pauses and resumes all indexing.
    pub pause_hotkey: String,
    /// Global hotkey for "grab text from screen". Empty switches it off.
    pub grab_hotkey: String,
    /// Index file contents (full-text search).
    pub index_contents: bool,
    /// Read the text in images and screenshots (Windows OCR).
    pub ocr: bool,
    /// Content extraction and OCR run only after this many seconds without
    /// keyboard or mouse input.
    pub idle_secs: u32,
    /// How hard content indexing works while the user is idle.
    pub index_speed: IndexSpeed,
    /// A persisted name snapshot with more entries than this is not loaded:
    /// loading it and building the fresh startup index would hold two big
    /// indexes at once. Search then starts empty and fills as the startup crawl
    /// runs. 0 never loads a snapshot.
    pub snapshot_max_entries: usize,
    /// Size limit of the content index (file text and OCR), in MB. When it is
    /// reached, no new content is added; nothing already indexed is removed.
    /// Name search is not affected. The settings screen allows no less than
    /// [`MIN_CONTENT_INDEX_MB`].
    pub content_index_max_mb: u64,
    /// Light, dark, or follow Windows (the default).
    pub theme: Theme,
    /// The window stays on top and does not hide when it loses focus.
    pub pinned: bool,
    /// Last window position (physical pixels) per monitor, keyed by
    /// [`monitor_key`]. A monitor that is gone simply has no use.
    pub window_positions: std::collections::BTreeMap<String, (i32, i32)>,
}

/// Identifies a monitor by name, size and place, so a saved position is only
/// used on the monitor it was saved on.
pub fn monitor_key(name: &str, x: i32, y: i32, w: u32, h: u32) -> String {
    format!("{name}|{x},{y}|{w}x{h}")
}

/// True if a saved window position leaves the top of a window of
/// `win_w` pixels grabbable inside the monitor at `(mx, my, mw, mh)`.
pub fn position_on_monitor(pos: (i32, i32), win_w: i32, mon: (i32, i32, i32, i32)) -> bool {
    let (mx, my, mw, mh) = mon;
    let (x, y) = pos;
    x + win_w > mx + 80 && x < mx + mw - 80 && y >= my && y < my + mh - 60
}

/// Measured growth is about 14 KB per document, so 2 GB holds roughly 140,000
/// documents.
pub const DEFAULT_CONTENT_INDEX_MB: u64 = 2048;
pub const MIN_CONTENT_INDEX_MB: u64 = 100;
pub const MAX_CONTENT_INDEX_MB: u64 = 1_000_000;
pub const MAX_IDLE_SECS: u32 = 3600;
pub const MAX_RESCAN_HOURS: u32 = 720;

/// Everything the Settings screen's Save sends at once.
#[derive(Debug, Clone)]
pub struct Changes {
    pub roots: Vec<PathBuf>,
    pub excluded_folders: Vec<PathBuf>,
    pub idle_secs: i64,
    pub rescan_hours: i64,
    pub index_speed: IndexSpeed,
    pub content_index_max_mb: i64,
    pub theme: Theme,
}

fn in_range(v: i64, lo: u64, hi: u64) -> bool {
    u64::try_from(v).is_ok_and(|v| (lo..=hi).contains(&v))
}

pub const DEFAULT_PAUSE_HOTKEY: &str = "Ctrl+Alt+P";
pub const DEFAULT_GRAB_HOTKEY: &str = "Ctrl+Alt+T";

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.to_owned(),
            index_roots: Vec::new(),
            excluded_names: Vec::new(),
            crawl_duty: 0.1,
            rescan_hours: 0,
            ranking: RankingWeights::default(),
            first_run_done: false,
            excluded_folders: Vec::new(),
            pause_hotkey: DEFAULT_PAUSE_HOTKEY.to_owned(),
            grab_hotkey: DEFAULT_GRAB_HOTKEY.to_owned(),
            index_contents: true,
            ocr: true,
            idle_secs: 10,
            index_speed: IndexSpeed::default(),
            snapshot_max_entries: 300_000,
            content_index_max_mb: DEFAULT_CONTENT_INDEX_MB,
            theme: Theme::default(),
            pinned: false,
            window_positions: std::collections::BTreeMap::new(),
        }
    }
}

impl Settings {
    /// The content index cap in bytes. The settings screen enforces
    /// [`MIN_CONTENT_INDEX_MB`]; a hand-edited file may go lower (never 0).
    pub fn content_index_max_bytes(&self) -> u64 {
        self.content_index_max_mb.max(1).saturating_mul(1024 * 1024)
    }

    /// True while the user has neither chosen folders nor dismissed the
    /// first-run choice. A Phase 2 settings file that already lists folders counts as chosen.
    pub fn needs_first_run(&self) -> bool {
        !self.first_run_done && self.index_roots.is_empty()
    }

    /// Loads settings. If the file is missing or broken, the copy kept from the
    /// last good save (`settings.json.bak`) is tried before giving up; a missing
    /// file with no copy yields the defaults.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let first = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(SettingsError::from),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        };
        let missing = !path.exists();
        if first.is_ok() && !missing {
            return first;
        }
        let backed_up = std::fs::read_to_string(backup_path(path))
            .ok()
            .and_then(|t| serde_json::from_str::<Self>(&t).ok());
        match backed_up {
            Some(s) => Ok(s),
            None => first,
        }
    }

    /// Checks every value first; nothing is changed when one is out of range.
    /// The message names the field and the allowed range.
    pub fn check(c: &Changes) -> Result<(), String> {
        let bad = |what: &str, lo: u64, hi: u64| {
            Err(format!("{what}: enter a number from {lo} to {hi}."))
        };
        if !in_range(c.idle_secs, 0, MAX_IDLE_SECS.into()) {
            return bad("Wait before reading", 0, MAX_IDLE_SECS.into());
        }
        if !in_range(c.rescan_hours, 0, MAX_RESCAN_HOURS.into()) {
            return bad("Check for changes every", 0, MAX_RESCAN_HOURS.into());
        }
        if !in_range(
            c.content_index_max_mb,
            MIN_CONTENT_INDEX_MB,
            MAX_CONTENT_INDEX_MB,
        ) {
            return bad(
                "File text size limit",
                MIN_CONTENT_INDEX_MB,
                MAX_CONTENT_INDEX_MB,
            );
        }
        Ok(())
    }

    /// All or nothing: validates, builds the new settings, saves them, and only
    /// then changes `self`. On any error `self` and the file are as before.
    pub fn apply(&mut self, c: Changes, path: &Path) -> Result<(), String> {
        Self::check(&c)?;
        let mut next = self.clone();
        next.index_roots = c.roots;
        next.excluded_folders = c.excluded_folders;
        next.idle_secs = u32::try_from(c.idle_secs).map_err(|e| e.to_string())?;
        next.rescan_hours = u32::try_from(c.rescan_hours).map_err(|e| e.to_string())?;
        next.index_speed = c.index_speed;
        next.content_index_max_mb =
            u64::try_from(c.content_index_max_mb).map_err(|e| e.to_string())?;
        next.theme = c.theme;
        next.first_run_done = true;
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }

    /// Writes to a temp file in the same folder, flushes it to disk, then
    /// renames it over the settings file, so a crash or a full disk never leaves
    /// a half-written file. The new good file is also copied to `settings.json.bak`.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        use std::io::Write;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = sibling(path, ".tmp");
        let written = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.flush()?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        // The copy is best effort: the main file is already safely in place.
        let _ = std::fs::copy(path, backup_path(path));
        Ok(())
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn backup_path(path: &Path) -> PathBuf {
    sibling(path, ".bak")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_is_needed_until_folders_are_chosen_or_the_choice_is_dismissed() {
        let mut s = Settings::default();
        assert!(s.needs_first_run());
        s.first_run_done = true;
        assert!(!s.needs_first_run());
        let phase2 = Settings {
            index_roots: vec![PathBuf::from("D:\\Work")],
            ..Settings::default()
        };
        assert!(!phase2.needs_first_run());
    }

    #[test]
    fn pin_and_positions_survive_a_save_and_old_files_load_unpinned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        assert!(!s.pinned && s.window_positions.is_empty());
        s.pinned = true;
        let key = monitor_key("DISPLAY1", 0, 0, 1920, 1080);
        s.window_positions.insert(key.clone(), (300, 120));
        s.save(&path).unwrap();
        let back = Settings::load(&path).unwrap();
        assert!(back.pinned);
        assert_eq!(back.window_positions.get(&key), Some(&(300, 120)));
        std::fs::write(&path, r#"{"hotkey":"Alt+Space"}"#).unwrap();
        let old = Settings::load(&path).unwrap();
        assert!(!old.pinned && old.window_positions.is_empty());
    }

    #[test]
    fn a_saved_position_must_still_be_on_its_monitor() {
        let mon = (0, 0, 1920, 1080);
        assert!(position_on_monitor((580, 200), 760, mon));
        assert!(
            !position_on_monitor((3000, 200), 760, mon),
            "monitor gone or smaller"
        );
        assert!(!position_on_monitor((580, 1070), 760, mon));
        assert!(!position_on_monitor((-900, 200), 760, mon));
    }

    #[test]
    fn huge_content_cap_does_not_overflow() {
        let s = Settings {
            content_index_max_mb: u64::MAX,
            ..Settings::default()
        };
        assert_eq!(s.content_index_max_bytes(), u64::MAX);
        let zero = Settings {
            content_index_max_mb: 0,
            ..Settings::default()
        };
        assert_eq!(zero.content_index_max_bytes(), 1024 * 1024);
    }

    fn changes() -> Changes {
        Changes {
            roots: vec![PathBuf::from(r"D:\Work")],
            excluded_folders: vec![],
            idle_secs: 5,
            rescan_hours: 12,
            index_speed: IndexSpeed::Fast,
            content_index_max_mb: 500,
            theme: Theme::Light,
        }
    }

    #[test]
    fn theme_defaults_to_auto_and_old_files_without_it_load_as_auto() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"idle_secs": 7}"#).unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.theme, Theme::Auto);
        assert_eq!(s.idle_secs, 7);
    }

    #[test]
    fn theme_is_saved_with_the_other_settings_or_not_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.save(&path).unwrap();
        // A bad value in the same Save leaves the theme unchanged on disk.
        let bad = Changes {
            idle_secs: -1,
            theme: Theme::Dark,
            ..changes()
        };
        assert!(s.apply(bad, &path).is_err());
        assert_eq!(Settings::load(&path).unwrap().theme, Theme::Auto);
        // A good Save stores it, and the file spells it as a lowercase word.
        s.apply(
            Changes {
                theme: Theme::Dark,
                ..changes()
            },
            &path,
        )
        .unwrap();
        assert_eq!(Settings::load(&path).unwrap().theme, Theme::Dark);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains(r#""theme": "dark""#));
    }

    #[test]
    fn save_applies_everything_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();

        // One bad value: nothing from the same Save is applied or written.
        for bad in [
            Changes {
                content_index_max_mb: 99,
                ..changes()
            },
            Changes {
                content_index_max_mb: i64::MAX,
                ..changes()
            },
            Changes {
                idle_secs: -1,
                ..changes()
            },
            Changes {
                rescan_hours: 721,
                ..changes()
            },
        ] {
            assert!(s.apply(bad, &path).is_err());
            assert_eq!(s, Settings::default());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }

        // The file cannot be written: still nothing applied.
        std::fs::create_dir(dir.path().join("settings.json.tmp")).unwrap();
        assert!(s.apply(changes(), &path).is_err());
        assert_eq!(s, Settings::default());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir(dir.path().join("settings.json.tmp")).unwrap();

        // Valid: everything applied and saved.
        s.apply(changes(), &path).unwrap();
        let back = Settings::load(&path).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.idle_secs, 5);
        assert_eq!(back.content_index_max_mb, 500);
        assert_eq!(back.index_roots, vec![PathBuf::from(r"D:\Work")]);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::load(&dir.path().join("settings.json")).unwrap();
        assert_eq!(s.hotkey, "Alt+Space");
    }

    #[test]
    fn scheduled_rescan_is_opt_in() {
        // A rescan briefly holds two indexes; see docs/decisions.md for the memory numbers.
        assert_eq!(Settings::default().rescan_hours, 0);
    }

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let s = Settings {
            hotkey: "Ctrl+Shift+K".into(),
            index_roots: vec![PathBuf::from("D:\\Work")],
            excluded_names: vec!["private".into()],
            crawl_duty: 0.5,
            rescan_hours: 24,
            ranking: RankingWeights {
                w_frecency: 1.0,
                ..RankingWeights::default()
            },
            first_run_done: true,
            excluded_folders: vec![PathBuf::from("D:\\Work\\private")],
            pause_hotkey: "Ctrl+Alt+K".into(),
            grab_hotkey: "Ctrl+Alt+G".into(),
            index_contents: false,
            ocr: false,
            idle_secs: 3,
            index_speed: IndexSpeed::Fast,
            snapshot_max_entries: 1234,
            content_index_max_mb: 500,
            theme: Theme::Dark,
            pinned: true,
            window_positions: [("M|0,0|1x1".to_owned(), (5, 6))].into(),
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"future_field": 1}"#).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(matches!(
            Settings::load(&path),
            Err(SettingsError::Parse(_))
        ));
    }

    #[test]
    fn truncated_file_recovers_from_the_last_good_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let s = Settings {
            index_roots: vec![PathBuf::from(r"D:\Work"), PathBuf::from(r"E:\Docs")],
            first_run_done: true,
            ..Settings::default()
        };
        s.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &text[..text.len() / 2]).unwrap();
        assert_eq!(Settings::load(&path).unwrap().index_roots, s.index_roots);
        // Missing main file recovers too, and no temp file is left behind.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        s.save(&path).unwrap();
        assert!(!dir.path().join("settings.json.tmp").exists());
    }

    #[test]
    fn a_failed_save_leaves_the_old_file_and_copy_intact() {
        // A write that cannot complete (here: the temp file cannot be created,
        // as when the disk is full) must not touch the good file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let good = Settings {
            index_roots: vec![PathBuf::from(r"D:\Work")],
            ..Settings::default()
        };
        good.save(&path).unwrap();
        std::fs::create_dir(dir.path().join("settings.json.tmp")).unwrap();
        let worse = Settings {
            index_roots: Vec::new(),
            ..Settings::default()
        };
        let err = worse.save(&path).unwrap_err();
        assert!(
            err.to_string().contains("settings file I/O failed"),
            "{err}"
        );
        assert_eq!(Settings::load(&path).unwrap(), good);
    }
}
