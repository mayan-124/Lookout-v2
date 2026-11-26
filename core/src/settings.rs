use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(windows)]
use crate::autostart::RunEntry;
use crate::ranking::RankingWeights;
use crate::uimode::Ui;

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

/// Colour theme of the window. The window owns the list of themes and falls
/// back to its default for an id it does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Appearance {
    pub theme: String,
    /// Custom accent as `#RRGGBB`; none uses the theme's own.
    #[serde(default)]
    pub accent: Option<String>,
}

impl Appearance {
    fn check(&self) -> Result<(), String> {
        let id = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if self.theme.is_empty() || self.theme.len() > 32 || !self.theme.chars().all(id) {
            return Err("Unknown theme.".to_owned());
        }
        let hex = |a: &str| {
            a.strip_prefix('#')
                .is_some_and(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()))
        };
        if !self.accent.as_deref().is_none_or(hex) {
            return Err("Accent color: use the form #RRGGBB.".to_owned());
        }
        Ok(())
    }
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
    /// Set once the user has finished or skipped the first-run setup (even
    /// without choosing folders). Until then Lookout shows it.
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
    /// Theme and accent; none until the window has saved its first choice.
    pub appearance: Option<Appearance>,
    /// How the disk charts are coloured: one of [`CHART_COLORS`].
    pub chart_colors: String,
    /// The window stays on top and does not hide when it loses focus.
    pub pinned: bool,
    /// Keeps the text of recent searches (encrypted) for the Up arrow.
    pub remember_searches: bool,
    /// A bar reopened within 30 seconds shows the last search again. The
    /// window holds it in memory; it is never saved.
    pub remember_last_search: bool,
    /// Lets programs running as this Windows user search through the `lookout`
    /// command (a local named pipe). Off unless the user turns it on.
    pub cli_access: bool,
    /// Start Lookout when this user signs in to Windows. `None` until it is
    /// chosen; only a new install's setup offers it ticked.
    pub start_with_windows: Option<bool>,
    /// Last window position (physical pixels) per monitor, keyed by
    /// [`monitor_key`]. A monitor that is gone simply has no use.
    pub window_positions: std::collections::BTreeMap<String, (i32, i32)>,
    /// What the window shows: mode, visibility switches and tips.
    pub ui: Ui,
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
}

fn in_range(v: i64, lo: u64, hi: u64) -> bool {
    u64::try_from(v).is_ok_and(|v| (lo..=hi).contains(&v))
}

/// Disk chart colour modes; the first is the default.
pub const CHART_COLORS: [&str; 3] = ["folder", "type", "age"];

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
            appearance: None,
            chart_colors: CHART_COLORS[0].to_owned(),
            remember_searches: true,
            remember_last_search: true,
            cli_access: false,
            start_with_windows: None,
            pinned: false,
            window_positions: std::collections::BTreeMap::new(),
            ui: Ui::default(),
        }
    }
}

impl Settings {
    /// Saves the "Remember my searches" switch; on any error nothing changes.
    pub fn set_remember_searches(&mut self, on: bool, path: &Path) -> Result<(), String> {
        let mut next = self.clone();
        next.remember_searches = on;
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }

    /// Saves the "Remember my last search" switch; on any error nothing changes.
    pub fn set_remember_last_search(&mut self, on: bool, path: &Path) -> Result<(), String> {
        let mut next = self.clone();
        next.remember_last_search = on;
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }

    /// "Reset window position": forgets where the window was dragged to, on
    /// every monitor; on any error nothing changes.
    pub fn reset_window_positions(&mut self, path: &Path) -> Result<(), String> {
        let mut next = self.clone();
        next.window_positions.clear();
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }

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

    /// An install that already lists folders never sees the setup: marks it
    /// done. True if that changed anything.
    pub fn mark_configured_done(&mut self) -> bool {
        let change = !self.first_run_done && !self.index_roots.is_empty();
        self.first_run_done |= change;
        change
    }

    /// The state of the "start with Windows" switch.
    pub fn starts_with_windows(&self) -> bool {
        self.start_with_windows == Some(true)
    }

    /// Whether the setup shows the switch ticked: the choice already made,
    /// else yes (only a new install is still without one).
    pub fn setup_offers_start_with_windows(&self) -> bool {
        self.start_with_windows.unwrap_or(true)
    }

    /// Called at start: an install that was set up before this option existed
    /// keeps it off until the user turns it on. True if that changed anything.
    pub fn keep_start_with_windows_off(&mut self) -> bool {
        let change = !self.needs_first_run() && self.start_with_windows.is_none();
        if change {
            self.start_with_windows = Some(false);
        }
        change
    }

