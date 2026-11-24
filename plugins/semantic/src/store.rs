//! The vector file: one int8 vector per file, in memory as a flat array that
//! a query scans brute-force, on disk as an append-only log of encrypted
//! frames (each frame sealed with the plugin's own key). Updates and removals
//! are appended; the log is rewritten when most of it is dead. A torn or
//! unreadable tail (crash, wrong key) is cut off at load.

use std::collections::BTreeMap;
use std::io::Write;
use std::ops::Bound;
use std::path::{Path, PathBuf};

use lookout_core::crypto::{self, Key};

use crate::embed::DIM;

const MAGIC: &[u8] = b"LKSV1\n";
const MAX_FRAME: usize = 64 << 20;
/// A bound on resident vectors (about 190 MB at 384 bytes each).
pub const MAX_ENTRIES: usize = 500_000;
const PUT: u8 = 1;
const REMOVE: u8 = 2;
const PUT_NO_VECTOR: u8 = 3;

pub struct Entry {
    pub path: String,
    pub mtime: u64,
    /// False for files that had no text: remembered, never returned.
    pub has_vector: bool,
}

pub struct Store {
    entries: Vec<Entry>,
    vecs: Vec<i8>,
    slot: BTreeMap<String, usize>,
    file: Option<PathBuf>,
    key: Option<Key>,
    log_bytes: u64,
}

