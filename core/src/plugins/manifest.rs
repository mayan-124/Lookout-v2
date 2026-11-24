//! `plugin.json`: what a plugin is and what it asks to be allowed to do.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::actions::Risk;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("plugin.json is not valid: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("plugin.json: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardAccess {
    #[default]
    None,
    Read,
    Write,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Permissions {
    /// Folders the plugin may read: `"user_selected"` (files the user picked in
    /// the bar) or other descriptions shown to the user.
    pub fs_read: Vec<String>,
    pub fs_write: Vec<String>,
    /// Hosts the plugin wants to reach. Not enforced in v1 (see docs/plugins.md).
    pub network: Vec<String>,
    /// May the plugin start other processes?
    pub spawn: bool,
    pub clipboard: ClipboardAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Executable, relative to the folder holding `plugin.json`.
    pub entry: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// First words of a query that are routed to this plugin.
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub permissions: Permissions,
    /// Keep running while enabled (for plugins that must watch something).
    #[serde(default)]
    pub background: bool,
    /// Settings the user may change in the plugin panel (numbers or text), with their defaults.
    #[serde(default)]
    pub config_defaults: serde_json::Value,
    /// One sentence shown on the approval screen after the permissions.
    #[serde(default)]
    pub approval_note: String,
    /// Optional one-line description for the plugin cards (about 110 characters).
    #[serde(default)]
    pub summary: String,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let m: Manifest = serde_json::from_str(text)?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let bad = |what: &str| Err(ManifestError::Invalid(what.to_owned()));
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        if !id_ok {
            return bad("id must be 1-64 characters of letters, digits, '.', '-' or '_'");
        }
        if self.name.trim().is_empty() {
            return bad("name is empty");
        }
        let entry = Path::new(&self.entry);
        let relative_and_inside = !self.entry.is_empty()
            && entry.is_relative()
            && !self.entry.contains(':')
            && entry
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if !relative_and_inside {
            return bad("entry must be a relative path inside the plugin folder");
        }
        Ok(())
    }

    /// The lowest risk any action of this plugin can have. A plugin can ask
    /// for more; it can never get less than what its permissions imply.
    pub fn risk_floor(&self) -> Risk {
        let p = &self.permissions;
        let wide = |v: &[String]| v.iter().any(|s| matches!(s.as_str(), "*" | "any" | "all"));
        if wide(&p.fs_write) {
            Risk::Dangerous
        } else if !p.fs_write.is_empty() || p.spawn || !p.network.is_empty() {
            Risk::Confirm
        } else {
            Risk::Safe
        }
    }

    /// What the user is shown before the plugin may run for the first time.
    pub fn describe_permissions(&self) -> Vec<String> {
        let p = &self.permissions;
        let list = |v: &[String]| {
            v.iter()
                .map(|s| {
                    if s == "user_selected" {
                        "files you pick in the bar".to_owned()
                    } else if s == "screenshots_folder" {
                        "the Lookout Screenshots folder in your Pictures".to_owned()
                    } else {
                        s.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut out = Vec::new();
        if !p.fs_read.is_empty() {
            out.push(format!("Read {}", list(&p.fs_read)));
        }
        if !p.fs_write.is_empty() {
            out.push(format!("Create, change or rename {}", list(&p.fs_write)));
        }
        match p.clipboard {
            ClipboardAccess::None => {}
            ClipboardAccess::Read => out.push("Read the clipboard".into()),
            ClipboardAccess::Write => out.push("Read and change the clipboard".into()),
        }
        if !p.network.is_empty() {
            out.push(format!(
                "Network: {} (not enforced by Lookout v1)",
                p.network.join(", ")
            ));
        }
        if p.spawn {
            out.push("Start other programs".into());
        }
        if out.is_empty() {
            out.push("No special permissions".into());
        }
        if self.background {
            out.push("Runs in the background".into());
        }
        if !self.approval_note.trim().is_empty() {
            out.push(self.approval_note.trim().to_owned());
        }
        out
    }

    /// Changes whenever the permissions or the program change, so a plugin that
    /// asks for more must be approved again. FNV-1a, stable across versions.
    pub fn approval_key(&self) -> String {
        let canonical = serde_json::to_string(&(
            &self.id,
            &self.entry,
            &self.args,
            &self.permissions,
            self.background,
        ))
        .unwrap_or_default();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in canonical.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(extra: &str) -> String {
        format!(r#"{{"id":"a.b","name":"A","version":"1","entry":"p.exe"{extra}}}"#)
    }

    #[test]
    fn permissions_set_the_risk_floor() {
        let floor = |extra: &str| Manifest::parse(&base(extra)).unwrap().risk_floor();
        assert_eq!(floor(""), Risk::Safe);
        assert_eq!(floor(r#","permissions":{"clipboard":"write"}"#), Risk::Safe);
        assert_eq!(
            floor(r#","permissions":{"fs_write":["user_selected"]}"#),
            Risk::Confirm
        );
        assert_eq!(floor(r#","permissions":{"spawn":true}"#), Risk::Confirm);
        assert_eq!(
            floor(r#","permissions":{"network":["example.com"]}"#),
            Risk::Confirm
        );
        assert_eq!(
            floor(r#","permissions":{"fs_write":["*"]}"#),
            Risk::Dangerous
        );
    }

    #[test]
    fn entries_outside_the_plugin_folder_and_bad_ids_are_rejected() {
        for entry in ["..\\x.exe", "C:\\x.exe", "\\x.exe", "a/../../x.exe", ""] {
            let text = format!(r#"{{"id":"a","name":"A","version":"1","entry":{entry:?}}}"#);
            assert!(Manifest::parse(&text).is_err(), "{entry}");
        }
        let text = r#"{"id":"a b","name":"A","version":"1","entry":"x.exe"}"#;
        assert!(Manifest::parse(text).is_err());
    }

    #[test]
    fn approval_key_changes_when_permissions_change() {
        let a = Manifest::parse(&base("")).unwrap();
        let b = Manifest::parse(&base(r#","permissions":{"spawn":true}"#)).unwrap();
        assert_ne!(a.approval_key(), b.approval_key());
        assert_eq!(a.approval_key(), a.approval_key());
    }

    #[test]
    fn the_screenshot_manifest_shows_its_summary_permissions_and_note() {
        let m = Manifest::parse(include_str!("../../../plugins/screenshot/plugin.json")).unwrap();
        assert_eq!(m.name, "Screenshot");
        assert_eq!(
            m.summary,
            "Capture, scroll-capture, record, and extract text from your screen; mark up, copy or save."
        );
        let shown = m.describe_permissions();
        assert!(shown.iter().any(|l| l.contains("Lookout Screenshots")));
        assert!(shown.iter().any(|l| l == "Read and change the clipboard"));
        assert!(shown.iter().any(|l| l == m.approval_note.trim()));
    }
}
