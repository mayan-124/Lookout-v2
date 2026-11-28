//! Pinned items: Lookout's own short list, kept in the encrypted `meta.db`.
//! A file, folder, app, Windows Settings page or web address can be pinned. Home
//! lists them, `fav` still does too, and "Unpin" takes one off again
//! (undoable). The table is called `saved`, so what earlier versions saved is
//! still here. File Explorer's own pinned list is never read.

use std::path::Path;
use std::sync::{Arc, PoisonError};

use serde::{Deserialize, Serialize};

use crate::actions::quick::{system_action, KIND_SYSTEM, KIND_URL, OPEN_URL};
use crate::actions::{
    Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target, UndoPlan,
};
use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::securedb::{DbError, SecureDb};
use crate::sources::{QueryContext, SharedApps, Source, Tier, ANSWER_SCORE};

pub const SOURCE: &str = "saved";
pub const ADD: &str = "saved_add";
pub const REMOVE: &str = "saved_remove";
pub const UP: &str = "pin_up";
pub const DOWN: &str = "pin_down";
const COMMAND: &str = "fav";

const APP: &str = "app";
const SYSTEM: &str = "system";
const URL: &str = "url";

/// The words to filter by, if the text asks for the pinned list: `fav`, or
/// `fav` and more words.
pub fn command(typed: &str) -> Option<&str> {
    let t = typed.trim();
    let head = t.get(..COMMAND.len())?;
    let rest = &t[COMMAND.len()..];
    (head.eq_ignore_ascii_case(COMMAND)
        && (rest.is_empty() || rest.starts_with(char::is_whitespace)))
    .then(|| rest.trim())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// The key: a file's or folder's path, a web address, or `app:…` and
    /// `system:…` for the others.
    pub path: String,
    pub folder: bool,
    /// Place in the list; the highest is listed first.
    pub added: i64,
    /// `None` for a file or folder.
    #[serde(default)]
    pub kind: Option<String>,
    /// An app's name.
    #[serde(default)]
    pub title: Option<String>,
}

impl Saved {
    /// The result this entry stands for. The disk is not touched. `None` for an
    /// entry that no longer means anything.
    pub fn item(&self) -> Option<Item> {
        let mut item = match self.kind.as_deref() {
            None => {
                let (kind, prefix) = if self.folder {
                    (Kind::Folder, "folder")
                } else {
                    (Kind::File, "file")
                };
                let id = format!("{prefix}:{}", self.path);
                let mut item = Item::new(id, kind, name_of(&self.path), SOURCE);
                item.subtitle = Some(self.path.clone());
                item.uri = Some(self.path.clone());
                item
            }
            Some(APP) => {
                let uri = self.path.strip_prefix("app:")?;
                let title = self.title.clone().unwrap_or_else(|| name_of(uri));
                let mut item = Item::new(self.path.clone(), Kind::App, title, SOURCE);
                item.subtitle = Some("Application".into());
                item.uri = Some(uri.to_owned());
                item
            }
            Some(SYSTEM) => {
                let entry = crate::quick::entry(self.path.strip_prefix("system:")?)?;
                let kind = Kind::Custom(KIND_SYSTEM.into());
                let mut item = Item::new(self.path.clone(), kind, entry.title, SOURCE);
                item.subtitle = Some(entry.subtitle.to_owned());
                item.uri = Some(entry.id.to_owned());
                item.metadata
                    .insert("default_action".into(), system_action(entry.what).into());
                item
            }
            Some(URL) => {
                let url = crate::quick::parse_web_url(&self.path)?;
                let mut item = Item::new(
                    format!("url:{}", url.redacted()),
                    Kind::Custom(KIND_URL.into()),
                    format!("Open {}", url.url),
                    SOURCE,
                );
                item.subtitle = Some("Opens in your default browser".into());
                item.uri = Some(url.url);
                item.metadata
                    .insert("default_action".into(), OPEN_URL.into());
                item
            }
            Some(_) => return None,
        };
        item.score_features.match_quality = 1.0;
        Some(item)
    }
}

