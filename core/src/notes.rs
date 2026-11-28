//! Short notes typed in the search box (`note: buy milk`), kept in the
//! encrypted `meta.db` next to the search history, so "delete my data" removes
//! them with it. Their text never reaches the undo log, the file or content
//! index, the search history, add-ons or the command line.
//!
//! A deleted note is only marked, so it can be undone; the newest
//! [`KEEP_DELETED`] are kept and "clear all" removes every one.

use std::sync::Arc;

use chrono::TimeZone;
use rusqlite::{params, Connection, OptionalExtension};

use crate::actions::disk::TextClipboard;
use crate::actions::{
    Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target, UndoPlan,
};
use crate::content::secrets;
use crate::index_service::unix_now;
use crate::item::{Item, Kind, ScoreFeatures};
use crate::query::ParsedQuery;
use crate::ranking::{self, RankingWeights};
use crate::securedb::{DbError, SecureDb};
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};
use crate::websearch::{plain_words, Claimed};

pub const MAX_CHARS: usize = 2_000;
/// Most notes kept: every save rewrites the encrypted database.
pub const MAX_NOTES: usize = 500;
pub const KEEP_DELETED: usize = 20;
/// Most notes a listing shows.
pub const LIST_LIMIT: usize = 50;
/// Most notes an ordinary search shows, and the shortest text that searches them.
pub const SEARCH_LIMIT: usize = 5;
pub const SEARCH_MIN_CHARS: usize = 3;

pub const SAVE: &str = "note_save";
pub const COPY: &str = "note_copy";
pub const DELETE: &str = "note_delete";
/// `Kind::Custom` names of note results.
pub const KIND_NOTE: &str = "note";
pub const KIND_NEW: &str = "note_new";
pub const KIND_INFO: &str = "note_info";
const SOURCE: &str = "notes";
const PREFIX: &str = "note:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Save(String),
    List,
}

/// True for text that starts `note:`: it is for notes only.
pub fn is_private(input: &str) -> bool {
    input
        .trim_start()
        .get(..PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(PREFIX))
}

/// `note: <text>` saves; `note:` and `notes` list.
pub fn command(input: &str) -> Option<Command> {
    let text = input.trim();
    if text.eq_ignore_ascii_case("notes") {
        return Some(Command::List);
    }
    if !is_private(text) {
        return None;
    }
    let rest = crate::quick::strip_controls(&text[PREFIX.len()..]);
    let rest = rest.trim();
    Some(if rest.is_empty() {
        Command::List
    } else {
        Command::Save(rest.to_owned())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: i64,
    pub text: String,
    /// Unix seconds.
    pub ts: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum NoteError {
    #[error("That note is empty")]
    Empty,
    #[error("A note can be at most {MAX_CHARS} characters")]
    TooLong,
    #[error("Lookout keeps at most {MAX_NOTES} notes; delete one first")]
    Full,
    #[error(transparent)]
    Db(#[from] DbError),
}

/// The table is made at first use, so a start never writes for it.
fn table(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS notes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            ts INTEGER NOT NULL,
            deleted INTEGER
        )",
    )
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: r.get(0)?,
        text: r.get(1)?,
        ts: r.get(2)?,
    })
}

pub struct NoteStore {
    db: Arc<SecureDb>,
}

impl NoteStore {
    pub fn new(db: Arc<SecureDb>) -> Self {
        Self { db }
    }

