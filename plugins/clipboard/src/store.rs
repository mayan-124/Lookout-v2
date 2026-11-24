//! The stored clips: masked before they are kept, encrypted on disk, pruned by
//! count and age.

use std::path::{Path, PathBuf};

use lookout_core::content::secrets;
use lookout_core::crypto::{self, Key};
use serde::{Deserialize, Serialize};

/// Longest text kept (characters). Longer clips are ignored, not cut.
pub const MAX_CHARS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clip {
    pub id: u64,
    /// Unix seconds.
    pub ts: i64,
    /// The text as stored: if `masked` it is the masked text, never the original.
    pub text: String,
    pub masked: bool,
    /// Pinned clips never expire and are never pruned.
    #[serde(default)]
    pub pinned: bool,
    /// Program that copied it (best effort), e.g. `chrome`.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_clips: usize,
    pub retention_secs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    Stored,
    /// Same text as an existing clip: that one moved to the top.
    Moved,
    Ignored,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not write the clipboard history: {0}")]
    Write(String),
}

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    next_id: u64,
    clips: Vec<Clip>,
}

pub struct Store {
    clips: Vec<Clip>,
    next_id: u64,
    path: Option<PathBuf>,
    key: Option<Key>,
}

impl Store {
    /// In memory only (tests).
    pub fn in_memory() -> Self {
        Self {
            clips: Vec::new(),
            next_id: 1,
            path: None,
            key: None,
        }
    }

