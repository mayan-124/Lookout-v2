//! Recent searches: the typed text only, in the encrypted `meta.db`, so
//! "delete my data" removes them with it. Text that looks like a secret is
//! never stored.

use std::sync::Arc;

use rusqlite::params;

use crate::content::secrets;
use crate::securedb::{DbError, SecureDb};

pub const MAX_ENTRIES: usize = 50;
/// Longer text is pasted content, not a search.
const MAX_CHARS: usize = 200;

pub struct SearchHistory {
    db: Arc<SecureDb>,
}

/// Whether typed text may be kept.
fn storable(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= MAX_CHARS
        && !text.chars().any(char::is_control)
        // A note belongs to the notes, not to the recent searches.
        && !crate::notes::is_private(text)
        && secrets::mask(text).1 == 0
}

impl SearchHistory {
    pub fn new(db: Arc<SecureDb>) -> Result<Self, DbError> {
        db.read(|c| {
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS search_history (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    text TEXT NOT NULL UNIQUE COLLATE NOCASE
                )",
            )
        })?;
        Ok(Self { db })
    }

    /// Keeps `text` as the newest search, unless `remember` is off or it must
    /// not be stored. True if it was stored.
    pub fn record(&self, text: &str, remember: bool) -> Result<bool, DbError> {
        let text = text.trim();
        if !remember || !storable(text) {
            return Ok(false);
        }
        self.db.write(|c| {
            c.execute("DELETE FROM search_history WHERE text = ?1", params![text])?;
            c.execute(
                "INSERT INTO search_history (text) VALUES (?1)",
                params![text],
            )?;
            c.execute(
                "DELETE FROM search_history WHERE seq NOT IN
                    (SELECT seq FROM search_history ORDER BY seq DESC LIMIT ?1)",
                params![MAX_ENTRIES as i64],
            )?;
            Ok(true)
        })
    }

    /// Newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<String>, DbError> {
        self.db.read(|c| {
            c.prepare("SELECT text FROM search_history ORDER BY seq DESC LIMIT ?1")?
                .query_map(params![limit as i64], |r| r.get(0))?
                .collect()
        })
    }

    /// Forgets one search.
    pub fn remove(&self, text: &str) -> Result<(), DbError> {
        // Overwrite the freed pages: the file is rewritten from them.
        self.db.write(|c| {
            c.execute_batch("PRAGMA secure_delete = ON")?;
            c.execute("DELETE FROM search_history WHERE text = ?1", params![text])?;
            Ok(())
        })
    }

    pub fn clear(&self) -> Result<(), DbError> {
        // Overwrite the freed pages: the file is rewritten from them.
        self.db
            .write(|c| c.execute_batch("PRAGMA secure_delete = ON; DELETE FROM search_history"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Key;
    use crate::storage::{DataPaths, Storage};

    fn history() -> SearchHistory {
        SearchHistory::new(Arc::new(SecureDb::in_memory().unwrap())).unwrap()
    }

    #[test]
    fn newest_first_distinct_and_capped() {
        let h = history();
        for n in 0..60 {
            assert!(h.record(&format!("query {n}"), true).unwrap());
        }
        let all = h.recent(100).unwrap();
        assert_eq!(all.len(), MAX_ENTRIES);
        assert_eq!(all[0], "query 59");
        assert_eq!(all[MAX_ENTRIES - 1], "query 10");
        // Typed again: moves to the top, kept once.
        h.record("  QUERY 20 ", true).unwrap();
        let all = h.recent(100).unwrap();
        assert_eq!(all.len(), MAX_ENTRIES);
        assert_eq!(all[0], "QUERY 20");
        assert_eq!(all.iter().filter(|t| t.ends_with(" 20")).count(), 1);
        assert_eq!(h.recent(10).unwrap().len(), 10);
    }

    #[test]
    fn secrets_and_pasted_text_are_never_stored() {
        let h = history();
        // Assembled here so no real-looking secret sits in the source.
        let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        let card = format!("4111 1111 1111 {}", "1111");
        let assigned = format!("password={}", "hunter2hunter2");
        for bad in [
            &aws,
            &card,
            &assigned,
            "",
            "   ",
            "two\nlines",
            "Note: buy milk",
        ] {
            assert!(!h.record(bad, true).unwrap(), "{bad}");
        }
        assert!(!h.record(&"x".repeat(MAX_CHARS + 1), true).unwrap());
        assert!(h.recent(100).unwrap().is_empty());
        assert!(h.record("tax report 2025", true).unwrap());
    }

    #[test]
    fn one_entry_can_be_removed() {
        let h = history();
        h.record("alpha", true).unwrap();
        h.record("beta", true).unwrap();
        h.remove("alpha").unwrap();
        assert_eq!(h.recent(10).unwrap(), ["beta"]);
    }

    #[test]
    fn switched_off_it_stores_nothing() {
        let h = history();
        assert!(!h.record("report", false).unwrap());
        assert!(h.recent(10).unwrap().is_empty());
    }

    #[test]
    fn clear_and_delete_my_data_remove_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DataPaths::new(dir.path());
        let key = Key::generate().unwrap();
        let open = || {
            let storage = Storage::with_key(paths.clone(), key.clone()).unwrap();
            SearchHistory::new(storage.db).unwrap()
        };
        open().record("quarterly numbers", true).unwrap();
        // Sealed on disk, and still there after a restart.
        let raw = std::fs::read(paths.meta_db()).unwrap();
        assert!(!raw.windows(9).any(|w| w == b"quarterly"));
        assert_eq!(open().recent(10).unwrap(), ["quarterly numbers"]);
        open().clear().unwrap();
        assert!(open().recent(10).unwrap().is_empty());
        // "Delete my data" wipes the file it lives in.
        open().record("quarterly numbers", true).unwrap();
        paths.wipe().unwrap();
        assert!(open().recent(10).unwrap().is_empty());
    }
}
