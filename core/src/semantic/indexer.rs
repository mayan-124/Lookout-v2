//! Feeds the semantic helper: when the user is idle, indexing is not paused
//! and the content worker is not busy, it takes the files the content index
//! knows (most used and most recently changed first), reads their text with the
//! same extractor and OCR, masks secrets, and sends the masked text to the
//! helper in small batches. The helper never sees a file. Excluded files are
//! never read: a file must be in the content index and pass the exclusion
//! rules again.
//!
//! The busy fraction follows `duty` (the same throttle as the other workers);
//! the helper call is part of the busy time because the caller waits for it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::client::{SemDoc, SemanticClient};
use crate::content::extract::{self, FileClass};
use crate::content::idle::IdleSource;
use crate::content::ocr::{Ocr, OcrError};
use crate::content::secrets;
use crate::content::service::ContentStats;
use crate::content::store::ContentStore;
use crate::exclude::Excludes;
use crate::frecency::FrecencyStore;
use crate::index_service::{unix_now, Throttle};
use crate::pause::PauseState;
use crate::ranking;

const POLL: Duration = Duration::from_millis(250);
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(30);
const PASS_EVERY: Duration = Duration::from_secs(600);
const NEEDS_CHUNK: usize = 128;
const BATCH_DOCS: usize = 4;
const BATCH_BYTES: usize = 16 * 1024;
/// More than ~512 tokens of English; the helper cuts at 512 tokens.
const MAX_CHARS: usize = 4000;
const LIST_PAGE: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticState {
    /// The plugin is not turned on.
    Off = 0,
    /// On, but something is missing or broken (see the reason).
    Unavailable = 1,
    /// On and waiting for idleness, an unpaused state, or the content worker.
    Waiting = 2,
    Indexing = 3,
    Ready = 4,
}

impl SemanticState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Unavailable,
            2 => Self::Waiting,
            3 => Self::Indexing,
            4 => Self::Ready,
            _ => Self::Off,
        }
    }
}

/// Shared between the worker, the query source and the status line.
#[derive(Default)]
pub struct SemanticProgress {
    state: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
    reason: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticStatus {
    pub state: SemanticState,
    /// Files handled in the current or last pass.
    pub done: u64,
    pub total: u64,
    /// Why semantic search does nothing right now.
    pub reason: Option<String>,
    /// Where the model, tokenizer and runtime go.
    pub assets_dir: String,
}

impl SemanticProgress {
    fn set(&self, s: SemanticState, reason: Option<String>) {
        self.state.store(s as u8, Ordering::Release);
        *self.reason.lock().unwrap_or_else(PoisonError::into_inner) = reason;
    }

