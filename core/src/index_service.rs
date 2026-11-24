//! Background crawl and file watching that keep a [`NameIndex`] fresh.
//!
//! The crawl is throttled to a duty cycle (default 10% busy) and stops while
//! the shared [`PauseState`] is set. Watcher events are debounced and applied
//! in batches; while paused they accumulate and are applied after resume.
//!
//! The Windows backend of `notify` does not report buffer overflow, so lost
//! events cannot be detected there. To repair any drift the index is rebuilt
//! from disk on a schedule and swapped in (see [`rescan`]).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use notify::{EventKind, RecursiveMode, Watcher};

use crate::exclude::Excludes;
use crate::frecency::FrecencyStore;
use crate::name_index::{IndexError, NameIndex};
use crate::pause::PauseState;
use crate::ranking;

pub type SharedIndex = Arc<RwLock<NameIndex>>;

pub fn new_shared_index() -> SharedIndex {
    Arc::new(RwLock::new(NameIndex::new()))
}

pub fn read(index: &SharedIndex) -> RwLockReadGuard<'_, NameIndex> {
    index.read().unwrap_or_else(PoisonError::into_inner)
}

pub fn write(index: &SharedIndex) -> RwLockWriteGuard<'_, NameIndex> {
    index.write().unwrap_or_else(PoisonError::into_inner)
}

/// Seconds since the Unix epoch, saturating into `u32` (valid until 2106).
pub fn unix_now() -> u32 {
    secs(SystemTime::now())
}

fn secs(t: SystemTime) -> u32 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
}

const DEBOUNCE: Duration = Duration::from_millis(200);
const THROTTLE_SLICE: Duration = Duration::from_millis(5);
const COMPACT_RATIO: f32 = 0.25;
/// Watcher events are applied after this long even if they never stop coming.
const MAX_BATCH_AGE: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("could not watch {0}: {1}")]
    Watch(PathBuf, notify::Error),
    #[error("could not start a thread: {0}")]
    Spawn(#[from] std::io::Error),
}

/// Called after the initial crawl and after every rescan has put a fresh
/// index in place (the owner uses it to re-apply frecency).
pub type ReindexHook = Arc<dyn Fn() + Send + Sync>;

/// Called for every file-system path the watcher reports (after exclusions),
/// whether it now exists or not. Content indexing listens to this.
pub type PathHook = Arc<dyn Fn(&Path) + Send + Sync>;

#[derive(Clone)]
pub struct IndexConfig {
    pub roots: Vec<PathBuf>,
    pub excludes: Excludes,
    /// Fraction of time the crawl may be busy, `0.01..=1.0`. 1.0 disables throttling.
    pub duty: f32,
    pub watch: bool,
    /// How often to rebuild the index from disk and swap it in, to repair any
    /// change the watcher missed. `None` disables the scheduled rescan.
    pub rescan_interval: Option<Duration>,
    pub on_reindexed: Option<ReindexHook>,
    /// Called whenever the index changed (crawl or rescan finished, watcher
    /// changes applied), so the owner knows a snapshot is out of date.
    pub on_changed: Option<ReindexHook>,
    pub on_path: Option<PathHook>,
    /// The index already holds a snapshot loaded from disk: the startup crawl
    /// then builds a fresh index and swaps it in (like a rescan) instead of
    /// adding every file a second time.
    pub preloaded: bool,
}

impl IndexConfig {
    pub fn new(roots: Vec<PathBuf>, excludes: Excludes) -> Self {
        Self {
            roots,
            excludes,
            duty: 0.1,
            watch: true,
            rescan_interval: None,
            on_reindexed: None,
            on_changed: None,
            on_path: None,
            preloaded: false,
        }
    }
}