pub struct SavedStore {
    db: Arc<SecureDb>,
}

impl SavedStore {
    pub fn new(db: Arc<SecureDb>) -> Result<Self, DbError> {
        db.write(|c| {
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS saved (
                    path TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
                    folder INTEGER NOT NULL,
                    added INTEGER NOT NULL
                )",
            )?;
            // A list from before apps and other items could be pinned.
            let upgraded = c
                .prepare("SELECT 1 FROM pragma_table_info('saved') WHERE name = 'kind'")?
                .exists([])?;
            if !upgraded {
                c.execute_batch(
                    "ALTER TABLE saved ADD COLUMN kind TEXT;
                     ALTER TABLE saved ADD COLUMN title TEXT;",
                )?;
            }
            Ok(())
        })?;
        Ok(Self { db })
    }

    /// In the order shown: the newest first until one is moved.
    pub fn list(&self) -> Result<Vec<Saved>, DbError> {
        self.db.read(|c| {
            let mut s = c.prepare(
                "SELECT path, folder, added, kind, title FROM saved ORDER BY added DESC",
            )?;
            let rows = s.query_map([], row)?;
            rows.collect()
        })
    }

    pub fn get(&self, path: &str) -> Result<Option<Saved>, DbError> {
        self.db.read(|c| {
            let mut s =
                c.prepare("SELECT path, folder, added, kind, title FROM saved WHERE path = ?1")?;
            let mut rows = s.query_map([path], row)?;
            rows.next().transpose()
        })
    }

    fn add(&self, s: &Saved) -> Result<(), DbError> {
        self.db.write(|c| {
            c.execute(
                "INSERT OR IGNORE INTO saved (path, folder, added, kind, title)
                 VALUES (?1, ?2, (SELECT IFNULL(MAX(added), 0) + 1 FROM saved), ?3, ?4)",
                rusqlite::params![s.path, s.folder, s.kind, s.title],
            )
        })?;
        Ok(())
    }

    /// Puts a removed entry back where it was in the list.
    fn restore(&self, s: &Saved) -> Result<(), DbError> {
        self.db.write(|c| {
            c.execute(
                "INSERT OR IGNORE INTO saved (path, folder, added, kind, title)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![s.path, s.folder, s.added, s.kind, s.title],
            )
        })?;
        Ok(())
    }

    fn remove(&self, path: &str) -> Result<(), DbError> {
        self.db
            .write(|c| c.execute("DELETE FROM saved WHERE path = ?1", [path]))?;
        Ok(())
    }

    /// The entry next to `path` in the shown order, above or below.
    fn neighbour(&self, path: &str, up: bool) -> Result<Option<(Saved, Saved)>, DbError> {
        let list = self.list()?;
        let Some(at) = list.iter().position(|s| s.path.eq_ignore_ascii_case(path)) else {
            return Ok(None);
        };
        let other = if up { at.checked_sub(1) } else { Some(at + 1) };
        Ok(other
            .and_then(|n| list.get(n))
            .map(|o| (list[at].clone(), o.clone())))
    }

    /// Two entries trade places, in one statement.
    fn swap(&self, a: &Saved, b: &Saved) -> Result<(), DbError> {
        self.db.write(|c| {
            c.execute(
                "UPDATE saved SET added = CASE path WHEN ?1 THEN ?2 ELSE ?3 END
                 WHERE path IN (?1, ?4)",
                rusqlite::params![a.path, b.added, a.added, b.path],
            )
        })?;
        Ok(())
    }
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Saved> {
    Ok(Saved {
        path: r.get(0)?,
        folder: r.get(1)?,
        added: r.get(2)?,
        kind: r.get(3)?,
        title: r.get(4)?,
    })
}

fn name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

/// The pinned list as results, only for `fav`. The disk is not touched, so a
/// pinned place that is offline never slows the search down.
pub struct SavedSource(pub Arc<SavedStore>);

