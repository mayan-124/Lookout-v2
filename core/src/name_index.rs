//! In-memory file name index.
//!
//! All names are stored lowercase in one contiguous arena, each wrapped in
//! `\0` separators (`\0name\0name\0`). A query searches the arena with a
//! SIMD substring finder instead of walking a trie or FST, because users
//! expect matches in the middle of a name and prefix structures cannot give
//! that. Original-case names are stored only when they differ from the
//! lowercase form. Entries are append-only; removals set a tombstone flag and
//! [`NameIndex::compact`] reclaims the space.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};

use memchr::memmem;

use crate::item::{Kind, ScoreFeatures};
use crate::ranking::{self, RankingWeights};

const NO_ORIG: u32 = u32::MAX;
const NO_PARENT: u32 = u32::MAX;
const FLAG_DIR: u8 = 1;
const FLAG_DEAD: u8 = 2;
/// Entries per scan segment; cancellation is checked between segments.
const SEGMENT: usize = 1 << 16;
const DEFAULT_PARALLEL_MIN: usize = 200_000;
const MAX_THREADS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("name index is full")]
    Full,
    #[error("name is empty, too long or contains a NUL byte")]
    InvalidName,
    #[error("unknown directory id {0}")]
    UnknownDir(u32),
}

/// 20 bytes per name. For folder entries `dir` is the folder's own id.
#[derive(Debug, Clone, Copy)]
struct Entry {
    lower_off: u32,
    orig_off: u32,
    dir: u32,
    modified: u32,
    orig_len: u16,
    flags: u8,
    frec: u8,
}

#[derive(Debug, Clone, Copy)]
struct DirRec {
    parent: u32,
    name_off: u32,
    name_len: u16,
    /// Lowercase display name in `dir_lower`.
    lower_off: u32,
    lower_len: u16,
    dead: bool,
}

pub struct SearchParams<'a> {
    /// Lowercase tokens; every one must occur in the name.
    pub tokens: &'a [Vec<u8>],
    pub kind: Option<&'a Kind>,
    pub limit: usize,
    /// Seconds since the Unix epoch, injected so tests are deterministic.
    pub now: u32,
    pub weights: RankingWeights,
    /// Only entries modified in `[from, to)` (unix seconds) match.
    pub time: Option<(u32, u32)>,
    /// When not empty, only files with one of these (lowercase) extensions match.
    pub exts: &'a [&'a str],
}