    /// Keeps `text`, with anything that looks like a secret masked.
    pub fn add(&self, text: &str, now: i64) -> Result<Note, NoteError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(NoteError::Empty);
        }
        if text.chars().count() > MAX_CHARS {
            return Err(NoteError::TooLong);
        }
        if self.count()? >= MAX_NOTES {
            return Err(NoteError::Full);
        }
        let text = secrets::mask(text).0.into_owned();
        let id = self.db.write(|c| {
            table(c)?;
            c.execute(
                "INSERT INTO notes (text, ts) VALUES (?1, ?2)",
                params![text, now],
            )?;
            Ok(c.last_insert_rowid())
        })?;
        Ok(Note { id, text, ts: now })
    }

    pub fn count(&self) -> Result<usize, DbError> {
        let n: i64 = self.db.read(|c| {
            table(c)?;
            c.query_row(
                "SELECT COUNT(*) FROM notes WHERE deleted IS NULL",
                [],
                |r| r.get(0),
            )
        })?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<Note>, DbError> {
        self.db.read(|c| {
            table(c)?;
            c.prepare(
                "SELECT id, text, ts FROM notes WHERE deleted IS NULL
                 ORDER BY id DESC LIMIT ?1",
            )?
            .query_map(params![limit as i64], row)?
            .collect()
        })
    }

    /// Notes holding every word, newest first. Words are lower case.
    pub fn search(&self, words: &[String], limit: usize) -> Result<Vec<Note>, DbError> {
        let mut found = self.recent(MAX_NOTES + KEEP_DELETED)?;
        found.retain(|n| {
            let text = n.text.to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        });
        found.truncate(limit);
        Ok(found)
    }

    pub fn get(&self, id: i64) -> Result<Option<Note>, DbError> {
        self.db.read(|c| {
            table(c)?;
            c.query_row(
                "SELECT id, text, ts FROM notes WHERE id = ?1 AND deleted IS NULL",
                params![id],
                row,
            )
            .optional()
        })
    }

    /// Takes the note away; [`restore`](Self::restore) brings it back. False if
    /// there was none.
    pub fn delete(&self, id: i64) -> Result<bool, DbError> {
        self.db.write(|c| {
            table(c)?;
            // Overwrite the pages freed by notes that fall off the end.
            c.execute_batch("PRAGMA secure_delete = ON")?;
            let changed = c.execute(
                "UPDATE notes SET deleted =
                    (SELECT COALESCE(MAX(deleted), 0) + 1 FROM notes)
                 WHERE id = ?1 AND deleted IS NULL",
                params![id],
            )?;
            c.execute(
                "DELETE FROM notes WHERE deleted IS NOT NULL AND id NOT IN
                    (SELECT id FROM notes WHERE deleted IS NOT NULL
                     ORDER BY deleted DESC LIMIT ?1)",
                params![KEEP_DELETED as i64],
            )?;
            Ok(changed > 0)
        })
    }

    /// False if the note is gone for good.
    pub fn restore(&self, id: i64) -> Result<bool, DbError> {
        self.db.write(|c| {
            table(c)?;
            c.execute(
                "UPDATE notes SET deleted = NULL WHERE id = ?1 AND deleted IS NOT NULL",
                params![id],
            )
            .map(|n| n > 0)
        })
    }

    /// Removes every note, the deleted ones too.
    pub fn clear(&self) -> Result<(), DbError> {
        self.db.write(|c| {
            table(c)?;
            c.execute_batch("PRAGMA secure_delete = ON; DELETE FROM notes")
        })
    }
}

fn saved_on(ts: i64) -> String {
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|t| format!("Note · saved {}", t.format("%-d %b %Y")))
        .unwrap_or_else(|| "Note".into())
}

fn info(title: impl Into<String>, subtitle: Option<&str>) -> Item {
    let mut item = Item::new("note:info", Kind::Custom(KIND_INFO.into()), title, SOURCE);
    item.subtitle = subtitle.map(str::to_owned);
    item.score = ANSWER_SCORE;
    item.score_features.match_quality = 1.0;
    item
}

fn item(note: Note) -> Item {
    let mut item = Item::new(
        format!("note:{}", note.id),
        Kind::Custom(KIND_NOTE.into()),
        note.text,
        SOURCE,
    );
    item.subtitle = Some(saved_on(note.ts));
    // The id only: the actions read the text from the store.
    item.uri = Some(note.id.to_string());
    item.metadata.insert("default_action".into(), COPY.into());
    item
}

/// First words Lookout answers itself: they are not offered as a note.
const OWN_WORDS: &[&str] = &[
    "help", "kill", "timer", "timers", "disk", "settings", "setup",
];

pub struct NoteSource {
    store: Arc<NoteStore>,
    weights: RankingWeights,
    claimed: Claimed,
}

impl NoteSource {
    pub fn new(store: Arc<NoteStore>, weights: RankingWeights) -> Self {
        Self {
            store,
            weights,
            claimed: Box::new(|_| false),
        }
    }

    /// Words add-ons answer, so no note is offered for them.
    pub fn unless_claimed(mut self, claimed: Claimed) -> Self {
        self.claimed = claimed;
        self
    }

