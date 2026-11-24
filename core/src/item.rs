//! The typed item every source produces (brief section 7.2).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Folder,
    App,
    Clipboard,
    Screenshot,
    Process,
    Port,
    Project,
    Snippet,
    Custom(String),
}

impl Kind {
    /// Value of a `kind:` filter, e.g. `kind:file`. Only kinds that exist in
    /// Phase 1 are accepted.
    pub fn from_filter(value: &str) -> Option<Kind> {
        match value {
            "file" | "files" => Some(Kind::File),
            "folder" | "folders" | "dir" => Some(Kind::Folder),
            "app" | "apps" => Some(Kind::App),
            "screenshot" | "screenshots" | "image" | "images" => Some(Kind::Screenshot),
            _ => None,
        }
    }
}

/// Inputs to the ranking formula (brief section 7.6), each in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ScoreFeatures {
    #[serde(rename = "match")]
    pub match_quality: f32,
    pub frecency: f32,
    pub recency: f32,
    pub semantic: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Stable id, e.g. `file:C:\docs\a.pdf`.
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub subtitle: Option<String>,
    /// Path or URL opened by the default action.
    pub uri: Option<String>,
    pub source: String,
    /// Seconds since the Unix epoch.
    pub modified_at: Option<u64>,
    pub created_at: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub score_features: ScoreFeatures,
    /// Final score from the ranking formula; higher is better.
    pub score: f32,
}

impl Item {
    pub fn new(id: impl Into<String>, kind: Kind, title: impl Into<String>, source: &str) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            subtitle: None,
            uri: None,
            source: source.to_owned(),
            modified_at: None,
            created_at: None,
            metadata: BTreeMap::new(),
            score_features: ScoreFeatures::default(),
            score: 0.0,
        }
    }
}