    /// Writes or removes the Run entry and saves the choice; if either fails,
    /// nothing changes.
    #[cfg(windows)]
    pub fn set_start_with_windows(
        &mut self,
        on: bool,
        path: &Path,
        entry: &RunEntry,
        exe: &Path,
    ) -> Result<(), String> {
        entry
            .set(on, exe)
            .map_err(|e| format!("Could not change the Windows startup entry: {e}"))?;
        let mut next = self.clone();
        next.start_with_windows = Some(on);
        if let Err(e) = next.save(path) {
            let _ = entry.set(self.starts_with_windows(), exe);
            return Err(format!("Could not save settings: {e}"));
        }
        *self = next;
        Ok(())
    }

    /// At every start, while the switch is on: puts a missing or outdated entry
    /// right. While it is off the registry is not touched.
    #[cfg(windows)]
    pub fn verify_start_with_windows(&self, entry: &RunEntry, exe: &Path) -> std::io::Result<()> {
        if self.starts_with_windows() {
            entry.set(true, exe)?;
        }
        Ok(())
    }

    /// The setup was finished or skipped; on any error nothing changes.
    pub fn finish_setup(&mut self, path: &Path) -> Result<(), String> {
        if self.first_run_done {
            return Ok(());
        }
        let mut next = self.clone();
        next.first_run_done = true;
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }

    /// The setup's "Start indexing": saves the folders and the two content
    /// options. A folder must exist unless it was chosen before (a drive that
    /// is away stays chosen). True if indexing has to restart.
    pub fn apply_setup(
        &mut self,
        roots: Vec<PathBuf>,
        index_contents: bool,
        ocr: bool,
        path: &Path,
    ) -> Result<bool, String> {
        let mut next = self.clone();
        next.index_roots = roots
            .into_iter()
            .filter(|p| p.is_dir() || self.index_roots.contains(p))
            .collect();
        next.index_contents = index_contents;
        next.ocr = ocr;
        let restart = next != *self;
        next.first_run_done = true;
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(restart)
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
        next.first_run_done = true;
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }

    /// Validates and saves the theme choice; on any error nothing changes.
    pub fn set_appearance(&mut self, a: Appearance, path: &Path) -> Result<(), String> {
        a.check()?;
        let mut next = self.clone();
        next.appearance = Some(a);
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }

