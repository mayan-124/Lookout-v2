//! The undo log (brief section 7.3): one SQLite row per action execution.
//!
//! A row is written with status `started` *before* the action runs and
//! updated afterwards, so an action that was running when the process died
//! shows up as `interrupted` after the next start instead of vanishing. If the
//! row cannot be written the action does not run.

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::securedb::{DbError, SecureDb};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("undo log database error: {0}")]
    Db(#[from] DbError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Written before the action ran. Left behind only by a crash.
    Started,
    Executed,
    Failed,
    Undone,
    UndoFailed,
    /// A `started` row found at startup: the process ended while it ran.
    Interrupted,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Started => "started",
            Status::Executed => "executed",
            Status::Failed => "failed",
            Status::Undone => "undone",
            Status::UndoFailed => "undo_failed",
            Status::Interrupted => "interrupted",
        }
    }

    fn parse(s: &str) -> Status {
        match s {
            "executed" => Status::Executed,
            "failed" => Status::Failed,
            "undone" => Status::Undone,
            "undo_failed" => Status::UndoFailed,
            "interrupted" => Status::Interrupted,
            _ => Status::Started,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub id: i64,
    /// Unix seconds when the action was confirmed and started.
    pub ts: i64,
    pub action_id: String,
    pub items: Vec<String>,
    pub params: serde_json::Value,
    pub undo_token: Option<String>,
    pub status: Status,
    pub detail: String,
}

/// The log lives in the shared, encrypted `meta.db`: command lines and working
/// folders it stores are never on disk in the clear.
pub struct UndoLog {
    db: Arc<SecureDb>,
}

impl UndoLog {
    pub fn open_in_memory() -> Result<Self, LogError> {
        Self::new(Arc::new(SecureDb::in_memory()?))
    }

    pub fn new(db: Arc<SecureDb>) -> Result<Self, LogError> {
        db.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS undo_log (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts INTEGER NOT NULL,
                    action_id TEXT NOT NULL,
                    items TEXT NOT NULL,
                    params TEXT NOT NULL,
                    undo_token TEXT,
                    status TEXT NOT NULL,
                    detail TEXT NOT NULL DEFAULT '',
                    finished_ts INTEGER
                )",
            )?;
            conn.execute(
                "UPDATE undo_log SET status = 'interrupted' WHERE status = 'started'",
                [],
            )
        })?;
        Ok(Self { db })
    }

    /// Records that `action_id` is about to run and returns the row id.
    pub fn begin(
        &self,
        action_id: &str,
        items: &[String],
        params: &serde_json::Value,
        now: i64,
    ) -> Result<i64, LogError> {
        let items = serde_json::to_string(items).unwrap_or_else(|_| "[]".into());
        let params = serde_json::to_string(params).unwrap_or_else(|_| "null".into());
        Ok(self.db.write(|conn| {
            conn.execute(
                "INSERT INTO undo_log (ts, action_id, items, params, status)
                 VALUES (?1, ?2, ?3, ?4, 'started')",
                params![now, action_id, items, params],
            )?;
            Ok(conn.last_insert_rowid())
        })?)
    }

    /// Records how an execution ended; `undo_token` makes the row undoable.
    pub fn finish(
        &self,
        id: i64,
        status: Status,
        undo_token: Option<&str>,
        detail: &str,
        now: i64,
    ) -> Result<(), LogError> {
        self.db.write(|conn| {
            conn.execute(
                "UPDATE undo_log SET status = ?2, undo_token = ?3, detail = ?4, finished_ts = ?5
                 WHERE id = ?1",
                params![id, status.as_str(), undo_token, detail, now],
            )
        })?;
        Ok(())
    }

    /// Records the outcome of undoing row `id`. The token is kept for the record.
    pub fn finish_undo(&self, id: i64, ok: bool, detail: &str, now: i64) -> Result<(), LogError> {
        let status = if ok {
            Status::Undone
        } else {
            Status::UndoFailed
        };
        self.db.write(|conn| {
            conn.execute(
                "UPDATE undo_log SET status = ?2, detail = ?3, finished_ts = ?4 WHERE id = ?1",
                params![id, status.as_str(), detail, now],
            )
        })?;
        Ok(())
    }

    /// The newest executed row that carries an undo token.
    pub fn latest_undoable(&self) -> Result<Option<Entry>, LogError> {
        Ok(self.db.read(|conn| {
            conn.query_row(
                "SELECT id, ts, action_id, items, params, undo_token, status, detail
                 FROM undo_log
                 WHERE status = 'executed' AND undo_token IS NOT NULL
                 ORDER BY id DESC LIMIT 1",
                [],
                row_to_entry,
            )
            .optional()
        })?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Entry>, LogError> {
        Ok(self.db.read(|conn| {
            conn.query_row(
                "SELECT id, ts, action_id, items, params, undo_token, status, detail
                 FROM undo_log WHERE id = ?1",
                [id],
                row_to_entry,
            )
            .optional()
        })?)
    }

    /// The newest `limit` rows, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<Entry>, LogError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        Ok(self.db.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, ts, action_id, items, params, undo_token, status, detail
                 FROM undo_log ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], row_to_entry)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })?)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    let items: String = row.get(3)?;
    let params: String = row.get(4)?;
    let status: String = row.get(6)?;
    Ok(Entry {
        id: row.get(0)?,
        ts: row.get(1)?,
        action_id: row.get(2)?,
        items: serde_json::from_str(&items).unwrap_or_default(),
        params: serde_json::from_str(&params).unwrap_or(serde_json::Value::Null),
        undo_token: row.get(5)?,
        status: Status::parse(&status),
        detail: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_survive_a_restart_with_their_undo_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("meta.db");
        let key = crate::crypto::Key::generate().unwrap();
        let open = || UndoLog::new(Arc::new(SecureDb::open(&path, key.clone()).unwrap())).unwrap();
        let id = {
            let log = open();
            let id = log
                .begin(
                    "kill_port",
                    &["port:3000".into()],
                    &json!({"port": 3000}),
                    10,
                )
                .unwrap();
            log.finish(id, Status::Executed, Some("{\"t\":1}"), "stopped", 11)
                .unwrap();
            id
        };
        let log = open();
        let e = log.latest_undoable().unwrap().unwrap();
        assert_eq!(e.id, id);
        assert_eq!(e.action_id, "kill_port");
        assert_eq!(e.items, ["port:3000"]);
        assert_eq!(e.params, json!({"port": 3000}));
        assert_eq!(e.undo_token.as_deref(), Some("{\"t\":1}"));
        assert_eq!(e.status, Status::Executed);
        assert_eq!(e.ts, 10);
    }

    #[test]
    fn a_row_left_started_becomes_interrupted_on_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let key = crate::crypto::Key::generate().unwrap();
        let open = || UndoLog::new(Arc::new(SecureDb::open(&path, key.clone()).unwrap())).unwrap();
        let id = {
            let log = open();
            log.begin("kill_port", &[], &json!(null), 1).unwrap()
        };
        let log = open();
        assert_eq!(log.get(id).unwrap().unwrap().status, Status::Interrupted);
        assert!(log.latest_undoable().unwrap().is_none());
    }

    #[test]
    fn only_executed_rows_with_a_token_are_undoable_and_newest_wins() {
        let log = UndoLog::open_in_memory().unwrap();
        let a = log.begin("a", &[], &json!(null), 1).unwrap();
        log.finish(a, Status::Executed, Some("A"), "", 1).unwrap();
        let b = log.begin("b", &[], &json!(null), 2).unwrap();
        log.finish(b, Status::Executed, Some("B"), "", 2).unwrap();
        let c = log.begin("c", &[], &json!(null), 3).unwrap();
        log.finish(c, Status::Executed, None, "", 3).unwrap();
        let d = log.begin("d", &[], &json!(null), 4).unwrap();
        log.finish(d, Status::Failed, Some("D"), "", 4).unwrap();
        assert_eq!(log.latest_undoable().unwrap().unwrap().id, b);
        log.finish_undo(b, true, "restarted", 5).unwrap();
        assert_eq!(log.latest_undoable().unwrap().unwrap().id, a);
        assert_eq!(log.get(b).unwrap().unwrap().status, Status::Undone);
        let ids: Vec<i64> = log.recent(10).unwrap().iter().map(|e| e.id).collect();
        assert_eq!(ids, [d, c, b, a]);
    }
}
