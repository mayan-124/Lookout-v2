//! The background worker that reads file contents and screenshots into the
//! full-text index.
//!
//! Every unit of work (reading a directory, extracting a file, running OCR)
//! first passes the *gate*: indexing must not be paused and the user must have
//! been idle for `idle_secs`. While the gate is closed the worker does nothing
//! but poll. A duty cycle keeps the busy fraction near `duty` (default 10%).
//!
//! Masked content is never indexed: text goes through `secrets::mask` before
//! it reaches the store; excluded folders and sensitive file types are never
//! opened at all.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::extract::{self, FileClass};
use super::idle::IdleSource;
use super::ocr::{Ocr, OcrError};
use super::office::{self, OfficeOutcome, OfficeText};
use super::secrets;
use super::store::{ContentDoc, ContentStore, DocKind};
use crate::exclude::Excludes;
use crate::index_service::Throttle;
use crate::pause::PauseState;

const POLL: Duration = Duration::from_millis(100);
/// "Index now" commits less often: every commit costs about as much as
/// indexing a few dozen files, and nobody searches mid-run.
const COMMIT_EVERY_BOOST: usize = 1000;
/// A file the watcher reports is indexed at once (no idle wait, no duty cap)
/// below this size...
const FAST_MAX_BYTES: u64 = 1024 * 1024;
/// ...unless more paths than this arrive together: that is a bulk change.
const FAST_BATCH_MAX: usize = 20;
/// Files handled between looks at how idle the user is.
const CHUNK: usize = 64;
/// At most this many files are queued by one scan (memory); the rest is found
/// by the next round.
const MAX_QUEUE: usize = 500_000;
/// Fast speed uses several threads only after this long without input.
const DEEP_IDLE_SECS: u64 = 60;
const MAX_FAST_THREADS: usize = 3;
/// Pictures smaller than this on both sides (icons) are not sent to OCR.
const MIN_OCR_SIDE: u32 = 64;
/// OCR text kept for pictures seen again (identical copies).
const OCR_CACHE_BYTES: usize = 16 * 1024 * 1024;
/// How much of an image is looked at for its pixel size.
const HEADER_BYTES: usize = 128 * 1024;

/// What the worker is doing, for the health panel.
pub const STATE_UP_TO_DATE: u8 = 0;
pub const STATE_RUNNING: u8 = 1;
pub const STATE_WAITING_IDLE: u8 = 2;
pub const STATE_PAUSED: u8 = 3;

pub struct ContentConfig {
    pub roots: Vec<PathBuf>,
    pub excludes: Excludes,
    pub store: Arc<ContentStore>,
    pub ocr: Option<Arc<dyn Ocr>>,
    /// Reads the text of .docx, .xlsx and .pptx files (the Office helper).
    pub office: Option<Arc<dyn OfficeText>>,
    pub idle: Arc<dyn IdleSource>,
    pub idle_secs: u32,
    pub duty: f32,
    /// Files indexed between saves (see `IndexSpeed::commit_every`).
    pub commit_every: usize,
    pub pause: PauseState,
    /// Size limit of the index in bytes; see `Settings::content_index_max_mb`.
    pub max_bytes: u64,
    /// Fast speed: after a minute without input, several files are read at once.
    pub fast: bool,
}

/// Delay unit for retries after helper timeouts: waits are 1x then 10x this.
pub static TIMEOUT_BACKOFF_SECS: AtomicU64 = AtomicU64::new(60);
const MAX_TIMEOUT_ATTEMPTS: u8 = 3;

/// Failures of one file version (path key holds it); in memory only.
#[derive(Debug, Clone, Copy)]
struct HelperFail {
    mtime: u64,
    len: u64,
    crashes: u8,
    timeouts: u8,
    last_timeout: Option<Instant>,
}

/// Counters for tests, the bench and the status line.
#[derive(Debug, Default)]
pub struct ContentStats {
    /// Text files read.
    pub extractions: AtomicU64,
    /// Images sent to OCR.
    pub ocr_runs: AtomicU64,
    /// Documents written to the index (text and OCR).
    pub docs: AtomicU64,
    pub errors: AtomicU64,
    /// True while a scan or a queued change is being worked on.
    pub busy: AtomicBool,
    /// Scan rounds finished (each round that did work).
    pub scans_done: AtomicU64,
    /// One of the `STATE_*` constants.
    pub state: AtomicU8,
    /// Files found by the current scan that are not yet done.
    pub queued: AtomicU64,
    /// Changed paths waiting to be handled.
    pub pending: AtomicU64,
    /// Documents (text, Office, PDF) to read in this scan, and how many are done.
    pub read_total: AtomicU64,
    pub read_done: AtomicU64,
    /// Pictures to read the text of in this scan (they come last), and how many are done.
    pub ocr_total: AtomicU64,
    pub ocr_done: AtomicU64,
    /// Pictures not sent to OCR: icons, and copies of one already read.
    pub ocr_skipped: AtomicU64,
    /// "Index now": ignore idle and the duty cap until the queue is empty.
    pub boost: AtomicBool,
    /// A rebuild was asked for: the worker drops what it is doing, empties the
    /// index and reads every file again.
    pub rebuild: AtomicBool,
    /// Rebuilds that have read everything again.
    pub rebuilt: AtomicU64,
    /// The most recent error text, if any.
    pub last_error: Mutex<Option<String>>,
    /// Time spent per stage, in nanoseconds (for the speed benchmark).
    pub stages: StageTimes,
    /// Size limit of the index in bytes.
    pub max_bytes: AtomicU64,
    /// Size of the index as of the last save.
    pub size_bytes: AtomicU64,
}