    /// Saves the disk charts' colour mode; on any error nothing changes.
    pub fn set_chart_colors(&mut self, mode: &str, path: &Path) -> Result<(), String> {
        if !CHART_COLORS.contains(&mode) {
            return Err("Unknown chart color mode.".to_owned());
        }
        let mut next = self.clone();
        next.chart_colors = mode.to_owned();
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
    fn existing_installs_with_folders_are_marked_done_and_skip_the_setup() {
        let mut fresh = Settings::default();
        assert!(!fresh.mark_configured_done());
        assert!(fresh.needs_first_run(), "no folders, not done: show it");
        let mut configured = Settings {
            index_roots: vec![PathBuf::from(r"D:\Work")],
            ..Settings::default()
        };
        assert!(!configured.needs_first_run());
        assert!(configured.mark_configured_done());
        assert!(configured.first_run_done && !configured.mark_configured_done());
        // Removing every folder later does not bring the setup back.
        configured.index_roots.clear();
        assert!(!configured.needs_first_run());
    }

    #[test]
    fn start_with_windows_is_offered_to_new_installs_and_off_for_existing_ones() {
        let mut fresh = Settings::default();
        assert!(!fresh.keep_start_with_windows_off());
        assert!(
            !fresh.starts_with_windows(),
            "off until the setup applies it"
        );
        assert!(fresh.setup_offers_start_with_windows());
        // An older settings file has no such field.
        let mut existing: Settings = serde_json::from_str(r#"{"first_run_done": true}"#).unwrap();
        assert!(existing.keep_start_with_windows_off());
        assert!(!existing.starts_with_windows());
        assert!(
            !existing.setup_offers_start_with_windows(),
            "run setup again"
        );
        // A choice already made is kept.
        existing.start_with_windows = Some(true);
        assert!(!existing.keep_start_with_windows_off());
        assert!(existing.starts_with_windows());
    }

    #[test]
    fn remembering_searches_is_on_until_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // Also for a settings file from before the switch existed.
        std::fs::write(&path, r#"{"first_run_done": true}"#).unwrap();
        let mut s = Settings::load(&path).unwrap();
        assert!(s.remember_searches);
        s.set_remember_searches(false, &path).unwrap();
        assert!(!Settings::load(&path).unwrap().remember_searches);
    }

    #[test]
    fn skipping_the_setup_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.finish_setup(&path).unwrap();
        assert!(!Settings::load(&path).unwrap().needs_first_run());
    }

    #[test]
    fn setup_saves_folders_and_options_and_can_run_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let docs = dir.path().to_path_buf();
        let gone = dir.path().join("missing");
        let mut s = Settings::default();
        assert!(s
            .apply_setup(vec![docs.clone(), gone.clone()], false, true, &path)
            .unwrap());
        assert_eq!(s.index_roots, std::slice::from_ref(&docs));
        assert!(!s.index_contents && s.ocr && s.first_run_done);
        assert_eq!(Settings::load(&path).unwrap(), s);
        // Again with the same choices: nothing to restart.
        assert!(!s
            .apply_setup(vec![docs.clone()], false, true, &path)
            .unwrap());
        // A chosen folder that is away for now stays chosen.
        s.index_roots.push(gone.clone());
        assert!(!s
            .apply_setup(vec![docs, gone.clone()], false, true, &path)
            .unwrap());
        assert!(s.index_roots.contains(&gone));
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
    fn last_search_is_on_by_default_and_reset_forgets_every_position() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        assert!(s.remember_last_search);
        s.window_positions
            .insert(monitor_key("DISPLAY1", 0, 0, 1920, 1080), (300, 120));
        s.pinned = true;
        s.set_remember_last_search(false, &path).unwrap();
        s.reset_window_positions(&path).unwrap();
        let back = Settings::load(&path).unwrap();
        assert!(!back.remember_last_search && back.window_positions.is_empty());
        assert!(back.pinned, "only the positions are reset");
        std::fs::write(&path, r#"{"hotkey":"Alt+Space"}"#).unwrap();
        assert!(Settings::load(&path).unwrap().remember_last_search);
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
        }
    }

    #[test]
    fn old_files_load_without_an_appearance_and_drop_the_old_theme_word() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"idle_secs": 7, "theme": "auto"}"#).unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.appearance, None);
        assert_eq!(s.idle_secs, 7);
    }

    #[test]
    fn the_chart_colour_mode_is_checked_saved_and_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        let mut s = Settings::load(&path).unwrap();
        assert_eq!(s.chart_colors, "folder");
        s.set_chart_colors("age", &path).unwrap();
        assert!(s.set_chart_colors("rainbow", &path).is_err());
        assert_eq!(s.chart_colors, "age");
        assert_eq!(Settings::load(&path).unwrap().chart_colors, "age");
    }

    #[test]
    fn appearance_is_saved_loaded_and_kept_by_the_settings_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        let chosen = Appearance {
            theme: "matrix".into(),
            accent: Some("#D2A8FF".into()),
        };
        let saved = |path: &Path| Settings::load(path).unwrap().appearance;
        s.set_appearance(chosen.clone(), &path).unwrap();
        assert_eq!(saved(&path), Some(chosen.clone()));
        s.apply(changes(), &path).unwrap();
        assert_eq!(saved(&path), Some(chosen.clone()));
        // Bad values change nothing.
        for (theme, accent) in [("", None), ("Neon Grid", None), ("matrix", Some("red"))] {
            let bad = Appearance {
                theme: theme.into(),
                accent: accent.map(str::to_owned),
            };
            assert!(s.set_appearance(bad, &path).is_err());
            assert_eq!(saved(&path), Some(chosen.clone()));
        }
        let theme_accent = Appearance {
            accent: None,
            ..chosen
        };
        s.set_appearance(theme_accent.clone(), &path).unwrap();
        assert_eq!(saved(&path), Some(theme_accent));
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
    fn command_line_access_is_off_by_default() {
        assert!(!Settings::default().cli_access);
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
            appearance: Some(Appearance {
                theme: "plasma".into(),
                accent: None,
            }),
            chart_colors: "type".into(),
            pinned: true,
            remember_searches: false,
            remember_last_search: false,
            cli_access: true,
            start_with_windows: Some(true),
            window_positions: [("M|0,0|1x1".to_owned(), (5, 6))].into(),
            ui: Ui {
                mode: Some(crate::uimode::Mode::Simple),
                hide_tips: true,
                ..Ui::default()
            },
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