impl Source for SavedSource {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        let Some(filter) = command(&q.raw) else {
            return;
        };
        let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
        let Ok(list) = self.0.list() else {
            crate::log::warn("the pinned list could not be read");
            return;
        };
        // Matches in the name come before matches in the rest of the path.
        let mut found: Vec<(bool, Item)> = list
            .into_iter()
            .filter_map(|s| s.item())
            .filter_map(|item| {
                let name = item.title.to_lowercase();
                let rest = item.uri.as_deref().unwrap_or_default().to_lowercase();
                words
                    .iter()
                    .all(|w| name.contains(w) || rest.contains(w))
                    .then(|| (words.iter().all(|w| name.contains(w)), item))
            })
            .collect();
        found.sort_by_key(|(in_name, _)| !in_name);
        let items: Vec<Item> = found
            .into_iter()
            .enumerate()
            .map(|(n, (_, mut item))| {
                // Keeps the list's own order.
                item.score = ANSWER_SCORE - n as f32 * 0.001;
                item
            })
            .collect();
        if !items.is_empty() {
            emit(items);
        }
    }
}

/// What a result would be pinned as; never an add-on's result.
fn entry_of(target: &Target, action: &str) -> Result<Saved, ActionError> {
    let refused = || ActionError::NotApplicable {
        action: action.into(),
    };
    let uri = target
        .uri
        .as_deref()
        .filter(|u| target.origin.is_none() && !u.is_empty())
        .ok_or_else(refused)?;
    let mut entry = Saved {
        path: uri.to_owned(),
        folder: false,
        added: 0,
        kind: None,
        title: None,
    };
    match &target.kind {
        Kind::File | Kind::Folder => {
            if !Path::new(uri).is_absolute() {
                return Err(refused());
            }
            entry.folder = target.kind == Kind::Folder;
        }
        Kind::App => {
            entry.path = format!("app:{uri}");
            entry.kind = Some(APP.into());
        }
        Kind::Custom(n) if n == KIND_SYSTEM => {
            crate::quick::entry(uri).ok_or_else(refused)?;
            entry.path = format!("system:{uri}");
            entry.kind = Some(SYSTEM.into());
        }
        Kind::Custom(n) if n == KIND_URL => {
            crate::quick::parse_web_url(uri).ok_or_else(refused)?;
            entry.kind = Some(URL.into());
        }
        _ => return Err(refused()),
    }
    Ok(entry)
}

fn pinnable(kind: &Kind) -> bool {
    match kind {
        Kind::File | Kind::Folder | Kind::App => true,
        Kind::Custom(n) => n == KIND_SYSTEM || n == KIND_URL,
        _ => false,
    }
}

fn title_of(entry: &Saved) -> String {
    entry
        .item()
        .map_or_else(|| name_of(&entry.path), |i| i.title)
}

fn failed(e: DbError) -> ActionError {
    ActionError::Failed(format!("could not update the pinned list: {e}"))
}

fn bad_plan<E>(_: E) -> ActionError {
    ActionError::Failed("bad plan".into())
}

fn plan_of(title: String, line: String, undoable: bool, snapshot: serde_json::Value) -> Plan {
    Plan {
        risk: Risk::Safe,
        preview: Preview {
            from: None,
            title,
            lines: vec![line],
            warnings: Vec::new(),
        },
        undoable,
        fingerprint: snapshot.to_string(),
        snapshot,
    }
}

/// Pins a result. The app list names a pinned app.
pub struct SaveAction(pub Arc<SavedStore>, pub SharedApps);

impl Action for SaveAction {
    fn id(&self) -> &str {
        ADD
    }