/// What the watcher thread tells the event loop.
enum Signal {
    Path(PathBuf),
    /// The watcher cannot vouch for completeness (error, or a backend that reports overflow).
    Rescan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexStatus {
    pub files: usize,
    pub folders: usize,
    pub crawling: bool,
    pub paused: bool,
}

pub(crate) struct Throttle {
    duty: f32,
    busy: Duration,
}

impl Throttle {
    pub(crate) fn new(duty: f32) -> Self {
        Self {
            duty: duty.clamp(0.01, 1.0),
            busy: Duration::ZERO,
        }
    }

    /// Accumulates busy time and sleeps once a slice is used up, so the
    /// average busy fraction approaches `duty` without sub-millisecond sleeps
    /// (which Windows rounds up to its ~15 ms timer tick).
    pub(crate) fn after_work(&mut self, spent: Duration) {
        if self.duty >= 1.0 {
            return;
        }
        self.busy += spent;
        if self.busy >= THROTTLE_SLICE {
            std::thread::sleep(self.busy.mul_f32(1.0 / self.duty - 1.0));
            self.busy = Duration::ZERO;
        }
    }
}

struct Ctx {
    index: SharedIndex,
    excludes: Excludes,
    roots: Vec<PathBuf>,
    pause: PauseState,
    stop: Arc<AtomicBool>,
    duty: f32,
    /// True while a crawl or rescan is building an index.
    crawling: Arc<AtomicBool>,
    rescans: Arc<AtomicU64>,
    on_reindexed: Option<ReindexHook>,
    on_changed: Option<ReindexHook>,
    on_path: Option<PathHook>,
}

impl Ctx {
    /// Blocks while the user has paused indexing (or until the service stops).
    fn wait_while_paused(&self) {
        while self.pause.is_paused() && !self.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn notify_reindexed(&self) {
        if let Some(hook) = &self.on_reindexed {
            hook();
        }
        self.notify_changed();
    }

    fn notify_changed(&self) {
        if let Some(hook) = &self.on_changed {
            hook();
        }
    }
}

pub struct IndexService {
    index: SharedIndex,
    pause: PauseState,
    stop: Arc<AtomicBool>,
    crawling: Arc<AtomicBool>,
    rescans: Arc<AtomicU64>,
    signals: Sender<Signal>,
    threads: Vec<JoinHandle<()>>,
    watcher: Option<notify::RecommendedWatcher>,
}

impl IndexService {
    pub fn start(
        index: SharedIndex,
        config: IndexConfig,
        pause: PauseState,
    ) -> Result<Self, ServiceError> {
        let roots: Vec<PathBuf> = config.roots.into_iter().filter(|r| r.is_dir()).collect();
        let stop = Arc::new(AtomicBool::new(false));
        let crawling = Arc::new(AtomicBool::new(!roots.is_empty()));
        let rescans = Arc::new(AtomicU64::new(0));
        let ctx = Arc::new(Ctx {
            index: index.clone(),
            excludes: config.excludes,
            roots: roots.clone(),
            pause: pause.clone(),
            stop: stop.clone(),
            duty: config.duty,
            crawling: crawling.clone(),
            rescans: rescans.clone(),
            on_reindexed: config.on_reindexed,
            on_changed: config.on_changed,
            on_path: config.on_path,
        });

        let (tx, rx) = mpsc::channel::<Signal>();
        let mut threads = Vec::new();
        let mut watcher = None;
        if config.watch && !roots.is_empty() {
            // Watch before crawling so changes made during the crawl are not missed.
            let events = tx.clone();
            let mut w =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                    Ok(ev) if ev.need_rescan() => {
                        let _ = events.send(Signal::Rescan);
                    }
                    Ok(ev) if !matches!(ev.kind, EventKind::Access(_)) => {
                        for p in ev.paths {
                            let _ = events.send(Signal::Path(p));
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        let _ = events.send(Signal::Rescan);
                    }
                })
                .map_err(|e| ServiceError::Watch(PathBuf::new(), e))?;
            for r in &roots {
                w.watch(r, RecursiveMode::Recursive)
                    .map_err(|e| ServiceError::Watch(r.clone(), e))?;
            }
            watcher = Some(w);
        }
        if !roots.is_empty() {
            let c = ctx.clone();
            let interval = config.rescan_interval;
            threads.push(
                std::thread::Builder::new()
                    .name("lookout-watch".into())
                    .spawn(move || event_loop(&c, &rx, interval))?,
            );
        }

