//! The stored clips: masked before they are kept, encrypted on disk, pruned by
//! count and age.

use std::collections::HashMap;
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
    /// Set for a copied picture. Its bytes are in a sealed file of their own,
    /// so they are never held in memory; `text` is only its label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// Size of the PNG.
    pub bytes: usize,
    /// Of the PNG, to notice the same picture being copied again.
    pub hash: u64,
}

/// Largest picture kept (PNG bytes). Larger ones are refused.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Most pictures kept; the oldest goes first.
pub const MAX_IMAGES: usize = 20;
/// Pictures expire after this long (sooner if text is kept for less).
pub const IMAGE_TTL_SECS: i64 = 24 * 60 * 60;

fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
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
    /// A picture over [`MAX_IMAGE_BYTES`].
    TooLarge,
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
    /// Picture bytes of a store with no file (tests).
    memory_images: HashMap<u64, Vec<u8>>,
}

impl Store {
    /// In memory only (tests).
    pub fn in_memory() -> Self {
        Self {
            clips: Vec::new(),
            next_id: 1,
            path: None,
            key: None,
            memory_images: HashMap::new(),
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
        let store = Self {
            next_id: disk
                .next_id
                .max(disk.clips.iter().map(|c| c.id + 1).max().unwrap_or(1)),
            clips: disk.clips,
            path: Some(path.to_owned()),
            key: Some(key),
            memory_images: HashMap::new(),
        };
        store.remove_orphans();
        store
    }

    fn image_dir(&self) -> Option<PathBuf> {
        Some(self.path.as_ref()?.parent()?.join("images"))
    }

    fn image_file(&self, id: u64) -> Option<PathBuf> {
        Some(self.image_dir()?.join(format!("{id}.bin")))
    }

    /// Deletes picture files that no clip names.
    fn remove_orphans(&self) {
        let Some(Ok(entries)) = self.image_dir().map(std::fs::read_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let named = self.clips.iter().any(|c| {
                c.image.is_some() && entry.file_name() == *format!("{}.bin", c.id).as_str()
            });
            if !named {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Deletes one picture's bytes.
    pub fn drop_image(&mut self, id: u64) {
        self.memory_images.remove(&id);
        if let Some(file) = self.image_file(id) {
            let _ = std::fs::remove_file(file);
        }
    }

    /// Deletes every picture (the setting is off). Returns true if any was listed.
    pub fn drop_images(&mut self) -> bool {
        let before = self.clips.len();
        self.clips.retain(|c| c.image.is_none());
        self.memory_images.clear();
        if let Some(dir) = self.image_dir() {
            let _ = std::fs::remove_dir_all(dir);
        }
        self.clips.len() != before
    }

    /// Keeps a PNG, sealed in its own file.
    pub fn add_image(
        &mut self,
        png: &[u8],
        (width, height): (u32, u32),
        source: Option<&str>,
        now: i64,
        limits: Limits,
    ) -> Result<Added, StoreError> {
        if png.len() > MAX_IMAGE_BYTES {
            return Ok(Added::TooLarge);
        }
        let info = ImageInfo {
            width,
            height,
            bytes: png.len(),
            hash: fingerprint(png),
        };
        if let Some(pos) = self
            .clips
            .iter()
            .position(|c| c.image.as_ref() == Some(&info))
        {
            let mut clip = self.clips.remove(pos);
            clip.ts = now;
            self.clips.push(clip);
            self.prune(now, limits);
            return Ok(Added::Moved);
        }
        let id = self.next_id;
        match (self.image_file(id), &self.key) {
            (Some(file), Some(key)) => {
                let write = |e: String| StoreError::Write(e);
                let sealed = crypto::seal(key, png).map_err(|e| write(e.to_string()))?;
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| write(e.to_string()))?;
                }
                crypto::write_atomic(&file, &sealed).map_err(|e| write(e.to_string()))?;
            }
            _ => {
                self.memory_images.insert(id, png.to_vec());
            }
        }
        self.clips.push(Clip {
            id,
            ts: now,
            text: format!("Image {width} × {height}"),
            masked: false,
            pinned: false,
            source: source.map(str::to_owned),
            image: Some(info),
        });
        self.next_id += 1;
        self.prune(now, limits);
        Ok(Added::Stored)
    }

    /// The PNG of a picture clip, read and decrypted now.
    pub fn image_png(&self, id: u64) -> Option<Vec<u8>> {
        self.get(id)?.image.as_ref()?;
        match (self.image_file(id), &self.key) {
            (Some(file), Some(key)) => crypto::open(key, &std::fs::read(file).ok()?).ok(),
            _ => self.memory_images.get(&id).cloned(),
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
        if let Some(pos) = self
            .clips
            .iter()
            .position(|c| c.image.is_none() && c.text == masked_text)
        {
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
            image: None,
        });
        self.next_id += 1;
        self.prune(now, limits);
        Added::Stored
    }

    /// Drops clips that are too old or beyond the most recent `max_clips`.
    /// Returns true if anything was removed.
    pub fn prune(&mut self, now: i64, limits: Limits) -> bool {
        let before = self.clips.len();
        let image_ttl = limits.retention_secs.min(IMAGE_TTL_SECS);
        let mut gone = Vec::new();
        self.clips.retain(|c| {
            let keep = if c.image.is_some() {
                now - c.ts < image_ttl
            } else {
                c.pinned || now - c.ts <= limits.retention_secs
            };
            if !keep && c.image.is_some() {
                gone.push(c.id);
            }
            keep
        });
        // Text and pictures have a count limit each. Oldest first, so the
        // oldest unpinned ones go.
        for (images, max) in [(false, limits.max_clips), (true, MAX_IMAGES)] {
            let counted = |c: &Clip| !c.pinned && c.image.is_some() == images;
            let mut extra = self
                .clips
                .iter()
                .filter(|c| counted(c))
                .count()
                .saturating_sub(max);
            self.clips.retain(|c| {
                if extra == 0 || !counted(c) {
                    true
                } else {
                    extra -= 1;
                    if images {
                        gone.push(c.id);
                    }
                    false
                }
            });
        }
        for id in gone {
            self.drop_image(id);
        }
        self.clips.len() != before
    }

    /// Deletes everything, in memory and on disk.
    pub fn clear(&mut self) -> Result<usize, StoreError> {
        let n = self.clips.len();
        self.clips.clear();
        self.memory_images.clear();
        if let Some(dir) = self.image_dir() {
            match std::fs::remove_dir_all(dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(StoreError::Write(e.to_string()))
                }
                _ => {}
            }
        }
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

    /// Takes one clip out; [`Store::restore`] puts it back.
    pub fn remove(&mut self, id: u64) -> Option<Clip> {
        let at = self.clips.iter().position(|c| c.id == id)?;
        Some(self.clips.remove(at))
    }

    /// Puts a removed clip back where its time places it. Nothing happens if the
    /// same text is in the history again.
    pub fn restore(&mut self, clip: Clip) {
        if self.clips.iter().any(|c| c.text == clip.text) {
            return;
        }
        let at = self.clips.partition_point(|c| c.ts <= clip.ts);
        self.clips.insert(at, clip);
    }

    /// Returns false if the clip is gone. Pictures cannot be pinned: they expire.
    pub fn set_pinned(&mut self, id: u64, pinned: bool) -> bool {
        self.clips
            .iter_mut()
            .find(|c| c.id == id && c.image.is_none())
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

    fn files(dir: &Path) -> usize {
        std::fs::read_dir(dir.join("images")).map_or(0, |d| d.count())
    }

    const DAY: Limits = Limits {
        max_clips: 3,
        retention_secs: 1_000_000,
    };

    #[test]
    fn pictures_are_limited_in_size_and_number_apart_from_text() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(&dir.path().join("clips.bin"), key());
        let big = vec![1u8; MAX_IMAGE_BYTES + 1];
        assert_eq!(
            s.add_image(&big, (1, 1), None, 1, DAY).unwrap(),
            Added::TooLarge
        );
        assert!(s.is_empty() && files(dir.path()) == 0);
        assert_eq!(
            s.add_image(&big[1..], (1, 1), None, 1, DAY).unwrap(),
            Added::Stored
        );
        s.add("text one", 1, DAY);
        for i in 0..MAX_IMAGES as u8 + 4 {
            s.add_image(&[2, i], (1, 1), None, 2, DAY).unwrap();
        }
        assert_eq!(s.recent().filter(|c| c.image.is_some()).count(), MAX_IMAGES);
        assert_eq!(files(dir.path()), MAX_IMAGES);
        assert!(s.recent().any(|c| c.text == "text one"));
        // The same picture again moves up instead of being kept twice.
        assert_eq!(
            s.add_image(&[2, 10], (1, 1), None, 3, DAY).unwrap(),
            Added::Moved
        );
        assert_eq!(files(dir.path()), MAX_IMAGES);
    }

    #[test]
    fn pictures_expire_after_a_day_even_if_text_is_kept_longer() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(&dir.path().join("clips.bin"), key());
        s.add("stays", 1, DAY);
        s.add_image(b"picture", (2, 2), None, 1, DAY).unwrap();
        let id = s.recent().next().unwrap().id;
        assert!(!s.set_pinned(id, true));
        assert!(!s.prune(IMAGE_TTL_SECS, DAY));
        assert!(s.prune(1 + IMAGE_TTL_SECS, DAY));
        assert_eq!(s.len(), 1);
        assert_eq!(files(dir.path()), 0);
        assert!(s.image_png(id).is_none());
    }

    #[test]
    fn pictures_are_sealed_on_disk_and_deleting_removes_their_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clips.bin");
        let mut s = Store::open(&path, key());
        let secret = b"\x89PNG readable picture bytes";
        s.add_image(secret, (2, 2), Some("paint"), 1, DAY).unwrap();
        s.add_image(b"another", (2, 2), None, 1, DAY).unwrap();
        s.save().unwrap();
        let id = s.listing().find(|c| c.source.is_some()).unwrap().id;
        let on_disk = std::fs::read(dir.path().join(format!("images/{id}.bin"))).unwrap();
        assert!(!String::from_utf8_lossy(&on_disk).contains("readable picture"));
        let mut s = Store::open(&path, key());
        assert_eq!(s.image_png(id).as_deref(), Some(secret.as_slice()));
        s.remove(id).unwrap();
        s.drop_image(id);
        assert_eq!(files(dir.path()), 1);
        // A file no clip names is swept away on the next start.
        std::fs::write(dir.path().join("images/99.bin"), "left over").unwrap();
        s.save().unwrap();
        let mut s = Store::open(&path, key());
        assert_eq!(files(dir.path()), 1);
        assert!(s.drop_images());
        assert!(s.is_empty() && !dir.path().join("images").exists());
        s.add_image(b"third", (2, 2), None, 1, DAY).unwrap();
        assert_eq!(s.clear().unwrap(), 1);
        assert!(!dir.path().join("images").exists());
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
