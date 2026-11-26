//! The full-text index: tantivy over an encrypted directory.
//!
//! One document per file: its masked text (indexed, not stored), a short
//! stored preview, modification time and whether the text came from the file
//! itself or from OCR. The writer exists only while a batch is being indexed;
//! it is dropped after each commit so an idle index holds no writer heap.

use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};

use tantivy::collector::TopDocs;
use tantivy::query::{
    AllQuery, BooleanQuery, ConstScoreQuery, Occur, Query, RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{
    Field, IndexRecordOption, OwnedValue, Schema, TantivyDocument, Value, FAST, INDEXED, STORED,
    STRING, TEXT,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Term};

use super::crypt_dir::EncryptedDirectory;
use crate::crypto::Key;
use crate::operators::{self, Operators};

/// Bump when OCR output changes enough that indexed images should be read again.
const OCR_VERSION: &str = "3";
const WRITER_HEAP: usize = 15_000_000;
/// Above this many segments a commit waits for its merge to finish.
const MERGE_ABOVE_SEGMENTS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("full-text index error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("full-text index folder: {0}")]
    Io(#[from] std::io::Error),
    #[error("full-text index folder: {0}")]
    Dir(#[from] tantivy::directory::error::OpenDirectoryError),
    #[error("the full-text index is closed")]
    Closed,
    #[error("the full-text index is already open in another Lookout")]
    InUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Text,
    Ocr,
}

impl DocKind {
    fn as_str(self) -> &'static str {
        match self {
            DocKind::Text => "text",
            DocKind::Ocr => "ocr",
        }
    }
}

pub struct ContentDoc {
    pub path: String,
    pub mtime: u64,
    pub kind: DocKind,
    /// File size in bytes, for `size:`.
    pub size: u64,
    /// Already masked (see `secrets`).
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContentHit {
    pub path: String,
    pub kind: DocKind,
    pub mtime: u64,
    pub preview: String,
    pub score: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ContentQuery<'a> {
    pub text: &'a str,
    pub images_only: bool,
    /// `[from, to)` in unix seconds.
    pub range: Option<(u64, u64)>,
    pub limit: usize,
    /// `ext:`, `size:` and `in:`, applied inside the search when
    /// [`ContentStore::filters_inside`]; `name:` is left to the caller.
    pub filter: Option<&'a Operators>,
}

#[derive(Clone, Copy)]
struct Fields {
    key: Field,
    path: Field,
    body: Field,
    preview: Field,
    kind: Field,
    mtime: Field,
    /// Absent in an index made before the operators.
    filters: Option<FilterFields>,
}

#[derive(Clone, Copy)]
struct FilterFields {
    /// Every ending of the file name after a dot.
    ext: Field,
    /// The folder from each of its parts on, below the indexed folder.
    under: Field,
    /// The folder's whole path.
    dir: Field,
    size: Field,
}

struct Open {
    index: Index,
    reader: IndexReader,
}

pub struct ContentStore {
    open: RwLock<Option<Open>>,
    writer: Mutex<Option<IndexWriter>>,
    fields: Fields,
    /// Opening found an unreadable index and made a new one.
    rebuilt: bool,
    /// Folder of the on-disk index; `None` for an in-memory one.
    dir: Option<std::path::PathBuf>,
    /// OS lock on a file next to the index folder, held while this store is
    /// open, so a second process cannot open the same index. The system drops
    /// it if the process dies.
    owner: Mutex<Option<std::fs::File>>,
    /// The indexed folders, for `in:`.
    roots: RwLock<Vec<PathBuf>>,
}

/// The current schema (`filters`), or the one from before the operators.
fn schema(filters: bool) -> (Schema, Fields) {
    let mut b = Schema::builder();
    let mut f = Fields {
        key: b.add_text_field("key", STRING),
        path: b.add_text_field("path", STORED),
        body: b.add_text_field("body", TEXT),
        preview: b.add_text_field("preview", STORED),
        kind: b.add_text_field("kind", STRING | STORED),
        mtime: b.add_u64_field("mtime", FAST | INDEXED | STORED),
        filters: None,
    };
    // Added last, so the fields above are the same in both.
    f.filters = filters.then(|| FilterFields {
        ext: b.add_text_field("ext", STRING),
        under: b.add_text_field("under", STRING),
        dir: b.add_text_field("dir", STRING),
        size: b.add_u64_field("size", FAST | INDEXED),
    });
    (b.build(), f)
}

type Clause = (Occur, Box<dyn Query>);

/// The clauses for `ext:`, `size:` and `in:`; they add nothing to the score.
fn filter_clauses(f: FilterFields, ops: &Operators) -> Result<Vec<Clause>, StoreError> {
    let term = |field: Field, text: &str| -> Box<dyn Query> {
        let term = Term::from_field_text(field, text);
        Box::new(TermQuery::new(term, IndexRecordOption::Basic))
    };
    let mut wanted: Vec<Box<dyn Query>> = Vec::new();
    if !ops.exts.is_empty() {
        wanted.push(Box::new(BooleanQuery::new(
            ops.exts
                .iter()
                .map(|e| (Occur::Should, term(f.ext, e)))
                .collect(),
        )));
    }
    for folder in &ops.folders {
        let field = if operators::is_absolute(folder) {
            f.dir
        } else {
            f.under
        };
        let pattern = format!("{}.*", literal(folder));
        wanted.push(Box::new(RegexQuery::from_pattern(&pattern, field)?));
    }
    if let Some((lo, hi)) = ops.size {
        let bound = |v: u32| Bound::Included(Term::from_field_u64(f.size, u64::from(v)));
        wanted.push(Box::new(RangeQuery::new(bound(lo), bound(hi))));
    }
    Ok(wanted
        .into_iter()
        .map(|q| {
            let free: Box<dyn Query> = Box::new(ConstScoreQuery::new(q, 0.0));
            (Occur::Must, free)
        })
        .collect())
}

/// `text` as a pattern that matches it only. Just the operators of the pattern
/// language are escaped: tantivy's parser refuses needless escapes.
fn literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Case-insensitive key for a Windows path.
pub fn path_key(path: &str) -> String {
    path.to_lowercase()
}

impl ContentStore {
    /// Opens (or creates) the encrypted index in `dir`. An index that cannot be
    /// read (other key, other schema, damage) is derived data and is rebuilt:
    /// the folder is emptied and a new index created; the caller then re-indexes.
    pub fn open(dir: &Path, key: &Key) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        let owner = Self::take_owner_lock(dir)?;
        let directory = || EncryptedDirectory::open(dir, key.clone());
        // An index made before the operators keeps working as it is: reading
        // every file again is not worth it unasked.
        let existing = || -> Option<(Open, Fields)> {
            let index = Index::open(directory().ok()?).ok()?;
            let found = index.schema();
            let fields = [true, false]
                .into_iter()
                .map(schema)
                .find(|(s, _)| *s == found)?
                .1;
            Some((Self::finish(index).ok()?, fields))
        };
        // A damaged index may make tantivy panic instead of failing.
        let had = dir.join("meta.json").exists();
        let found =
            crate::guard::contain("the content index could not be read", existing).flatten();
        let rebuilt = had && found.is_none();
        if rebuilt {
            crate::log::error("the content index was damaged and is being rebuilt");
        }
        let (open, fields) = match found {
            Some(found) => found,
            // Nothing there, or nothing readable: a new, empty index.
            None => {
                std::fs::remove_dir_all(dir)?;
                std::fs::create_dir_all(dir)?;
                let (schema, fields) = schema(true);
                let index = Index::open_or_create(directory()?, schema)?;
                (Self::finish(index)?, fields)
            }
        };
        let store = Self {
            open: RwLock::new(Some(open)),
            writer: Mutex::new(None),
            fields,
            rebuilt,
            dir: Some(dir.to_owned()),
            owner: Mutex::new(Some(owner)),
            roots: RwLock::default(),
        };
        store.forget_old_ocr(dir);
        Ok(store)
    }

    /// Images OCR'd before the 2x upscale read little or nothing from small
    /// text. Once, their entries are dropped so the next scan reads them again
    /// (images only; text files keep theirs). A marker file next to the index
    /// folder remembers that this was done.
    fn forget_old_ocr(&self, dir: &Path) {
        let Some(name) = dir.file_name() else { return };
        let marker = dir.with_file_name(format!("{}.ocr-version", name.to_string_lossy()));
        if std::fs::read_to_string(&marker).is_ok_and(|v| v.trim() == OCR_VERSION) {
            return;
        }
        let f = self.fields;
        let dropped = self
            .with_writer(|w| {
                w.delete_term(Term::from_field_text(f.kind, DocKind::Ocr.as_str()));
                Ok(())
            })
            .and_then(|()| self.commit());
        if dropped.is_ok() {
            let _ = std::fs::write(&marker, OCR_VERSION);
        }
    }

    /// Whether [`open`](Self::open) found an index it could not read and made a new one.
    pub fn was_rebuilt(&self) -> bool {
        self.rebuilt
    }

    /// Removes every document.
    pub fn clear(&self) -> Result<(), StoreError> {
        self.with_writer(|w| w.delete_all_documents().map(|_| ()))?;
        self.commit()
    }

    pub fn open_in_ram() -> Result<Self, StoreError> {
        let (schema, fields) = schema(true);
        let open = Self::finish(Index::create_in_ram(schema))?;
        Ok(Self {
            open: RwLock::new(Some(open)),
            writer: Mutex::new(None),
            fields,
            rebuilt: false,
            dir: None,
            owner: Mutex::new(None),
            roots: RwLock::default(),
        })
    }

    /// Whether this index filters `ext:`, `size:` and `in:` itself.
    pub fn filters_inside(&self) -> bool {
        self.fields.filters.is_some()
    }

    /// The indexed folders, before any file is added.
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        *self.roots.write().unwrap_or_else(PoisonError::into_inner) = roots;
    }

    fn take_owner_lock(dir: &Path) -> Result<std::fs::File, StoreError> {
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        let path = dir.with_file_name(format!("{name}.owner-lock"));
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        match f.try_lock() {
            Ok(()) => Ok(f),
            Err(std::fs::TryLockError::WouldBlock) => Err(StoreError::InUse),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// Size of the committed index: bytes on disk, or the in-memory size.
    pub fn size_bytes(&self) -> u64 {
        if let Some(dir) = &self.dir {
            return std::fs::read_dir(dir).map_or(0, |rd| {
                rd.flatten()
                    .filter_map(|e| e.metadata().ok())
                    .filter(|m| m.is_file())
                    .map(|m| m.len())
                    .sum()
            });
        }
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        guard.as_ref().map_or(0, |o| {
            o.reader
                .searcher()
                .space_usage()
                .map_or(0, |u| u.total().get_bytes())
        })
    }

    fn finish(index: Index) -> Result<Open, StoreError> {
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Open { index, reader })
    }

    fn with_writer<R>(
        &self,
        f: impl FnOnce(&mut IndexWriter) -> tantivy::Result<R>,
    ) -> Result<R, StoreError> {
        let mut slot = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
            let open = guard.as_ref().ok_or(StoreError::Closed)?;
            *slot = Some(open.index.writer_with_num_threads(1, WRITER_HEAP)?);
        }
        match slot.as_mut() {
            Some(w) => Ok(f(w)?),
            None => Err(StoreError::Closed),
        }
    }

    /// Adds or replaces the document for `doc.path`. Visible after [`commit`](Self::commit).
    pub fn upsert(&self, doc: &ContentDoc) -> Result<(), StoreError> {
        let f = self.fields;
        let key = path_key(&doc.path);
        let mut d = TantivyDocument::default();
        d.add_text(f.key, &key);
        d.add_text(f.path, &doc.path);
        d.add_text(f.body, &doc.text);
        d.add_text(f.preview, super::extract::preview(&doc.text, 240));
        d.add_text(f.kind, doc.kind.as_str());
        d.add_u64(f.mtime, doc.mtime);
        if let Some(x) = f.filters {
            let path = Path::new(&doc.path);
            let name = operators::folded_name(path);
            for (dot, _) in name.match_indices('.') {
                d.add_text(x.ext, &name[dot + 1..]);
            }
            let roots = self.roots.read().unwrap_or_else(PoisonError::into_inner);
            let (under, full) = operators::folder_text(path, &roots);
            let parts = under.match_indices('\\').map(|(i, _)| i + 1);
            for from in std::iter::once(0).chain(parts) {
                d.add_text(x.under, &under[from..]);
            }
            d.add_text(x.dir, &full);
            d.add_u64(x.size, u64::from(operators::size_code(doc.size)));
        }
        self.with_writer(|w| {
            w.delete_term(Term::from_field_text(f.key, &key));
            w.add_document(d)?;
            Ok(())
        })
    }

    pub fn remove_path(&self, path: &str) -> Result<(), StoreError> {
        let f = self.fields;
        let key = path_key(path);
        self.with_writer(|w| {
            w.delete_term(Term::from_field_text(f.key, &key));
            Ok(())
        })
    }

    /// Removes every document below the folder `dir`.
    pub fn remove_prefix(&self, dir: &str) -> Result<(), StoreError> {
        let f = self.fields;
        let pattern = format!(
            "{}\\\\.*",
            regex::escape(path_key(dir).trim_end_matches('\\'))
        );
        let q = RegexQuery::from_pattern(&pattern, f.key)?;
        self.with_writer(|w| {
            w.delete_query(Box::new(q))?;
            Ok(())
        })
    }

    /// Like [`commit`](Self::commit) but never merges: used only by the
    /// benchmark to build an index with a chosen number of segments.
    pub fn commit_unmerged(&self) -> Result<(), StoreError> {
        let mut slot = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(w) = slot.as_mut() {
            w.set_merge_policy(Box::new(tantivy::merge_policy::NoMergePolicy));
            w.commit()?;
        }
        *slot = None;
        drop(slot);
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(o) = guard.as_ref() {
            o.reader.reload()?;
        }
        Ok(())
    }

    /// Makes pending changes durable and searchable, then releases the writer.
    pub fn commit(&self) -> Result<(), StoreError> {
        let mut slot = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(w) = slot.as_mut() {
            w.commit()?;
        }
        // Dropping the writer abandons merges it started, so many small commits
        // (one per saved file) would pile up tiny segments. Past a few segments,
        // wait for the merge the commit scheduled.
        let writer = slot.take();
        drop(slot);
        if let Some(w) = writer {
            let many = {
                let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
                guard.as_ref().is_some_and(|o| {
                    o.index
                        .searchable_segment_ids()
                        .is_ok_and(|ids| ids.len() > MERGE_ABOVE_SEGMENTS)
                })
            };
            if many {
                w.wait_merging_threads()?;
            }
        }
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(o) = guard.as_ref() {
            o.reader.reload()?;
        }
        Ok(())
    }

    /// Frees what searching keeps in memory (term dictionaries, stored blocks) by
    /// reopening the readers; the next search reads them again.
    pub fn drop_caches(&self) {
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(o) = guard.as_ref() {
            if o.reader.reload().is_err() {
                crate::log::warn("could not release the content index caches");
            }
        }
    }

    /// Drops everything held open (so the folder can be deleted on Windows).
    pub fn close(&self) {
        *self.writer.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.open.write().unwrap_or_else(PoisonError::into_inner) = None;
        *self.owner.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Number of segments in the index (many tiny ones slow search down).
    pub fn segment_count(&self) -> usize {
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        guard
            .as_ref()
            .map_or(0, |o| o.reader.searcher().segment_readers().len())
    }

    pub fn doc_count(&self) -> u64 {
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        guard.as_ref().map_or(0, |o| o.reader.searcher().num_docs())
    }

    /// Modification time recorded when `path` was last indexed.
    pub fn indexed_mtime(&self, path: &str) -> Option<u64> {
        let f = self.fields;
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        let o = guard.as_ref()?;
        let searcher = o.reader.searcher();
        let q = TermQuery::new(
            Term::from_field_text(f.key, &path_key(path)),
            IndexRecordOption::Basic,
        );
        let top = searcher
            .search(&q, &TopDocs::with_limit(1).order_by_score())
            .ok()?;
        let (_, addr) = top.first()?;
        let doc: TantivyDocument = searcher.doc(*addr).ok()?;
        doc.get_first(f.mtime).and_then(|v| v.as_u64())
    }

    /// Modified time of every indexed file, by lower-cased path (see [`path_key`]).
    pub fn all_mtimes(&self) -> std::collections::HashMap<String, u64> {
        let f = self.fields;
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        let Some(o) = guard.as_ref() else {
            return std::collections::HashMap::new();
        };
        let searcher = o.reader.searcher();
        let n = usize::try_from(searcher.num_docs()).unwrap_or(0);
        let Ok(top) = searcher.search(&AllQuery, &TopDocs::with_limit(n.max(1)).order_by_score())
        else {
            return std::collections::HashMap::new();
        };
        top.into_iter()
            .filter_map(|(_, a)| {
                let d: TantivyDocument = searcher.doc(a).ok()?;
                Some((
                    path_key(d.get_first(f.path)?.as_str()?),
                    d.get_first(f.mtime)?.as_u64()?,
                ))
            })
            .collect()
    }

    /// Every indexed path (original case). Used to find files that vanished.
    pub fn all_paths(&self) -> Vec<String> {
        let f = self.fields;
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        let Some(o) = guard.as_ref() else {
            return Vec::new();
        };
        let searcher = o.reader.searcher();
        let n = usize::try_from(searcher.num_docs()).unwrap_or(0);
        let Ok(top) = searcher.search(&AllQuery, &TopDocs::with_limit(n.max(1)).order_by_score())
        else {
            return Vec::new();
        };
        top.into_iter()
            .filter_map(|(_, a)| {
                let d: TantivyDocument = searcher.doc(a).ok()?;
                Some(d.get_first(f.path)?.as_str()?.to_owned())
            })
            .collect()
    }

    pub fn search(&self, q: &ContentQuery<'_>) -> Result<Vec<ContentHit>, StoreError> {
        let f = self.fields;
        let guard = self.open.read().unwrap_or_else(PoisonError::into_inner);
        let o = guard.as_ref().ok_or(StoreError::Closed)?;
        let mut tokenizer = o.index.tokenizer_for_field(f.body)?;
        let mut words: Vec<String> = Vec::new();
        let mut stream = tokenizer.token_stream(q.text);
        while stream.advance() {
            words.push(stream.token().text.clone());
        }
        if words.is_empty() || q.limit == 0 {
            return Ok(Vec::new());
        }
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (i, w) in words.iter().enumerate() {
            let term = TermQuery::new(
                Term::from_field_text(f.body, w),
                IndexRecordOption::WithFreqs,
            );
            if i + 1 == words.len() && w.chars().count() >= 2 {
                // The word being typed also matches as a prefix.
                let prefix = RegexQuery::from_pattern(&format!("{}.*", regex::escape(w)), f.body)?;
                clauses.push((
                    Occur::Must,
                    Box::new(BooleanQuery::new(vec![
                        (Occur::Should, Box::new(term)),
                        (Occur::Should, Box::new(prefix)),
                    ])),
                ));
            } else {
                clauses.push((Occur::Must, Box::new(term)));
            }
        }
        if q.images_only {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f.kind, DocKind::Ocr.as_str()),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some((from, to)) = q.range {
            clauses.push((
                Occur::Must,
                Box::new(RangeQuery::new(
                    Bound::Included(Term::from_field_u64(f.mtime, from)),
                    Bound::Excluded(Term::from_field_u64(f.mtime, to)),
                )),
            ));
        }
        if let (Some(x), Some(ops)) = (f.filters, q.filter) {
            clauses.extend(filter_clauses(x, ops)?);
        }
        let query = BooleanQuery::new(clauses);
        let searcher = o.reader.searcher();
        let top = searcher.search(&query, &TopDocs::with_limit(q.limit).order_by_score())?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let d: TantivyDocument = searcher.doc(addr)?;
            let text = |fld: Field| {
                d.get_first(fld)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            let owned_u64 = |fld: Field| match d.get_first(fld).map(OwnedValue::from) {
                Some(OwnedValue::U64(v)) => v,
                _ => 0,
            };
            hits.push(ContentHit {
                path: text(f.path),
                kind: if text(f.kind) == "ocr" {
                    DocKind::Ocr
                } else {
                    DocKind::Text
                },
                mtime: owned_u64(f.mtime),
                preview: text(f.preview),
                score,
            });
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(store: &ContentStore, text: &str, typed: &str) -> Vec<String> {
        let ops = operators::parse(typed).ops;
        let mut found: Vec<String> = store
            .search(&ContentQuery {
                text,
                limit: 30,
                filter: Some(&ops),
                ..ContentQuery::default()
            })
            .unwrap()
            .into_iter()
            .map(|h| h.path)
            .collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn operators_are_applied_inside_the_search() {
        let s = ContentStore::open_in_ram().unwrap();
        let root = r"C:\Users\u\Documents";
        s.set_roots(vec![PathBuf::from(root)]);
        // Many better matches than the one that passes the filters.
        for n in 0..100 {
            let path = format!(r"{root}\n{n}.txt");
            s.upsert(&doc(&path, "budget budget budget", DocKind::Text, 1))
                .unwrap();
        }
        let odd = r"R&D - Old #1 ~x (a) [b] {c} +d^e$";
        let pdf = format!(r"{root}\Work\{odd}\Reports\Q1 plan.PDF");
        let mut one = doc(
            &pdf,
            "one budget among very many other words",
            DocKind::Text,
            1,
        );
        one.size = 5 * 1024 * 1024;
        s.upsert(&one).unwrap();
        s.commit().unwrap();
        assert!(s.filters_inside());
        for typed in [
            "ext:pdf",
            "ext:doc,pdf",
            "size:>1mb",
            "size:1mb..10mb",
            "in:work",
            "in:documents/work",
            "in:reports",
            "in:rep",
            r"in:c:\users\u\documents\work",
            r#"in:"r&d - old #1 ~x (a) [b] {c} +d^e$""#,
            "ext:pdf in:work size:>1mb",
        ] {
            assert_eq!(filtered(&s, "budget", typed), [pdf.as_str()], "{typed}");
            let ops = operators::parse(typed).ops;
            // The same answer as the path check used elsewhere.
            if ops.size.is_none() {
                assert!(ops.accepts_path(Path::new(&pdf), &[root.into()]), "{typed}");
            }
        }
        for typed in [
            "ext:df",
            "size:>10mb",
            "in:users",
            "in:ports",
            "in:q1",
            "in:work/ports",
        ] {
            assert!(filtered(&s, "budget", typed).is_empty(), "{typed}");
        }
        assert_eq!(filtered(&s, "budget", "ext:txt").len(), 30);
        // Filters choose the files; they do not change their score.
        let plain = s.search(&q("among")).unwrap();
        let ops = operators::parse("ext:pdf size:>1mb in:work").ops;
        let narrowed = s
            .search(&ContentQuery {
                filter: Some(&ops),
                ..q("among")
            })
            .unwrap();
        assert!((plain[0].score - narrowed[0].score).abs() < 1e-4);
    }

    #[test]
    fn an_index_from_before_the_operators_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("content");
        let key = Key::generate().unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        {
            let (old, f) = schema(false);
            let d = EncryptedDirectory::open(&folder, key.clone()).unwrap();
            let index = Index::open_or_create(d, old).unwrap();
            let mut w: IndexWriter = index.writer_with_num_threads(1, WRITER_HEAP).unwrap();
            let mut d = TantivyDocument::default();
            d.add_text(f.key, r"c:\a.txt");
            d.add_text(f.path, r"C:\a.txt");
            d.add_text(f.body, "budget");
            d.add_text(f.kind, "text");
            d.add_u64(f.mtime, 7);
            w.add_document(d).unwrap();
            w.commit().unwrap();
        }
        let s = ContentStore::open(&folder, &key).unwrap();
        assert!(!s.filters_inside());
        assert_eq!(s.doc_count(), 1);
        s.upsert(&doc(r"C:\b.txt", "budget", DocKind::Text, 8))
            .unwrap();
        s.commit().unwrap();
        // The filter is ignored here; the source checks the paths itself.
        assert_eq!(filtered(&s, "budget", "ext:pdf").len(), 2);
        drop(s);
        let fresh = ContentStore::open(&dir.path().join("new"), &key).unwrap();
        assert!(fresh.filters_inside());
    }

    fn doc(path: &str, text: &str, kind: DocKind, mtime: u64) -> ContentDoc {
        ContentDoc {
            path: path.into(),
            mtime,
            kind,
            size: text.len() as u64,
            text: text.into(),
        }
    }

    fn q(text: &str) -> ContentQuery<'_> {
        ContentQuery {
            text,
            limit: 10,
            ..ContentQuery::default()
        }
    }

    #[test]
    fn finds_words_prefixes_and_requires_all_words() {
        let s = ContentStore::open_in_ram().unwrap();
        s.upsert(&doc(
            r"C:\n\a.txt",
            "The quarterly invoice for Acme",
            DocKind::Text,
            10,
        ))
        .unwrap();
        s.upsert(&doc(
            r"C:\n\b.txt",
            "Holiday photos and invoices",
            DocKind::Text,
            20,
        ))
        .unwrap();
        s.commit().unwrap();
        assert_eq!(s.search(&q("quarterly invoice")).unwrap().len(), 1);
        assert_eq!(
            s.search(&q("invoice")).unwrap().len(),
            2,
            "prefix of the last word"
        );
        assert_eq!(s.search(&q("invo")).unwrap().len(), 2);
        assert!(s.search(&q("quarterly holiday")).unwrap().is_empty());
        assert!(s.search(&q("zebra")).unwrap().is_empty());
        let hit = &s.search(&q("acme")).unwrap()[0];
        assert_eq!(hit.path, r"C:\n\a.txt");
        assert!(hit.preview.starts_with("The quarterly"));
    }

    #[test]
    fn upsert_replaces_remove_deletes_and_prefix_removes_a_folder() {
        let s = ContentStore::open_in_ram().unwrap();
        s.upsert(&doc(r"C:\d\a.txt", "alpha", DocKind::Text, 1))
            .unwrap();
        s.upsert(&doc(r"C:\d\sub\b.txt", "alpha", DocKind::Text, 1))
            .unwrap();
        s.upsert(&doc(r"C:\d2\c.txt", "alpha", DocKind::Text, 1))
            .unwrap();
        s.commit().unwrap();
        assert_eq!(s.doc_count(), 3);
        s.upsert(&doc(r"c:\D\A.txt", "beta", DocKind::Text, 2))
            .unwrap();
        s.commit().unwrap();
        assert_eq!(s.doc_count(), 3, "same path in another case replaces");
        assert_eq!(s.indexed_mtime(r"C:\d\a.txt"), Some(2));
        s.remove_prefix(r"C:\d").unwrap();
        s.commit().unwrap();
        let left: Vec<_> = s.all_paths();
        assert_eq!(left, [r"C:\d2\c.txt"], "C:\\d2 is a different folder");
        s.remove_path(r"C:\d2\c.txt").unwrap();
        s.commit().unwrap();
        assert_eq!(s.doc_count(), 0);
    }

    #[test]
    fn filters_by_kind_and_time_range() {
        let s = ContentStore::open_in_ram().unwrap();
        s.upsert(&doc("a.png", "invoice 4821", DocKind::Ocr, 100))
            .unwrap();
        s.upsert(&doc("b.txt", "invoice 4821", DocKind::Text, 200))
            .unwrap();
        s.commit().unwrap();
        let imgs = ContentQuery {
            images_only: true,
            ..q("invoice")
        };
        assert_eq!(s.search(&imgs).unwrap()[0].path, "a.png");
        let late = ContentQuery {
            range: Some((150, 300)),
            ..q("invoice")
        };
        let r = s.search(&late).unwrap();
        assert_eq!((r.len(), r[0].path.as_str()), (1, "b.txt"));
    }

    #[test]
    fn on_disk_index_is_encrypted_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let key = Key::generate().unwrap();
        {
            let s = ContentStore::open(dir.path(), &key).unwrap();
            s.upsert(&doc(
                "a.txt",
                "zanzibar mongoose telegram",
                DocKind::Text,
                5,
            ))
            .unwrap();
            s.commit().unwrap();
            s.close();
        }
        for e in std::fs::read_dir(dir.path()).unwrap().flatten() {
            if e.path().is_file() {
                let bytes = std::fs::read(e.path()).unwrap();
                assert!(
                    !bytes
                        .windows(8)
                        .any(|w| w == b"zanzibar" || w == b"mongoose"),
                    "plaintext word found in {:?}",
                    e.path()
                );
                assert!(!bytes.windows(5).any(|w| w == b"a.txt"));
            }
        }
        let s = ContentStore::open(dir.path(), &key).unwrap();
        assert_eq!(s.search(&q("mongoose")).unwrap().len(), 1);
        s.close();
        // A different key cannot read it: the index is rebuilt empty.
        let s = ContentStore::open(dir.path(), &Key::generate().unwrap()).unwrap();
        assert_eq!(s.doc_count(), 0);
    }

    #[test]
    fn old_ocr_entries_are_dropped_once_and_text_entries_stay() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("content");
        let key = crate::crypto::Key::generate().unwrap();
        {
            let s = ContentStore::open(&dir, &key).unwrap();
            s.upsert(&doc(
                r"C:
\shot.png",
                "words",
                DocKind::Ocr,
                1,
            ))
            .unwrap();
            s.upsert(&doc(
                r"C:
.txt",
                "words",
                DocKind::Text,
                1,
            ))
            .unwrap();
            s.commit().unwrap();
        }
        // Pretend the index was built before the upscale existed.
        std::fs::write(base.path().join("content.ocr-version"), "1").unwrap();
        let s = ContentStore::open(&dir, &key).unwrap();
        assert_eq!(
            s.indexed_mtime(
                r"C:
\shot.png"
            ),
            None
        );
        assert_eq!(
            s.indexed_mtime(
                r"C:
.txt"
            ),
            Some(1)
        );
        // Fresh OCR entries made now survive the next start.
        s.upsert(&doc(
            r"C:
\shot.png",
            "words",
            DocKind::Ocr,
            2,
        ))
        .unwrap();
        s.commit().unwrap();
        drop(s);
        let s = ContentStore::open(&dir, &key).unwrap();
        assert_eq!(
            s.indexed_mtime(
                r"C:
\shot.png"
            ),
            Some(2)
        );
    }
}