    fn label(&self) -> &str {
        "Pin to Home"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        pinnable(kind)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let mut entry = entry_of(target, ADD)?;
        if self.0.get(&entry.path).map_err(failed)?.is_some() {
            return Err(ActionError::NothingToDo("That is already pinned.".into()));
        }
        if entry.kind.as_deref() == Some(APP) {
            let uri = target.uri.as_deref().unwrap_or_default();
            entry.title = self
                .1
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|a| a.path.to_string_lossy() == uri)
                .map(|a| a.name.clone());
        }
        let snapshot = serde_json::to_value(&entry).map_err(bad_plan)?;
        Ok(plan_of(
            format!("Pin {}", title_of(&entry)),
            entry.path,
            false,
            snapshot,
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let entry: Saved = serde_json::from_value(plan.snapshot.clone()).map_err(bad_plan)?;
        self.0.add(&entry).map_err(failed)?;
        Ok(Executed {
            message: "Pinned to Home".into(),
            undo_token: None,
        })
    }
}

pub struct RemoveAction(pub Arc<SavedStore>);

impl Action for RemoveAction {
    fn id(&self) -> &str {
        REMOVE
    }

    fn label(&self) -> &str {
        "Unpin"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        pinnable(kind)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let key = entry_of(target, REMOVE)?.path;
        let entry = self
            .0
            .get(&key)
            .map_err(failed)?
            .ok_or_else(|| ActionError::NothingToDo("That is not pinned.".into()))?;
        let snapshot = serde_json::to_value(&entry).map_err(bad_plan)?;
        Ok(plan_of(
            format!("Unpin {}", title_of(&entry)),
            entry.path,
            true,
            snapshot,
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let entry: Saved = serde_json::from_value(plan.snapshot.clone()).map_err(bad_plan)?;
        self.0.remove(&entry.path).map_err(failed)?;
        Ok(Executed {
            message: format!("Unpinned {}", title_of(&entry)),
            undo_token: Some(plan.snapshot.to_string()),
        })
    }

    fn plan_undo(&self, token: &str) -> Result<UndoPlan, ActionError> {
        let entry: Saved = serde_json::from_str(token).map_err(|_| ActionError::NotUndoable)?;
        if self.0.get(&entry.path).map_err(failed)?.is_some() {
            return Err(ActionError::Stale(
                "That has been pinned again already.".into(),
            ));
        }
        Ok(UndoPlan {
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: format!("Pin {} again", title_of(&entry)),
                lines: vec![entry.path],
                warnings: Vec::new(),
            },
            fingerprint: token.to_owned(),
        })
    }

    fn undo(
        &self,
        token: &str,
        _plan: &UndoPlan,
        _proof: &Authorized,
    ) -> Result<String, ActionError> {
        let entry: Saved = serde_json::from_str(token).map_err(|_| ActionError::NotUndoable)?;
        self.0.restore(&entry).map_err(failed)?;
        Ok("Pinned again".into())
    }
}

/// "Move up" and "Move down" in the pinned list.
pub struct MoveAction {
    pub store: Arc<SavedStore>,
    pub up: bool,
}

impl Action for MoveAction {
    fn id(&self) -> &str {
        if self.up {
            UP
        } else {
            DOWN
        }
    }

