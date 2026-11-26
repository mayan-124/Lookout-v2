//! The file-name index survives restarts as an encrypted snapshot
//! (`names.idx`), so search works the moment Lookout starts instead of after
//! the first crawl. The snapshot is only trusted when it was made for the same
//! folders and exclusions as the current settings: otherwise it could hold
//! names from a folder the user has since removed or excluded.
//!
//! Anything that happened while Lookout was not running is repaired by the
//! startup crawl, which builds a fresh index and swaps it in
//! ([`crate::index_service::IndexConfig::preloaded`]).

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::crypto::{Key, OpenReader, SealWriter};
use crate::index_service::{read, SharedIndex};
use crate::name_index::NameIndex;
use crate::pause::PauseState;

const MIN_SAVE_INTERVAL: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(250);

/// What a snapshot is valid for: the roots and every exclusion.
pub fn fingerprint(roots: &[PathBuf], excludes_key: &str) -> String {
    let mut r: Vec<String> = roots
        .iter()
        .map(|p| p.to_string_lossy().to_lowercase())
        .collect();
    r.sort();
    format!("roots={}|{excludes_key}", r.join(";"))
}

/// A stable string for the exclusions in `settings`.
pub fn excludes_key(names: &[String], folders: &[PathBuf]) -> String {
    let mut n: Vec<String> = names.iter().map(|s| s.to_lowercase()).collect();
    n.sort();
    let mut f: Vec<String> = folders
        .iter()
        .map(|p| p.to_string_lossy().to_lowercase())
        .collect();
    f.sort();
    format!("names={}|folders={}", n.join(";"), f.join(";"))
}

pub fn save(index: &SharedIndex, path: &Path, key: &Key, fingerprint: &str) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let file = File::create(&tmp)?;
        let mut w = SealWriter::new(BufWriter::new(file), key)?;
        w.write_all(&u32::try_from(fingerprint.len()).unwrap_or(0).to_le_bytes())?;
        w.write_all(fingerprint.as_bytes())?;
        let counts = read(index).counts();
        let total = u32::try_from(counts.files + counts.folders).unwrap_or(u32::MAX);
        w.write_all(&total.to_le_bytes())?;
        // The read lock is held while exporting; queries are unaffected and the
        // watcher waits (an export of 1M names takes well under a second).
        read(index).export(&mut w)?;
        let file = w.finish()?.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Loads the snapshot if it exists, decrypts and was made for `fingerprint`.
/// Anything else (missing, unreadable, made for other folders) yields `None`.
pub fn load(path: &Path, key: &Key, fingerprint: &str, max_entries: usize) -> Option<NameIndex> {
    try_load(path, key, fingerprint, max_entries).ok().flatten()
}

/// A snapshot that is there but cannot be read back.
#[derive(Debug)]
pub struct Damaged;

/// [`load`], telling a damaged snapshot apart from one that is missing, too
/// large or made for other settings.
pub fn try_load(
    path: &Path,
    key: &Key,
    fingerprint: &str,
    max_entries: usize,
) -> Result<Option<NameIndex>, Damaged> {
    crate::guard::contain("the name index could not be read", || {
        read_snapshot(path, key, fingerprint, max_entries)
    })
    .unwrap_or(Err(Damaged))
}

fn read_snapshot(
    path: &Path,
    key: &Key,
    fingerprint: &str,
    max_entries: usize,
) -> Result<Option<NameIndex>, Damaged> {
    // Missing, or held by another program: not damage.
    let Ok(file) = File::open(path) else {
        return Ok(None);
    };
    let bad = |_: io::Error| Damaged;
    let mut r = OpenReader::new(BufReader::new(file), key).map_err(|_| Damaged)?;
    let mut len = [0u8; 4];
    r.read_exact(&mut len).map_err(bad)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 1 << 20 {
        return Err(Damaged);
    }
    let mut fp = vec![0u8; len];
    r.read_exact(&mut fp).map_err(bad)?;
    if fp != fingerprint.as_bytes() {
        return Ok(None);
    }
    let mut total = [0u8; 4];
    r.read_exact(&mut total).map_err(bad)?;
    if u32::from_le_bytes(total) as usize > max_entries {
        return Ok(None);
    }
    NameIndex::import(&mut r).map(Some).map_err(|_| Damaged)
}

/// Entry count (files plus folders) recorded in a snapshot, without loading it.
/// `None` if it is missing, unreadable or made for other settings.
pub fn snapshot_entries(path: &Path, key: &Key, fingerprint: &str) -> Option<usize> {
    let file = File::open(path).ok()?;
    let mut r = OpenReader::new(BufReader::new(file), key).ok()?;
    let mut len = [0u8; 4];
    r.read_exact(&mut len).ok()?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 1 << 20 {
        return None;
    }
    let mut fp = vec![0u8; len];
    r.read_exact(&mut fp).ok()?;
    if fp != fingerprint.as_bytes() {
        return None;
    }
    let mut total = [0u8; 4];
    r.read_exact(&mut total).ok()?;
    Some(u32::from_le_bytes(total) as usize)
}