    pub fn state(&self) -> SemanticState {
        SemanticState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub fn snapshot(&self, assets_dir: &Path) -> SemanticStatus {
        SemanticStatus {
            state: self.state(),
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            reason: self
                .reason
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            assets_dir: assets_dir.display().to_string(),
        }
    }
}

pub struct SemanticConfig {
    pub client: SemanticClient,
    pub roots: Vec<PathBuf>,
    pub excludes: Excludes,
    pub store: Arc<ContentStore>,
    pub content: Arc<ContentStats>,
    pub ocr: Option<Arc<dyn Ocr>>,
    pub idle: Arc<dyn IdleSource>,
    pub idle_secs: u32,
    pub duty: f32,
    pub pause: PauseState,
    pub frecency: Arc<FrecencyStore>,
    pub progress: Arc<SemanticProgress>,
}

pub struct SemanticIndexer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct Worker {
    cfg: SemanticConfig,
    stop: Arc<AtomicBool>,
    throttle: Throttle,
}

impl SemanticIndexer {
    pub fn start(cfg: SemanticConfig) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut worker = Worker {
            throttle: Throttle::new(cfg.duty),
            cfg,
            stop: stop.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("lookout-semantic".into())
            .spawn(move || worker.run())?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SemanticIndexer {
    fn drop(&mut self) {
        self.stop();
    }
}

enum PassEnd {
    Finished,
    Stopped,
    Failed(String),
}

impl Worker {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn sleep(&self, d: Duration) {
        let end = Instant::now() + d;
        while !self.stopped() && Instant::now() < end {
            std::thread::sleep(POLL.min(d));
        }
    }

    fn run(&mut self) {
        let mut last_sig: Option<(u64, u64)> = None;
        let mut last_pass: Option<Instant> = None;
        let mut retry_at: Option<Instant> = None;
        while !self.stopped() {
            self.sleep(POLL);
            let c = &self.cfg;
            if !c.client.usable() {
                c.progress.set(SemanticState::Off, None);
                last_sig = None;
                continue;
            }
            if let Some(why) = c.client.missing_reason() {
                c.progress.set(SemanticState::Unavailable, Some(why));
                last_sig = None;
                continue;
            }
            if retry_at.is_some_and(|t| Instant::now() < t) {
                continue;
            }
            let sig = (c.store.doc_count(), c.content.docs.load(Ordering::Relaxed));
            let due = last_sig != Some(sig) || last_pass.is_none_or(|t| t.elapsed() > PASS_EVERY);
            if !due {
                continue;
            }
            if c.content.busy.load(Ordering::Acquire) {
                c.progress.set(SemanticState::Waiting, None);
                continue;
            }
            match self.pass() {
                PassEnd::Finished => {
                    last_sig = Some(sig);
                    last_pass = Some(Instant::now());
                    retry_at = None;
                    self.cfg.progress.set(SemanticState::Ready, None);
                }
                PassEnd::Stopped => return,
                PassEnd::Failed(why) => {
                    self.cfg.progress.set(SemanticState::Unavailable, Some(why));
                    retry_at = Some(Instant::now() + RETRY_AFTER_FAILURE);
                }
            }
        }
    }

    /// Blocks until indexing may run: not paused and the user idle. False if stopping.
    fn gate(&self) -> bool {
        let mut waiting = false;
        loop {
            if self.stopped() {
                return false;
            }
            let paused = self.cfg.pause.is_paused();
            let busy = self.cfg.idle.idle_for()
                < Duration::from_secs(u64::from(self.cfg.idle_secs))
                || self.cfg.content.busy.load(Ordering::Acquire);
            if !paused && !busy {
                if waiting {
                    self.cfg.progress.set(SemanticState::Indexing, None);
                }
                return true;
            }
            waiting = true;
            self.cfg.progress.set(SemanticState::Waiting, None);
            std::thread::sleep(POLL);
        }
    }

    fn excluded(&self, path: &Path) -> bool {
        match self.cfg.roots.iter().find(|r| path.starts_with(r)) {
            Some(root) => self.cfg.excludes.excludes_path(path, root),
            None => true,
        }
    }

    /// Files known to the content index, best candidates first: recently
    /// changed and often opened.
    fn candidates(&self) -> Vec<(String, u64)> {
        let now = unix_now();
        let now_i = i64::from(now);
        let mut v: Vec<(String, u64, f32)> = self
            .cfg
            .store
            .all_paths()
            .into_iter()
            .filter_map(|p| {
                let m = self.cfg.store.indexed_mtime(&p)?;
                let used = self
                    .cfg
                    .frecency
                    .score(&format!("file:{p}"), now_i)
                    .min(4.0) as f32
                    / 4.0;
                let prio = ranking::recency(u32::try_from(m).unwrap_or(0), now) + used;
                Some((p, m, prio))
            })
            .collect();
        v.sort_by(|a, b| b.2.total_cmp(&a.2));
        v.into_iter().map(|(p, m, _)| (p, m)).collect()
    }

    fn pass(&mut self) -> PassEnd {
        let stop = self.stop.clone();
        let cancelled = move || stop.load(Ordering::Acquire);
        let progress = self.cfg.progress.clone();
        match self.cfg.client.status(&cancelled) {
            Ok(s) if s.ready => {}
            Ok(s) => return PassEnd::Failed(s.reason.unwrap_or_else(|| "not ready".into())),
            Err(e) => return PassEnd::Failed(e.to_string()),
        }
        let cands = self.candidates();
        progress.total.store(cands.len() as u64, Ordering::Relaxed);
        progress.done.store(0, Ordering::Relaxed);
        let mut batch: Vec<SemDoc> = Vec::new();
        let mut bytes = 0usize;
        for chunk in cands.chunks(NEEDS_CHUNK) {
            if !self.gate() {
                return PassEnd::Stopped;
            }
            let need: HashSet<String> = match self.cfg.client.needs(chunk, &cancelled) {
                Ok(n) => n.into_iter().collect(),
                Err(e) => return PassEnd::Failed(e.to_string()),
            };
            progress.done.fetch_add(
                chunk.len().saturating_sub(need.len()) as u64,
                Ordering::Relaxed,
            );
            for (path, mtime) in chunk.iter().filter(|(p, _)| need.contains(p)) {
                if !self.gate() {
                    return PassEnd::Stopped;
                }
                let started = Instant::now();
                let doc = self.read_doc(path, *mtime);
                self.throttle.after_work(started.elapsed());
                let Some(doc) = doc else {
                    progress.done.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                bytes += doc.text.len();
                batch.push(doc);
                if batch.len() >= BATCH_DOCS || bytes >= BATCH_BYTES {
                    if let Err(why) = self.send(&mut batch, &cancelled) {
                        return PassEnd::Failed(why);
                    }
                    bytes = 0;
                }
            }
        }
        if let Err(why) = self.send(&mut batch, &cancelled) {
            return PassEnd::Failed(why);
        }
        self.sweep(&cancelled)
    }

    fn send(
        &mut self,
        batch: &mut Vec<SemDoc>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        if batch.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let n = batch.len() as u64;
        let r = self.cfg.client.index_batch(batch, cancelled);
        self.throttle.after_work(started.elapsed());
        batch.clear();
        r.map_err(|e| e.to_string())?;
        self.cfg.progress.done.fetch_add(n, Ordering::Relaxed);
        Ok(())
    }

    /// Text of one file, masked and cut. `None`: nothing to send (gone, excluded
    /// or unreadable now). Files without text are sent with empty text, so the
    /// helper remembers them and does not ask again.
    fn read_doc(&self, path: &str, mtime: u64) -> Option<SemDoc> {
        let p = Path::new(path);
        let class = extract::classify(p);
        if class == FileClass::Skip || self.excluded(p) {
            return None;
        }
        let text = match class {
            FileClass::Text => extract::read_text(p).ok().flatten().unwrap_or_default(),
            FileClass::Image => match self.cfg.ocr.as_deref().map(|o| o.recognize(p)) {
                Some(Ok(t)) => t,
                Some(Err(OcrError::Unavailable)) | None => return None,
                Some(Err(_)) => String::new(),
            },
            FileClass::Office | FileClass::Skip => return None,
        };
        let (masked, _) = secrets::mask(&text);
        let text: String = masked.chars().take(MAX_CHARS).collect();
        let name = p
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Some(SemDoc {
            path: path.to_owned(),
            name,
            mtime,
            text,
        })
    }

    /// Drops vectors of files that left the content index.
    fn sweep(&mut self, cancelled: &dyn Fn() -> bool) -> PassEnd {
        let mut after: Option<String> = None;
        loop {
            if cancelled() {
                return PassEnd::Stopped;
            }
            let page = match self.cfg.client.list(after.as_deref(), LIST_PAGE) {
                Ok(p) => p,
                Err(e) => return PassEnd::Failed(e.to_string()),
            };
            let Some(last) = page.last().cloned() else {
                return PassEnd::Finished;
            };
            let gone: Vec<String> = page
                .into_iter()
                .filter(|p| self.cfg.store.indexed_mtime(p).is_none())
                .collect();
            if !gone.is_empty() {
                if let Err(e) = self.cfg.client.remove(&gone) {
                    return PassEnd::Failed(e.to_string());
                }
            }
            after = Some(last);
        }
    }
}