        let c = ctx;
        let preloaded = config.preloaded;
        threads.push(
            std::thread::Builder::new()
                .name("lookout-crawl".into())
                .spawn(move || {
                    if preloaded {
                        // Serves the snapshot until the fresh crawl replaces it.
                        rescan(&c);
                    } else {
                        crawl_roots(&c);
                        write(&c.index).shrink_to_fit();
                        // A crawl cut short by a stop is incomplete: it must not mark
                        // the index changed (a snapshot of it would replace a good one).
                        if !c.stop.load(Ordering::Acquire) {
                            c.notify_reindexed();
                        }
                    }
                    c.crawling.store(false, Ordering::Release);
                })?,
        );

        Ok(Self {
            index,
            pause,
            stop,
            crawling,
            rescans,
            signals: tx,
            threads,
            watcher,
        })
    }

    /// Asks the event loop to rebuild the index from disk and swap it in.
    pub fn request_rescan(&self) {
        let _ = self.signals.send(Signal::Rescan);
    }

    /// How many rescans have completed.
    pub fn rescan_count(&self) -> u64 {
        self.rescans.load(Ordering::Acquire)
    }

    /// Flag that is true while the initial crawl is running.
    pub fn crawl_flag(&self) -> Arc<AtomicBool> {
        self.crawling.clone()
    }

    pub fn status(&self) -> IndexStatus {
        let counts = read(&self.index).counts();
        IndexStatus {
            files: counts.files,
            folders: counts.folders,
            crawling: self.crawling.load(Ordering::Acquire),
            paused: self.pause.is_paused(),
        }
    }

