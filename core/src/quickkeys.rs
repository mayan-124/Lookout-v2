//! Quick keys: a short alias (2 to 8 letters or digits) for one result.
//! Typing the alias and a space puts that result on top; Enter then goes
//! through the same action engine as for any result, so previews and
//! confirmations are never skipped. Aliases live in the encrypted `meta.db`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;

use crate::actions::quick::{KIND_SYSTEM, KIND_URL};
use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::quick;
use crate::securedb::{DbError, SecureDb};
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};

pub const MIN_LEN: usize = 2;
pub const MAX_LEN: usize = 8;
pub const MAX_KEYS: usize = 50;
pub const KIND_MISSING: &str = "quickkey_missing";

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error(
        "A quick key needs {MIN_LEN} to {MAX_LEN} letters or digits, with no spaces or symbols."
    )]
    Format,
    #[error("\"{0}\" looks like a web address or a path. Pick a plain word.")]
    UrlLike(String),
    #[error("\"{0}\" is reserved for a built-in command. Pick another word.")]
    Reserved(String),
    #[error("\"{alias}\" is already used for {target}.")]
    Taken { alias: String, target: String },
    #[error("You can have at most {MAX_KEYS} quick keys. Delete one first.")]
    TooMany,
    #[error("Quick keys can be set for apps, files, folders, web addresses and system actions.")]
    NotSupported,
    #[error("Enter the path of an existing file or folder, or a web address (apps and system actions are set from search results).")]
    BadTarget,
    #[error("There is no quick key \"{0}\".")]
    Unknown(String),
    #[error("{0}")]
    Db(#[from] DbError),
}

/// Words that mean something else in the search bar: the system entries and
/// their keywords, and the built-in commands.
fn is_reserved(alias: &str) -> bool {
    const FIXED: &[&str] = &[
        "timer", "timers", "settings", "setting", "help", "kill", "port",
    ];
    FIXED.contains(&alias)
        || quick::ENTRIES.iter().any(|e| {
            e.id == alias
                || e.keywords
                    .iter()
                    .any(|k| *k == alias || k.split_whitespace().any(|w| w == alias))
        })
}

/// Checks the form of an alias and returns it in lower case. Does not look at
/// what is already stored.
pub fn validate_alias(typed: &str) -> Result<String, KeyError> {
    let t = typed.trim().to_lowercase();
    let url_like = t.contains(['.', '/', '\\', ':', '@', '?', '#'])
        || ["http", "www", "ftp", "file", "mailto"]
            .iter()
            .any(|p| t.starts_with(p));
    if url_like {
        return Err(KeyError::UrlLike(t));
    }
    if is_reserved(&t) {
        return Err(KeyError::Reserved(t));
    }
    if !(MIN_LEN..=MAX_LEN).contains(&t.chars().count())
        || !t.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(KeyError::Format);
    }
    Ok(t)
}

/// Whether a result may get a quick key: apps, files, folders, web addresses
/// and the system entries, never a plugin's items.
pub fn eligible(item: &Item) -> bool {
    !item.source.starts_with("plugin:")
        && item.uri.is_some()
        && match &item.kind {
            Kind::File | Kind::Folder | Kind::App => true,
            Kind::Custom(n) => n == KIND_URL || n == KIND_SYSTEM,
            _ => false,
        }
}