    fn save_row(&self, text: String) -> Item {
        let length = text.chars().count();
        if length > MAX_CHARS {
            return info(
                NoteError::TooLong.to_string(),
                Some(&format!("This one has {length}")),
            );
        }
        if self.store.count().is_ok_and(|n| n >= MAX_NOTES) {
            return info(NoteError::Full.to_string(), None);
        }
        let masked = secrets::mask(&text).1 > 0;
        let mut item = Item::new(
            "note:new",
            Kind::Custom(KIND_NEW.into()),
            format!("Save note: {text}"),
            SOURCE,
        );
        item.subtitle = Some(if masked {
            "What looks like a key or card number is masked before saving".into()
        } else {
            "Kept encrypted on this PC".into()
        });
        item.uri = Some(text);
        item.score = ANSWER_SCORE;
        item.score_features.match_quality = 1.0;
        item.metadata.insert("default_action".into(), SAVE.into());
        item
    }

    fn list(&self) -> Vec<Item> {
        let notes = self.store.recent(LIST_LIMIT).unwrap_or_default();
        if notes.is_empty() {
            return vec![info("No notes yet", Some("Type note: buy milk"))];
        }
        notes
            .into_iter()
            .enumerate()
            .map(|(n, note)| {
                let mut row = item(note);
                // Newest first, above everything else.
                row.score = ANSWER_SCORE - n as f32 * 0.01;
                row.score_features.match_quality = 1.0;
                row
            })
            .collect()
    }

    fn found(&self, q: &ParsedQuery) -> Vec<Item> {
        // Filters are about files.
        if q.kind.is_some() || q.time.is_some() || !q.ops.is_empty() {
            return Vec::new();
        }
        // "clip " and "shot " search an add-on, and Home asks for "clip ".
        let command = q.tokens.first().is_some_and(|t| {
            ["clip", "clips", "clipboard", "shot", "screenshot"].contains(&t.as_str())
        });
        if command && (q.trailing_space || q.tokens.len() > 1) {
            return Vec::new();
        }
        let letters: usize = q.tokens.iter().map(|t| t.chars().count()).sum();
        if letters < SEARCH_MIN_CHARS {
            return Vec::new();
        }
        let words: Vec<String> = q.tokens.iter().map(|t| t.to_lowercase()).collect();
        let now = unix_now();
        self.store
            .search(&words, SEARCH_LIMIT)
            .unwrap_or_default()
            .into_iter()
            .map(|note| {
                let text = note.text.to_lowercase();
                let starts = words.iter().all(|w| {
                    text.split(|c: char| !c.is_alphanumeric())
                        .any(|part| part.starts_with(w.as_str()))
                });
                let features = ScoreFeatures {
                    match_quality: if starts { 0.75 } else { 0.55 },
                    recency: ranking::recency(u32::try_from(note.ts).unwrap_or(0), now),
                    ..ScoreFeatures::default()
                };
                let mut row = item(note);
                row.score = self.weights.score(&features);
                row.score_features = features;
                row
            })
            .collect()
    }
}

impl Source for NoteSource {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        let items = match command(&q.raw) {
            Some(Command::Save(text)) => vec![self.save_row(text)],
            Some(Command::List) => self.list(),
            None => self.found(q),
        };
        if !items.is_empty() {
            emit(items);
        }
    }

    /// "Save as note" for ordinary words that found nothing else.
    fn fallback(&self, q: &ParsedQuery) -> Option<Item> {
        let first = q.tokens.first()?;
        let text = crate::quick::strip_controls(&q.raw);
        let text = text.trim();
        if command(text).is_some()
            || !plain_words(q)
            || OWN_WORDS.contains(&first.as_str())
            || (self.claimed)(first)
            || text.chars().count() > MAX_CHARS
        {
            return None;
        }
        let mut row = self.save_row(text.to_owned());
        // The store is full: nothing to offer.
        if row.kind != Kind::Custom(KIND_NEW.into()) {
            return None;
        }
        row.title = format!("Save as note: {text}");
        // Under a web search, where one is chosen.
        row.score = ANSWER_SCORE - 1.0;
        row.metadata.insert("fallback".into(), true.into());
        Some(row)
    }
}