    /// Blocks until the initial crawl is done or `timeout` passes.
    pub fn wait_for_crawl(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.crawling.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.watcher = None; // closes the event channel
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for IndexService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Lifts previously opened files and folders in the index to their stored
/// frecency. Returns how many were found.
pub fn apply_frecency(index: &SharedIndex, store: &FrecencyStore, now: i64) -> usize {
    let mut idx = write(index);
    store
        .snapshot(now)
        .into_iter()
        .filter(|(id, _)| id.starts_with("file:") || id.starts_with("folder:"))
        .filter(|(id, score)| {
            let path = id.split_once(':').map_or("", |(_, p)| p);
            idx.set_frecency(Path::new(path), ranking::frecency_level(*score))
        })
        .count()
}

fn crawl_roots(ctx: &Ctx) {
    let mut throttle = Throttle::new(ctx.duty);
    for root in &ctx.roots {
        let meta_time = fs::metadata(root).ok().and_then(|m| m.modified().ok());
        let id = write(&ctx.index).add_root(root, meta_time.map_or(0, secs));
        let Ok(id) = id else { continue };
        crawl_tree(ctx, id, root.clone(), &mut throttle);
    }
}

/// Depth-first walk below an already indexed directory.
fn crawl_tree(ctx: &Ctx, dir_id: u32, dir_path: PathBuf, throttle: &mut Throttle) {
    let mut stack = vec![(dir_id, dir_path)];
    while let Some((id, path)) = stack.pop() {
        ctx.wait_while_paused();
        if ctx.stop.load(Ordering::Relaxed) {
            return;
        }
        let started = Instant::now();
        let Ok(read_dir) = fs::read_dir(&path) else {
            continue;
        };
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for ent in read_dir.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            let Ok(ft) = ent.file_type() else { continue };
            if ft.is_symlink()
                || ctx.excludes.is_excluded(&name)
                || ctx.excludes.is_excluded_folder(&path.join(&name))
            {
                continue;
            }
            let modified = ent
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map_or(0, secs);
            if ft.is_dir() {
                dirs.push((name, modified));
            } else if ft.is_file() {
                files.push((name, modified));
            }
        }
        {
            let mut idx = write(&ctx.index);
            for (name, modified) in files {
                if let Err(IndexError::Full) = idx.add_file(id, &name, modified) {
                    return;
                }
            }
            for (name, modified) in dirs {
                match idx.add_dir(id, &name, modified) {
                    Ok(child) => {
                        let sub = path.join(&name);
                        // Cache-like folders are listed, but their contents are not read.
                        if !crate::exclude::skips_contents(&sub, &name) {
                            stack.push((child, sub));
                        }
                    }
                    Err(IndexError::Full) => return,
                    Err(_) => {}
                }
            }
        }
        throttle.after_work(started.elapsed());
    }
}

/// Applies watcher events in batches and runs rescans. Nothing is applied
/// while indexing is paused; events and rescan requests wait for the resume.
fn event_loop(ctx: &Ctx, rx: &Receiver<Signal>, rescan_every: Option<Duration>) {
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut oldest: Option<Instant> = None;
    let mut rescan_wanted = false;
    let mut last_rescan = Instant::now();
    loop {
        if ctx.stop.load(Ordering::Relaxed) {
            return;
        }
        let quiet = match rx.recv_timeout(DEBOUNCE) {
            Ok(Signal::Path(p)) => {
                pending.insert(p);
                oldest.get_or_insert_with(Instant::now);
                false
            }
            Ok(Signal::Rescan) => {
                rescan_wanted = true;
                false
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if rescan_every.is_some_and(|every| last_rescan.elapsed() >= every) {
            rescan_wanted = true;
        }
        if ctx.pause.is_paused() {
            continue;
        }
        if rescan_wanted {
            rescan(ctx);
            rescan_wanted = false;
            last_rescan = Instant::now();
            // Events that arrived during the rescan are applied below; repeating
            // a change the rescan already saw is harmless.
        }
        // Apply after a quiet moment, or after MAX_BATCH_AGE if events never stop.
        let due = quiet || oldest.is_some_and(|t| t.elapsed() >= MAX_BATCH_AGE);
        if due && !pending.is_empty() {
            apply_changes(ctx, pending.drain());
            oldest = None;
        }
    }
}

/// Crawls every root into a fresh index and swaps it in. Queries keep using
/// the old index until the swap, which is a pointer-sized replace under the
/// write lock. Peak memory is two indexes.
fn rescan(ctx: &Ctx) {
    ctx.crawling.store(true, Ordering::Release);
    let fresh = new_shared_index();
    let scratch = Ctx {
        index: fresh.clone(),
        excludes: ctx.excludes.clone(),
        roots: ctx.roots.clone(),
        pause: ctx.pause.clone(),
        stop: ctx.stop.clone(),
        duty: ctx.duty,
        crawling: ctx.crawling.clone(),
        rescans: ctx.rescans.clone(),
        on_reindexed: None,
        on_changed: None,
        on_path: None,
    };
    crawl_roots(&scratch);
    if !ctx.stop.load(Ordering::Relaxed) {
        let mut built = std::mem::take(&mut *write(&fresh));
        built.shrink_to_fit();
        // The old index is dropped after the lock is released.
        let old = std::mem::replace(&mut *write(&ctx.index), built);
        drop(old);
        ctx.rescans.fetch_add(1, Ordering::AcqRel);
        ctx.notify_reindexed();
    }
    ctx.crawling.store(false, Ordering::Release);
}

fn apply_changes(ctx: &Ctx, paths: impl Iterator<Item = PathBuf>) {
    for path in paths {
        let Some(root) = ctx.roots.iter().find(|r| path.starts_with(r)) else {
            continue;
        };
        if ctx.excludes.excludes_path(&path, root) {
            continue;
        }
        if let Some(hook) = &ctx.on_path {
            hook(&path);
        }
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {}
            Ok(meta) => {
                let Some(parent) = path.parent() else {
                    continue;
                };
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };
                let modified = meta.modified().map_or(0, secs);
                let parent_id = read(&ctx.index).dir_id_for_path(parent);
                let Some(parent_id) = parent_id else { continue };
                if meta.is_dir() {
                    let new_dir = {
                        let mut idx = write(&ctx.index);
                        if idx.dir_id_for_path(&path).is_some() {
                            None
                        } else {
                            idx.add_dir(parent_id, &name, modified).ok()
                        }
                    };
                    if let Some(id) = new_dir {
                        crawl_tree(ctx, id, path.clone(), &mut Throttle::new(1.0));
                    }
                } else if meta.is_file() {
                    let mut idx = write(&ctx.index);
                    if !idx.contains_file(&path) {
                        let _ = idx.add_file(parent_id, &name, modified);
                    }
                }
            }
            Err(_) => {
                write(&ctx.index).remove_path(&path);
            }
        }
    }
    {
        let mut idx = write(&ctx.index);
        if idx.dead_ratio() > COMPACT_RATIO {
            idx.compact();
        }
    }
    ctx.notify_changed();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_index::SearchParams;
    use crate::ranking::RankingWeights;

    fn config(root: &Path) -> IndexConfig {
        let mut c = IndexConfig::new(vec![root.to_path_buf()], Excludes::default());
        c.duty = 1.0;
        c
    }

    fn find(index: &SharedIndex, word: &str) -> Vec<String> {
        let tokens = vec![word.to_lowercase().into_bytes()];
        let idx = read(index);
        let p = SearchParams {
            tokens: &tokens,
            kind: None,
            limit: 50,
            now: unix_now(),
            weights: RankingWeights::default(),
            time: None,
        };
        idx.search(&p, &|| false)
            .iter()
            .filter_map(|h| idx.entry_info(h.entry))
            .map(|i| i.name)
            .collect()
    }

    /// Polls until `cond` holds (file watchers are asynchronous).
    fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for: {what}");
    }