    fn label(&self) -> &str {
        if self.up {
            "Move up"
        } else {
            "Move down"
        }
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        pinnable(kind)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let key = entry_of(target, self.id())?.path;
        if self
            .store
            .neighbour(&key, self.up)
            .map_err(failed)?
            .is_none()
        {
            return Err(ActionError::NothingToDo(
                if self.up {
                    "It cannot move further up."
                } else {
                    "It cannot move further down."
                }
                .into(),
            ));
        }
        Ok(plan_of(
            self.label().to_owned(),
            key.clone(),
            false,
            serde_json::json!({ "path": key }),
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let key = plan.snapshot["path"].as_str().ok_or_else(|| bad_plan(()))?;
        if let Some((a, b)) = self.store.neighbour(key, self.up).map_err(failed)? {
            self.store.swap(&a, &b).map_err(failed)?;
        }
        Ok(Executed {
            message: "Moved".into(),
            undo_token: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::undo_log;
    use crate::apps::AppEntry;
    use std::sync::atomic::AtomicU64;

    fn store() -> Arc<SavedStore> {
        Arc::new(SavedStore::new(Arc::new(SecureDb::in_memory().unwrap())).unwrap())
    }

    fn target(path: &str, kind: Kind) -> Target {
        Target {
            item_id: format!("file:{path}"),
            kind,
            uri: Some(path.to_owned()),
            port: None,
            origin: None,
        }
    }

    fn ask(store: &Arc<SavedStore>, text: &str) -> Vec<String> {
        let q = crate::query::parse(text);
        let ctx = QueryContext::new(1, Arc::new(AtomicU64::new(1)));
        let mut out = Vec::new();
        SavedSource(store.clone()).query(&q, &ctx, &mut |i| out.extend(i));
        out.into_iter().map(|i| i.title).collect()
    }

    fn pin(store: &Arc<SavedStore>) -> SaveAction {
        let apps = vec![AppEntry {
            name: "Paint".into(),
            lower: "paint".into(),
            path: r"C:\start\mspaint.lnk".into(),
        }];
        SaveAction(store.clone(), Arc::new(std::sync::RwLock::new(apps)))
    }

    fn save(store: &Arc<SavedStore>, path: &str, kind: Kind) {
        let act = pin(store);
        let plan = act.plan(&target(path, kind)).unwrap();
        assert_eq!(plan.risk, Risk::Safe);
        act.execute(&plan, &Authorized::for_tests()).unwrap();
    }

    fn shift(store: &Arc<SavedStore>, path: &str, up: bool) -> Result<(), ActionError> {
        let act = MoveAction {
            store: store.clone(),
            up,
        };
        let plan = act.plan(&target(path, Kind::Folder))?;
        act.execute(&plan, &Authorized::for_tests()).map(drop)
    }

    #[test]
    fn fav_alone_or_with_words_is_the_command() {
        assert_eq!(command("fav"), Some(""));
        assert_eq!(command(" FAV  "), Some(""));
        assert_eq!(command("fav tax 2025"), Some("tax 2025"));
        for no in ["fa", "favorite", "favs x", "my fav", "", "é"] {
            assert_eq!(command(no), None, "{no:?}");
        }
    }

    #[test]
    fn pinned_places_are_listed_newest_first_and_filtered_as_typed() {
        let s = store();
        save(&s, r"C:\work\reports", Kind::Folder);
        save(&s, r"C:\home\tax.pdf", Kind::File);
        assert_eq!(ask(&s, "fav"), ["tax.pdf", "reports"]);
        assert_eq!(ask(&s, "fav "), ["tax.pdf", "reports"]);
        assert_eq!(ask(&s, "fav rep"), ["reports"]);
        // A word from the path matches too; a name match goes first.
        assert_eq!(ask(&s, "fav work"), ["reports"]);
        assert!(ask(&s, "fav zzz").is_empty());
        assert!(ask(&s, "tax").is_empty(), "only the command lists them");
        // The same place twice is refused, whatever the letter case.
        let again = pin(&s).plan(&target(r"c:\HOME\tax.pdf", Kind::File));
        assert!(matches!(again, Err(ActionError::NothingToDo(_))));
    }

    #[test]
    fn apps_settings_pages_and_web_addresses_can_be_pinned_too() {
        let s = store();
        save(&s, r"C:\start\mspaint.lnk", Kind::App);
        save(&s, "wifi_page", Kind::Custom(KIND_SYSTEM.into()));
        save(
            &s,
            "https://example.com/a?q=1",
            Kind::Custom(KIND_URL.into()),
        );
        let items: Vec<Item> = s.list().unwrap().iter().filter_map(Saved::item).collect();
        let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Open https://example.com/a?q=1",
                "Open Wi-Fi settings",
                "Paint"
            ]
        );
        assert_eq!(items[2].id, r"app:C:\start\mspaint.lnk");
        assert_eq!(items[1].metadata["default_action"], "system_page");
        // The id reaches the action log: no path or query of the address.
        assert!(!items[0].id.contains("q=1"), "{}", items[0].id);
        // Each can be taken off again through the result it is listed as.
        for item in &items {
            let t = Target {
                item_id: item.id.clone(),
                kind: item.kind.clone(),
                uri: item.uri.clone(),
                port: None,
                origin: None,
            };
            let act = RemoveAction(s.clone());
            let plan = act.plan(&t).unwrap();
            act.execute(&plan, &Authorized::for_tests()).unwrap();
        }
        assert!(s.list().unwrap().is_empty());
        // A window or an answer is not something to keep.
        for kind in [Kind::Custom("window".into()), Kind::Clipboard, Kind::Port] {
            assert!(!pin(&s).applies_to(&kind), "{kind:?}");
        }
    }

    #[test]
    fn pinned_entries_move_up_and_down_and_stop_at_the_ends() {
        let s = store();
        for name in ["one", "two", "three"] {
            save(&s, &format!(r"C:\a\{name}"), Kind::Folder);
        }
        assert_eq!(ask(&s, "fav"), ["three", "two", "one"]);
        shift(&s, r"C:\a\one", true).unwrap();
        assert_eq!(ask(&s, "fav"), ["three", "one", "two"]);
        shift(&s, r"C:\a\three", false).unwrap();
        assert_eq!(ask(&s, "fav"), ["one", "three", "two"]);
        for (path, up) in [(r"C:\a\one", true), (r"C:\a\two", false)] {
            assert!(matches!(
                shift(&s, path, up),
                Err(ActionError::NothingToDo(_))
            ));
        }
    }

    #[test]
    fn a_list_saved_by_an_earlier_version_is_kept() {
        let db = Arc::new(SecureDb::in_memory().unwrap());
        db.write(|c| {
            c.execute_batch(
                "CREATE TABLE saved (
                    path TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
                    folder INTEGER NOT NULL,
                    added INTEGER NOT NULL
                );
                INSERT INTO saved VALUES ('C:\\old\\kept.txt', 0, 1);",
            )
        })
        .unwrap();
        // Opened twice: the columns are added once.
        SavedStore::new(db.clone()).unwrap();
        let s = Arc::new(SavedStore::new(db).unwrap());
        assert_eq!(ask(&s, "fav"), ["kept.txt"]);
        save(&s, r"C:\new", Kind::Folder);
        assert_eq!(ask(&s, "fav"), ["new", "kept.txt"]);
    }

    #[test]
    fn unpin_goes_through_the_gate_and_can_be_undone() {
        let db = Arc::new(SecureDb::in_memory().unwrap());
        let s = Arc::new(SavedStore::new(db.clone()).unwrap());
        save(&s, r"C:\a\one", Kind::Folder);
        save(&s, r"C:\a\two", Kind::Folder);
        let engine = crate::actions::ActionEngine::new(
            vec![Arc::new(RemoveAction(s.clone()))],
            undo_log::UndoLog::new(db).unwrap(),
            crate::actions::GateConfig::default(),
        );
        let p = engine
            .prepare(REMOVE, target(r"C:\a\one", Kind::Folder))
            .unwrap();
        assert!(p.undoable);
        assert!(engine.run(&p.ticket).unwrap().undoable);
        assert_eq!(ask(&s, "fav"), ["two"]);
        let undo = engine.prepare_undo().unwrap();
        engine.run(&undo.ticket).unwrap();
        assert_eq!(ask(&s, "fav"), ["two", "one"], "back in its old place");
    }

    #[test]
    fn only_lookouts_own_results_can_be_pinned() {
        let act = pin(&store());
        let mut addon = target(r"C:\a", Kind::File);
        addon.origin = Some(crate::actions::PluginOrigin {
            plugin_id: "x".into(),
            action_id: "y".into(),
            also: Vec::new(),
            params: serde_json::Value::Null,
        });
        assert!(act.plan(&addon).is_err());
        assert!(act.plan(&target("relative", Kind::File)).is_err());
        assert!(act
            .plan(&target("no_such_page", Kind::Custom(KIND_SYSTEM.into())))
            .is_err());
    }
}
