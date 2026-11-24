//! `meta.db` encrypted at rest.
//!
//! SQLite runs entirely in memory. After every write the whole database is
//! serialized, sealed with the DPAPI-protected key (see [`crate::crypto`]) and
//! atomically swapped into `meta.db`, so the file on disk is never plaintext
//! and never half written. The database is small (frecency counts and the undo
//! log), which is what makes whole-file sealing affordable; a database that
//! grew to hundreds of megabytes would need a page-level scheme such as SQLCipher.
//!
//! A plaintext `meta.db` left by Phase 2 is read once and rewritten encrypted.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use rusqlite::{Connection, MAIN_DB};

use crate::crypto::{self, CryptoError, Key};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("encrypted database: {0}")]
    Crypto(#[from] CryptoError),
    #[error("database file I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

struct Persist {
    path: PathBuf,
    key: Key,
}

pub struct SecureDb {
    conn: Mutex<Connection>,
    persist: Option<Persist>,
    /// Set by "delete my index": nothing may be written to disk afterwards.
    detached: AtomicBool,
}

const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

impl SecureDb {
    /// A database that is never written to disk (tests, benchmarks).
    pub fn in_memory() -> Result<Self, DbError> {
        Ok(Self {
            conn: Mutex::new(Connection::open_in_memory()?),
            persist: None,
            detached: AtomicBool::new(false),
        })
    }

    /// Opens (or creates) the encrypted database at `path`.
    pub fn open(path: &Path, key: Key) -> Result<Self, DbError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut conn = Connection::open_in_memory()?;
        let mut migrate = false;
        match std::fs::read(path) {
            Ok(bytes) if crypto::looks_sealed(&bytes) => {
                let plain = crypto::open(&key, &bytes)?;
                load(&mut conn, &plain)?;
            }
            Ok(bytes) if bytes.starts_with(SQLITE_MAGIC) => {
                load(&mut conn, &bytes)?;
                migrate = true;
            }
            Ok(bytes) if bytes.is_empty() => {}
            Ok(_) => return Err(CryptoError::BadMagic.into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let db = Self {
            conn: Mutex::new(conn),
            persist: Some(Persist {
                path: path.to_owned(),
                key,
            }),
            detached: AtomicBool::new(false),
        };
        if migrate {
            db.flush()?;
        }
        Ok(db)
    }

    /// Runs `f` on the database without saving.
    pub fn read<R>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<R>,
    ) -> Result<R, DbError> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(f(&conn)?)
    }

    /// Runs `f`, then saves the encrypted database. If saving fails the error
    /// is returned, so a caller that must not proceed without a durable record
    /// (the undo log) can stop.
    pub fn write<R>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<R>,
    ) -> Result<R, DbError> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let out = f(&conn)?;
        self.save(&mut conn)?;
        Ok(out)
    }

    /// Like [`write`](Self::write) for setup statements that need no result.
    pub fn flush(&self) -> Result<(), DbError> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        self.save(&mut conn)
    }

    fn save(&self, conn: &mut Connection) -> Result<(), DbError> {
        let Some(p) = &self.persist else {
            return Ok(());
        };
        if self.detached.load(Ordering::Acquire) {
            return Ok(());
        }
        let data = conn.serialize(MAIN_DB)?;
        let sealed = crypto::seal(&p.key, &data)?;
        drop(data);
        crypto::write_atomic(&p.path, &sealed)?;
        Ok(())
    }

    /// Stops all writes to disk (used while the files are being deleted).
    pub fn detach(&self) {
        self.detached.store(true, Ordering::Release);
    }
}

fn load(conn: &mut Connection, mut bytes: &[u8]) -> Result<(), DbError> {
    let len = bytes.len();
    conn.deserialize_read_exact(MAIN_DB, &mut bytes, len, false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(path: &Path, key: &Key) -> SecureDb {
        let db = SecureDb::open(path, key.clone()).unwrap();
        db.write(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS t (v TEXT)"))
            .unwrap();
        db
    }

    #[test]
    fn file_on_disk_is_sealed_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let key = Key::generate().unwrap();
        {
            let db = make(&path, &key);
            db.write(|c| {
                c.execute(
                    "INSERT INTO t VALUES ('node server.js --token hunter2hunter2')",
                    [],
                )
            })
            .unwrap();
        }
        let raw = std::fs::read(&path).unwrap();
        assert!(crypto::looks_sealed(&raw));
        assert!(!raw.windows(5).any(|w| w == b"hunter"));
        assert!(!raw.windows(6).any(|w| w == b"server"));
        let db = SecureDb::open(&path, key).unwrap();
        let v: String = db
            .read(|c| c.query_row("SELECT v FROM t", [], |r| r.get(0)))
            .unwrap();
        assert!(v.contains("hunter2"));
    }

    #[test]
    fn another_key_cannot_open_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        drop(make(&path, &Key::generate().unwrap()));
        assert!(SecureDb::open(&path, Key::generate().unwrap()).is_err());
    }

    #[test]
    fn a_plaintext_database_from_phase_2_is_migrated_to_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('old row');")
                .unwrap();
        }
        assert!(std::fs::read(&path).unwrap().starts_with(SQLITE_MAGIC));
        let key = Key::generate().unwrap();
        let db = SecureDb::open(&path, key.clone()).unwrap();
        let v: String = db
            .read(|c| c.query_row("SELECT v FROM t", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(v, "old row");
        let raw = std::fs::read(&path).unwrap();
        assert!(crypto::looks_sealed(&raw), "rewritten encrypted at once");
        assert!(!raw.windows(7).any(|w| w == b"old row"));
    }

    #[test]
    fn detached_database_never_touches_the_disk_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let key = Key::generate().unwrap();
        let db = make(&path, &key);
        db.detach();
        std::fs::remove_file(&path).unwrap();
        db.write(|c| c.execute("INSERT INTO t VALUES ('x')", []))
            .unwrap();
        assert!(!path.exists());
    }
}