    fn make_tree(root: &Path) {
        fs::create_dir_all(root.join("docs/deep")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("AppData/Local/Cache")).unwrap();
        fs::write(root.join("docs/Alpha Report.pdf"), b"x").unwrap();
        fs::write(root.join("docs/deep/alpha notes.txt"), b"x").unwrap();
        fs::write(root.join("node_modules/pkg/alpha.js"), b"x").unwrap();
        fs::write(root.join("AppData/Local/Cache/alpha.cfg"), b"x").unwrap();
        fs::write(root.join("top.txt"), b"x").unwrap();
    }

    #[test]
    fn crawl_indexes_files_and_honours_exclusions() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let index = new_shared_index();
        let mut cfg = config(dir.path());
        cfg.watch = false;
        let svc = IndexService::start(index.clone(), cfg, PauseState::new()).unwrap();
        assert!(svc.wait_for_crawl(Duration::from_secs(10)));
        let mut got = find(&index, "alpha");
        got.sort();
        assert_eq!(got, ["Alpha Report.pdf", "alpha notes.txt"]);
        assert_eq!(find(&index, "top.txt"), ["top.txt"]);
        let status = svc.status();
        assert!(!status.crawling);
        assert_eq!(status.files, 3);
        svc.stop();
    }

    #[test]
    fn missing_roots_are_skipped() {
        let svc = IndexService::start(
            new_shared_index(),
            config(Path::new("/definitely/not/here")),
            PauseState::new(),
        )
        .unwrap();
        assert!(svc.wait_for_crawl(Duration::from_secs(5)));
        assert_eq!(svc.status().files, 0);
        svc.stop();
    }

