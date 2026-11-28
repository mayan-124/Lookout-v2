//! Saved files and folders: Lookout's own short list, kept in the encrypted
//! `meta.db`. "Save" on a result adds it, `fav` lists it, and "Remove" takes it
//! off again (undoable). File Explorer's own pinned list is never read.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::actions::{
    Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target, UndoPlan,
};
use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::securedb::{DbError, SecureDb};
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};

pub const SOURCE: &str = "saved";
pub const ADD: &str = "saved_add";
pub const REMOVE: &str = "saved_remove";
const COMMAND: &str = "fav";

/// The words to filter by, if the text asks for the saved list: `fav`, or
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
    pub path: String,
    pub folder: bool,
    /// Order of saving; the newest is listed first.
    pub added: i64,
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
            )
        })?;
        Ok(Self { db })
    }

    /// Newest first.
    pub fn list(&self) -> Result<Vec<Saved>, DbError> {
        self.db.read(|c| {
            let mut s = c.prepare("SELECT path, folder, added FROM saved ORDER BY added DESC")?;
            let rows = s.query_map([], row)?;
            rows.collect()
        })
    }

    pub fn get(&self, path: &str) -> Result<Option<Saved>, DbError> {
        self.db.read(|c| {
            let mut s = c.prepare("SELECT path, folder, added FROM saved WHERE path = ?1")?;
            let mut rows = s.query_map([path], row)?;
            rows.next().transpose()
        })
    }

    fn add(&self, path: &str, folder: bool) -> Result<(), DbError> {
        self.db.write(|c| {
            c.execute(
                "INSERT OR IGNORE INTO saved (path, folder, added)
                 VALUES (?1, ?2, (SELECT IFNULL(MAX(added), 0) + 1 FROM saved))",
                rusqlite::params![path, folder],
            )
        })?;
        Ok(())
    }

    /// Puts a removed entry back where it was in the list.
    fn restore(&self, s: &Saved) -> Result<(), DbError> {
        self.db.write(|c| {
            c.execute(
                "INSERT OR IGNORE INTO saved (path, folder, added) VALUES (?1, ?2, ?3)",
                rusqlite::params![s.path, s.folder, s.added],
            )
        })?;
        Ok(())
    }

    fn remove(&self, path: &str) -> Result<(), DbError> {
        self.db
            .write(|c| c.execute("DELETE FROM saved WHERE path = ?1", [path]))?;
        Ok(())
    }
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Saved> {
    Ok(Saved {
        path: r.get(0)?,
        folder: r.get(1)?,
        added: r.get(2)?,
    })
}

fn name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

/// The saved list as results, only for `fav`. The disk is not touched, so a
/// saved place that is offline never slows the search down.
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
            crate::log::warn("the saved list could not be read");
            return;
        };
        // Matches in the name come before matches in the rest of the path.
        let mut found: Vec<(bool, Saved)> = list
            .into_iter()
            .filter_map(|s| {
                let (path, name) = (s.path.to_lowercase(), name_of(&s.path).to_lowercase());
                words
                    .iter()
                    .all(|w| path.contains(w))
                    .then(|| (words.iter().all(|w| name.contains(w)), s))
            })
            .collect();
        found.sort_by_key(|(in_name, _)| !in_name);
        let items: Vec<Item> = found
            .into_iter()
            .enumerate()
            .map(|(n, (_, s))| {
                let (kind, prefix) = if s.folder {
                    (Kind::Folder, "folder")
                } else {
                    (Kind::File, "file")
                };
                let mut item = Item::new(
                    format!("{prefix}:{}", s.path),
                    kind,
                    name_of(&s.path),
                    SOURCE,
                );
                item.subtitle = Some(s.path.clone());
                item.uri = Some(s.path);
                // Keeps the list's own order.
                item.score = ANSWER_SCORE - n as f32 * 0.001;
                item.score_features.match_quality = 1.0;
                item
            })
            .collect();
        if !items.is_empty() {
            emit(items);
        }
    }
}

/// The file or folder a result stands for; never an add-on's result.
fn path_of<'a>(target: &'a Target, action: &str) -> Result<&'a str, ActionError> {
    target
        .uri
        .as_deref()
        .filter(|u| target.origin.is_none() && Path::new(u).is_absolute())
        .ok_or_else(|| ActionError::NotApplicable {
            action: action.into(),
        })
}

