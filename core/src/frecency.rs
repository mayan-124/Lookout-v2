//! Frecency: an exponentially decayed open count (14-day half-life), kept in
//! SQLite so it survives restarts and held in memory for fast reads.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::params;

use crate::securedb::{DbError, SecureDb};

const HALF_LIFE_SECS: f64 = 14.0 * 86_400.0;

#[derive(Debug, thiserror::Error)]
pub enum FrecencyError {
    #[error("frecency database error: {0}")]
    Db(#[from] DbError),
}

#[derive(Debug, Clone, Copy)]
struct Record {
    score: f64,
    last: i64,
}

pub struct FrecencyStore {
    db: Arc<SecureDb>,
    records: Mutex<HashMap<String, Record>>,
}

fn decayed(r: Record, now: i64) -> f64 {
    let age = (now - r.last).max(0) as f64;
    r.score * 0.5f64.powf(age / HALF_LIFE_SECS)
}

impl FrecencyStore {
    pub fn open_in_memory() -> Result<Self, FrecencyError> {
        Self::new(Arc::new(SecureDb::in_memory()?))
    }

    /// Uses the shared (encrypted) `meta.db`.
    pub fn new(db: Arc<SecureDb>) -> Result<Self, FrecencyError> {
        let records = db.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS frecency (
                    id TEXT PRIMARY KEY NOT NULL,
                    score REAL NOT NULL,
                    last_ts INTEGER NOT NULL
                )",
            )?;
            let mut records = HashMap::new();
            let mut stmt = conn.prepare("SELECT id, score, last_ts FROM frecency")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    Record {
                        score: row.get(1)?,
                        last: row.get(2)?,
                    },
                ))
            })?;
            for row in rows {
                let (id, rec) = row?;
                records.insert(id, rec);
            }
            Ok(records)
        })?;
        Ok(Self {
            db,
            records: Mutex::new(records),
        })
    }

    /// Records one open of `id` at unix time `now` and returns its new score.
    pub fn record_open(&self, id: &str, now: i64) -> Result<f64, FrecencyError> {
        let rec = {
            let mut map = self.records.lock().unwrap_or_else(PoisonError::into_inner);
            let prev = map.get(id).map_or(0.0, |r| decayed(*r, now));
            let rec = Record {
                score: prev + 1.0,
                last: now,
            };
            map.insert(id.to_owned(), rec);
            rec
        };
        self.db.write(|conn| {
            conn.execute(
                "INSERT INTO frecency (id, score, last_ts) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET score = excluded.score, last_ts = excluded.last_ts",
                params![id, rec.score, rec.last],
            )
        })?;
        Ok(rec.score)
    }

    /// Current decayed score of `id` (0.0 if never opened).
    pub fn score(&self, id: &str, now: i64) -> f64 {
        let map = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(id).map_or(0.0, |r| decayed(*r, now))
    }

    /// All ids with their current decayed scores.
    pub fn snapshot(&self, now: i64) -> Vec<(String, f64)> {
        let map = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        map.iter()
            .map(|(id, r)| (id.clone(), decayed(*r, now)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn repeated_opens_raise_the_score() {
        let s = FrecencyStore::open_in_memory().unwrap();
        assert_eq!(s.score("a", 0), 0.0);
        s.record_open("a", 0).unwrap();
        let one = s.score("a", 0);
        s.record_open("a", 0).unwrap();
        assert!(s.score("a", 0) > one);
    }

    #[test]
    fn score_halves_every_14_days() {
        let s = FrecencyStore::open_in_memory().unwrap();
        s.record_open("a", 0).unwrap();
        let later = s.score("a", 14 * DAY);
        assert!((later - 0.5).abs() < 1e-9, "{later}");
    }

    #[test]
    fn recent_use_beats_old_heavy_use() {
        let s = FrecencyStore::open_in_memory().unwrap();
        for _ in 0..5 {
            s.record_open("old", 0).unwrap();
        }
        s.record_open("new", 100 * DAY).unwrap();
        assert!(s.score("new", 100 * DAY) > s.score("old", 100 * DAY));
    }

    #[test]
    fn survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("meta.db");
        let key = crate::crypto::Key::generate().unwrap();
        let open =
            || FrecencyStore::new(Arc::new(SecureDb::open(&path, key.clone()).unwrap())).unwrap();
        {
            let s = open();
            s.record_open("file:/x", 1000).unwrap();
            s.record_open("file:/x", 1000).unwrap();
        }
        let s = open();
        assert!((s.score("file:/x", 1000) - 2.0).abs() < 1e-9);
        assert_eq!(s.snapshot(1000).len(), 1);
    }
}