    #[test]
    fn pause_really_stops_indexing_and_resume_continues() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let pause = PauseState::new();
        pause.set(true);
        let index = new_shared_index();
        let mut cfg = config(dir.path());
        cfg.watch = false;
        let svc = IndexService::start(index.clone(), cfg, pause.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        let s = svc.status();
        assert!(s.crawling && s.paused);
        assert_eq!(s.files, 0, "no file may be indexed while paused");
        pause.set(false);
        assert!(svc.wait_for_crawl(Duration::from_secs(10)));
        assert_eq!(svc.status().files, 3);
        svc.stop();
    }

    #[test]
    fn watcher_applies_create_modify_rename_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let index = new_shared_index();
        let svc =
            IndexService::start(index.clone(), config(dir.path()), PauseState::new()).unwrap();
        assert!(svc.wait_for_crawl(Duration::from_secs(10)));

        fs::write(dir.path().join("docs/fresh budget.xlsx"), b"x").unwrap();
        eventually("created file appears", || {
            find(&index, "fresh budget") == ["fresh budget.xlsx"]
        });

        fs::rename(
            dir.path().join("docs/fresh budget.xlsx"),
            dir.path().join("docs/renamed budget.xlsx"),
        )
        .unwrap();
        eventually("rename applied", || {
            find(&index, "budget") == ["renamed budget.xlsx"]
        });

        fs::create_dir_all(dir.path().join("docs/newdir")).unwrap();
        fs::write(dir.path().join("docs/newdir/inside.txt"), b"x").unwrap();
        eventually("new directory contents appear", || {
            !find(&index, "inside").is_empty()
        });

        fs::remove_file(dir.path().join("docs/renamed budget.xlsx")).unwrap();
        eventually("deleted file disappears", || {
            find(&index, "budget").is_empty()
        });

        fs::remove_dir_all(dir.path().join("docs/newdir")).unwrap();
        eventually("deleted directory disappears", || {
            find(&index, "inside").is_empty() && find(&index, "newdir").is_empty()
        });