/// Saves the snapshot in the background whenever the index has changed, and
/// once more when stopped normally.
pub struct Persister {
    stop: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    keep_final_save: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Persister {
    pub fn start(
        index: SharedIndex,
        path: PathBuf,
        key: Key,
        fingerprint: String,
        pause: PauseState,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let dirty = Arc::new(AtomicBool::new(false));
        let keep_final_save = Arc::new(AtomicBool::new(true));
        let (s, d, k) = (stop.clone(), dirty.clone(), keep_final_save.clone());
        let thread = std::thread::Builder::new()
            .name("lookout-names-save".into())
            .spawn(move || {
                let mut last = Instant::now().checked_sub(MIN_SAVE_INTERVAL);
                let save_now = |d: &AtomicBool| {
                    let saved = crate::guard::contain("saving the name index failed", || {
                        save(&index, &path, &key, &fingerprint)
                    });
                    if !matches!(saved, Some(Ok(()))) {
                        crate::log::error("could not save the name index");
                        d.store(true, Ordering::Release);
                    }
                };
                loop {
                    std::thread::sleep(POLL);
                    if s.load(Ordering::Acquire) {
                        break;
                    }
                    let due = last.is_none_or(|t| t.elapsed() >= MIN_SAVE_INTERVAL);
                    if due && !pause.is_paused() && d.swap(false, Ordering::AcqRel) {
                        save_now(&d);
                        last = Some(Instant::now());
                    }
                }
                if k.load(Ordering::Acquire) && d.swap(false, Ordering::AcqRel) {
                    save_now(&d);
                }
            })?;
        Ok(Self {
            stop,
            dirty,
            keep_final_save,
            thread: Some(thread),
        })
    }

    /// Tells the saver the index changed since the last snapshot.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    pub fn dirty_flag(&self) -> Arc<AtomicBool> {
        self.dirty.clone()
    }

    /// Stops the thread. With `save_first` the pending changes are written.
    pub fn stop(&mut self, save_first: bool) {
        self.keep_final_save.store(save_first, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Persister {
    fn drop(&mut self) {
        self.stop(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_service::{new_shared_index, write};

    fn sample() -> SharedIndex {
        let index = new_shared_index();
        {
            let mut w = write(&index);
            let root = w.add_root(Path::new("C:\\Docs"), 1).unwrap();
            w.add_file(root, "invoice april.txt", 2).unwrap();
        }
        index
    }

    #[test]
    fn snapshot_is_encrypted_and_only_loads_for_the_same_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("names.idx");
        let key = Key::generate().unwrap();
        let fp = fingerprint(&[PathBuf::from("C:\\Docs")], &excludes_key(&[], &[]));
        save(&sample(), &path, &key, &fp).unwrap();

        let raw = std::fs::read(&path).unwrap();
        assert!(
            !raw.windows(7).any(|w| w == b"invoice"),
            "file names are not on disk in the clear"
        );
        assert!(!raw.windows(4).any(|w| w == b"Docs"));

        let idx = load(&path, &key, &fp, usize::MAX).unwrap();
        assert_eq!(idx.counts().files, 1);
        assert!(idx.contains_file(Path::new("C:\\Docs\\invoice april.txt")));

        let other = fingerprint(
            &[PathBuf::from("C:\\Docs")],
            &excludes_key(&[], &[PathBuf::from("C:\\Docs\\Private")]),
        );
        assert!(
            load(&path, &key, &other, usize::MAX).is_none(),
            "a new exclusion invalidates the snapshot"
        );
        assert!(load(&path, &Key::generate().unwrap(), &fp, usize::MAX).is_none());
        std::fs::write(&path, b"garbage").unwrap();
        assert!(load(&path, &key, &fp, usize::MAX).is_none());
    }

    #[test]
    fn a_snapshot_above_the_size_limit_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("names.idx");
        let key = Key::generate().unwrap();
        save(&sample(), &path, &key, "fp").unwrap(); // 1 folder + 1 file = 2 entries
        assert_eq!(snapshot_entries(&path, &key, "fp"), Some(2));
        assert!(load(&path, &key, "fp", 2).is_some());
        assert!(load(&path, &key, "fp", 1).is_none());
        assert!(load(&path, &key, "fp", 0).is_none());
    }

    #[test]
    fn persister_saves_changes_and_saves_again_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("names.idx");
        let key = Key::generate().unwrap();
        let index = sample();
        let mut p = Persister::start(
            index.clone(),
            path.clone(),
            key.clone(),
            "fp".into(),
            PauseState::new(),
        )
        .unwrap();
        p.mark_dirty();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < deadline, "never saved");
            std::thread::sleep(Duration::from_millis(50));
        }
        {
            let mut w = write(&index);
            let root = w.dir_id_for_path(Path::new("C:\\Docs")).unwrap();
            w.add_file(root, "second.txt", 3).unwrap();
        }
        p.mark_dirty();
        p.stop(true);
        assert_eq!(
            load(&path, &key, "fp", usize::MAX).unwrap().counts().files,
            2
        );
    }

    #[test]
    fn stopping_without_saving_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("names.idx");
        let mut p = Persister::start(
            sample(),
            path.clone(),
            Key::generate().unwrap(),
            "fp".into(),
            PauseState::new(),
        )
        .unwrap();
        p.mark_dirty();
        p.stop(false);
        assert!(!path.exists());
    }
}
