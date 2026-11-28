//! The interface mode: what the window shows (dock, help, tips, hints).
//! Nothing here switches the collection or processing of data on or off.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::settings::{Settings, CHART_COLORS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Simple,
    #[default]
    Standard,
    Full,
}

/// One "show in the dock and help" switch per interface feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Show {
    pub disk: bool,
    pub clipboard: bool,
    pub capture: bool,
    pub timers: bool,
    pub quick_keys: bool,
    pub filters: bool,
    pub notes: bool,
}

impl Mode {
    /// The switches this mode sets.
    pub const fn preset(self) -> Show {
        let simple = matches!(self, Mode::Simple);
        Show {
            disk: matches!(self, Mode::Full),
            clipboard: !simple,
            capture: !simple,
            timers: true,
            quick_keys: !simple,
            filters: true,
            notes: !simple,
        }
    }
}

impl Default for Show {
    fn default() -> Self {
        Mode::Standard.preset()
    }
}

/// The saved interface choices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    /// `None` until a start has recorded it; shown as Standard.
    pub mode: Option<Mode>,
    /// The user's switches while they differ from the mode's.
    pub show: Option<Show>,
    /// No tip card on Home.
    pub hide_tips: bool,
    /// Settings > Advanced is expanded.
    pub advanced_open: bool,
    /// An existing install still has to be told that Disk left the dock.
    pub disk_notice: bool,
    /// The bar stays open after a copy or a saved note.
    pub keep_open_after_copy: bool,
    /// Home is only the search box until something is typed.
    pub compact_home: bool,
}

/// What the window reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UiState {
    pub mode: Mode,
    /// The switches differ from the mode's: its name reads "Custom".
    pub custom: bool,
    pub show: Show,
    pub hide_tips: bool,
    pub advanced_open: bool,
    pub disk_notice: bool,
    pub hide_after_copy: bool,
    pub compact_home: bool,
}

impl Ui {
    pub fn mode(&self) -> Mode {
        self.mode.unwrap_or_default()
    }

    pub fn show(&self) -> Show {
        self.show.unwrap_or_else(|| self.mode().preset())
    }

    pub fn custom(&self) -> bool {
        self.show() != self.mode().preset()
    }

    pub fn state(&self) -> UiState {
        UiState {
            mode: self.mode(),
            custom: self.custom(),
            show: self.show(),
            hide_tips: self.hide_tips,
            advanced_open: self.advanced_open,
            disk_notice: self.disk_notice,
            hide_after_copy: !self.keep_open_after_copy,
            compact_home: self.compact_home,
        }
    }

    /// Picking a mode sets every switch to the mode's.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = Some(mode);
        self.show = None;
        self.disk_notice &= !self.show().disk;
    }

    pub fn set_show(&mut self, show: Show) {
        self.show = (show != self.mode().preset()).then_some(show);
        self.disk_notice &= !show.disk;
    }

    /// Called at start. A first start records Standard; an install that was set
    /// up before had Disk in the dock, so it is told once where Disk went.
    /// True if something changed.
    pub fn adopt(&mut self, existing: bool) -> bool {
        if self.mode.is_some() {
            return false;
        }
        self.mode = Some(Mode::Standard);
        self.disk_notice = existing && !self.show().disk;
        true
    }
}

impl Settings {
    /// Changes the interface choices and saves them; on any error nothing changes.
    pub fn update_ui(&mut self, path: &Path, change: impl FnOnce(&mut Ui)) -> Result<(), String> {
        let mut next = self.clone();
        change(&mut next.ui);
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }

    /// "Reset to recommended": visibility and layout only (the window resets the
    /// theme, which it owns). Folders, privacy switches, hotkeys and saved data
    /// are left alone.
    pub fn reset_ui(&mut self, path: &Path) -> Result<(), String> {
        let mut next = self.clone();
        next.ui = Ui {
            mode: Some(Mode::Standard),
            ..Ui::default()
        };
        next.chart_colors = CHART_COLORS[0].to_owned();
        next.pinned = false;
        next.window_positions.clear();
        next.save(path)
            .map_err(|e| format!("Could not save settings: {e}"))?;
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::settings::{Appearance, IndexSpeed};

    const MODES: [Mode; 3] = [Mode::Simple, Mode::Standard, Mode::Full];

    fn show(on: [bool; 7]) -> Show {
        let [disk, clipboard, capture, timers, quick_keys, filters, notes] = on;
        Show {
            disk,
            clipboard,
            capture,
            timers,
            quick_keys,
            filters,
            notes,
        }
    }

    /// Settings with every data and privacy choice away from its default.
    fn configured() -> Settings {
        Settings {
            hotkey: "Ctrl+Shift+K".into(),
            index_roots: vec![PathBuf::from(r"D:\Work")],
            excluded_folders: vec![PathBuf::from(r"D:\Work\private")],
            index_contents: false,
            ocr: false,
            index_speed: IndexSpeed::Fast,
            content_index_max_mb: 500,
            cli_access: true,
            start_with_windows: Some(true),
            first_run_done: true,
            ..Settings::default()
        }
    }

    #[test]
    fn each_mode_sets_its_switches() {
        assert_eq!(
            Mode::Simple.preset(),
            show([false, false, false, true, false, true, false])
        );
        assert_eq!(
            Mode::Standard.preset(),
            show([false, true, true, true, true, true, true])
        );
        assert_eq!(Mode::Full.preset(), show([true; 7]));
        assert_eq!(Ui::default().mode(), Mode::Standard);
        assert_eq!(Ui::default().show(), Mode::Standard.preset());
    }

    #[test]
    fn modes_and_switches_never_change_data_or_privacy_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for start in [configured(), Settings::default()] {
            let mut s = start.clone();
            for mode in MODES {
                s.update_ui(&path, |u| u.set_mode(mode)).unwrap();
                s.update_ui(&path, |u| u.set_show(show([false; 7])))
                    .unwrap();
                let untouched = Settings {
                    ui: start.ui.clone(),
                    ..s.clone()
                };
                assert_eq!(untouched, start, "{mode:?}");
            }
            assert_eq!(Settings::load(&path).unwrap(), s);
        }
    }