        // Excluded locations never get indexed, even by the watcher.
        fs::write(dir.path().join("node_modules/pkg/watched alpha.js"), b"x").unwrap();
        fs::write(dir.path().join("sentinel.txt"), b"x").unwrap();
        eventually("sentinel appears", || !find(&index, "sentinel").is_empty());
        assert!(find(&index, "watched alpha").is_empty());
        svc.stop();
    }

    #[test]
    fn paused_watcher_applies_changes_after_resume() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let pause = PauseState::new();
        let index = new_shared_index();
        let svc = IndexService::start(index.clone(), config(dir.path()), pause.clone()).unwrap();
        assert!(svc.wait_for_crawl(Duration::from_secs(10)));
        pause.set(true);
        fs::write(dir.path().join("while paused.txt"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert!(find(&index, "while paused").is_empty());
        pause.set(false);
        eventually("applied after resume", || {
            !find(&index, "while paused").is_empty()
        });
        svc.stop();
    }

    /// Starts a service with no watcher, so only a rescan can notice later changes.
    fn start_unwatched(
        dir: &Path,
        tweak: impl FnOnce(&mut IndexConfig),
    ) -> (IndexService, SharedIndex) {
        let index = new_shared_index();
        let mut cfg = config(dir);
        cfg.watch = false;
        tweak(&mut cfg);
        let svc = IndexService::start(index.clone(), cfg, PauseState::new()).unwrap();
        assert!(svc.wait_for_crawl(Duration::from_secs(10)));
        (svc, index)
    }

    fn change_disk_behind_the_index(root: &Path) {
        fs::write(root.join("docs/appeared later.pdf"), b"x").unwrap();
        fs::remove_file(root.join("top.txt")).unwrap();
        fs::create_dir_all(root.join("fresh")).unwrap();
        fs::write(root.join("fresh/inside fresh.txt"), b"x").unwrap();
        fs::remove_dir_all(root.join("docs/deep")).unwrap();
    }

    #[test]
    fn rescan_repairs_changes_nobody_reported() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let (svc, index) = start_unwatched(dir.path(), |_| {});
        change_disk_behind_the_index(dir.path());
        assert!(
            find(&index, "appeared later").is_empty(),
            "no watcher: change must be unseen"
        );
        assert_eq!(find(&index, "top.txt"), ["top.txt"]);

        svc.request_rescan();
        eventually("rescan completes", || svc.rescan_count() == 1);
        assert_eq!(find(&index, "appeared later"), ["appeared later.pdf"]);
        assert!(find(&index, "top.txt").is_empty());
        assert_eq!(find(&index, "inside fresh"), ["inside fresh.txt"]);
        assert!(
            find(&index, "alpha notes").is_empty(),
            "deleted directory is gone"
        );
        assert!(!svc.status().crawling);
        svc.stop();
    }

    #[test]
    fn scheduled_rescan_runs_without_being_asked() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let (svc, index) = start_unwatched(dir.path(), |c| {
            c.rescan_interval = Some(Duration::from_millis(400));
        });
        change_disk_behind_the_index(dir.path());
        eventually("scheduled rescan applies the change", || {
            find(&index, "appeared later") == ["appeared later.pdf"]
        });
        assert!(svc.rescan_count() >= 1);
        svc.stop();
    }

    #[test]
    fn reindex_hook_runs_after_the_crawl_and_after_each_rescan() {
        use std::sync::atomic::AtomicUsize;
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let (svc, _index) = start_unwatched(dir.path(), |cfg| {
            cfg.on_reindexed = Some(Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        svc.request_rescan();
        eventually("hook after rescan", || calls.load(Ordering::SeqCst) == 2);
        svc.stop();
    }

    #[test]
    fn a_rescan_requested_while_paused_waits_for_the_resume() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let pause = PauseState::new();
        let index = new_shared_index();
        let mut cfg = config(dir.path());
        cfg.watch = false;
        let svc = IndexService::start(index.clone(), cfg, pause.clone()).unwrap();
        assert!(svc.wait_for_crawl(Duration::from_secs(10)));
        pause.set(true);
        svc.request_rescan();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            svc.rescan_count(),
            0,
            "paused: the rescan must not have run"
        );
        assert_eq!(
            find(&index, "top.txt"),
            ["top.txt"],
            "the index keeps serving queries"
        );
        pause.set(false);
        eventually("rescan completes after resume", || svc.rescan_count() == 1);
        assert_eq!(find(&index, "top.txt"), ["top.txt"]);
        svc.stop();
    }

    #[test]
    fn busy_event_streams_are_still_applied_within_the_max_batch_age() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let index = new_shared_index();
        let svc =
            IndexService::start(index.clone(), config(dir.path()), PauseState::new()).unwrap();
        assert!(svc.wait_for_crawl(Duration::from_secs(10)));
        // Keep the watcher busy so there is never a 200 ms quiet period.
        let busy_dir = dir.path().join("docs");
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut n = 0u32;
            while !s.load(Ordering::Relaxed) {
                let _ = fs::write(busy_dir.join(format!("churn {n}.tmp")), b"x");
                n += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        eventually("churn files indexed although events never stop", || {
            !find(&index, "churn 5").is_empty()
        });
        stop.store(true, Ordering::Relaxed);
        let _ = writer.join();
        svc.stop();
    }

    #[test]
    fn throttle_sleeps_in_proportion_to_work() {
        let mut t = Throttle::new(0.5);
        let start = Instant::now();
        t.after_work(Duration::from_millis(2)); // below the slice: no sleep
        assert!(start.elapsed() < Duration::from_millis(2));
        t.after_work(Duration::from_millis(10)); // 12 ms busy at 50% duty: ~12 ms sleep
        assert!(start.elapsed() >= Duration::from_millis(10));
        let mut unthrottled = Throttle::new(1.0);
        let start = Instant::now();
        unthrottled.after_work(Duration::from_secs(5));
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
