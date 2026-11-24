//! Where Lookout keeps what it learns about the user's files, and how to
//! destroy all of it ("delete my index").
//!
//! ```text
//! lookout.key   the encryption key, wrapped by Windows DPAPI
//! meta.db       frecency and the undo log (sealed, see securedb)
//! names.idx     snapshot of the file-name index (sealed)
//! content/      the full-text index (tantivy files, ChaCha20-encrypted)
//! ```
//!
//! `settings.json` is not part of the index: it holds the user's choices
//! (folders, exclusions, hotkeys) and survives "delete my index".

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::crypto::{CryptoError, Key, KeyStore};
use crate::securedb::{DbError, SecureDb};

/// Set once this process has set data aside, so the same run never deletes it.
static SET_ASIDE_THIS_RUN: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct DataPaths {
    root: PathBuf,
}

impl DataPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    pub fn key_file(&self) -> PathBuf {
        self.root.join("lookout.key")
    }

    pub fn meta_db(&self) -> PathBuf {
        self.root.join("meta.db")
    }

    pub fn names_file(&self) -> PathBuf {
        self.root.join("names.idx")
    }

    /// A disk usage scan the user chose to save (sealed with the index key).
    pub fn disk_scan(&self) -> PathBuf {
        self.root.join("diskscan.bin")
    }

    pub fn content_dir(&self) -> PathBuf {
        self.root.join("content")
    }

    /// The one-time OCR re-read marker next to the content index folder.
    pub fn ocr_marker(&self) -> PathBuf {
        self.root.join("content.ocr-version")
    }

    /// Every file or folder that holds index data or the key.
    fn index_artifacts(&self) -> Vec<PathBuf> {
        let tmp = |p: PathBuf| {
            let mut s = p.into_os_string();
            s.push(".tmp");
            PathBuf::from(s)
        };
        vec![
            self.meta_db(),
            tmp(self.meta_db()),
            self.names_file(),
            tmp(self.names_file()),
            self.disk_scan(),
            tmp(self.disk_scan()),
            self.content_dir(),
            self.ocr_marker(),
            self.key_file(),
            tmp(self.key_file()),
        ]
    }

    fn unreadable_name(p: &Path) -> PathBuf {
        let mut s = p.as_os_str().to_owned();
        s.push(".unreadable");
        PathBuf::from(s)
    }

    fn live_artifacts(&self) -> [PathBuf; 4] {
        [
            self.meta_db(),
            self.names_file(),
            self.content_dir(),
            self.key_file(),
        ]
    }

    /// Where data that could not be unlocked is kept: `<name>.unreadable`.
    fn set_aside_artifacts(&self) -> Vec<PathBuf> {
        self.live_artifacts()
            .iter()
            .map(|p| Self::unreadable_name(p))
            .collect()
    }

    /// Set-aside data that still exists.
    pub fn set_aside_leftovers(&self) -> Vec<PathBuf> {
        self.set_aside_artifacts()
            .into_iter()
            .filter(|p| p.exists())
            .collect()
    }

    /// Renames `meta.db`, `names.idx`, `content/` and the key file to
    /// `*.unreadable` instead of deleting them (they may be recoverable, for
    /// example by signing in as the Windows user that made them).
    pub fn set_aside(&self) -> io::Result<()> {
        self.remove_all(&self.set_aside_artifacts())?;
        for p in self.live_artifacts() {
            if p.exists() {
                std::fs::rename(&p, Self::unreadable_name(&p))?;
            }
        }
        Ok(())
    }

    /// What of the index, the key and set-aside data still exists on disk.
    pub fn leftovers(&self) -> Vec<PathBuf> {
        self.index_artifacts()
            .into_iter()
            .chain(self.set_aside_artifacts())
            .filter(|p| p.exists())
            .collect()
    }

    fn remove_all(&self, paths: &[PathBuf]) -> io::Result<()> {
        let mut last_err = None;
        for p in paths {
            let r = if p.is_dir() {
                std::fs::remove_dir_all(p)
            } else {
                std::fs::remove_file(p)
            };
            match r {
                Err(e) if e.kind() != io::ErrorKind::NotFound => last_err = Some((p.clone(), e)),
                _ => {}
            }
        }
        match last_err {
            Some((p, e)) => Err(io::Error::new(
                e.kind(),
                format!("could not delete {}: {e}", p.display()),
            )),
            None => Ok(()),
        }
    }

    /// Deletes the index, `meta.db` and the key. Callers must have stopped
    /// everything that writes to them first. Fails if anything remains.
    pub fn wipe(&self) -> io::Result<()> {
        let mut all = self.index_artifacts();
        all.extend(self.set_aside_artifacts());
        self.remove_all(&all)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("storage I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// An open storage location: the key plus the encrypted `meta.db`.
#[derive(Clone)]
pub struct Storage {
    pub paths: Option<DataPaths>,
    pub key: Option<Key>,
    pub db: Arc<SecureDb>,
}

impl Storage {
    /// Nothing is written to disk (unit tests, benchmarks).
    pub fn in_memory() -> Result<Self, StorageError> {
        Ok(Self {
            paths: None,
            key: None,
            db: Arc::new(SecureDb::in_memory()?),
        })
    }

    /// Opens `paths` with an explicit key (tests use a random one).
    pub fn with_key(paths: DataPaths, key: Key) -> Result<Self, StorageError> {
        let db = Arc::new(SecureDb::open(&paths.meta_db(), key.clone())?);
        Ok(Self {
            paths: Some(paths),
            key: Some(key),
            db,
        })
    }

    /// Opens `paths` with the DPAPI-protected key, creating it on first use.
    ///
    /// If the key file exists but cannot be unwrapped (another Windows user or
    /// machine, a damaged file), or the database cannot be opened with a valid
    /// key, the old data is **set aside** (`*.unreadable`, see
    /// [`DataPaths::set_aside`]), never deleted silently, and a fresh key is
    /// made; the returned flag is then true so the caller can tell the user.
    /// Set-aside data from an earlier run is deleted by the next successful
    /// start (the user was told then). "Delete my index" deletes it too.
    pub fn open(paths: DataPaths) -> Result<(Self, bool), StorageError> {
        let keys = KeyStore::new(paths.key_file());
        let mut set_aside = false;
        let key = match keys.load_or_create() {
            Ok(k) => k,
            Err(CryptoError::Dpapi(_)) if keys.exists() => {
                paths.set_aside()?;
                set_aside = true;
                keys.load_or_create()?
            }
            Err(e) => return Err(e.into()),
        };
        let storage = match Self::with_key(paths.clone(), key) {
            Ok(s) => s,
            Err(StorageError::Db(DbError::Crypto(_))) => {
                paths.set_aside()?;
                set_aside = true;
                let key = KeyStore::new(paths.key_file()).load_or_create()?;
                Self::with_key(paths.clone(), key)?
            }
            Err(e) => return Err(e),
        };
        if set_aside {
            SET_ASIDE_THIS_RUN.store(true, Ordering::Release);
        } else if !SET_ASIDE_THIS_RUN.load(Ordering::Acquire) {
            // Opened fine and nothing was set aside during this run: whatever an
            // earlier run set aside was reported then, so it can go now.
            let _ = paths.remove_all(&paths.set_aside_artifacts());
        }
        Ok((storage, set_aside))
    }

    /// Stops all further writes to `meta.db` (used by "delete my index").
    pub fn detach(&self) {
        self.db.detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_key_and_encrypted_db_and_wipe_removes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DataPaths::new(dir.path().join("data"));
        let (s, reset) = Storage::open(paths.clone()).unwrap();
        assert!(!reset);
        s.db.write(|c| c.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('a')"))
            .unwrap();
        std::fs::create_dir_all(paths.content_dir()).unwrap();
        std::fs::write(paths.content_dir().join("seg"), b"x").unwrap();
        std::fs::write(paths.names_file(), b"y").unwrap();
        std::fs::write(paths.settings(), b"{}").unwrap();
        assert!(paths.key_file().exists());

        s.detach();
        paths.wipe().unwrap();
        assert!(paths.leftovers().is_empty(), "{:?}", paths.leftovers());
        assert!(
            paths.settings().exists(),
            "settings are the user's choices, not the index"
        );
    }

    // The "this run" flag is process-wide, so the tests that touch it run one
    // after the other under this lock.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn an_unreadable_key_sets_data_aside_instead_of_deleting_it() {
        let _g = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let paths = DataPaths::new(dir.path());
        std::fs::write(paths.key_file(), b"not a dpapi blob").unwrap();
        std::fs::write(paths.meta_db(), b"LKO1 sealed-by-an-old-key").unwrap();
        std::fs::write(paths.names_file(), b"names").unwrap();
        std::fs::create_dir_all(paths.content_dir()).unwrap();
        std::fs::write(paths.content_dir().join("seg"), b"segment").unwrap();

        let (_s, set_aside) = Storage::open(paths.clone()).unwrap();
        assert!(set_aside, "the caller must be told");
        let aside = |n: &str| dir.path().join(n);
        assert_eq!(
            std::fs::read(aside("meta.db.unreadable")).unwrap(),
            b"LKO1 sealed-by-an-old-key"
        );
        assert!(aside("names.idx.unreadable").exists());
        assert!(aside("content.unreadable").join("seg").exists());
        assert!(aside("lookout.key.unreadable").exists());
        // A usable key and an openable database exist now.
        assert!(paths.key_file().exists());

        // Opening again in the same run keeps the set-aside data (the user may not have seen the message yet).
        assert!(!Storage::open(paths.clone()).unwrap().1);
        assert_eq!(paths.set_aside_leftovers().len(), 4);
    }

    #[test]
    fn set_aside_data_is_deleted_by_the_next_start_and_by_delete_my_index() {
        let _g = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let paths = DataPaths::new(dir.path());
        std::fs::write(dir.path().join("meta.db.unreadable"), b"old").unwrap();
        std::fs::create_dir_all(dir.path().join("content.unreadable")).unwrap();
        // A next start is a new process; here the process-wide flag is cleared to simulate it.
        SET_ASIDE_THIS_RUN.store(false, Ordering::Release);
        let (_s, set_aside) = Storage::open(paths.clone()).unwrap();
        assert!(!set_aside);
        assert!(paths.set_aside_leftovers().is_empty());

        // Delete my index removes set-aside data as well.
        std::fs::write(dir.path().join("names.idx.unreadable"), b"old").unwrap();
        paths.wipe().unwrap();
        assert!(paths.leftovers().is_empty());
    }
}