// --------------------------------------------------------------- actions

fn failed(e: impl std::fmt::Display) -> ActionError {
    ActionError::Failed(e.to_string())
}

fn id_of(text: &str) -> Result<i64, ActionError> {
    text.parse()
        .map_err(|_| ActionError::Refused("that row is not a note".into()))
}

fn gone() -> ActionError {
    ActionError::Stale("that note is no longer there".into())
}

fn preview(title: &str, line: &str) -> Preview {
    Preview {
        from: None,
        title: title.into(),
        lines: vec![line.to_owned()],
        warnings: vec![],
    }
}

/// Saves what was typed. The text travels in the fingerprint, which is never
/// logged; the snapshot, which is, holds none of it.
pub struct SaveNote(pub Arc<NoteStore>);

impl Action for SaveNote {
    fn id(&self) -> &str {
        SAVE
    }
    fn label(&self) -> &str {
        "Save note"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::Custom(n) if n == KIND_NEW)
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let text = target
            .uri
            .clone()
            .ok_or_else(|| ActionError::Refused("there is nothing to save".into()))?;
        Ok(Plan {
            risk: Risk::Safe,
            preview: preview("Save note", &text),
            undoable: true,
            snapshot: serde_json::json!({ "chars": text.chars().count() }),
            fingerprint: text,
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        let note = self
            .0
            .add(&plan.fingerprint, i64::from(unix_now()))
            .map_err(failed)?;
        let message = if note.text == plan.fingerprint.trim() {
            "Note saved"
        } else {
            "Note saved with the secret masked"
        };
        Ok(Executed {
            message: message.into(),
            undo_token: Some(note.id.to_string()),
        })
    }
    fn plan_undo(&self, token: &str) -> Result<UndoPlan, ActionError> {
        Ok(UndoPlan {
            risk: Risk::Safe,
            preview: preview("Undo saving a note", "The note is deleted"),
            fingerprint: format!("{SAVE}|{}", id_of(token)?),
        })
    }
    fn undo(&self, token: &str, _: &UndoPlan, _: &Authorized) -> Result<String, ActionError> {
        // Already gone is fine: undo can then move on to what came before.
        Ok(if self.0.delete(id_of(token)?).map_err(failed)? {
            "Note deleted".into()
        } else {
            "That note was already deleted".into()
        })
    }
}

pub struct CopyNote(pub Arc<NoteStore>, pub Arc<dyn TextClipboard>);

impl Action for CopyNote {
    fn id(&self) -> &str {
        COPY
    }
    fn label(&self) -> &str {
        "Copy note"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::Custom(n) if n == KIND_NOTE)
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let id = id_of(target.uri.as_deref().unwrap_or_default())?;
        let note = self.0.get(id).map_err(failed)?.ok_or_else(gone)?;
        Ok(Plan {
            risk: Risk::Safe,
            preview: preview("Copy note", &note.text),
            undoable: false,
            snapshot: serde_json::json!({ "id": id }),
            fingerprint: format!("{COPY}|{id}"),
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        let id = plan.snapshot["id"].as_i64().ok_or_else(gone)?;
        let note = self.0.get(id).map_err(failed)?.ok_or_else(gone)?;
        self.1.set_text(&note.text).map_err(ActionError::Failed)?;
        Ok(Executed {
            message: "Note copied".into(),
            undo_token: None,
        })
    }
}

pub struct DeleteNote(pub Arc<NoteStore>);

impl Action for DeleteNote {
    fn id(&self) -> &str {
        DELETE
    }
    fn label(&self) -> &str {
        "Delete note"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::Custom(n) if n == KIND_NOTE)
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let id = id_of(target.uri.as_deref().unwrap_or_default())?;
        let note = self.0.get(id).map_err(failed)?.ok_or_else(gone)?;
        Ok(Plan {
            risk: Risk::Safe,
            preview: preview("Delete note", &note.text),
            undoable: true,
            snapshot: serde_json::json!({ "id": id }),
            fingerprint: format!("{DELETE}|{id}"),
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        let id = plan.snapshot["id"].as_i64().ok_or_else(gone)?;
        if !self.0.delete(id).map_err(failed)? {
            return Err(gone());
        }
        Ok(Executed {
            message: "Note deleted".into(),
            undo_token: Some(id.to_string()),
        })
    }
    fn plan_undo(&self, token: &str) -> Result<UndoPlan, ActionError> {
        Ok(UndoPlan {
            risk: Risk::Safe,
            preview: preview("Undo deleting a note", "The note comes back"),
            fingerprint: format!("{DELETE}|{}", id_of(token)?),
        })
    }
    fn undo(&self, token: &str, _: &UndoPlan, _: &Authorized) -> Result<String, ActionError> {
        Ok(if self.0.restore(id_of(token)?).map_err(failed)? {
            "Note restored".into()
        } else {
            "That note was deleted for good and cannot come back".into()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::PoisonError;

    use super::*;
    use crate::actions::undo_log::UndoLog;
    use crate::actions::{ActionEngine, GateConfig};
    use crate::crypto::Key;
    use crate::query::parse;
    use crate::storage::{DataPaths, Storage};

    fn store() -> Arc<NoteStore> {
        Arc::new(NoteStore::new(Arc::new(SecureDb::in_memory().unwrap())))
    }

    fn search(store: &Arc<NoteStore>, text: &str) -> Vec<Item> {
        let source = NoteSource::new(store.clone(), RankingWeights::default());
        let ctx = QueryContext::new(0, Arc::default());
        let mut out = Vec::new();
        source.query(&parse(text), &ctx, &mut |items| out = items);
        out
    }

    fn titles(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.title.as_str()).collect()
    }

    #[test]
    fn typed_commands_are_read() {
        let save = |t: &str| Some(Command::Save(t.into()));
        assert_eq!(command("note: buy milk"), save("buy milk"));
        assert_eq!(command("  Note:Buy Milk "), save("Buy Milk"));
        for list in ["notes", "Notes", "note:", "note:   "] {
            assert_eq!(command(list), Some(Command::List), "{list}");
        }
        for other in ["note", "notes.txt", "my notes", "notepad", "no", "né: x"] {
            assert_eq!(command(other), None, "{other}");
        }
        assert!(is_private("note: x") && !is_private("notes"));
    }

    #[test]
    fn a_note_is_not_read_as_filters_or_operators() {
        for text in [
            "note: ext:pdf last week",
            "note: call in:Downloads size:big",
        ] {
            let q = parse(text);
            assert!(!q.is_empty(), "{text}");
            assert!(
                q.ops.is_empty() && q.time.is_none() && q.kind.is_none(),
                "{text}"
            );
            assert!(q.understood.operators.is_empty() && !q.understood.invalid_operator);
            assert!(q.understood.date.is_none());
        }
    }

    #[test]
    fn save_list_and_search_from_three_characters() {
        let s = store();
        assert_eq!(titles(&search(&s, "notes")), ["No notes yet"]);
        s.add("buy milk", 100).unwrap();
        s.add("Call the plumber", 200).unwrap();
        assert_eq!(
            titles(&search(&s, "note:")),
            ["Call the plumber", "buy milk"]
        );
        assert_eq!(titles(&search(&s, "milk")), ["buy milk"]);
        assert_eq!(titles(&search(&s, "MIL buy")), ["buy milk"]);
        assert_eq!(titles(&search(&s, "plu")), ["Call the plumber"]);
        for short in ["m", "mi", "bu"] {
            assert!(search(&s, short).is_empty(), "{short}");
        }
        s.add("paper clip", 300).unwrap();
        assert_eq!(titles(&search(&s, "clip")), ["paper clip"]);
        assert!(search(&s, "clip ").is_empty() && search(&s, "clip paper").is_empty());
        assert!(s.delete(3).unwrap());
        for filtered in ["milk ext:txt", "milk last week", "kind:file milk"] {
            assert!(search(&s, filtered).is_empty(), "{filtered}");
        }
        let new = search(&s, "note: water plants");
        assert_eq!(titles(&new), ["Save note: water plants"]);
        assert_eq!(s.count().unwrap(), 2, "typing alone saves nothing");
    }

    #[test]
    fn sizes_are_limited_and_secrets_masked() {
        let s = store();
        let long = "x".repeat(MAX_CHARS + 1);
        assert!(matches!(s.add(&long, 1), Err(NoteError::TooLong)));
        assert!(s.add(&"é".repeat(MAX_CHARS), 1).is_ok());
        assert!(matches!(s.add("  ", 1), Err(NoteError::Empty)));
        let row = search(&s, &format!("note: {long}"));
        assert_eq!(row[0].kind, Kind::Custom(KIND_INFO.into()));
        let card = format!("card 4111 1111 1111 {}", "1111");
        assert_ne!(s.add(&card, 1).unwrap().text, card);
        for n in s.count().unwrap()..MAX_NOTES {
            s.add(&format!("note {n}"), 1).unwrap();
        }
        assert!(matches!(s.add("one more", 1), Err(NoteError::Full)));
        let row = search(&s, "note: one more");
        assert!(row[0].title.contains("at most 500"), "{}", row[0].title);
    }

    #[test]
    fn only_the_newest_deleted_notes_can_come_back() {
        let s = store();
        let ids: Vec<i64> = (0..=KEEP_DELETED)
            .map(|n| s.add(&format!("n{n}"), 1).unwrap().id)
            .collect();
        for id in &ids {
            assert!(s.delete(*id).unwrap());
        }
        assert!(!s.restore(ids[0]).unwrap());
        assert!(s.restore(ids[1]).unwrap());
        assert_eq!(s.count().unwrap(), 1);
        s.clear().unwrap();
        assert!(!s.restore(ids[2]).unwrap());
        assert!(s.recent(10).unwrap().is_empty());
    }

    #[derive(Default)]
    struct Pad(Mutex<String>);

    impl TextClipboard for Pad {
        fn set_text(&self, text: &str) -> Result<(), String> {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) = text.to_owned();
            Ok(())
        }
    }

    #[test]
    fn save_copy_delete_and_undo_through_the_action_engine() {
        let db = Arc::new(SecureDb::in_memory().unwrap());
        let s = Arc::new(NoteStore::new(db.clone()));
        let pad = Arc::new(Pad::default());
        let actions: Vec<Arc<dyn Action>> = vec![
            Arc::new(SaveNote(s.clone())),
            Arc::new(CopyNote(s.clone(), pad.clone())),
            Arc::new(DeleteNote(s.clone())),
        ];
        let engine = ActionEngine::new(actions, UndoLog::new(db).unwrap(), GateConfig::default());
        let run = |action: &str, row: &Item| {
            let target = Target {
                item_id: row.id.clone(),
                kind: row.kind.clone(),
                uri: row.uri.clone(),
                port: None,
                origin: None,
            };
            let p = engine.prepare(action, target).unwrap();
            engine.run(&p.ticket).unwrap()
        };
        let undo = || {
            let p = engine.prepare_undo().unwrap();
            engine.run(&p.ticket).unwrap().message
        };
        let new = search(&s, "note: secret plan alpha").remove(0);
        assert!(run(SAVE, &new).undoable);
        let row = search(&s, "alpha").remove(0);
        run(COPY, &row);
        assert_eq!(*pad.0.lock().unwrap(), "secret plan alpha");
        assert!(run(DELETE, &row).undoable);
        assert!(search(&s, "alpha").is_empty());
        assert_eq!(undo(), "Note restored");
        assert_eq!(titles(&search(&s, "alpha")), ["secret plan alpha"]);
        assert_eq!(undo(), "Note deleted");
        assert!(search(&s, "alpha").is_empty());
        // The log holds no note text.
        let log = serde_json::to_string(&engine.history(50).unwrap()).unwrap();
        assert!(!log.contains("alpha"), "{log}");
    }

    #[test]
    fn delete_my_data_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DataPaths::new(dir.path());
        let key = Key::generate().unwrap();
        let open = || NoteStore::new(Storage::with_key(paths.clone(), key.clone()).unwrap().db);
        open().add("quarterly numbers", 1).unwrap();
        // Sealed on disk, and still there after a restart.
        let raw = std::fs::read(paths.meta_db()).unwrap();
        assert!(!raw.windows(9).any(|w| w == b"quarterly"));
        assert_eq!(open().recent(10).unwrap()[0].text, "quarterly numbers");
        paths.wipe().unwrap();
        assert!(paths.leftovers().is_empty());
        assert!(open().recent(10).unwrap().is_empty());
    }
}