fn failed(e: DbError) -> ActionError {
    ActionError::Failed(format!("could not update the saved list: {e}"))
}

fn plan_of(title: String, path: &str, undoable: bool, snapshot: serde_json::Value) -> Plan {
    Plan {
        risk: Risk::Safe,
        preview: Preview {
            from: None,
            title,
            lines: vec![path.to_owned()],
            warnings: Vec::new(),
        },
        undoable,
        fingerprint: snapshot.to_string(),
        snapshot,
    }
}

pub struct SaveAction(pub Arc<SavedStore>);

impl Action for SaveAction {
    fn id(&self) -> &str {
        ADD
    }

    fn label(&self) -> &str {
        "Save"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::File | Kind::Folder)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let path = path_of(target, ADD)?;
        if self.0.get(path).map_err(failed)?.is_some() {
            return Err(ActionError::NothingToDo(
                "That is already saved. Type \"fav\" to see it.".into(),
            ));
        }
        let folder = target.kind == Kind::Folder;
        Ok(plan_of(
            format!("Save {}", name_of(path)),
            path,
            false,
            serde_json::json!({ "path": path, "folder": folder }),
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let path = plan.snapshot["path"]
            .as_str()
            .ok_or_else(|| ActionError::Failed("bad plan".into()))?;
        let folder = plan.snapshot["folder"].as_bool().unwrap_or(false);
        self.0.add(path, folder).map_err(failed)?;
        Ok(Executed {
            message: "Saved. Type \"fav\" to see it.".into(),
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
        "Remove from saved"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::File | Kind::Folder)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let path = path_of(target, REMOVE)?;
        let entry = self
            .0
            .get(path)
            .map_err(failed)?
            .ok_or_else(|| ActionError::NothingToDo("That is not saved.".into()))?;
        let snapshot =
            serde_json::to_value(&entry).map_err(|_| ActionError::Failed("bad plan".into()))?;
        Ok(plan_of(
            format!("Remove {} from saved", name_of(path)),
            path,
            true,
            snapshot,
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let entry: Saved = serde_json::from_value(plan.snapshot.clone())
            .map_err(|_| ActionError::Failed("bad plan".into()))?;
        self.0.remove(&entry.path).map_err(failed)?;
        Ok(Executed {
            message: "Removed from saved".into(),
            undo_token: Some(plan.snapshot.to_string()),
        })
    }

    fn plan_undo(&self, token: &str) -> Result<UndoPlan, ActionError> {
        let entry: Saved = serde_json::from_str(token).map_err(|_| ActionError::NotUndoable)?;
        if self.0.get(&entry.path).map_err(failed)?.is_some() {
            return Err(ActionError::Stale(
                "That has been saved again already.".into(),
            ));
        }
        Ok(UndoPlan {
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: format!("Put {} back in saved", name_of(&entry.path)),
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
        Ok("Back in saved".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::undo_log;
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

    fn save(store: &Arc<SavedStore>, path: &str, kind: Kind) {
        let act = SaveAction(store.clone());
        let plan = act.plan(&target(path, kind)).unwrap();
        assert_eq!(plan.risk, Risk::Safe);
        act.execute(&plan, &Authorized::for_tests()).unwrap();
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
    fn saved_places_are_listed_newest_first_and_filtered_as_typed() {
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
        let again = SaveAction(s.clone()).plan(&target(r"c:\HOME\tax.pdf", Kind::File));
        assert!(matches!(again, Err(ActionError::NothingToDo(_))));
    }

    #[test]
    fn remove_goes_through_the_gate_and_can_be_undone() {
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
    fn only_lookouts_own_files_and_folders_can_be_saved() {
        let act = SaveAction(store());
        assert!(!act.applies_to(&Kind::App));
        let mut addon = target(r"C:\a", Kind::File);
        addon.origin = Some(crate::actions::PluginOrigin {
            plugin_id: "x".into(),
            action_id: "y".into(),
            also: Vec::new(),
            params: serde_json::Value::Null,
        });
        assert!(act.plan(&addon).is_err());
        assert!(act.plan(&target("relative", Kind::File)).is_err());
    }
}