#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub entry: u32,
    pub score: f32,
    pub features: ScoreFeatures,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub modified: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NameStats {
    pub count: usize,
    pub min: usize,
    pub p50: usize,
    pub p95: usize,
    pub max: usize,
    pub mean: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexCounts {
    pub files: usize,
    pub folders: usize,
}

pub struct NameIndex {
    lower: Vec<u8>,
    orig: Vec<u8>,
    entries: Vec<Entry>,
    dirs: Vec<DirRec>,
    dir_names: Vec<u8>,
    /// Lowercase folder names, `\0` separated, for parent-folder matches.
    dir_lower: Vec<u8>,
    /// Lowercase full path of every live directory to its id.
    dir_lookup: HashMap<String, u32>,
    files_live: usize,
    folders_live: usize,
    dead_entries: usize,
    parallel_min: usize,
    max_threads: usize,
}

impl Default for NameIndex {
    fn default() -> Self {
        Self::new()
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Lowercase form of a name for matching, with accents removed (see
/// [`ranking::fold_lower`]): "istanbul" finds "İstanbul", "cafe" finds "café".
/// Whether a lowercase name ends in `.ext` for one of `exts`.
fn has_ext(name: &[u8], exts: &[&str]) -> bool {
    name.iter()
        .rposition(|&b| b == b'.')
        .is_some_and(|dot| exts.iter().any(|e| e.as_bytes() == &name[dot + 1..]))
}

pub(crate) fn lower_name(name: &str) -> String {
    ranking::fold_lower(name)
}

/// Index of the last entry in `lo..hi` whose offset is `<= abs`. Requires
/// `entries[lo].lower_off <= abs`. Gallops, so cost is logarithmic in the distance moved.
fn advance(entries: &[Entry], lo: usize, hi: usize, abs: u32) -> usize {
    let mut base = lo;
    let mut step = 1usize;
    while base + step < hi && entries[base + step].lower_off <= abs {
        base += step;
        step *= 2;
    }
    let mut l = base;
    let mut r = (base + step).min(hi);
    while r - l > 1 {
        let m = l + (r - l) / 2;
        if entries[m].lower_off <= abs {
            l = m;
        } else {
            r = m;
        }
    }
    l
}

impl NameIndex {
    pub fn new() -> Self {
        Self {
            // Leading separator so that `\0name\0` matches the first name too.
            lower: vec![0],
            orig: Vec::new(),
            entries: Vec::new(),
            dirs: Vec::new(),
            dir_names: Vec::new(),
            dir_lower: Vec::new(),
            dir_lookup: HashMap::new(),
            files_live: 0,
            folders_live: 0,
            dead_entries: 0,
            parallel_min: DEFAULT_PARALLEL_MIN,
            max_threads: MAX_THREADS,
        }
    }

    /// Entry count at which scans switch to multiple threads (tests lower it).
    pub fn set_parallel_threshold(&mut self, entries: usize) {
        self.parallel_min = entries;
    }

    /// Most threads one scan may use (default 8; the benchmark lowers it).
    pub fn set_max_threads(&mut self, threads: usize) {
        self.max_threads = threads.max(1);
    }

    /// Length (in characters) distribution of live file names.
    pub fn file_name_stats(&self) -> Option<NameStats> {
        let mut lens: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.entries[i].flags & (FLAG_DIR | FLAG_DEAD) == 0)
            .map(|i| match self.entry_info(i as u32) {
                Some(info) => info.name.chars().count(),
                None => 0,
            })
            .collect();
        if lens.is_empty() {
            return None;
        }
        lens.sort_unstable();
        let at = |p: f64| lens[((lens.len() as f64 * p) as usize).min(lens.len() - 1)];
        Some(NameStats {
            count: lens.len(),
            min: lens[0],
            p50: at(0.5),
            p95: at(0.95),
            max: lens[lens.len() - 1],
            mean: lens.iter().sum::<usize>() as f64 / lens.len() as f64,
        })
    }

    pub fn counts(&self) -> IndexCounts {
        IndexCounts {
            files: self.files_live,
            folders: self.folders_live,
        }
    }

    pub fn dead_ratio(&self) -> f32 {
        if self.entries.is_empty() {
            0.0
        } else {
            self.dead_entries as f32 / self.entries.len() as f32
        }
    }

    /// Bytes held by the index buffers (capacity, not just length).
    pub fn heap_bytes(&self) -> usize {
        self.lower.capacity()
            + self.orig.capacity()
            + self.entries.capacity() * std::mem::size_of::<Entry>()
            + self.dirs.capacity() * std::mem::size_of::<DirRec>()
            + self.dir_names.capacity()
            + self.dir_lower.capacity()
            + self.dir_lookup.capacity() * 48
            + self.dir_lookup.keys().map(String::len).sum::<usize>()
    }

    pub fn shrink_to_fit(&mut self) {
        self.lower.shrink_to_fit();
        self.orig.shrink_to_fit();
        self.entries.shrink_to_fit();
        self.dirs.shrink_to_fit();
        self.dir_names.shrink_to_fit();
        self.dir_lower.shrink_to_fit();
        self.dir_lookup.shrink_to_fit();
    }

    // ---------------------------------------------------------------- inserts

    fn push_entry(
        &mut self,
        dir: u32,
        name: &str,
        flags: u8,
        modified: u32,
    ) -> Result<(), IndexError> {
        if name.is_empty() || name.len() > usize::from(u16::MAX) || name.contains('\0') {
            return Err(IndexError::InvalidName);
        }
        let lower = lower_name(name);
        let lower_off = u32::try_from(self.lower.len()).map_err(|_| IndexError::Full)?;
        if self.lower.len() + lower.len() + 1 > u32::MAX as usize
            || self.orig.len() + name.len() > u32::MAX as usize - 1
        {
            return Err(IndexError::Full);
        }
        let (orig_off, orig_len) = if lower == name {
            (NO_ORIG, 0)
        } else {
            let off = u32::try_from(self.orig.len()).map_err(|_| IndexError::Full)?;
            self.orig.extend_from_slice(name.as_bytes());
            (
                off,
                u16::try_from(name.len()).map_err(|_| IndexError::InvalidName)?,
            )
        };
        self.lower.extend_from_slice(lower.as_bytes());
        self.lower.push(0);
        self.entries.push(Entry {
            lower_off,
            orig_off,
            dir,
            modified,
            orig_len,
            flags,
            frec: 0,
        });
        if flags & FLAG_DIR != 0 {
            self.folders_live += 1;
        } else {
            self.files_live += 1;
        }
        Ok(())
    }

    /// `display` is the folder's own name (for a crawl root, its last component).
    fn push_dir_record(
        &mut self,
        parent: u32,
        name: &str,
        display: &str,
    ) -> Result<u32, IndexError> {
        let id = u32::try_from(self.dirs.len()).map_err(|_| IndexError::Full)?;
        let name_off = u32::try_from(self.dir_names.len()).map_err(|_| IndexError::Full)?;
        let name_len = u16::try_from(name.len()).map_err(|_| IndexError::InvalidName)?;
        let lower = lower_name(display);
        let lower_off = u32::try_from(self.dir_lower.len()).map_err(|_| IndexError::Full)?;
        let lower_len = u16::try_from(lower.len()).map_err(|_| IndexError::InvalidName)?;
        self.dir_names.extend_from_slice(name.as_bytes());
        self.dir_lower.extend_from_slice(lower.as_bytes());
        self.dir_lower.push(0);
        self.dirs.push(DirRec {
            parent,
            name_off,
            name_len,
            lower_off,
            lower_len,
            dead: false,
        });
        Ok(id)
    }

    /// Adds a crawl root (a directory with no parent in the index).
    pub fn add_root(&mut self, path: &Path, modified: u32) -> Result<u32, IndexError> {
        let full = path.to_string_lossy().into_owned();
        let display = path
            .file_name()
            .map_or_else(|| full.clone(), |n| n.to_string_lossy().into_owned());
        let id = self.push_dir_record(NO_PARENT, &full, &display)?;
        self.push_entry(id, &display, FLAG_DIR, modified)?;
        self.dir_lookup.insert(path_key(path), id);
        Ok(id)
    }

    /// Adds a directory under `parent` and makes it searchable.
    pub fn add_dir(&mut self, parent: u32, name: &str, modified: u32) -> Result<u32, IndexError> {
        let parent_path = self
            .dir_path(parent)
            .ok_or(IndexError::UnknownDir(parent))?;
        let id = self.push_dir_record(parent, name, name)?;
        self.push_entry(id, name, FLAG_DIR, modified)?;
        self.dir_lookup
            .insert(path_key(&parent_path.join(name)), id);
        Ok(id)
    }

    pub fn add_file(&mut self, dir: u32, name: &str, modified: u32) -> Result<(), IndexError> {
        if self.dirs.get(dir as usize).is_none_or(|d| d.dead) {
            return Err(IndexError::UnknownDir(dir));
        }
        self.push_entry(dir, name, 0, modified)
    }

    // ---------------------------------------------------------------- lookups

    fn dir_name(&self, d: &DirRec) -> String {
        let start = d.name_off as usize;
        let end = start + usize::from(d.name_len);
        self.dir_names
            .get(start..end)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    }

    /// Full path of a directory id.
    pub fn dir_path(&self, dir: u32) -> Option<PathBuf> {
        let mut names = Vec::new();
        let mut cur = dir;
        while cur != NO_PARENT {
            let rec = self.dirs.get(cur as usize)?;
            names.push(self.dir_name(rec));
            cur = rec.parent;
            if names.len() > 4096 {
                return None;
            }
        }
        let mut it = names.into_iter().rev();
        let mut path = PathBuf::from(it.next()?);
        for n in it {
            path.push(n);
        }
        Some(path)
    }

    pub fn dir_id_for_path(&self, path: &Path) -> Option<u32> {
        let id = *self.dir_lookup.get(&path_key(path))?;
        self.dirs.get(id as usize).filter(|d| !d.dead).map(|_| id)
    }

    fn lower_len(&self, i: usize) -> usize {
        let start = self.entries[i].lower_off as usize;
        let end = self
            .entries
            .get(i + 1)
            .map_or(self.lower.len(), |n| n.lower_off as usize);
        end - start - 1
    }

    fn lower_name(&self, i: usize) -> &[u8] {
        let start = self.entries[i].lower_off as usize;
        &self.lower[start..start + self.lower_len(i)]
    }

    pub fn entry_info(&self, entry: u32) -> Option<EntryInfo> {
        let i = entry as usize;
        let e = *self.entries.get(i)?;
        if e.flags & FLAG_DEAD != 0 {
            return None;
        }
        let name = if e.orig_off == NO_ORIG {
            String::from_utf8_lossy(self.lower_name(i)).into_owned()
        } else {
            let s = e.orig_off as usize;
            String::from_utf8_lossy(self.orig.get(s..s + usize::from(e.orig_len))?).into_owned()
        };
        let is_dir = e.flags & FLAG_DIR != 0;
        let dir_path = self.dir_path(e.dir)?;
        let path = if is_dir {
            dir_path
        } else {
            dir_path.join(&name)
        };
        Some(EntryInfo {
            name,
            path,
            is_dir,
            modified: e.modified,
        })
    }

    /// Live files of `dir` named `name`, ignoring case only: "cafe.txt" and
    /// "café.txt" are different files although they are searched alike.
    fn find_files_exact(&self, dir: u32, name: &str) -> Vec<usize> {
        let name_lower = lower_name(name);
        let plain = |s: &str| s.to_lowercase();
        let mut needle = Vec::with_capacity(name_lower.len() + 2);
        needle.push(0);
        needle.extend_from_slice(name_lower.as_bytes());
        needle.push(0);
        let mut found = Vec::new();
        // Not `find_iter`: equal names next to each other share the zero between them.
        let mut from = 0;
        while let Some(rel) = memmem::find(&self.lower[from..], &needle) {
            let pos = from + rel;
            from = pos + 1;
            let Ok(off) = u32::try_from(pos + 1) else {
                continue;
            };
            if let Ok(i) = self.entries.binary_search_by_key(&off, |e| e.lower_off) {
                let e = &self.entries[i];
                if e.dir == dir
                    && e.flags & (FLAG_DIR | FLAG_DEAD) == 0
                    && plain(&self.entry_name(i)) == plain(name)
                {
                    found.push(i);
                }
            }
        }
        found
    }

    pub fn contains_file(&self, path: &Path) -> bool {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return false;
        };
        let Some(dir) = self.dir_id_for_path(parent) else {
            return false;
        };
        !self
            .find_files_exact(dir, &name.to_string_lossy())
            .is_empty()
    }

    // --------------------------------------------------------------- removals

    fn kill(&mut self, i: usize) {
        let e = &mut self.entries[i];
        if e.flags & FLAG_DEAD != 0 {
            return;
        }
        e.flags |= FLAG_DEAD;
        self.dead_entries += 1;
        if e.flags & FLAG_DIR != 0 {
            self.folders_live -= 1;
        } else {
            self.files_live -= 1;
        }
    }

    /// Removes a file or a directory (with everything below it). Returns
    /// whether anything was removed.
    pub fn remove_path(&mut self, path: &Path) -> bool {
        if let Some(id) = self.dir_id_for_path(path) {
            self.remove_dir_tree(id);
            return true;
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return false;
        };
        let Some(dir) = self.dir_id_for_path(parent) else {
            return false;
        };
        let found = self.find_files_exact(dir, &name.to_string_lossy());
        for &i in &found {
            self.kill(i);
        }
        !found.is_empty()
    }

    fn remove_dir_tree(&mut self, root: u32) {
        let root = root as usize;
        if let Some(d) = self.dirs.get_mut(root) {
            d.dead = true;
        }
        // A child is always created after its parent, so one forward pass finds every descendant.
        for i in root + 1..self.dirs.len() {
            let parent = self.dirs[i].parent;
            if parent != NO_PARENT && self.dirs[parent as usize].dead {
                self.dirs[i].dead = true;
            }
        }
        for i in 0..self.entries.len() {
            let dir = self.entries[i].dir as usize;
            if self.dirs[dir].dead {
                self.kill(i);
            }
        }
    }

    /// Rebuilds the arenas without tombstoned entries.
    pub fn compact(&mut self) {
        if self.dead_entries == 0 {
            return;
        }
        let mut lower = Vec::with_capacity(self.lower.len());
        lower.push(0);
        let mut orig = Vec::with_capacity(self.orig.len());
        let mut entries = Vec::with_capacity(self.entries.len() - self.dead_entries);
        for i in 0..self.entries.len() {
            let mut e = self.entries[i];
            if e.flags & FLAG_DEAD != 0 {
                continue;
            }
            let name = self.lower_name(i);
            // Offsets stay below u32::MAX because the old arenas already did.
            let lower_off = lower.len() as u32;
            lower.extend_from_slice(name);
            lower.push(0);
            if e.orig_off != NO_ORIG {
                let s = e.orig_off as usize;
                let o = orig.len() as u32;
                orig.extend_from_slice(&self.orig[s..s + usize::from(e.orig_len)]);
                e.orig_off = o;
            }
            e.lower_off = lower_off;
            entries.push(e);
        }
        self.lower = lower;
        self.orig = orig;
        self.entries = entries;
        self.dead_entries = 0;
    }

    /// Sets the frecency level of the file or folder at `path`.
    pub fn set_frecency(&mut self, path: &Path, level: u8) -> bool {
        if let Some(id) = self.dir_id_for_path(path) {
            // The folder entry carries the directory's own id.
            if let Some(e) = self
                .entries
                .iter_mut()
                .find(|e| e.dir == id && e.flags & (FLAG_DIR | FLAG_DEAD) == FLAG_DIR)
            {
                e.frec = level;
                return true;
            }
            return false;
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return false;
        };
        let Some(dir) = self.dir_id_for_path(parent) else {
            return false;
        };
        let found = self.find_files_exact(dir, &name.to_string_lossy());
        for &i in &found {
            self.entries[i].frec = level;
        }
        !found.is_empty()
    }

    // ----------------------------------------------------------------- search

    /// Whether a live entry passes the kind and time filters.
    fn admitted(&self, i: usize, e: &Entry, p: &SearchParams<'_>) -> bool {
        if e.flags & FLAG_DEAD != 0 {
            return false;
        }
        let is_dir = e.flags & FLAG_DIR != 0;
        if !p.exts.is_empty() && (is_dir || !has_ext(self.lower_name(i), p.exts)) {
            return false;
        }
        match p.kind {
            Some(Kind::File) if is_dir => return false,
            Some(Kind::Folder) if !is_dir => return false,
            Some(Kind::File | Kind::Folder) | None => {}
            Some(_) => return false,
        }
        if let Some((from, to)) = p.time {
            if e.modified < from || e.modified >= to {
                return false;
            }
        }
        true
    }

    fn features(e: &Entry, p: &SearchParams<'_>, m: f32) -> (ScoreFeatures, f32) {
        let features = ScoreFeatures {
            match_quality: m,
            frecency: ranking::frecency_from_level(e.frec),
            recency: ranking::recency(e.modified, p.now),
            semantic: None,
        };
        (features, p.weights.score(&features))
    }

    /// The word starts of an entry's name that come from capital letters. The
    /// original-case name is kept exactly for names that have capitals, so the
    /// starts are read from it instead of being stored a second time.
    fn camel_bits(&self, e: &Entry) -> u128 {
        if e.orig_off == NO_ORIG {
            return 0;
        }
        let s = e.orig_off as usize;
        self.orig
            .get(s..s + usize::from(e.orig_len))
            .map_or(0, |b| ranking::camel_starts(&String::from_utf8_lossy(b)))
    }

    /// Lowercase name of the folder that directly contains entry `e`.
    fn parent_lower(&self, e: &Entry) -> Option<&[u8]> {
        let parent = if e.flags & FLAG_DIR != 0 {
            self.dirs.get(e.dir as usize)?.parent
        } else {
            e.dir
        };
        let d = self
            .dirs
            .get(parent as usize)
            .filter(|_| parent != NO_PARENT)?;
        let start = d.lower_off as usize;
        self.dir_lower.get(start..start + usize::from(d.lower_len))
    }

    /// Quality of a name where each token is in the file name or else in the
    /// parent folder's name, at least one in each. Ranks above a file-name-only
    /// match by [`COMBINED_BONUS`].
    fn folder_quality(
        &self,
        e: &Entry,
        name: &[u8],
        tokens: &[&[u8]],
        camel: &dyn Fn() -> u128,
    ) -> Option<f32> {
        let parent = self.parent_lower(e)?;
        let (mut in_name, mut in_dir) = (Vec::new(), Vec::new());
        for &t in tokens {
            if memmem::find(name, t).is_some() {
                in_name.push(t);
            } else if memmem::find(parent, t).is_some() {
                in_dir.push(t);
            } else {
                return None;
            }
        }
        if in_name.is_empty() || in_dir.is_empty() {
            return None;
        }
        let by_name = ranking::match_quality_camel(name, &in_name, camel)?;
        let by_dir = ranking::match_quality(parent, &in_dir)?;
        let n = tokens.len() as f32;
        let mixed = (in_name.len() as f32 * by_name + in_dir.len() as f32 * by_dir) / n;
        Some((mixed + COMBINED_BONUS).min(1.0))
    }

    /// Scores one entry against the query, or `None` if it does not match.
    /// `folders` allows tokens missing from the name to match the parent folder.
    fn evaluate(
        &self,
        i: usize,
        p: &SearchParams<'_>,
        tokens: &[&[u8]],
        folders: bool,
    ) -> Option<(ScoreFeatures, f32)> {
        let e = &self.entries[i];
        if !self.admitted(i, e, p) {
            return None;
        }
        // With no words (a time-only query) every entry in the window matches equally
        // and recency decides the order.
        let m = if tokens.is_empty() {
            0.5
        } else {
            let name = self.lower_name(i);
            let camel = || self.camel_bits(e);
            match ranking::match_quality_tiered(name, tokens, &camel) {
                Some(m) => m,
                None if folders => self.folder_quality(e, name, tokens, &camel)?,
                None => return None,
            }
        };
        Some(Self::features(e, p, m))
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_segment(
        &self,
        seg_lo: usize,
        seg_hi: usize,
        finder: &memmem::Finder<'_>,
        p: &SearchParams<'_>,
        tokens: &[&[u8]],
        folders: bool,
        heap: &mut BinaryHeap<Reverse<(i64, u32)>>,
    ) {
        let start = self.entries[seg_lo].lower_off as usize;
        let end = self
            .entries
            .get(seg_hi)
            .map_or(self.lower.len(), |e| e.lower_off as usize);
        let hay = &self.lower[start..end];
        let mut pos = 0usize;
        let mut idx = seg_lo;
        while let Some(found) = finder.find(&hay[pos..]) {
            let abs = (start + pos + found) as u32;
            idx = advance(&self.entries, idx, seg_hi, abs);
            if let Some((_, score)) = self.evaluate(idx, p, tokens, folders) {
                let key = (f64::from(score) * 1_000_000.0) as i64;
                let cand = (key, idx as u32);
                if heap.len() < p.limit {
                    heap.push(Reverse(cand));
                } else if heap.peek().is_some_and(|Reverse(min)| cand > *min) {
                    heap.pop();
                    heap.push(Reverse(cand));
                }
            }
            // Skip the rest of this name: continue at the next entry.
            let next = idx + 1;
            if next >= seg_hi {
                break;
            }
            pos = self.entries[next].lower_off as usize - start;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_range(
        &self,
        lo: usize,
        hi: usize,
        finder: &memmem::Finder<'_>,
        p: &SearchParams<'_>,
        tokens: &[&[u8]],
        folders: bool,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Vec<(i64, u32)> {
        let mut heap = BinaryHeap::with_capacity(p.limit + 1);
        let mut seg_lo = lo;
        while seg_lo < hi {
            if cancelled() {
                break;
            }
            let seg_hi = (seg_lo + SEGMENT).min(hi);
            self.scan_segment(seg_lo, seg_hi, finder, p, tokens, folders, &mut heap);
            seg_lo = seg_hi;
        }
        heap.into_iter().map(|Reverse(c)| c).collect()
    }

    /// A query with only a time window: the most recent entries inside it.
    fn search_by_time(
        &self,
        p: &SearchParams<'_>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Vec<Hit> {
        if p.time.is_none() || p.limit == 0 {
            return Vec::new();
        }
        let mut heap = BinaryHeap::with_capacity(p.limit + 1);
        for i in 0..self.entries.len() {
            if i % SEGMENT == 0 && cancelled() {
                break;
            }
            let Some((_, score)) = self.evaluate(i, p, &[], false) else {
                continue;
            };
            let cand = ((f64::from(score) * 1_000_000.0) as i64, i as u32);
            if heap.len() < p.limit {
                heap.push(Reverse(cand));
            } else if heap.peek().is_some_and(|Reverse(min)| cand > *min) {
                heap.pop();
                heap.push(Reverse(cand));
            }
        }
        let mut cands: Vec<(i64, u32)> = heap.into_iter().map(|Reverse(c)| c).collect();
        cands.sort_unstable_by(|a, b| b.cmp(a));
        cands
            .into_iter()
            .filter_map(|(_, idx)| {
                let (features, score) = self.evaluate(idx as usize, p, &[], false)?;
                Some(Hit {
                    entry: idx,
                    score,
                    features,
                })
            })
            .collect()
    }

    /// Top `limit` matches, best first. Checks `cancelled` between segments
    /// of 65,536 entries and returns what it has found so far if it fires.
    pub fn search(&self, p: &SearchParams<'_>, cancelled: &(dyn Fn() -> bool + Sync)) -> Vec<Hit> {
        self.search_noted(p, cancelled).0
    }

    /// Like [`search`](Self::search), plus the corrected word when the hits come
    /// from the typo fallback (a one-word query of 5+ letters with no other hit).
    pub fn search_noted(
        &self,
        p: &SearchParams<'_>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> (Vec<Hit>, Option<String>) {
        let n = self.entries.len();
        let Some(needle) = p.tokens.iter().max_by_key(|t| t.len()) else {
            return (self.search_by_time(p, cancelled), None);
        };
        if n == 0 || p.limit == 0 || needle.is_empty() {
            return (Vec::new(), None);
        }
        let tokens: Vec<&[u8]> = p.tokens.iter().map(Vec::as_slice).collect();
        // Parent-folder matches only matter for several words, and only when
        // some word occurs in a folder name at all (one scan of the small
        // folder-name arena). Otherwise this is the plain file-name search.
        let in_dirs: Vec<bool> = if tokens.len() >= 2 {
            tokens
                .iter()
                .map(|t| memmem::find(&self.dir_lower, t).is_some())
                .collect()
        } else {
            Vec::new()
        };
        let folders = in_dirs.contains(&true);
        // A word that is in no folder name must be in the file name: scan for the
        // longest of those. If every word is in some folder name, scan for each.
        let needles: Vec<&[u8]> = if folders {
            let required: Vec<&[u8]> = tokens
                .iter()
                .zip(&in_dirs)
                .filter(|(_, d)| !**d)
                .map(|(t, _)| *t)
                .collect();
            match required.iter().max_by_key(|t| t.len()) {
                Some(t) => vec![*t],
                None => tokens.clone(),
            }
        } else {
            vec![needle.as_slice()]
        };

        let threads = if n >= self.parallel_min {
            std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(self.max_threads)
        } else {
            1
        };
        let mut cands: Vec<(i64, u32)> = Vec::new();
        for needle in needles {
            let finder = memmem::Finder::new(needle);
            if threads <= 1 {
                cands.extend(self.scan_range(0, n, &finder, p, &tokens, folders, cancelled));
            } else {
                let chunk = n.div_ceil(threads);
                cands.extend(std::thread::scope(|s| {
                    let handles: Vec<_> = (0..threads)
                        .map(|t| {
                            let (lo, hi) = (t * chunk, ((t + 1) * chunk).min(n));
                            let (finder, tokens) = (&finder, &tokens);
                            s.spawn(move || {
                                self.scan_range(lo, hi, finder, p, tokens, folders, cancelled)
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|h| h.join().unwrap_or_default())
                        .collect::<Vec<_>>()
                }));
            }
        }
        cands.sort_unstable_by(|a, b| b.cmp(a));
        cands.dedup_by_key(|c| c.1);
        cands.truncate(p.limit);
        let hits: Vec<Hit> = cands
            .into_iter()
            .filter_map(|(_, idx)| {
                let (features, score) = self.evaluate(idx as usize, p, &tokens, folders)?;
                Some(Hit {
                    entry: idx,
                    score,
                    features,
                })
            })
            .collect();
        if hits.is_empty() && !cancelled() {
            if let [t] = tokens.as_slice() {
                if (2..=4).contains(&t.len()) && t.iter().all(u8::is_ascii_lowercase) {
                    return (self.search_initials(p, t, cancelled), None);
                }
                if t.len() >= 5 && t.iter().all(u8::is_ascii_lowercase) {
                    return self.search_typo(p, t, cancelled);
                }
            }
        }
        (hits, None)
    }

    fn fallback_range<F: Fn(&[u8]) -> bool + Sync>(
        &self,
        lo: usize,
        hi: usize,
        p: &SearchParams<'_>,
        quality: f32,
        pred: &F,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Vec<(i64, u32)> {
        let mut heap = BinaryHeap::with_capacity(p.limit + 1);
        for i in lo..hi {
            if i % SEGMENT == 0 && cancelled() {
                break;
            }
            let e = &self.entries[i];
            if !self.admitted(i, e, p) || !pred(self.lower_name(i)) {
                continue;
            }
            let (_, score) = Self::features(e, p, quality);
            let cand = ((f64::from(score) * 1_000_000.0) as i64, i as u32);
            if heap.len() < p.limit {
                heap.push(Reverse(cand));
            } else if heap.peek().is_some_and(|Reverse(min)| cand > *min) {
                heap.pop();
                heap.push(Reverse(cand));
            }
        }
        heap.into_iter().map(|Reverse(c)| c).collect()
    }

    /// Only when a one-word search found nothing: names whose words start with
    /// the letters of `token` ("vsc" finds "Visual Studio Code").
    fn search_initials(
        &self,
        p: &SearchParams<'_>,
        token: &[u8],
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Vec<Hit> {
        self.search_fallback(
            p,
            INITIALS_QUALITY,
            &|name| initials_match(name, token),
            cancelled,
        )
    }

    /// Only when a one-word search of 5+ letters found nothing (initials do not
    /// apply): names with a word one edit away from `token`, and that word.
    fn search_typo(
        &self,
        p: &SearchParams<'_>,
        token: &[u8],
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> (Vec<Hit>, Option<String>) {
        let hits = self.search_fallback(
            p,
            TYPO_QUALITY,
            &|name| typo_word(name, token).is_some(),
            cancelled,
        );
        let word = hits.first().and_then(|h| {
            let name = self.lower_name(h.entry as usize);
            typo_word(name, token).map(|w| String::from_utf8_lossy(w).into_owned())
        });
        (hits, word)
    }

    /// Threaded scan of every name with `pred`; hits get a fixed match quality.
    fn search_fallback<F: Fn(&[u8]) -> bool + Sync>(
        &self,
        p: &SearchParams<'_>,
        quality: f32,
        pred: &F,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Vec<Hit> {
        let n = self.entries.len();
        let threads = if n >= self.parallel_min {
            std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(self.max_threads)
        } else {
            1
        };
        let chunk = n.div_ceil(threads).max(1);
        let mut cands: Vec<(i64, u32)> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    let (lo, hi) = ((t * chunk).min(n), ((t + 1) * chunk).min(n));
                    s.spawn(move || self.fallback_range(lo, hi, p, quality, pred, cancelled))
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        });
        cands.sort_unstable_by(|a, b| b.cmp(a));
        cands
            .into_iter()
            .map(|(_, idx)| {
                let (features, score) = Self::features(&self.entries[idx as usize], p, quality);
                Hit {
                    entry: idx,
                    score,
                    features,
                }
            })
            .collect()
    }
}

/// Added to the quality of a name matched partly through its parent folder, so
/// it ranks above a name matched in the file name alone.
const COMBINED_BONUS: f32 = 0.3;
/// Fixed quality of an initials match: above a mid-word hit, below a word start.
pub(crate) const INITIALS_QUALITY: f32 = 0.6;

/// Fixed quality of a typo match: below an initials match.
pub(crate) const TYPO_QUALITY: f32 = 0.5;

/// The first word of `name` (split at space, dot, underscore, dash; the
/// extension ignored) that is one edit from `token` (an insert, a delete, a
/// replace or a swap of two neighbouring letters). `token` has 5+ letters.
pub(crate) fn typo_word<'a>(name: &'a [u8], token: &[u8]) -> Option<&'a [u8]> {
    let stem = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    let (first, last) = (token[0], token[token.len() - 1]);
    stem.split(|b| matches!(b, b' ' | b'.' | b'_' | b'-'))
        .find(|w| {
            w.len() + 1 >= token.len()
                && w.len() <= token.len() + 1
                // One edit never changes both ends of a word this long.
                && (w[0] == first || w[w.len() - 1] == last)
                && within_one_edit(w, token)
        })
}

fn within_one_edit(a: &[u8], b: &[u8]) -> bool {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let i = long.iter().zip(short).take_while(|(x, y)| x == y).count();
    if i == long.len() {
        return true;
    }
    match long.len() - short.len() {
        0 => {
            long[i + 1..] == short[i + 1..]
                || (i + 1 < long.len()
                    && long[i] == short[i + 1]
                    && long[i + 1] == short[i]
                    && long[i + 2..] == short[i + 2..])
        }
        1 => long[i + 1..] == short[i..],
        _ => false,
    }
}

/// Whether the words of `name` (split at space, dot, underscore, dash; the
/// extension ignored) start with exactly the letters of `token`.
fn initials_match(name: &[u8], token: &[u8]) -> bool {
    // Nearly every name fails on its first letter; check that before anything else.
    let first = name
        .iter()
        .find(|b| !matches!(b, b' ' | b'.' | b'_' | b'-'));
    if first != token.first() {
        return false;
    }
    let stem = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    let mut firsts = stem
        .split(|b| matches!(b, b' ' | b'.' | b'_' | b'-'))
        .filter(|w| !w.is_empty())
        .map(|w| w[0]);
    token.iter().all(|&t| firsts.next() == Some(t)) && firsts.next().is_none()
}

// ------------------------------------------------------------- persistence

const SNAPSHOT_MAGIC: &[u8; 4] = b"NIX1";
/// Sanity limit when reading a snapshot (the index itself is capped far lower).
const SNAPSHOT_MAX: u32 = 100_000_000;

fn put_u32(w: &mut impl std::io::Write, v: u32) -> std::io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn put_str(w: &mut impl std::io::Write, s: &str) -> std::io::Result<()> {
    put_u32(w, u32::try_from(s.len()).unwrap_or(u32::MAX))?;
    w.write_all(s.as_bytes())
}

fn get_u32(r: &mut impl std::io::Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn get_str(r: &mut impl std::io::Read) -> std::io::Result<String> {
    let len = get_u32(r)? as usize;
    if len > usize::from(u16::MAX) * 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "name too long in snapshot",
        ));
    }
    let mut b = vec![0u8; len];
    r.read_exact(&mut b)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

impl NameIndex {
    fn entry_name(&self, i: usize) -> String {
        let e = &self.entries[i];
        if e.orig_off == NO_ORIG {
            String::from_utf8_lossy(self.lower_name(i)).into_owned()
        } else {
            let s = e.orig_off as usize;
            self.orig
                .get(s..s + usize::from(e.orig_len))
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default()
        }
    }

    /// Writes every live folder and file (not frecency, which is reapplied from
    /// `meta.db` on load). The caller encrypts the stream.
    pub fn export(&self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        w.write_all(SNAPSHOT_MAGIC)?;
        // Folder ordinals: parents always precede their children.
        let mut ordinal = vec![u32::MAX; self.dirs.len()];
        let mut modified = vec![0u32; self.dirs.len()];
        for e in &self.entries {
            if e.flags & (FLAG_DIR | FLAG_DEAD) == FLAG_DIR {
                modified[e.dir as usize] = e.modified;
            }
        }
        let live: Vec<usize> = (0..self.dirs.len())
            .filter(|&i| !self.dirs[i].dead)
            .collect();
        for (n, &i) in live.iter().enumerate() {
            ordinal[i] = u32::try_from(n).unwrap_or(u32::MAX);
        }
        put_u32(w, u32::try_from(live.len()).unwrap_or(u32::MAX))?;
        for &i in &live {
            let d = &self.dirs[i];
            let parent = if d.parent == NO_PARENT {
                u32::MAX
            } else {
                ordinal[d.parent as usize]
            };
            put_u32(w, parent)?;
            put_u32(w, modified[i])?;
            put_str(w, &self.dir_name(d))?;
        }
        put_u32(w, u32::try_from(self.files_live).unwrap_or(u32::MAX))?;
        for (i, e) in self.entries.iter().enumerate() {
            if e.flags & (FLAG_DIR | FLAG_DEAD) != 0 {
                continue;
            }
            put_u32(w, ordinal[e.dir as usize])?;
            put_u32(w, e.modified)?;
            put_str(w, &self.entry_name(i))?;
        }
        Ok(())
    }

    /// Reads a snapshot written by [`export`](Self::export).
    pub fn import(r: &mut impl std::io::Read) -> std::io::Result<Self> {
        let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_owned());
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != SNAPSHOT_MAGIC {
            return Err(bad("not a name index snapshot"));
        }
        let mut idx = NameIndex::new();
        let dirs = get_u32(r)?;
        if dirs > SNAPSHOT_MAX {
            return Err(bad("implausible folder count"));
        }
        let mut ids: Vec<u32> = Vec::with_capacity(dirs as usize);
        for _ in 0..dirs {
            let parent = get_u32(r)?;
            let modified = get_u32(r)?;
            let name = get_str(r)?;
            let id = if parent == u32::MAX {
                idx.add_root(Path::new(&name), modified)
            } else {
                let p = *ids
                    .get(parent as usize)
                    .ok_or_else(|| bad("folder order"))?;
                idx.add_dir(p, &name, modified)
            }
            .map_err(|e| bad(&e.to_string()))?;
            ids.push(id);
        }
        let files = get_u32(r)?;
        if files > SNAPSHOT_MAX {
            return Err(bad("implausible file count"));
        }
        for _ in 0..files {
            let dir = get_u32(r)?;
            let modified = get_u32(r)?;
            let name = get_str(r)?;
            let d = *ids.get(dir as usize).ok_or_else(|| bad("file folder"))?;
            idx.add_file(d, &name, modified)
                .map_err(|e| bad(&e.to_string()))?;
        }
        idx.shrink_to_fit();
        Ok(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_capital_i_is_found_by_plain_i_and_removable() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("C:\\Data"), 5).unwrap();
        idx.add_file(root, "\u{130}stanbul.txt", 1).unwrap();
        let tokens = vec![b"istanbul".to_vec()];
        let p = SearchParams {
            tokens: &tokens,
            kind: None,
            limit: 10,
            now: 100,
            weights: RankingWeights::default(),
            time: None,
            exts: &[],
        };
        assert_eq!(idx.search(&p, &|| false).len(), 1);
        assert!(idx.remove_path(Path::new("C:\\Data\\\u{130}stanbul.txt")));
        assert!(idx.search(&p, &|| false).is_empty());
    }

    #[test]
    fn snapshot_round_trips_folders_files_case_and_times() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("C:\\Data"), 5).unwrap();
        let sub = idx.add_dir(root, "Reports 2025", 6).unwrap();
        idx.add_file(sub, "Q1 Summary.TXT", 7).unwrap();
        idx.add_file(root, "readme.md", 8).unwrap();
        let gone = idx.add_dir(root, "Old", 1).unwrap();
        idx.add_file(gone, "dead.txt", 1).unwrap();
        idx.remove_path(Path::new("C:\\Data\\Old"));
        let mut buf = Vec::new();
        idx.export(&mut buf).unwrap();
        let back = NameIndex::import(&mut buf.as_slice()).unwrap();
        assert_eq!(back.counts(), idx.counts());
        assert_eq!(back.counts().files, 2);
        assert!(back.contains_file(Path::new("C:\\Data\\Reports 2025\\Q1 Summary.TXT")));
        assert!(back.contains_file(Path::new("C:\\Data\\readme.md")));
        assert!(!back.contains_file(Path::new("C:\\Data\\Old\\dead.txt")));
        let tokens = [b"summary".to_vec()];
        let p = SearchParams {
            tokens: &tokens,
            kind: None,
            limit: 5,
            now: 100,
            weights: RankingWeights::default(),
            time: None,
            exts: &[],
        };
        let hits = back.search(&p, &|| false);
        let info = back.entry_info(hits[0].entry).unwrap();
        assert_eq!(info.name, "Q1 Summary.TXT");
        assert_eq!(info.modified, 7);
        // Damaged input is an error, not a panic.
        assert!(NameIndex::import(&mut &buf[..buf.len() / 2]).is_err());
        assert!(NameIndex::import(&mut &b"XXXX"[..]).is_err());
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    const NOW: u32 = 1_000_000_000;

    fn params<'a>(tokens: &'a [Vec<u8>], limit: usize) -> SearchParams<'a> {
        SearchParams {
            tokens,
            kind: None,
            limit,
            now: NOW,
            weights: RankingWeights::default(),
            time: None,
            exts: &[],
        }
    }

    fn toks(words: &[&str]) -> Vec<Vec<u8>> {
        words.iter().map(|w| w.as_bytes().to_vec()).collect()
    }

    fn names(idx: &NameIndex, hits: &[Hit]) -> Vec<String> {
        hits.iter()
            .filter_map(|h| idx.entry_info(h.entry))
            .map(|i| i.name)
            .collect()
    }

    #[test]
    fn extension_filter_limits_matches_to_those_files() {
        let (idx, _) = sample();
        let none = toks(&[]);
        let mut p = params(&none, 10);
        p.time = Some((NOW - 2 * 86_400, NOW));
        p.exts = &["pdf", "jpg"];
        let hits = idx.search(&p, &|| false);
        let mut found = names(&idx, &hits);
        found.sort();
        assert_eq!(found, ["Photo (1).JPG", "annual report.pdf"]);
        let t = toks(&["report"]);
        let mut p = params(&t, 10);
        p.exts = &["pdf"];
        assert_eq!(
            names(&idx, &idx.search(&p, &|| false)),
            ["annual report.pdf"]
        );
    }

    fn sample() -> (NameIndex, u32) {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        for name in [
            "Report.docx",
            "annual report.pdf",
            "notes.txt",
            "Photo (1).JPG",
        ] {
            idx.add_file(root, name, NOW - 86_400).unwrap();
        }
        let sub = idx.add_dir(root, "Reports", NOW).unwrap();
        idx.add_file(sub, "q1.xlsx", NOW).unwrap();
        (idx, root)
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn finds_substrings_case_insensitively_and_keeps_original_case() {
        let (idx, _) = sample();
        let t = toks(&["photo"]);
        let hits = idx.search(&params(&t, 10), &never);
        assert_eq!(names(&idx, &hits), ["Photo (1).JPG"]);
    }

    #[test]
    fn all_tokens_required_in_any_order() {
        let (idx, _) = sample();
        let t = toks(&["pdf", "annual"]);
        let hits = idx.search(&params(&t, 10), &never);
        assert_eq!(names(&idx, &hits), ["annual report.pdf"]);
        let t = toks(&["pdf", "photo"]);
        assert!(idx.search(&params(&t, 10), &never).is_empty());
    }

    #[test]
    fn ranks_better_matches_first_and_respects_limit() {
        let (idx, _) = sample();
        let t = toks(&["report"]);
        let hits = idx.search(&params(&t, 10), &never);
        let got = names(&idx, &hits);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], "Report.docx");
        let limited = idx.search(&params(&t, 1), &never);
        assert_eq!(names(&idx, &limited), ["Report.docx"]);
    }

    #[test]
    fn kind_filter_separates_files_and_folders() {
        let (idx, _) = sample();
        let t = toks(&["report"]);
        let mut p = params(&t, 10);
        p.kind = Some(&Kind::Folder);
        assert_eq!(names(&idx, &idx.search(&p, &never)), ["Reports"]);
        p.kind = Some(&Kind::File);
        assert_eq!(idx.search(&p, &never).len(), 2);
        p.kind = Some(&Kind::App);
        assert!(idx.search(&p, &never).is_empty());
    }

    #[test]
    fn paths_are_reconstructed() {
        let (idx, _) = sample();
        let t = toks(&["q1"]);
        let hit = idx.search(&params(&t, 1), &never)[0];
        let info = idx.entry_info(hit.entry).unwrap();
        assert_eq!(info.path, Path::new("/r").join("Reports").join("q1.xlsx"));
        assert!(!info.is_dir);
    }

    #[test]
    fn frecency_lifts_an_otherwise_equal_match() {
        let (mut idx, root) = sample();
        idx.add_file(root, "budget a.xlsx", NOW).unwrap();
        idx.add_file(root, "budget b.xlsx", NOW).unwrap();
        let t = toks(&["budget"]);
        // Equal matches tie; the later entry wins the tie, so "b" leads before boosting "a".
        let before = names(&idx, &idx.search(&params(&t, 2), &never));
        assert_eq!(before[0], "budget b.xlsx");
        assert!(idx.set_frecency(&Path::new("/r").join("budget a.xlsx"), 255));
        let after = names(&idx, &idx.search(&params(&t, 2), &never));
        assert_eq!(after[0], "budget a.xlsx");
    }

    #[test]
    fn removing_a_file_hides_it_and_compaction_keeps_the_rest() {
        let (mut idx, _) = sample();
        assert!(idx.remove_path(&Path::new("/r").join("notes.txt")));
        assert!(!idx.remove_path(&Path::new("/r").join("notes.txt")));
        let t = toks(&["notes"]);
        assert!(idx.search(&params(&t, 10), &never).is_empty());
        let before = idx.counts();
        idx.compact();
        assert_eq!(idx.counts(), before);
        assert_eq!(idx.dead_ratio(), 0.0);
        let t = toks(&["photo"]);
        assert_eq!(
            names(&idx, &idx.search(&params(&t, 10), &never)),
            ["Photo (1).JPG"]
        );
        let t = toks(&["q1"]);
        assert_eq!(idx.search(&params(&t, 10), &never).len(), 1);
    }

    #[test]
    fn removing_a_directory_removes_its_whole_subtree() {
        let (mut idx, root) = sample();
        let sub = idx
            .dir_id_for_path(&Path::new("/r").join("Reports"))
            .unwrap();
        let deep = idx.add_dir(sub, "deep", NOW).unwrap();
        idx.add_file(deep, "deep file.txt", NOW).unwrap();
        assert!(idx.remove_path(&Path::new("/r").join("Reports")));
        for w in ["q1", "deep", "reports"] {
            let t = toks(&[w]);
            let hits = idx.search(&params(&t, 10), &never);
            // "reports" still matches the other report files, never the directory.
            assert!(names(&idx, &hits)
                .iter()
                .all(|n| n != "Reports" && n != "q1.xlsx" && n != "deep"));
        }
        assert!(idx.add_file(sub, "late.txt", NOW).is_err());
        // The parent is untouched.
        assert!(idx.add_file(root, "ok.txt", NOW).is_ok());
    }

    #[test]
    fn contains_file_and_duplicate_detection() {
        let (idx, _) = sample();
        assert!(idx.contains_file(&Path::new("/r").join("NOTES.TXT")));
        assert!(!idx.contains_file(&Path::new("/r").join("missing.txt")));
        assert!(!idx.contains_file(&Path::new("/other").join("notes.txt")));
    }

    #[test]
    fn invalid_names_are_rejected() {
        let (mut idx, root) = sample();
        assert!(matches!(
            idx.add_file(root, "", NOW),
            Err(IndexError::InvalidName)
        ));
        assert!(matches!(
            idx.add_file(root, "a\0b", NOW),
            Err(IndexError::InvalidName)
        ));
        assert!(matches!(
            idx.add_file(99, "x", NOW),
            Err(IndexError::UnknownDir(99))
        ));
    }

    #[test]
    fn unicode_names_match_case_insensitively() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        idx.add_file(root, "ÅNGSTRÖM Notes.txt", NOW).unwrap();
        // The sources fold the query the same way as names (accents removed).
        let t = toks(&[&ranking::fold_lower("Ångström")]);
        let hits = idx.search(&params(&t, 5), &never);
        assert_eq!(names(&idx, &hits), ["ÅNGSTRÖM Notes.txt"]);
    }

    #[test]
    fn accented_names_are_found_without_accents_but_stay_distinct() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        idx.add_file(root, "café.txt", NOW).unwrap();
        idx.add_file(root, "cafe.txt", NOW).unwrap();
        let t = toks(&["cafe"]);
        assert_eq!(idx.search(&params(&t, 5), &never).len(), 2);
        assert!(idx.remove_path(&Path::new("/r").join("café.txt")));
        assert!(idx.contains_file(&Path::new("/r").join("cafe.txt")));
        assert!(!idx.contains_file(&Path::new("/r").join("café.txt")));
    }

    #[test]
    fn cancellation_stops_the_scan() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        for i in 0..(SEGMENT * 3) {
            idx.add_file(root, &format!("file{i}.txt"), NOW).unwrap();
        }
        let t = toks(&["file"]);
        let calls = AtomicUsize::new(0);
        let cancel_after_first = || calls.fetch_add(1, Ordering::SeqCst) >= 1;
        let partial = idx.search(&params(&t, 1_000_000), &cancel_after_first);
        // One segment scanned (65,536 entries), the other two skipped.
        assert!(partial.len() <= SEGMENT);
        assert!(!partial.is_empty());
        assert!(calls.load(Ordering::SeqCst) <= 3);
        let all = idx.search(&params(&t, 1_000_000), &never);
        assert_eq!(all.len(), SEGMENT * 3);
    }

    #[test]
    fn parallel_scan_gives_the_same_top_results_as_serial() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        for i in 0..5000 {
            let name = if i % 97 == 0 {
                format!("report {i}.pdf")
            } else {
                format!("misc{i}.dat")
            };
            idx.add_file(root, &name, NOW - (i as u32 % 400) * 86_400)
                .unwrap();
        }
        let t = toks(&["report"]);
        let serial = idx.search(&params(&t, 20), &never);
        idx.set_parallel_threshold(10);
        let parallel = idx.search(&params(&t, 20), &never);
        assert_eq!(names(&idx, &serial), names(&idx, &parallel));
        assert_eq!(serial.len(), 20);
    }

    #[test]
    fn thread_cap_does_not_change_results_and_stats_describe_names() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        for i in 0..3000 {
            idx.add_file(root, &format!("doc {i}.txt"), NOW).unwrap();
        }
        idx.set_parallel_threshold(10);
        let t = toks(&["doc"]);
        let wide = names(&idx, &idx.search(&params(&t, 25), &never));
        idx.set_max_threads(1);
        let narrow = names(&idx, &idx.search(&params(&t, 25), &never));
        assert_eq!(wide, narrow);
        idx.set_max_threads(0); // clamps to one thread instead of dividing by zero
        assert_eq!(names(&idx, &idx.search(&params(&t, 25), &never)), narrow);

        let s = idx.file_name_stats().unwrap();
        assert_eq!(s.count, 3000);
        assert_eq!((s.min, s.max), (9, 12)); // "doc 0.txt" .. "doc 2999.txt"
        assert!(s.p50 >= s.min && s.p95 <= s.max && s.mean > 9.0 && s.mean < 12.0);
        assert!(NameIndex::new().file_name_stats().is_none());
    }

    #[test]
    fn gallop_finds_the_containing_entry() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        for i in 0..1000 {
            idx.add_file(root, &format!("n{i:04}"), NOW).unwrap();
        }
        for target in [0usize, 1, 2, 17, 500, 999, 1000] {
            let abs = idx.entries[target].lower_off + 1; // a byte inside the name
            let got = advance(&idx.entries, 0, idx.entries.len(), abs);
            assert_eq!(got, target);
        }
    }

    fn found(idx: &NameIndex, words: &[&str]) -> Vec<String> {
        let t = toks(words);
        names(idx, &idx.search(&params(&t, 10), &never))
    }

    fn folders_sample() -> NameIndex {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        let budget = idx.add_dir(root, "Budget", NOW).unwrap();
        let misc = idx.add_dir(root, "misc", NOW).unwrap();
        idx.add_file(budget, "report.docx", NOW).unwrap();
        idx.add_file(budget, "notes.txt", NOW).unwrap();
        idx.add_file(misc, "budget report.docx", NOW).unwrap();
        idx.add_file(misc, "report.pdf", NOW).unwrap();
        idx
    }

    #[test]
    fn a_word_missing_from_the_name_can_match_the_parent_folder() {
        let idx = folders_sample();
        let got = found(&idx, &["budget", "report"]);
        assert!(got.contains(&"report.docx".to_owned()), "{got:?}");
        // Only the immediate parent counts, and report.pdf has no budget anywhere.
        assert!(!got.contains(&"report.pdf".to_owned()), "{got:?}");
        // The grandparent is not the parent: a file below Budget\Sub does not match.
        let mut idx = folders_sample();
        let sub = idx.add_dir(1, "Sub", NOW).unwrap();
        idx.add_file(sub, "report.xlsx", NOW).unwrap();
        assert!(!found(&idx, &["budget", "report"]).contains(&"report.xlsx".to_owned()));
    }

    #[test]
    fn folder_plus_file_matches_rank_above_file_only_matches() {
        let mut idx = folders_sample();
        idx.add_file(2, "budget big report.docx", NOW).unwrap();
        let got = found(&idx, &["budget", "report"]);
        // The query is a whole name: exact first. Then the folder match beats the name-only one.
        assert_eq!(got[0], "budget report.docx", "{got:?}");
        assert_eq!(got[1], "report.docx", "{got:?}");
        assert_eq!(got[2], "budget big report.docx", "{got:?}");
    }

    #[test]
    fn folder_matching_leaves_normal_queries_alone() {
        let idx = folders_sample();
        // One word: files that only sit in a matching folder are not hits.
        assert_eq!(found(&idx, &["budget"]), ["Budget", "budget report.docx"]);
        // Every word in the name: the same hit and the same score as without folders.
        let t = toks(&["budget", "report"]);
        let hits = idx.search(&params(&t, 10), &never);
        let plain = hits
            .iter()
            .find(|h| idx.entry_info(h.entry).unwrap().name == "budget report.docx")
            .unwrap();
        let tokens: Vec<&[u8]> = t.iter().map(Vec::as_slice).collect();
        let (_, direct) = idx
            .evaluate(plain.entry as usize, &params(&t, 10), &tokens, false)
            .unwrap();
        assert!((plain.score - direct).abs() < 1e-6);
    }

    #[test]
    fn initials_find_names_only_when_nothing_else_matches() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        idx.add_file(root, "Visual Studio Code.lnk", NOW).unwrap();
        idx.add_file(root, "very_special-cat.txt", NOW).unwrap();
        idx.add_file(root, "Notes.txt", NOW).unwrap();
        let got = found(&idx, &["vsc"]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.contains(&"Visual Studio Code.lnk".to_owned()));
        assert!(got.contains(&"very_special-cat.txt".to_owned()));
        // The extension is not a word, and the letters must be all of the initials.
        assert!(found(&idx, &["vscl"]).is_empty());
        assert!(found(&idx, &["vs"]).is_empty());
    }

    #[test]
    fn initials_are_not_used_when_a_normal_search_has_hits_or_the_query_is_not_2_to_4_letters() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        idx.add_file(root, "Visual Studio Code.lnk", NOW).unwrap();
        idx.add_file(root, "vsc-notes.txt", NOW).unwrap();
        assert_eq!(found(&idx, &["vsc"]), ["vsc-notes.txt"]);
        // 5 letters, one letter, digits and two words never use initials.
        idx.add_file(root, "a b c d e.txt", NOW).unwrap();
        assert!(found(&idx, &["abcde"]).is_empty());
        assert!(found(&idx, &["a"])
            .iter()
            .all(|n| n.contains('a') || n.contains('A')));
        assert!(found(&idx, &["vsc", "zzz"]).is_empty());
    }

    #[test]
    fn capital_letters_inside_a_name_count_as_word_starts() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        idx.add_file(root, "powertoysrun.exe", NOW).unwrap();
        idx.add_file(root, "PowerToysRun.exe", NOW).unwrap();
        let got = found(&idx, &["toys"]);
        assert_eq!(got, ["PowerToysRun.exe", "powertoysrun.exe"]);
    }

    #[test]
    fn a_lowercase_only_name_ranks_as_before_without_capitals() {
        let (idx, _) = sample();
        // "port" is mid-word in Report.docx (no capital there): still the same two hits.
        let got = found(&idx, &["port"]);
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn an_uppercase_query_reaches_the_search_lowercased_and_finds_initials() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        idx.add_file(root, "Visual Studio Code.lnk", NOW).unwrap();
        // The search box parses the text first, and the parser lowercases words.
        let parsed = crate::query::parse("VSC");
        let t: Vec<Vec<u8>> = parsed
            .tokens
            .iter()
            .map(|t| t.as_bytes().to_vec())
            .collect();
        let hits = idx.search(&params(&t, 10), &never);
        assert_eq!(names(&idx, &hits), ["Visual Studio Code.lnk"]);
    }

    #[test]
    fn a_misspelled_word_finds_the_name_when_nothing_else_matches() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        idx.add_file(root, "document.pdf", NOW).unwrap();
        idx.add_file(root, "receipt.txt", NOW).unwrap();
        idx.add_file(root, "Notes.txt", NOW).unwrap();
        let t = toks(&["documnet"]);
        let (hits, word) = idx.search_noted(&params(&t, 10), &never);
        assert_eq!(names(&idx, &hits), ["document.pdf"]);
        assert_eq!(word.as_deref(), Some("document"));
        let t = toks(&["reciept"]);
        let (hits, word) = idx.search_noted(&params(&t, 10), &never);
        assert_eq!(names(&idx, &hits), ["receipt.txt"]);
        assert_eq!(word.as_deref(), Some("receipt"));
        // insert, delete, replace
        assert_eq!(found(&idx, &["documen"]), ["document.pdf"]); // substring: normal hit
        assert_eq!(found(&idx, &["docuument"]), ["document.pdf"]);
        assert_eq!(found(&idx, &["docoment"]), ["document.pdf"]);
        // two edits are too far
        assert!(found(&idx, &["docmuent"]).len() == 1); // one swap of "um" -> ok
        assert!(found(&idx, &["dokumnet"]).is_empty());
    }

    #[test]
    fn typo_matching_is_not_used_for_short_queries_or_when_there_are_hits() {
        let mut idx = NameIndex::new();
        let root = idx.add_root(Path::new("/r"), NOW).unwrap();
        idx.add_file(root, "report.txt", NOW).unwrap();
        idx.add_file(root, "reports.txt", NOW).unwrap();
        // 4 letters: no typo search ("reprt" is 5 and does find; "rept" does not).
        assert!(found(&idx, &["rept"]).is_empty());
        assert_eq!(found(&idx, &["reprt"]), ["report.txt"]);
        // A normal hit means no correction.
        let t = toks(&["report"]);
        let (hits, word) = idx.search_noted(&params(&t, 10), &never);
        assert_eq!(hits.len(), 2);
        assert!(word.is_none());
        // Two words never use it.
        assert!(found(&idx, &["reprt", "txt"]).is_empty());
    }
}