    /// Opens `path`, encrypted with `key`. A file that cannot be read (another
    /// key, damage) is set aside and the history starts empty.
    pub fn open(path: &Path, key: Key) -> Self {
        let disk = std::fs::read(path)
            .ok()
            .and_then(|bytes| match crypto::open(&key, &bytes) {
                Ok(plain) => serde_json::from_slice::<Disk>(&plain).ok(),
                Err(_) => None,
            });
        if disk.is_none() && path.exists() {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".unreadable");
            let _ = std::fs::rename(path, PathBuf::from(aside));
        }
        let disk = disk.unwrap_or_default();
        Self {
            next_id: disk
                .next_id
                .max(disk.clips.iter().map(|c| c.id + 1).max().unwrap_or(1)),
            clips: disk.clips,
            path: Some(path.to_owned()),
            key: Some(key),
        }
    }

    /// Masks `text` and keeps it. Whatever was masked is gone for good.
    pub fn add(&mut self, text: &str, now: i64, limits: Limits) -> Added {
        self.add_from(text, None, now, limits)
    }

    /// Like [`Store::add`], remembering which program the text came from.
    pub fn add_from(
        &mut self,
        text: &str,
        source: Option<&str>,
        now: i64,
        limits: Limits,
    ) -> Added {
        if text.trim().is_empty() || text.chars().count() > MAX_CHARS {
            return Added::Ignored;
        }
        let (masked_text, hits) = secrets::mask(text);
        let masked_text = masked_text.into_owned();
        if let Some(pos) = self.clips.iter().position(|c| c.text == masked_text) {
            let mut clip = self.clips.remove(pos);
            clip.ts = now;
            if clip.source.is_none() {
                clip.source = source.map(str::to_owned);
            }
            self.clips.push(clip);
            self.prune(now, limits);
            return Added::Moved;
        }
        self.clips.push(Clip {
            id: self.next_id,
            ts: now,
            text: masked_text,
            masked: hits > 0,
            pinned: false,
            source: source.map(str::to_owned),
        });
        self.next_id += 1;
        self.prune(now, limits);
        Added::Stored
    }

    /// Drops clips that are too old or beyond the most recent `max_clips`.
    /// Returns true if anything was removed.
    pub fn prune(&mut self, now: i64, limits: Limits) -> bool {
        let before = self.clips.len();
        self.clips
            .retain(|c| c.pinned || now - c.ts <= limits.retention_secs);
        let unpinned = self.clips.iter().filter(|c| !c.pinned).count();
        if unpinned > limits.max_clips {
            let mut extra = unpinned - limits.max_clips;
            // Oldest first, so the oldest unpinned clips go.
            self.clips.retain(|c| {
                if c.pinned || extra == 0 {
                    true
                } else {
                    extra -= 1;
                    false
                }
            });
        }
        self.clips.len() != before
    }

    /// Deletes everything, in memory and on disk.
    pub fn clear(&mut self) -> Result<usize, StoreError> {
        let n = self.clips.len();
        self.clips.clear();
        if let Some(path) = &self.path {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(StoreError::Write(e.to_string()))
                }
                _ => {}
            }
        }
        Ok(n)
    }

    /// Newest first.
    pub fn recent(&self) -> impl Iterator<Item = &Clip> {
        self.clips.iter().rev()
    }

    /// Pinned clips first, then the rest, each newest first.
    pub fn listing(&self) -> impl Iterator<Item = &Clip> {
        let pinned = self.clips.iter().rev().filter(|c| c.pinned);
        pinned.chain(self.clips.iter().rev().filter(|c| !c.pinned))
    }

    /// Returns false if the clip is gone.
    pub fn set_pinned(&mut self, id: u64, pinned: bool) -> bool {
        self.clips
            .iter_mut()
            .find(|c| c.id == id)
            .map(|c| c.pinned = pinned)
            .is_some()
    }

    pub fn get(&self, id: u64) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn len(&self) -> usize {
        self.clips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    pub fn save(&self) -> Result<(), StoreError> {
        let (Some(path), Some(key)) = (&self.path, &self.key) else {
            return Ok(());
        };
        let disk = serde_json::to_vec(&Disk {
            next_id: self.next_id,
            clips: self.clips.clone(),
        })
        .map_err(|e| StoreError::Write(e.to_string()))?;
        let sealed = crypto::seal(key, &disk).map_err(|e| StoreError::Write(e.to_string()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| StoreError::Write(e.to_string()))?;
        }
        crypto::write_atomic(path, &sealed).map_err(|e| StoreError::Write(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Limits = Limits {
        max_clips: 3,
        retention_secs: 100,
    };

    fn key() -> Key {
        Key::from_bytes(&[3u8; 32]).unwrap()
    }

    #[test]
    fn secrets_are_masked_and_only_the_masked_text_is_kept() {
        let mut s = Store::in_memory();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        assert_eq!(s.add(&format!("token {secret} end"), 10, L), Added::Stored);
        let clip = s.recent().next().unwrap();
        assert!(clip.masked);
        assert!(!clip.text.contains(secret));
        assert!(clip.text.contains("[masked]"));
        // A card number passing the Luhn check, and a plain text, for contrast.
        s.add("card 4111 1111 1111 1111", 11, L);
        assert!(s.recent().next().unwrap().masked);
        s.add("hello world", 12, L);
        assert!(!s.recent().next().unwrap().masked);
    }

    #[test]
    fn nothing_readable_reaches_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clips.bin");
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let mut s = Store::open(&path, key());
        s.add(&format!("key {secret}"), 1, L);
        s.add("plain readable sentence", 2, L);
        s.save().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let hay = String::from_utf8_lossy(&bytes);
        assert!(!hay.contains(secret));
        assert!(
            !hay.contains("plain readable"),
            "history is stored in the clear"
        );
        // It reads back with the key, and not with another.
        let again = Store::open(&path, key());
        assert_eq!(again.len(), 2);
        let other = Store::open(&path, Key::from_bytes(&[4u8; 32]).unwrap());
        assert!(other.is_empty());
        assert!(dir.path().join("clips.bin.unreadable").exists());
    }

    #[test]
    fn count_age_and_duplicates_are_enforced() {
        let mut s = Store::in_memory();
        for (i, t) in ["a", "b", "c", "d"].iter().enumerate() {
            s.add(t, 10 + i as i64, L);
        }
        let texts: Vec<_> = s.recent().map(|c| c.text.clone()).collect();
        assert_eq!(texts, ["d", "c", "b"]);
        assert_eq!(s.add("b", 20, L), Added::Moved);
        assert_eq!(s.recent().next().unwrap().text, "b");
        assert_eq!(s.len(), 3);
        // Older than the retention time: gone.
        assert!(s.prune(200, L));
        assert!(s.is_empty());
        assert_eq!(s.add("   ", 1, L), Added::Ignored);
        assert_eq!(s.add(&"x".repeat(MAX_CHARS + 1), 1, L), Added::Ignored);
    }

    #[test]
    fn pinned_clips_survive_age_and_count_and_come_first() {
        let mut s = Store::in_memory();
        s.add("keep me", 1, L);
        let id = s.recent().next().unwrap().id;
        assert!(s.set_pinned(id, true));
        for (i, t) in ["a", "b", "c", "d"].iter().enumerate() {
            s.add(t, 10 + i as i64, L);
        }
        // Three unpinned fit the limit; the pinned one does not count.
        assert_eq!(s.len(), 4);
        s.prune(10_000, L);
        let texts: Vec<_> = s.listing().map(|c| c.text.clone()).collect();
        assert_eq!(texts, ["keep me"]);
        assert!(s.set_pinned(id, false));
        s.prune(10_000, L);
        assert!(s.is_empty());
        assert!(!s.set_pinned(999, true));
    }

    #[test]
    fn the_source_program_is_kept_and_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clips.bin");
        let mut s = Store::open(&path, key());
        s.add_from("hello", Some("chrome"), 1, L);
        s.add_from("hello", Some("other"), 2, L);
        s.save().unwrap();
        let again = Store::open(&path, key());
        assert_eq!(
            again.recent().next().unwrap().source.as_deref(),
            Some("chrome")
        );
    }

    #[test]
    fn clearing_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clips.bin");
        let mut s = Store::open(&path, key());
        s.add("one", 1, L);
        s.save().unwrap();
        assert!(path.exists());
        assert_eq!(s.clear().unwrap(), 1);
        assert!(!path.exists() && s.is_empty());
    }
}