fn quantize(v: &[f32]) -> Vec<i8> {
    v.iter()
        .map(|x| (x * 127.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

impl Store {
    /// `file` and `key` both set: persistent. Otherwise memory only.
    pub fn open(file: Option<PathBuf>, key: Option<Key>) -> Self {
        let mut s = Self {
            entries: Vec::new(),
            vecs: Vec::new(),
            slot: BTreeMap::new(),
            file: file.filter(|_| key.is_some()),
            key,
            log_bytes: 0,
        };
        s.load();
        s
    }

    fn load(&mut self) {
        let (Some(path), Some(key)) = (self.file.clone(), self.key.clone()) else {
            return;
        };
        let Ok(data) = std::fs::read(&path) else {
            return;
        };
        if !data.starts_with(MAGIC) {
            self.reset_file();
            return;
        }
        let mut pos = MAGIC.len();
        while pos + 4 <= data.len() {
            let len = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                as usize;
            let end = pos + 4 + len;
            if len > MAX_FRAME || end > data.len() {
                break;
            }
            let Ok(plain) = crypto::open(&key, &data[pos + 4..end]) else {
                break;
            };
            if !self.apply(&plain) {
                break;
            }
            pos = end;
        }
        self.log_bytes = pos as u64;
        if pos < data.len() {
            // Cut off what could not be read, so new frames follow good ones.
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
                let _ = f.set_len(pos as u64);
            }
        }
        self.maybe_compact();
    }

    fn reset_file(&mut self) {
        if let Some(p) = &self.file {
            let _ = std::fs::remove_file(p);
        }
        self.log_bytes = 0;
    }

    /// Applies decoded ops; false if the frame is malformed.
    fn apply(&mut self, mut b: &[u8]) -> bool {
        while let Some((&op, rest)) = b.split_first() {
            b = rest;
            let Some((len, rest)) = b.split_first_chunk::<4>() else {
                return false;
            };
            let len = u32::from_le_bytes(*len) as usize;
            if rest.len() < len {
                return false;
            }
            let Ok(path) = std::str::from_utf8(&rest[..len]) else {
                return false;
            };
            b = &rest[len..];
            match op {
                REMOVE => self.remove_one(path),
                PUT | PUT_NO_VECTOR => {
                    let Some((m, rest)) = b.split_first_chunk::<8>() else {
                        return false;
                    };
                    b = rest;
                    let mtime = u64::from_le_bytes(*m);
                    if op == PUT {
                        if b.len() < DIM {
                            return false;
                        }
                        let v: Vec<i8> = b[..DIM].iter().map(|x| *x as i8).collect();
                        b = &b[DIM..];
                        self.put_mem(path, mtime, Some(v));
                    } else {
                        self.put_mem(path, mtime, None);
                    }
                }
                _ => return false,
            }
        }
        true
    }

    fn put_mem(&mut self, path: &str, mtime: u64, v: Option<Vec<i8>>) {
        let has_vector = v.is_some();
        let v = v.unwrap_or_else(|| vec![0; DIM]);
        if let Some(&i) = self.slot.get(path) {
            self.entries[i].mtime = mtime;
            self.entries[i].has_vector = has_vector;
            self.vecs[i * DIM..(i + 1) * DIM].copy_from_slice(&v);
        } else {
            self.slot.insert(path.to_owned(), self.entries.len());
            self.entries.push(Entry {
                path: path.to_owned(),
                mtime,
                has_vector,
            });
            self.vecs.extend_from_slice(&v);
        }
    }

    fn remove_one(&mut self, path: &str) {
        let Some(i) = self.slot.remove(path) else {
            return;
        };
        let last = self.entries.len() - 1;
        if i != last {
            let (a, b) = self.vecs.split_at_mut(last * DIM);
            a[i * DIM..(i + 1) * DIM].copy_from_slice(&b[..DIM]);
            self.entries.swap(i, last);
            if let Some(s) = self.slot.get_mut(&self.entries[i].path) {
                *s = i;
            }
        }
        self.entries.pop();
        self.vecs.truncate(last * DIM);
    }

    fn encode_put(out: &mut Vec<u8>, path: &str, mtime: u64, v: Option<&[i8]>) {
        out.push(if v.is_some() { PUT } else { PUT_NO_VECTOR });
        out.extend_from_slice(&(path.len() as u32).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(&mtime.to_le_bytes());
        if let Some(v) = v {
            out.extend(v.iter().map(|x| *x as u8));
        }
    }

    fn append(&mut self, plain: &[u8]) -> Result<(), String> {
        let (Some(path), Some(key)) = (&self.file, &self.key) else {
            return Ok(());
        };
        let sealed = crypto::seal(key, plain).map_err(|e| e.to_string())?;
        let mut frame = Vec::with_capacity(sealed.len() + 4 + MAGIC.len());
        if self.log_bytes == 0 {
            frame.extend_from_slice(MAGIC);
        }
        frame.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
        frame.extend_from_slice(&sealed);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        f.write_all(&frame).map_err(|e| e.to_string())?;
        self.log_bytes += frame.len() as u64;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn vector_count(&self) -> usize {
        self.entries.iter().filter(|e| e.has_vector).count()
    }

    pub fn mtime_of(&self, path: &str) -> Option<u64> {
        self.slot.get(path).map(|&i| self.entries[i].mtime)
    }

    /// Stores documents: `Some(vector)` or `None` for a file without text.
    pub fn put_many(&mut self, docs: Vec<(String, u64, Option<Vec<f32>>)>) -> Result<(), String> {
        let new = docs
            .iter()
            .filter(|(p, _, _)| !self.slot.contains_key(p))
            .count();
        if self.entries.len() + new > MAX_ENTRIES {
            return Err(format!("the index is full ({MAX_ENTRIES} files)"));
        }
        let mut plain = Vec::new();
        for (path, mtime, v) in docs {
            let q = v.as_deref().map(quantize);
            Self::encode_put(&mut plain, &path, mtime, q.as_deref());
            self.put_mem(&path, mtime, q);
        }
        self.append(&plain)?;
        self.maybe_compact();
        Ok(())
    }

    pub fn remove_many(&mut self, paths: &[String]) -> Result<usize, String> {
        let mut plain = Vec::new();
        let mut n = 0;
        for p in paths {
            if self.slot.contains_key(p) {
                plain.push(REMOVE);
                plain.extend_from_slice(&(p.len() as u32).to_le_bytes());
                plain.extend_from_slice(p.as_bytes());
                self.remove_one(p);
                n += 1;
            }
        }
        if n > 0 {
            self.append(&plain)?;
            self.maybe_compact();
        }
        Ok(n)
    }

    /// Paths in ascending order after `after`.
    pub fn list(&self, after: Option<&str>, limit: usize) -> Vec<String> {
        let lo = after.map_or(Bound::Unbounded, Bound::Excluded);
        self.slot
            .range::<str, _>((lo, Bound::Unbounded))
            .take(limit)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Cosine similarities (approximate, from int8) of the best `limit` files
    /// at or above `min`, best first. `query` is a unit vector.
    pub fn search(&self, query: &[f32], limit: usize, min: f32) -> Vec<(String, f32)> {
        let q = quantize(query);
        let scale = 127.0 * 127.0;
        let mut hits: Vec<(usize, f32)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if !e.has_vector {
                continue;
            }
            let v = &self.vecs[i * DIM..(i + 1) * DIM];
            let dot: i32 = v
                .iter()
                .zip(&q)
                .map(|(a, b)| i32::from(*a) * i32::from(*b))
                .sum();
            let sim = dot as f32 / scale;
            if sim >= min {
                hits.push((i, sim));
            }
        }
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.truncate(limit);
        hits.into_iter()
            .map(|(i, s)| (self.entries[i].path.clone(), s))
            .collect()
    }

    fn maybe_compact(&mut self) {
        let live = self.entries.len() as u64 * (DIM as u64 + 120);
        if self.file.is_some() && self.log_bytes > 3 * live + (1 << 20) {
            let _ = self.compact();
        }
    }

    fn compact(&mut self) -> Result<(), String> {
        let (Some(path), Some(key)) = (self.file.clone(), self.key.clone()) else {
            return Ok(());
        };
        let mut out = MAGIC.to_vec();
        for chunk in self.entries.chunks(256) {
            let mut plain = Vec::new();
            for e in chunk {
                let Some(&i) = self.slot.get(&e.path) else {
                    continue;
                };
                let v = e.has_vector.then(|| &self.vecs[i * DIM..(i + 1) * DIM]);
                Self::encode_put(&mut plain, &e.path, e.mtime, v);
            }
            let sealed = crypto::seal(&key, &plain).map_err(|e| e.to_string())?;
            out.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
            out.extend_from_slice(&sealed);
        }
        crypto::write_atomic(&path, &out).map_err(|e| e.to_string())?;
        self.log_bytes = out.len() as u64;
        Ok(())
    }

    /// Forgets everything and deletes the file.
    pub fn clear(&mut self) -> usize {
        let n = self.entries.len();
        self.entries.clear();
        self.vecs.clear();
        self.slot.clear();
        self.reset_file();
        n
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }
}