/// The part of a result that is kept: no scores, no snippets.
fn stored_form(item: &Item) -> Item {
    let mut out = Item::new(
        item.id.clone(),
        item.kind.clone(),
        item.title.clone(),
        &item.source,
    );
    out.subtitle = item.subtitle.clone();
    out.uri = item.uri.clone();
    if let Some(a) = item.metadata.get("default_action") {
        out.metadata.insert("default_action".into(), a.clone());
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyInfo {
    pub alias: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub kind: String,
}

pub struct QuickKeys {
    db: Arc<SecureDb>,
    map: Mutex<BTreeMap<String, Item>>,
}

impl QuickKeys {
    pub fn new(db: Arc<SecureDb>) -> Result<Self, DbError> {
        let rows = db.write(|c| {
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS quick_keys (
                    alias TEXT PRIMARY KEY NOT NULL,
                    item_json TEXT NOT NULL
                )",
            )?;
            let mut s = c.prepare("SELECT alias, item_json FROM quick_keys")?;
            let rows = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        let map = rows
            .into_iter()
            .filter_map(|(a, j)| Some((a, serde_json::from_str::<Item>(&j).ok()?)))
            .collect();
        Ok(Self {
            db,
            map: Mutex::new(map),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Item>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn list(&self) -> Vec<KeyInfo> {
        self.lock()
            .iter()
            .map(|(a, i)| KeyInfo {
                alias: a.clone(),
                title: i.title.clone(),
                subtitle: i.subtitle.clone(),
                kind: match &i.kind {
                    Kind::Custom(n) if n == KIND_URL => "web address".to_owned(),
                    Kind::Custom(_) => "system".to_owned(),
                    Kind::Folder => "folder".to_owned(),
                    Kind::App => "app".to_owned(),
                    _ => "file".to_owned(),
                },
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Assigns `typed` to `item`. Fails with a message that names the current
    /// owner when the alias is taken.
    pub fn add(&self, typed: &str, item: &Item) -> Result<String, KeyError> {
        if !eligible(item) {
            return Err(KeyError::NotSupported);
        }
        let alias = validate_alias(typed)?;
        let stored = stored_form(item);
        let json = serde_json::to_string(&stored).map_err(|_| KeyError::NotSupported)?;
        let mut map = self.lock();
        if let Some(other) = map.get(&alias) {
            return Err(KeyError::Taken {
                alias,
                target: other.title.clone(),
            });
        }
        if map.len() >= MAX_KEYS {
            return Err(KeyError::TooMany);
        }
        self.db.write(|c| {
            c.execute(
                "INSERT INTO quick_keys (alias, item_json) VALUES (?1, ?2)",
                rusqlite::params![alias, json],
            )
        })?;
        map.insert(alias.clone(), stored);
        Ok(alias)
    }

    /// Assigns `typed` to a folder, file or web address typed or pasted in
    /// Settings (apps and system actions are set from the results instead).
    pub fn add_typed(&self, typed: &str, target: &str) -> Result<String, KeyError> {
        let t = quick::strip_controls(target);
        let t = t.trim().trim_matches('"');
        let p = Path::new(t);
        let item = if !t.is_empty() && p.exists() {
            let (kind, prefix) = if p.is_dir() {
                (Kind::Folder, "folder")
            } else {
                (Kind::File, "file")
            };
            let name = p
                .file_name()
                .map_or_else(|| t.to_owned(), |n| n.to_string_lossy().into_owned());
            let mut i = Item::new(format!("{prefix}:{t}"), kind, name, "files");
            i.subtitle = Some(t.to_owned());
            i.uri = Some(t.to_owned());
            i
        } else if let Some(u) = quick::parse_web_url(t) {
            let mut i = Item::new(
                format!("url:{}", u.redacted()),
                Kind::Custom(KIND_URL.into()),
                format!("Open {}", u.url),
                "link",
            );
            i.subtitle = Some("Opens in your default browser".into());
            i.uri = Some(u.url);
            i.metadata
                .insert("default_action".into(), "open_url".into());
            i
        } else {
            return Err(KeyError::BadTarget);
        };
        self.add(typed, &item)
    }

    /// Changes the alias of an existing quick key.
    pub fn rename(&self, old: &str, typed: &str) -> Result<String, KeyError> {
        let old = old.trim().to_lowercase();
        let new = validate_alias(typed)?;
        let mut map = self.lock();
        let item = map
            .get(&old)
            .cloned()
            .ok_or(KeyError::Unknown(old.clone()))?;
        if new == old {
            return Ok(new);
        }
        if let Some(other) = map.get(&new) {
            return Err(KeyError::Taken {
                alias: new,
                target: other.title.clone(),
            });
        }
        self.db.write(|c| {
            c.execute(
                "UPDATE quick_keys SET alias = ?1 WHERE alias = ?2",
                rusqlite::params![new, old],
            )
        })?;
        map.remove(&old);
        map.insert(new.clone(), item);
        Ok(new)
    }

    pub fn remove(&self, alias: &str) -> Result<(), KeyError> {
        let alias = alias.trim().to_lowercase();
        let mut map = self.lock();
        if map.remove(&alias).is_none() {
            return Err(KeyError::Unknown(alias));
        }
        self.db
            .write(|c| c.execute("DELETE FROM quick_keys WHERE alias = ?1", [&alias]))?;
        Ok(())
    }

    /// The result an alias stands for, as shown on top of the list. A file,
    /// folder or app that is gone becomes a "target missing" result that
    /// cannot be run.
    pub fn resolve(&self, alias: &str) -> Option<Item> {
        let stored = self.lock().get(&alias.to_lowercase())?.clone();
        Some(resolved(&alias.to_lowercase(), stored))
    }
}

fn resolved(alias: &str, mut item: Item) -> Item {
    let gone = match &item.kind {
        Kind::File | Kind::Folder | Kind::App => {
            item.uri.as_deref().is_none_or(|u| !Path::new(u).exists())
        }
        Kind::Custom(n) if n == KIND_URL => item
            .uri
            .as_deref()
            .is_none_or(|u| quick::parse_web_url(u).is_none()),
        Kind::Custom(_) => item.uri.as_deref().and_then(quick::entry).is_none(),
        _ => true,
    };
    if gone {
        let mut m = Item::new(
            format!("quickkey:{alias}"),
            Kind::Custom(KIND_MISSING.into()),
            format!("Quick key \"{alias}\": target missing"),
            "quickkey",
        );
        m.subtitle = Some(format!(
            "{} no longer exists. Change or delete this quick key in Settings.",
            item.uri.as_deref().unwrap_or(&item.title)
        ));
        m.score = ANSWER_SCORE + 1.0;
        m.score_features.match_quality = 1.0;
        return m;
    }
    // System entries are described by the fixed table, never by stored text.
    if let Kind::Custom(n) = &item.kind {
        if n == KIND_SYSTEM {
            if let Some(e) = item.uri.as_deref().and_then(quick::entry) {
                item.title = e.title.to_owned();
                item.subtitle = Some(e.subtitle.to_owned());
            }
        }
    }
    item.source = "quickkey".into();
    item.metadata.insert("quick_key".into(), alias.into());
    item.score = ANSWER_SCORE + 1.0;
    item.score_features.match_quality = 1.0;
    item
}

/// Puts the quick key's result on top when the text is exactly an alias
/// followed by whitespace. Anything else (no space, more words) is untouched.
pub struct QuickKeySource {
    keys: Arc<QuickKeys>,
}

impl QuickKeySource {
    pub fn new(keys: Arc<QuickKeys>) -> Self {
        Self { keys }
    }
}

impl Source for QuickKeySource {
    fn id(&self) -> &'static str {
        "quickkey"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if !q.trailing_space || q.kind.is_some() || !(MIN_LEN..=MAX_LEN).contains(&q.raw.len()) {
            return;
        }
        if let Some(item) = self.keys.resolve(&q.raw) {
            emit(vec![item]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn keys() -> QuickKeys {
        QuickKeys::new(Arc::new(SecureDb::in_memory().unwrap())).unwrap()
    }

    fn file(path: &str) -> Item {
        let mut i = Item::new(format!("file:{path}"), Kind::File, "notes.txt", "files");
        i.uri = Some(path.to_owned());
        i.score = 5.0;
        i
    }

    fn system(id: &str) -> Item {
        let mut i = Item::new(
            format!("system:{id}"),
            Kind::Custom(KIND_SYSTEM.into()),
            "Open Sound settings",
            "system",
        );
        i.uri = Some(id.to_owned());
        i.metadata
            .insert("default_action".into(), "system_page".into());
        i
    }

    #[test]
    fn good_aliases_are_lower_cased() {
        assert_eq!(validate_alias("GH").unwrap(), "gh");
        assert_eq!(validate_alias(" proj1 ").unwrap(), "proj1");
        assert_eq!(validate_alias("abcdefgh").unwrap(), "abcdefgh");
        assert_eq!(validate_alias("42").unwrap(), "42");
    }

    #[test]
    fn length_and_characters_are_checked() {
        for bad in [
            "",
            "a",
            "abcdefghi",
            "my key",
            "ab-cd",
            "ab_c",
            "é1",
            "a b",
            "ab!",
        ] {
            assert!(
                matches!(validate_alias(bad), Err(KeyError::Format)),
                "{bad}"
            );
        }
    }

    #[test]
    fn reserved_words_are_refused() {
        for bad in [
            "timer",
            "timers",
            "settings",
            "help",
            "dark",
            "DARK",
            "bluetooth",
            "wifi",
            "night",
            "airplane",
            "battery",
            "storage",
            "update",
            "sound",
            "display",
            "dnd",
            "focus",
        ] {
            assert!(
                matches!(validate_alias(bad), Err(KeyError::Reserved(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn things_that_look_like_addresses_or_paths_are_refused() {
        for bad in [
            "a.com", "http", "https", "www", "wwwx", "c:", "c:\\x", "a/b", "\\\\srv", "ftp",
            "file", "a@b", "x.y",
        ] {
            assert!(
                matches!(validate_alias(bad), Err(KeyError::UrlLike(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn duplicates_conflict_and_name_the_owner() {
        let k = keys();
        assert_eq!(k.add("nt", &file("C:\\a\\notes.txt")).unwrap(), "nt");
        let e = k.add("NT", &file("C:\\b\\other.txt")).unwrap_err();
        assert!(
            matches!(&e, KeyError::Taken { alias, target } if alias == "nt" && target == "notes.txt")
        );
        assert!(e.to_string().contains("already used"));
        assert_eq!(k.len(), 1);
    }

    #[test]
    fn at_most_fifty_and_only_supported_results() {
        let k = keys();
        for n in 0..MAX_KEYS {
            k.add(&format!("k{n:02}"), &file(&format!("C:\\f{n}")))
                .unwrap();
        }
        assert!(matches!(
            k.add("extra", &file("C:\\x")),
            Err(KeyError::TooMany)
        ));
        let other = keys();
        let mut port = Item::new("port:80", Kind::Port, "Port 80", "ports");
        port.uri = Some("80".into());
        assert!(matches!(
            other.add("pp", &port),
            Err(KeyError::NotSupported)
        ));
        let mut plugin = file("C:\\x");
        plugin.source = "plugin:clipboard".into();
        assert!(matches!(
            other.add("pq", &plugin),
            Err(KeyError::NotSupported)
        ));
    }

    #[test]
    fn typed_targets_are_paths_that_exist_or_web_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let k = keys();
        let d = dir.path().to_string_lossy().into_owned();
        assert_eq!(k.add_typed("dd", &d).unwrap(), "dd");
        assert_eq!(k.add_typed("ww", "Example.com/x").unwrap(), "ww");
        assert!(matches!(
            k.add_typed("zz", "C:\\no\\such\\thing"),
            Err(KeyError::BadTarget)
        ));
        assert!(matches!(
            k.add_typed("zz", "javascript:alert(1)"),
            Err(KeyError::BadTarget)
        ));
        assert!(matches!(k.add_typed("dd", &d), Err(KeyError::Taken { .. })));
        let kinds: Vec<_> = k.list().into_iter().map(|i| i.kind).collect();
        assert_eq!(kinds, ["folder", "web address"]);
        assert_eq!(
            k.resolve("ww").unwrap().uri.as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn rename_and_remove() {
        let k = keys();
        k.add("aa", &file("C:\\a")).unwrap();
        k.add("bb", &file("C:\\b")).unwrap();
        assert!(matches!(k.rename("aa", "bb"), Err(KeyError::Taken { .. })));
        assert!(matches!(k.rename("aa", "dark"), Err(KeyError::Reserved(_))));
        assert_eq!(k.rename("aa", "cc").unwrap(), "cc");
        assert!(k.resolve("aa").is_none() && k.resolve("cc").is_some());
        k.remove("cc").unwrap();
        assert!(matches!(k.remove("cc"), Err(KeyError::Unknown(_))));
    }

    #[test]
    fn aliases_are_stored_encrypted_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let key = crate::crypto::Key::from_bytes(&[9u8; 32]).unwrap();
        {
            let k = QuickKeys::new(Arc::new(SecureDb::open(&path, key.clone()).unwrap())).unwrap();
            k.add("zq", &system("sound")).unwrap();
        }
        let k = QuickKeys::new(Arc::new(SecureDb::open(&path, key).unwrap())).unwrap();
        assert_eq!(k.list().len(), 1);
        assert_eq!(k.list()[0].alias, "zq");
        assert!(!String::from_utf8_lossy(&std::fs::read(&path).unwrap()).contains("quick_keys"));
    }

    fn ask(src: &QuickKeySource, text: &str) -> Vec<Item> {
        let q = crate::query::parse(text);
        let ctx = QueryContext::new(1, Arc::new(AtomicU64::new(1)));
        let mut out = Vec::new();
        src.query(&q, &ctx, &mut |i| out.extend(i));
        out
    }

    #[test]
    fn an_alias_fires_only_on_an_exact_word_plus_a_trailing_space() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("notes.txt");
        std::fs::write(&f, "x").unwrap();
        let k = Arc::new(keys());
        k.add("nt", &file(&f.to_string_lossy())).unwrap();
        let src = QuickKeySource::new(k);
        let hit = ask(&src, "nt ");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].id, format!("file:{}", f.to_string_lossy()));
        assert!(hit[0].score >= ANSWER_SCORE);
        assert_eq!(ask(&src, "NT  ").len(), 1);
        for no in [
            "nt",
            "n",
            "nt x",
            "nt  x ",
            "ntt ",
            "xnt ",
            " ",
            "",
            "nt. ",
            "kind:file nt ",
        ] {
            assert!(ask(&src, no).is_empty(), "{no:?}");
        }
    }

    #[test]
    fn a_missing_target_is_a_clear_result_that_cannot_be_acted_on() {
        let k = Arc::new(keys());
        k.add("gone", &file("C:\\definitely\\not\\here.txt"))
            .unwrap();
        let src = QuickKeySource::new(k);
        let hit = ask(&src, "gone ");
        assert_eq!(hit.len(), 1);
        assert!(hit[0].title.contains("target missing"));
        assert_eq!(hit[0].kind, Kind::Custom(KIND_MISSING.into()));
        assert!(!hit[0].metadata.contains_key("default_action"));
        assert!(hit[0].uri.is_none());
    }

    #[test]
    fn a_system_alias_keeps_its_action_and_fixed_text() {
        let k = Arc::new(keys());
        k.add("snd", &system("sound")).unwrap();
        let hit = ask(&QuickKeySource::new(k), "snd ");
        assert_eq!(hit[0].metadata["default_action"], "system_page");
        assert_eq!(hit[0].uri.as_deref(), Some("sound"));
        assert_eq!(hit[0].title, "Open Sound settings");
    }
}