/// Nanoseconds spent in each stage of content indexing.
#[derive(Debug, Default)]
pub struct StageTimes {
    pub walk: AtomicU64,
    pub unchanged_check: AtomicU64,
    pub read: AtomicU64,
    pub extract: AtomicU64,
    pub mask: AtomicU64,
    pub ocr: AtomicU64,
    pub write: AtomicU64,
    pub commit: AtomicU64,
    /// Sleeping for the duty cap.
    pub throttle: AtomicU64,
    /// Read + decode time and file count per kind of file.
    pub text_files: AtomicU64,
    pub text_ns: AtomicU64,
    pub office_files: AtomicU64,
    pub office_ns: AtomicU64,
    pub pdf_files: AtomicU64,
    pub pdf_ns: AtomicU64,
}

impl StageTimes {
    pub fn add(counter: &AtomicU64, since: Instant) {
        counter.fetch_add(
            u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

impl ContentStats {
    pub fn record_error(&self, msg: String) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        *self
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(msg);
    }

    /// True once the index has reached its size limit: nothing new is added.
    pub fn full(&self) -> bool {
        let max = self.max_bytes.load(Ordering::Relaxed);
        max > 0 && self.size_bytes.load(Ordering::Relaxed) >= max
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// How one file is to be handled.
#[derive(Clone, Copy)]
struct Mode {
    /// The watcher said it changed: index it even if the modified time (whole
    /// seconds) looks unchanged.
    force: bool,
    /// May skip the idle wait and the duty cap if it is small text.
    fast: bool,
}
const SCAN: Mode = Mode {
    force: false,
    fast: false,
};

enum Job {
    Path(PathBuf),
    Rescan,
}

/// Sends changed paths to a running [`ContentService`].
#[derive(Clone)]
pub struct Notifier(Sender<Job>);

impl Notifier {
    pub fn notify(&self, path: &Path) {
        let _ = self.0.send(Job::Path(path.to_owned()));
    }
}

pub struct ContentService {
    stop: Arc<AtomicBool>,
    tx: Sender<Job>,
    thread: Option<JoinHandle<()>>,
    stats: Arc<ContentStats>,
}

struct Worker {
    cfg: ContentConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<ContentStats>,
    throttle: Throttle,
    uncommitted: usize,
    rx: Receiver<Job>,
    pending: HashSet<PathBuf>,
    rescan: bool,
    /// Modified times of everything indexed, loaded once per scan so the
    /// "unchanged?" test is a map lookup instead of an index search per file.
    known: Option<HashMap<String, u64>>,
    ext: Arc<Extractor>,
    /// The last scan's queue was capped; scan again for the rest.
    more: bool,
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl ContentService {
    pub fn start(cfg: ContentConfig) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(ContentStats::default());
        stats.max_bytes.store(cfg.max_bytes, Ordering::Relaxed);
        stats
            .size_bytes
            .store(cfg.store.size_bytes(), Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<Job>();
        let _ = tx.send(Job::Rescan);
        let ext = Arc::new(Extractor {
            stats: stats.clone(),
            ocr: cfg.ocr.clone(),
            office: cfg.office.clone(),
            crashed: Mutex::new(HashMap::new()),
            ocr_cache: Mutex::new(OcrCache::default()),
        });
        let mut worker = Worker {
            throttle: Throttle::new(cfg.duty),
            cfg,
            stop: stop.clone(),
            stats: stats.clone(),
            uncommitted: 0,
            rx,
            pending: HashSet::new(),
            rescan: false,
            known: None,
            ext,
            more: false,
        };
        let thread = std::thread::Builder::new()
            .name("lookout-content".into())
            .spawn(move || {
                for _ in 0..crate::guard::RESTARTS {
                    if crate::guard::contain("reading file contents failed", || worker.run())
                        .is_some()
                    {
                        break;
                    }
                    worker.known = None;
                    worker.stats.busy.store(false, Ordering::Release);
                    worker.clear_progress();
                }
            })?;
        Ok(Self {
            stop,
            tx,
            thread: Some(thread),
            stats,
        })
    }

    pub fn stats(&self) -> Arc<ContentStats> {
        self.stats.clone()
    }

    /// Queues a path the file watcher reported (created, changed or removed).
    pub fn notify_path(&self, path: &Path) {
        let _ = self.tx.send(Job::Path(path.to_owned()));
    }

    /// A cheap handle the file watcher's callback can own.
    pub fn notifier(&self) -> Notifier {
        Notifier(self.tx.clone())
    }

    /// Changes the size limit. Raising it past the current size resumes
    /// indexing: a rescan picks up what was skipped while the index was full.
    pub fn set_max_bytes(&self, max_bytes: u64) {
        self.stats.max_bytes.store(max_bytes, Ordering::Relaxed);
        let _ = self.tx.send(Job::Rescan);
    }

    pub fn request_rescan(&self) {
        let _ = self.tx.send(Job::Rescan);
    }

    /// "Index now": scan at once, without waiting for idle and without the
    /// duty cap, until nothing is left (or [`stop_index_now`](Self::stop_index_now)).
    pub fn index_now(&self) {
        self.stats.boost.store(true, Ordering::Release);
        let _ = self.tx.send(Job::Rescan);
    }

    /// Empties the content index and reads every file again, without waiting
    /// for the user to be away.
    pub fn rebuild(&self) {
        self.stats.rebuild.store(true, Ordering::Release);
        let _ = self.tx.send(Job::Rescan);
    }

    pub fn stop_index_now(&self) {
        self.stats.boost.store(false, Ordering::Release);
    }

    /// Stops the worker and waits for it (the in-flight file finishes first).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for ContentService {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Worker {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn boosting(&self) -> bool {
        self.stats.boost.load(Ordering::Acquire)
    }

    fn set_state(&self, s: u8) {
        self.stats.state.store(s, Ordering::Release);
    }

    fn take(&mut self, job: Job) {
        match job {
            Job::Path(p) => {
                self.pending.insert(p);
            }
            Job::Rescan => self.rescan = true,
        }
    }

    fn drain(&mut self) {
        while let Ok(job) = self.rx.try_recv() {
            self.take(job);
        }
        self.stats
            .pending
            .store(self.pending.len() as u64, Ordering::Relaxed);
    }

    fn rebuild_wanted(&self) -> bool {
        self.stats.rebuild.load(Ordering::Acquire)
    }

    fn run(&mut self) {
        let mut rebuilding = false;
        while !self.stopped() {
            match self.rx.recv_timeout(POLL) {
                Ok(job) => self.take(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.drain();
            if self.stats.rebuild.swap(false, Ordering::AcqRel) {
                if self.cfg.store.clear().is_err() {
                    crate::log::error("the content index could not be emptied");
                }
                self.uncommitted = 0;
                self.stats.docs.store(0, Ordering::Relaxed);
                self.stats.boost.store(true, Ordering::Release);
                self.rescan = true;
                rebuilding = true;
            }
            if !self.rescan && self.pending.is_empty() {
                continue;
            }
            self.stats.busy.store(true, Ordering::Release);
            self.set_state(STATE_RUNNING);
            if self.rescan {
                self.rescan = false;
                self.full_scan();
            }
            let bulk = self.pending.len() > FAST_BATCH_MAX;
            let paths: Vec<PathBuf> = self.pending.drain().collect();
            for p in paths {
                if self.stopped() {
                    break;
                }
                self.handle_path(
                    &p,
                    Mode {
                        force: true,
                        fast: !bulk,
                    },
                );
            }
            self.stats.pending.store(0, Ordering::Relaxed);
            self.commit();
            self.drain();
            if rebuilding && !self.rescan && !self.rebuild_wanted() && !self.stopped() {
                rebuilding = false;
                self.stats.rebuilt.fetch_add(1, Ordering::AcqRel);
            }
            if !self.rescan {
                // The queue is empty: "Index now" is over.
                self.stats.boost.store(false, Ordering::Release);
            }
            self.set_state(STATE_UP_TO_DATE);
            self.stats.scans_done.fetch_add(1, Ordering::Release);
            self.stats.busy.store(false, Ordering::Release);
        }
        self.commit();
    }

    /// Blocks while indexing is paused. False if the service is stopping.
    fn wait_unpaused(&mut self) -> bool {
        while self.cfg.pause.is_paused() {
            if self.stopped() {
                return false;
            }
            self.set_state(STATE_PAUSED);
            std::thread::sleep(POLL);
        }
        !self.stopped()
    }

    /// While the gate is closed, small changed text files (and removals) are
    /// still handled at once. Never while paused.
    fn serve_fast(&mut self) {
        self.drain();
        if self.cfg.pause.is_paused() || self.pending.len() > FAST_BATCH_MAX {
            return;
        }
        let ready: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|p| self.fast_eligible(p))
            .cloned()
            .collect();
        for p in ready {
            self.pending.remove(&p);
            self.handle_path(
                &p,
                Mode {
                    force: true,
                    fast: true,
                },
            );
        }
        self.stats
            .pending
            .store(self.pending.len() as u64, Ordering::Relaxed);
        self.commit();
    }

    fn fast_eligible(&self, path: &Path) -> bool {
        if self.excluded(path) {
            return true; // only removes what may be there
        }
        match fs::symlink_metadata(path) {
            Err(_) => true, // gone: only a removal
            Ok(m) => {
                m.is_file()
                    && m.len() < FAST_MAX_BYTES
                    && extract::classify(path) == FileClass::Text
            }
        }
    }

    /// Blocks until the gate is open (not paused, user idle). Returns false if
    /// the service is stopping. Anything already extracted is committed once
    /// before waiting, so a pause never strands finished work in memory.
    fn gate(&mut self) -> bool {
        let mut flushed = false;
        loop {
            if self.stopped() || self.rebuild_wanted() {
                return false;
            }
            let paused = self.cfg.pause.is_paused();
            let busy =
                self.cfg.idle.idle_for() < Duration::from_secs(u64::from(self.cfg.idle_secs));
            if !paused && (!busy || self.boosting()) {
                self.set_state(STATE_RUNNING);
                return true;
            }
            self.set_state(if paused {
                STATE_PAUSED
            } else {
                STATE_WAITING_IDLE
            });
            if !flushed {
                self.commit();
                flushed = true;
            }
            self.serve_fast();
            std::thread::sleep(POLL);
        }
    }

    fn commit(&mut self) {
        if self.uncommitted > 0 {
            let t = Instant::now();
            let r = self.cfg.store.commit();
            StageTimes::add(&self.stats.stages.commit, t);
            if let Err(e) = r {
                crate::log::error("the content index could not be saved");
                self.stats
                    .record_error(format!("could not save the file text: {e}"));
            }
            self.uncommitted = 0;
            self.refresh_size();
        }
    }

    /// Re-reads the index size. It grows only when something is saved, so this
    /// runs after saves; the limit may be passed by at most one save's worth.
    fn refresh_size(&self) {
        self.stats
            .size_bytes
            .store(self.cfg.store.size_bytes(), Ordering::Relaxed);
    }

    fn excluded(&self, path: &Path) -> bool {
        self.cfg.excludes.excluded_under(path, &self.cfg.roots)
    }

    /// Removes documents that are in a skipped place (or under no root) before
    /// anything is read, so a stricter skip list applies at once.
    fn tidy(&mut self) {
        for p in self.cfg.store.all_paths() {
            if self.excluded(Path::new(&p)) {
                let _ = self.cfg.store.remove_path(&p);
                self.uncommitted += 1;
            }
        }
        self.commit();
    }

    /// Walks every root, queues what is new or changed (documents first, then
    /// images), works the queue, and drops documents of files that are gone,
    /// excluded or no longer under a root.
    fn full_scan(&mut self) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut complete = true;
        self.more = false;
        self.refresh_size();
        self.tidy();
        self.known = Some(self.cfg.store.all_mtimes());
        let mut queue = Vec::new();
        for root in self.cfg.roots.clone() {
            if !self.collect(&root, &mut seen, &mut queue) {
                complete = false;
                break;
            }
        }
        if complete && !self.run_queue(queue) {
            complete = false;
        }
        if complete && !self.stopped() {
            let gone: Vec<String> = self
                .known
                .iter()
                .flat_map(HashMap::keys)
                .filter(|k| !seen.contains(*k))
                .cloned()
                .collect();
            for p in gone {
                let _ = self.cfg.store.remove_path(&p);
                self.uncommitted += 1;
            }
            if self.more {
                // The queue was capped (memory): the rest is found next round.
                self.rescan = true;
            }
        }
        self.known = None;
        self.clear_progress();
        self.commit();
    }

    fn clear_progress(&self) {
        let s = &self.stats;
        for c in [
            &s.queued,
            &s.read_total,
            &s.read_done,
            &s.ocr_total,
            &s.ocr_done,
        ] {
            c.store(0, Ordering::Relaxed);
        }
    }

    /// Walks `dir` and adds every file that needs reading to `out`. Online-only
    /// cloud files are never opened (that would download them). Returns false
    /// if interrupted by a stop.
    fn collect(&mut self, dir: &Path, seen: &mut HashSet<String>, out: &mut Vec<Cand>) -> bool {
        let mut stack = vec![dir.to_owned()];
        while let Some(d) = stack.pop() {
            if !self.gate() {
                return false;
            }
            let walk_t = Instant::now();
            let Ok(rd) = fs::read_dir(&d) else { continue };
            for ent in rd.flatten() {
                let path = ent.path();
                let name = ent.file_name().to_string_lossy().into_owned();
                let Ok(ft) = ent.file_type() else { continue };
                if ft.is_symlink()
                    || self.cfg.excludes.is_excluded(&name)
                    || self.cfg.excludes.is_excluded_folder(&path)
                    || (ft.is_dir() && crate::exclude::skips_contents(&path, &name))
                {
                    continue;
                }
                let meta = ent.metadata().ok();
                if ft.is_file() {
                    seen.insert(super::store::path_key(&path.to_string_lossy()));
                }
                if meta.as_ref().is_some_and(is_online_only) {
                    continue;
                }
                if ft.is_dir() {
                    stack.push(path);
                } else if ft.is_file() {
                    if out.len() >= MAX_QUEUE {
                        self.more = true;
                    } else if let Some(c) = self.prepare(&path, SCAN, meta) {
                        out.push(c);
                    }
                }
            }
            StageTimes::add(&self.stats.stages.walk, walk_t);
        }
        true
    }

    /// Works a queue: documents first (newest first), then images. One file at
    /// a time under the gate and the duty cap; in Fast speed, after a minute
    /// without input, several at once (back to one the moment input arrives).
    /// Returns false if stopping.
    fn run_queue(&mut self, mut queue: Vec<Cand>) -> bool {
        queue.sort_by(|a, b| {
            (a.class == FileClass::Image)
                .cmp(&(b.class == FileClass::Image))
                .then(b.mtime.cmp(&a.mtime))
        });
        let images = queue.iter().filter(|c| c.class == FileClass::Image).count() as u64;
        let s = &self.stats;
        s.queued.store(queue.len() as u64, Ordering::Relaxed);
        s.read_total
            .store(queue.len() as u64 - images, Ordering::Relaxed);
        s.read_done.store(0, Ordering::Relaxed);
        s.ocr_total.store(images, Ordering::Relaxed);
        s.ocr_done.store(0, Ordering::Relaxed);
        let mut at = 0;
        while at < queue.len() {
            if !self.gate() {
                return false;
            }
            let end = (at + CHUNK).min(queue.len());
            if self.deep_idle() {
                at += self.parallel_chunk(&queue[at..end]);
                continue;
            }
            for c in &queue[at..end] {
                if !self.gate() {
                    return false;
                }
                if !self.stats.full() {
                    let started = Instant::now();
                    let out = self.ext.extract_contained(c);
                    self.finish(c, out, false);
                    if !self.boosting() {
                        let t = Instant::now();
                        self.throttle.after_work(started.elapsed());
                        StageTimes::add(&self.stats.stages.throttle, t);
                    }
                }
                self.progress(c.class);
            }
            at = end;
        }
        true
    }

    fn progress(&self, class: FileClass) {
        let s = &self.stats;
        let done = if class == FileClass::Image {
            &s.ocr_done
        } else {
            &s.read_done
        };
        done.fetch_add(1, Ordering::Relaxed);
        let _ = s
            .queued
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |q| {
                Some(q.saturating_sub(1))
            });
    }

    fn deep_idle_needed(&self) -> Duration {
        Duration::from_secs(DEEP_IDLE_SECS.max(u64::from(self.cfg.idle_secs)))
    }

    /// Fast speed and no input for a minute (or the user's idle time, if
    /// longer): more than one file may be read at once.
    fn deep_idle(&self) -> bool {
        self.cfg.fast
            && !self.cfg.pause.is_paused()
            && self.cfg.idle.idle_for() >= self.deep_idle_needed()
    }

    /// Reads a chunk with several threads while the user stays away. Returns
    /// how many items were handled (the rest is left for the single-file path).
    fn parallel_chunk(&mut self, items: &[Cand]) -> usize {
        let threads = std::thread::available_parallelism()
            .map_or(2, |n| n.get() / 2)
            .clamp(2, MAX_FAST_THREADS);
        let next = AtomicUsize::new(0);
        let (tx, rx) = mpsc::sync_channel::<(usize, Outcome)>(threads * 2);
        let (ext, idle, pause, stop) = (
            self.ext.clone(),
            self.cfg.idle.clone(),
            self.cfg.pause.clone(),
            self.stop.clone(),
        );
        let need = self.deep_idle_needed();
        let full = self.stats.full();
        std::thread::scope(|scope| {
            for _ in 0..threads {
                let tx = tx.clone();
                let (ext, idle, pause, stop, next) = (&ext, &idle, &pause, &stop, &next);
                scope.spawn(move || loop {
                    // Input, a pause or a stop ends this thread at once.
                    if stop.load(Ordering::Acquire) || pause.is_paused() || idle.idle_for() < need {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::AcqRel);
                    if i >= items.len() {
                        break;
                    }
                    let out = if full {
                        None
                    } else {
                        ext.extract_contained(&items[i])
                    };
                    if tx.send((i, out)).is_err() {
                        break;
                    }
                });
            }
            drop(tx);
            for (i, out) in rx {
                self.finish(&items[i], out, false);
                self.progress(items[i].class);
            }
        });
        next.load(Ordering::Acquire).min(items.len())
    }

    /// Handles one watcher event. Returns nothing: errors are counted.
    fn handle_path(&mut self, path: &Path, mode: Mode) {
        if self.excluded(path) {
            // An event inside an excluded place: make sure nothing of it is indexed.
            let _ = self.cfg.store.remove_path(&path.to_string_lossy());
            self.uncommitted += 1;
            return;
        }
        match fs::symlink_metadata(path) {
            Err(_) => {
                let p = path.to_string_lossy();
                let _ = self.cfg.store.remove_path(&p);
                let _ = self.cfg.store.remove_prefix(&p);
                self.uncommitted += 1;
            }
            Ok(m) if m.file_type().is_symlink() => {}
            Ok(m) if m.is_dir() => {
                let mut seen = HashSet::new();
                let mut queue = Vec::new();
                if self.collect(path, &mut seen, &mut queue) {
                    self.run_queue(queue);
                    self.clear_progress();
                }
            }
            Ok(_) => {
                self.index_file(path, mode);
            }
        }
    }

    /// Decides whether a file needs reading: `None` if it is skipped, excluded,
    /// unchanged, or cannot be read right now.
    fn prepare(&self, path: &Path, mode: Mode, meta: Option<fs::Metadata>) -> Option<Cand> {
        let class = extract::classify(path);
        if class == FileClass::Skip || self.excluded(path) {
            return None;
        }
        let check_t = Instant::now();
        let meta = match meta.filter(fs::Metadata::is_file) {
            Some(m) => m,
            None => fs::metadata(path).ok()?,
        };
        let mtime = meta.modified().map_or(0, secs);
        let key = path.to_string_lossy();
        let indexed = match &self.known {
            Some(known) if !mode.force => known.get(&super::store::path_key(&key)).copied(),
            _ if mode.force => None,
            _ => self.cfg.store.indexed_mtime(&key),
        };
        StageTimes::add(&self.stats.stages.unchanged_check, check_t);
        if indexed == Some(mtime) {
            return None;
        }
        if class == FileClass::Image && self.cfg.ocr.is_none() {
            return None;
        }
        if class == FileClass::Office && !self.cfg.office.as_ref().is_some_and(|o| o.available()) {
            return None;
        }
        Some(Cand {
            path: path.to_owned(),
            mtime,
            len: meta.len(),
            class,
        })
    }

    /// Indexes one file if it is new or changed (watcher events). Returns false if stopping.
    fn index_file(&mut self, path: &Path, mode: Mode) -> bool {
        let Some(c) = self.prepare(path, mode, None) else {
            return true;
        };
        if self.stats.full() {
            // Index full: no new or updated content or OCR. Skipped files are
            // not recorded, so a later scan with a higher limit reads them.
            return true;
        }
        let instant = mode.fast && c.class == FileClass::Text && c.len < FAST_MAX_BYTES;
        let ready = if instant {
            self.wait_unpaused()
        } else {
            self.gate()
        };
        if !ready {
            return false;
        }
        let started = Instant::now();
        let out = self.ext.extract(&c);
        self.finish(&c, out, instant);
        if !instant && !self.boosting() {
            let t = Instant::now();
            self.throttle.after_work(started.elapsed());
            StageTimes::add(&self.stats.stages.throttle, t);
        }
        true
    }

    /// Masks, writes and counts what was read from one file.
    fn finish(&mut self, c: &Cand, outcome: Outcome, instant: bool) {
        let Some((kind, text)) = outcome else { return };
        let mask_t = Instant::now();
        let (masked, _) = secrets::mask(&text);
        let masked = masked.into_owned();
        StageTimes::add(&self.stats.stages.mask, mask_t);
        let doc = ContentDoc {
            path: c.path.to_string_lossy().into_owned(),
            mtime: c.mtime,
            kind,
            size: c.len,
            text: masked,
        };
        let write_t = Instant::now();
        let upserted = self.cfg.store.upsert(&doc);
        StageTimes::add(&self.stats.stages.write, write_t);
        match upserted {
            Ok(()) => {
                if let Some(known) = &mut self.known {
                    known.insert(super::store::path_key(&doc.path), doc.mtime);
                }
                self.stats.docs.fetch_add(1, Ordering::Relaxed);
                self.uncommitted += 1;
            }
            Err(e) => {
                crate::log::warn("a file could not be added to the content index");
                self.stats
                    .record_error(format!("could not read {}: {e}", c.path.display()));
            }
        }
        let batch = if self.boosting() {
            COMMIT_EVERY_BOOST
        } else {
            self.cfg.commit_every
        };
        if instant || self.uncommitted >= batch {
            self.commit();
        }
    }
}

/// One file to read.
struct Cand {
    path: PathBuf,
    mtime: u64,
    len: u64,
    class: FileClass,
}

/// What reading a file gave: `None` means try again another day.
type Outcome = Option<(DocKind, String)>;

/// A cloud file that is not on this disk (opening it would download it).
#[cfg(windows)]
fn is_online_only(m: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    m.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(not(windows))]
fn is_online_only(_: &fs::Metadata) -> bool {
    false
}

/// Reads the content of files. Shared by the worker and its helper threads.
struct Extractor {
    stats: Arc<ContentStats>,
    ocr: Option<Arc<dyn Ocr>>,
    office: Option<Arc<dyn OfficeText>>,
    /// Office files that made the helper die: path -> (mtime, size, crashes
    /// of that version). Two crashes and the file is left alone until its
    /// mtime or size changes.
    crashed: Mutex<HashMap<String, HelperFail>>,
    /// OCR text of images already read in this run, by (size, content hash).
    ocr_cache: Mutex<OcrCache>,
}

#[derive(Default)]
struct OcrCache {
    map: HashMap<(u64, u64), String>,
    bytes: usize,
}

impl Extractor {
    /// [`extract`](Self::extract); a file that makes it panic is skipped.
    fn extract_contained(&self, c: &Cand) -> Outcome {
        crate::guard::contain("a file could not be read", || self.extract(c)).flatten()
    }

    fn extract(&self, c: &Cand) -> Outcome {
        let path = &c.path;
        match c.class {
            FileClass::Text => {
                self.stats.extractions.fetch_add(1, Ordering::Relaxed);
                let read_t = Instant::now();
                let bytes = extract::read_bytes(path);
                StageTimes::add(&self.stats.stages.read, read_t);
                let extract_t = Instant::now();
                let decoded = bytes.map(|b| extract::decode_text(&b));
                StageTimes::add(&self.stats.stages.extract, extract_t);
                self.stats.stages.text_files.fetch_add(1, Ordering::Relaxed);
                StageTimes::add(&self.stats.stages.text_ns, read_t);
                match decoded {
                    Ok(Some(text)) => Some((DocKind::Text, text)),
                    Ok(None) => Some((DocKind::Text, String::new())),
                    Err(e) => {
                        self.stats
                            .record_error(format!("could not read {}: {e}", path.display()));
                        None
                    }
                }
            }
            FileClass::Image => self.extract_image(path),
            FileClass::Office => {
                let t = Instant::now();
                let r = self.extract_office(path, c.len, c.mtime);
                let st = &self.stats.stages;
                let (n, ns) = if office::ext_of(path) == "pdf" {
                    (&st.pdf_files, &st.pdf_ns)
                } else {
                    (&st.office_files, &st.office_ns)
                };
                n.fetch_add(1, Ordering::Relaxed);
                StageTimes::add(ns, t);
                r
            }
            FileClass::Skip => None,
        }
    }

    /// OCR of one image. Icons are skipped, and a picture seen before (same
    /// bytes) gets the text already found.
    fn extract_image(&self, path: &Path) -> Outcome {
        let ocr = self.ocr.clone()?;
        // Locked by another program or gone: not recorded, so a later scan
        // tries again (the OCR engine would only report a failure).
        let Ok(print) = fs::File::open(path).and_then(|mut f| fingerprint(&mut f)) else {
            self.stats
                .record_error(format!("could not open {}", path.display()));
            return None;
        };
        if print.size.is_some_and(|(w, h)| w.max(h) < MIN_OCR_SIDE) {
            self.stats.ocr_skipped.fetch_add(1, Ordering::Relaxed);
            return Some((DocKind::Ocr, String::new()));
        }
        let key = (print.len, print.hash);
        if let Some(text) = self
            .ocr_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map
            .get(&key)
        {
            self.stats.ocr_skipped.fetch_add(1, Ordering::Relaxed);
            return Some((DocKind::Ocr, text.clone()));
        }
        self.stats.ocr_runs.fetch_add(1, Ordering::Relaxed);
        let ocr_t = Instant::now();
        let result = ocr.recognize(path);
        StageTimes::add(&self.stats.stages.ocr, ocr_t);
        match result {
            Ok(text) => {
                let mut cache = self
                    .ocr_cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if cache.bytes + text.len() <= OCR_CACHE_BYTES {
                    cache.bytes += text.len() + 16;
                    cache.map.insert(key, text.clone());
                }
                Some((DocKind::Ocr, text))
            }
            // Engine missing: try again some other day, remember nothing.
            Err(OcrError::Unavailable) => None,
            Err(OcrError::TooLarge(_)) => {
                self.stats
                    .record_error(format!("skipped: too large: {}", path.display()));
                Some((DocKind::Ocr, String::new()))
            }
            Err(_) => {
                // This image cannot be read; record it so it is not retried every scan.
                self.stats
                    .record_error(format!("could not read the text in {}", path.display()));
                Some((DocKind::Ocr, String::new()))
            }
        }
    }

    /// Text of one Office file through the helper. `None`: try again another
    /// day (helper unreachable). A skipped file is recorded as empty, with the
    /// reason in the log, so it is not retried on every scan.
    fn extract_office(&self, path: &Path, len: u64, mtime: u64) -> Outcome {
        let skip = |_why: &str| {
            crate::log::warn("a document was skipped");
            Some((DocKind::Text, String::new()))
        };
        if len > office::MAX_OFFICE_BYTES {
            return skip("larger than 50 MB");
        }
        let office = self.office.clone()?;
        let ckey = path.to_string_lossy().into_owned();
        let fail = match self.crashed().get(&ckey) {
            Some(f) if f.mtime == mtime && f.len == len => *f,
            _ => HelperFail {
                mtime,
                len,
                crashes: 0,
                timeouts: 0,
                last_timeout: None,
            },
        };
        if fail.crashes >= 2 || fail.timeouts >= MAX_TIMEOUT_ATTEMPTS {
            return None; // failed; waits for a change
        }
        let unit = TIMEOUT_BACKOFF_SECS.load(Ordering::Relaxed);
        let delay = Duration::from_secs(if fail.timeouts <= 1 { unit } else { unit * 10 });
        if fail.last_timeout.is_some_and(|t| t.elapsed() < delay) {
            return None; // timed out lately; waits out the delay
        }
        let read_t = Instant::now();
        // At most one byte past the limit: a file that grew since the size check
        // is not read whole.
        let bytes = fs::File::open(path).and_then(|f| {
            let mut buf = Vec::new();
            f.take(office::MAX_OFFICE_BYTES + 1).read_to_end(&mut buf)?;
            Ok(buf)
        });
        StageTimes::add(&self.stats.stages.read, read_t);
        let bytes = match bytes {
            Ok(b) if b.len() as u64 > office::MAX_OFFICE_BYTES => {
                return skip("larger than 50 MB");
            }
            Ok(b) => b,
            Err(e) => {
                // Locked by another program or gone: not recorded, so a later scan tries again.
                self.stats
                    .record_error(format!("could not read {}: {e}", path.display()));
                return None;
            }
        };
        self.stats.extractions.fetch_add(1, Ordering::Relaxed);
        let extract_t = Instant::now();
        let result = office.extract(&office::ext_of(path), &bytes);
        StageTimes::add(&self.stats.stages.extract, extract_t);
        if !matches!(&result, Err(e) if office::is_crash(e) || office::is_timeout(e)) {
            self.crashed().remove(&ckey);
        }
        match result {
            Ok(OfficeOutcome::Text(text)) => Some((DocKind::Text, text)),
            Ok(OfficeOutcome::Skipped(why)) => skip(&why),
            Err(e) if office::is_crash(&e) => {
                let n = fail.crashes + 1;
                self.crashed().insert(
                    ckey,
                    HelperFail {
                        crashes: n,
                        last_timeout: None,
                        ..fail
                    },
                );
                self.stats.record_error(format!(
                    "failed: helper crashed on {} (attempt {n})",
                    path.display()
                ));
                None
            }
            Err(e) if office::is_timeout(&e) => {
                let n = fail.timeouts + 1;
                self.crashed().insert(
                    ckey,
                    HelperFail {
                        timeouts: n,
                        last_timeout: Some(Instant::now()),
                        ..fail
                    },
                );
                self.stats.record_error(format!(
                    "failed: helper timed out on {} (attempt {n} of {MAX_TIMEOUT_ATTEMPTS})",
                    path.display()
                ));
                None
            }
            Err(e) => {
                self.stats.record_error(format!(
                    "could not read the text of {}: {e}",
                    path.display()
                ));
                None
            }
        }
    }

    fn crashed(&self) -> std::sync::MutexGuard<'_, HashMap<String, HelperFail>> {
        self.crashed.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Size, content hash and (if the header says) pixel size of an image file.
struct Fingerprint {
    len: u64,
    hash: u64,
    size: Option<(u32, u32)>,
}

fn fingerprint(f: &mut fs::File) -> std::io::Result<Fingerprint> {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut head = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
        if head.len() < HEADER_BYTES {
            head.extend_from_slice(&buf[..n.min(HEADER_BYTES - head.len())]);
        }
        len += n as u64;
    }
    Ok(Fingerprint {
        len,
        hash: hasher.finish(),
        size: image_size(&head),
    })
}

/// Pixel size from the header of a PNG, GIF, BMP or JPEG; `None` if unknown.
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u32::from(u16::from_be_bytes([s[0], s[1]])))
    };
    let le16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u32::from(u16::from_le_bytes([s[0], s[1]])))
    };
    let le32 = |i: usize| {
        b.get(i..i + 4)
            .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]).unsigned_abs())
    };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let be32 = |i: usize| {
            b.get(i..i + 4)
                .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        };
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"BM") {
        return Some((le32(18)?, le32(22)?));
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = b[i + 1];
            match m {
                0xFF => i += 1,
                0x01 | 0xD0..=0xD9 => i += 2,
                0xC0..=0xCF if !matches!(m, 0xC4 | 0xC8 | 0xCC) => {
                    return Some((be16(i + 7)?, be16(i + 5)?));
                }
                _ => i += 2 + be16(i + 2)? as usize,
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::image_size;

    #[test]
    fn reads_the_pixel_size_from_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&[0, 0, 0, 32, 0, 0, 0, 16]);
        assert_eq!(image_size(&png), Some((32, 16)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[10, 0, 20, 0]);
        assert_eq!(image_size(&gif), Some((10, 20)));
        let mut bmp = vec![0u8; 30];
        bmp[..2].copy_from_slice(b"BM");
        bmp[18..22].copy_from_slice(&40i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-50i32).to_le_bytes());
        assert_eq!(image_size(&bmp), Some((40, 50)));
        // JPEG: an APP0 segment first, then SOF0 (height 48, width 64).
        let jpg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8, 0, 48, 0, 64, 1, 1, 0,
        ];
        assert_eq!(image_size(&jpg), Some((64, 48)));
        assert_eq!(image_size(b"RIFFxxxxWEBP"), None);
        assert_eq!(image_size(&[0xFF, 0xD8]), None);
    }
}