    #[test]
    fn custom_means_the_switches_differ_from_the_mode() {
        let mut ui = Ui::default();
        assert!(!ui.custom());
        let mut with_disk = ui.show();
        with_disk.disk = true;
        ui.set_show(with_disk);
        assert!(ui.custom() && ui.show().disk);
        // Switched back by hand: the mode's name again.
        ui.set_show(Mode::Standard.preset());
        assert!(!ui.custom() && ui.show.is_none());
        ui.set_show(with_disk);
        ui.set_mode(Mode::Simple);
        assert!(!ui.custom());
        assert_eq!(ui.show(), Mode::Simple.preset());
    }

    #[test]
    fn an_existing_install_gets_the_disk_notice_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A settings file from before there were modes.
        std::fs::write(&path, r#"{"first_run_done": true}"#).unwrap();
        let mut s = Settings::load(&path).unwrap();
        assert!(s.ui.adopt(!s.needs_first_run()));
        assert!(s.ui.disk_notice && s.ui.mode() == Mode::Standard);
        // Still pending after a restart that came before an answer.
        s.save(&path).unwrap();
        let mut s = Settings::load(&path).unwrap();
        assert!(!s.ui.adopt(true) && s.ui.disk_notice);
        s.update_ui(&path, |u| u.disk_notice = false).unwrap();
        let mut s = Settings::load(&path).unwrap();
        assert!(!s.ui.adopt(true) && !s.ui.disk_notice);
    }

    #[test]
    fn turning_disk_on_answers_the_notice_and_new_installs_get_none() {
        let mut ui = Ui::default();
        ui.adopt(true);
        let mut on = ui.show();
        on.disk = true;
        ui.set_show(on);
        assert!(!ui.disk_notice);
        let mut ui = Ui::default();
        ui.adopt(true);
        ui.set_mode(Mode::Full);
        assert!(!ui.disk_notice);
        let mut fresh = Settings::default();
        assert!(fresh.ui.adopt(!fresh.needs_first_run()));
        assert!(!fresh.ui.disk_notice && fresh.ui.mode() == Mode::Standard);
        // A mode already chosen is kept.
        fresh.ui.set_mode(Mode::Simple);
        assert!(!fresh.ui.adopt(true));
        assert_eq!(fresh.ui.mode(), Mode::Simple);
    }

    #[test]
    fn reset_to_recommended_leaves_data_and_privacy_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // Stands in for the index, clipboard history and other saved data.
        let data = dir.path().join("lookout.redb");
        std::fs::write(&data, b"saved data").unwrap();
        let mut s = configured();
        s.appearance = Some(Appearance {
            theme: "matrix".into(),
            accent: None,
        });
        s.chart_colors = "age".into();
        s.pinned = true;
        s.window_positions.insert("M".into(), (1, 2));
        s.ui = Ui {
            mode: Some(Mode::Full),
            show: Some(show([false; 7])),
            hide_tips: true,
            advanced_open: true,
            disk_notice: true,
            keep_open_after_copy: true,
            compact_home: true,
        };
        s.reset_ui(&path).unwrap();
        assert_eq!(s.ui.mode(), Mode::Standard);
        assert!(!s.ui.custom() && !s.ui.hide_tips && !s.ui.advanced_open);
        assert!(!s.ui.disk_notice && !s.pinned && s.window_positions.is_empty());
        assert_eq!(s.chart_colors, "folder");
        let expected = Settings {
            ui: s.ui.clone(),
            appearance: s.appearance.clone(),
            ..configured()
        };
        assert_eq!(s, expected);
        assert_eq!(std::fs::read(&data).unwrap(), b"saved data");
        assert_eq!(Settings::load(&path).unwrap(), s);
    }
}
