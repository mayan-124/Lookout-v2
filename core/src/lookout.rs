//! Assembles the Phase 1 pieces (index service, frecency, sources, engine)
//! behind one handle, so the UI shell only forwards calls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::actions::disk::{
    BatchRemoveAction, CopyFileAction, CopyPathAction, ExplorerReveal, RecycleAction, RevealAction,
    ShellRecycler, WindowsClipboardWriter,
};
use crate::actions::kill_port::{self, KillPort, PortSource};
use crate::actions::open::{self, Launcher, OpenAction};
use crate::actions::quick::{
    self as quick_actions, OpenUrlAction, PageAction, QuickSource, ThemeAction, WindowsTheme,
};
use crate::actions::system::ProcessBackend;
use crate::actions::undo_log::{Entry, UndoLog};
use crate::actions::{
    Action, ActionEngine, ActionError, Done, GateConfig, PluginOrigin, Prepared, Target,
};
use crate::answers::{AnswerSource, CopyAnswer};
use crate::apps;
use crate::content::idle::IdleSource;
use crate::content::ocr::Ocr;
use crate::content::office::{OfficeText, PluginOffice};
use crate::content::service::{ContentConfig, ContentService, ContentStats};
use crate::content::source::ContentSource;
use crate::content::store::ContentStore;
use crate::engine::{Engine, EngineConfig, Sink, Update};
use crate::exclude::Excludes;
use crate::frecency::{FrecencyError, FrecencyStore};
use crate::index_service::{
    apply_frecency, new_shared_index, read, unix_now, write, IndexConfig, IndexService,
    ReindexHook, ServiceError, SharedIndex,
};
use crate::item::{Item, Kind};
use crate::names_store::{self, Persister};
use crate::notes::{CopyNote, DeleteNote, NoteSource, NoteStore, SaveNote};
use crate::pause::PauseState;
use crate::plugins::{DragData, HostConfig, PluginActions, PluginHost, PluginSource};
use crate::ranking;
use crate::semantic::{
    SemanticClient, SemanticConfig, SemanticIndexer, SemanticProgress, SemanticSource,
    SemanticStatus, PLUGIN_ID,
};
use crate::settings::Settings;
use crate::share::{Payload, Refusal, Rules};
use crate::sources::{AppsSource, CalculatorSource, FilesSource, Source, UnitsSource};
use crate::storage::{Storage, StorageError};
use crate::synthetic;

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Frecency(#[from] FrecencyError),
    #[error("could not start the query engine: {0}")]
    Engine(#[source] std::io::Error),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    ActionLog(#[from] crate::actions::undo_log::LogError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Db(#[from] crate::securedb::DbError),
    #[error("could not start the index saver: {0}")]
    Saver(#[source] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum WipeError {
    #[error("could not delete the index: {0}")]
    Io(#[from] std::io::Error),
}

pub struct StartOptions {
    pub settings: Settings,
    /// Replaces `settings.index_roots` when non-empty (developer flag).
    pub roots_override: Vec<PathBuf>,
    /// Fill the index with this many generated names instead of crawling (benchmarks).
    pub synthetic_files: Option<usize>,
    /// With `synthetic_files`: also index the contents of the configured folders.
    /// Off unless explicitly asked for, so a generated-data run never reads real folders.
    pub synthetic_uses_folders: bool,
    pub start_menu_dirs: Vec<PathBuf>,
    /// Also list the Store apps Windows knows about. Tests leave this off.
    pub store_apps: bool,
    /// Where the encrypted `meta.db`, name snapshot and content index live.
    pub storage: Storage,
    /// When the user last used the keyboard or mouse; content work waits for idleness.
    pub idle: Arc<dyn IdleSource>,
    /// Reads text in images. Only used if `settings.ocr` is on.
    pub ocr: Arc<dyn Ocr>,
    /// How "open" hands a path to Windows. Tests pass a recorder.
    pub launcher: Arc<dyn Launcher>,
    /// Process and port access for `kill port`; `None` uses the real system.
    pub system: Option<Arc<dyn ProcessBackend>>,
    pub gate: GateConfig,
    /// Plugin folders and data folder; `None` runs without a plugin host.
    pub plugins: Option<HostConfig>,
}

/// Live state of every background feature, for the Settings health panel.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Health {
    pub name_index: NameIndexHealth,
    pub content: ContentHealth,
    pub screenshots: ScreenshotHealth,
    pub plugins: Vec<PluginHealth>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NameIndexHealth {
    pub count: u64,
    pub state: String,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContentHealth {
    /// running, waiting for idle, paused, off or up to date.
    pub state: String,
    pub reason: String,
    pub files_done: u64,
    pub files_queued: u64,
    /// Reading text (documents) in this scan, then pictures: done of total.
    pub read_done: u64,
    pub read_total: u64,
    pub ocr_done: u64,
    pub ocr_total: u64,
    pub last_error: Option<String>,
    pub index_now_running: bool,
    /// Size of the content index and its limit, in bytes.
    pub size_bytes: u64,
    pub max_bytes: u64,
    /// The limit is reached: nothing new is added.
    pub full: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScreenshotHealth {
    pub state: String,
    pub reason: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginHealth {
    pub name: String,
    pub state: String,
    pub reason: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    pub files: usize,
    pub folders: usize,
    pub apps: usize,
    pub roots: usize,
    pub indexing: bool,
    pub paused: bool,
    /// A `size:` search may still miss files: the sizes are being collected.
    pub sizes_pending: bool,
    /// The user has not yet chosen folders (first run): the UI shows the chooser.
    pub needs_setup: bool,
    /// Documents in the full-text index (file contents and screenshot text).
    pub content_docs: u64,
    /// Semantic indexing: files done and files to do in the current pass (0 when off).
    pub semantic_done: u64,
    pub semantic_total: u64,
    /// File reading in the current scan: documents first, pictures after.
    pub read_done: u64,
    pub read_total: u64,
    pub ocr_done: u64,
    pub ocr_total: u64,
    /// File reading is holding back because the user is at the keyboard or mouse.
    pub waiting_for_idle: bool,
    /// File reading will stop again at the next keyboard or mouse input.
    pub pauses_on_activity: bool,
}

struct SemanticRuntime {
    client: SemanticClient,
    progress: Arc<SemanticProgress>,
    indexer: Mutex<Option<SemanticIndexer>>,
}

struct ContentRuntime {
    ocr: Option<Arc<dyn Ocr>>,
    idle_secs: u32,
    store: Arc<ContentStore>,
    service: Mutex<Option<ContentService>>,
    stats: Arc<ContentStats>,
}

pub struct Lookout {
    engine: Engine,
    index: SharedIndex,
    service: Mutex<Option<IndexService>>,
    frecency: Arc<FrecencyStore>,
    pause: PauseState,
    populating: Arc<AtomicBool>,
    targets: Arc<Mutex<HashMap<String, Target>>>,
    actions: ActionEngine,
    roots: usize,
    apps: crate::sources::SharedApps,
    needs_setup: bool,
    storage: Storage,
    persister: Mutex<Option<Persister>>,
    wiped: AtomicBool,
    content: Option<ContentRuntime>,
    /// Why there is no content index although it is switched on.
    content_error: Option<String>,
    index_contents: bool,
    plugins: Option<Arc<PluginHost>>,
    semantic: Option<SemanticRuntime>,
    /// Disk usage analyzer: idle until the user starts a scan.
    disk: crate::diskusage::DiskService,
    /// Clean up list and duplicate finder (on demand).
    tidy: crate::cleanup::Tidy,
    quick_keys: Arc<crate::quickkeys::QuickKeys>,
    history: crate::history::SearchHistory,
    notes: Arc<NoteStore>,
    timers: Arc<crate::timers::TimerService>,
    /// Results of the latest update that may get a quick key.
    keyable: Arc<Mutex<HashMap<String, crate::item::Item>>>,
    /// The same sources the bar uses, for command-line searches (see `cli_search`).
    cli_sources: Vec<Arc<dyn Source>>,
    /// The folders in the name index, and what is skipped inside them.
    home_roots: Vec<PathBuf>,
    excludes: Excludes,
    share_rules: Arc<Rules>,
    /// The common extensions in the name index, counted when a crawl ends.
    extensions: Arc<Mutex<Vec<String>>>,
    /// File sizes are still missing from the name index; the crawl brings them.
    sizes_pending: Arc<AtomicBool>,
    /// The files of the latest Home "Recent" list, which can be opened too.
    home_targets: Mutex<HashMap<String, Target>>,
    safe_mode: bool,
    /// Unix seconds when the last crawl of this run finished (0: none yet).
    last_crawl: Arc<AtomicU64>,
    /// One sentence for the user about this start, until the shell takes it.
    notice: Mutex<Option<&'static str>>,
    /// The rebuild in progress.
    rebuild: Mutex<Option<RebuildMark>>,
}

/// Where the counters stood when a rebuild was asked for.
#[derive(Clone, Copy)]
struct RebuildMark {
    names: Option<u64>,
    contents: Option<u64>,
}

/// One line about the rebuild, and whether it is over.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RebuildProgress {
    pub line: String,
    pub done: bool,
}

const REPAIRED: &str = "The search index was damaged, so Lookout is rebuilding it.";

/// Extensions offered while `ext:` is typed.
const SUGGESTED_EXTENSIONS: usize = 8;

/// What the search box offers while an operator is typed.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct OperatorValues {
    /// Most common first.
    pub extensions: Vec<String>,
    /// The indexed folders' names.
    pub folders: Vec<String>,
}

fn count_extensions(index: &SharedIndex, into: &Mutex<Vec<String>>) {
    let found = read(index).top_extensions(SUGGESTED_EXTENSIONS);
    *into.lock().unwrap_or_else(PoisonError::into_inner) = found;
}

/// The roots earlier versions indexed: they added `%LOCALAPPDATA%` and
/// `%APPDATA%` on their own. Only used to load such a snapshot and prune it.
fn with_appdata_roots(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    if roots.is_empty() {
        return roots;
    }
    for var in ["LOCALAPPDATA", "APPDATA"] {
        let Some(dir) = std::env::var_os(var).map(PathBuf::from) else {
            continue;
        };
        if dir.is_dir() && !roots.iter().any(|r| dir.starts_with(r)) {
            roots.push(dir);
        }
    }
    roots
}

impl Lookout {
    pub fn start(opts: StartOptions, sink: Sink, pause: PauseState) -> Result<Self, StartError> {
        let storage = opts.storage.clone();
        let pause_for_disk = pause.clone();
        let disk_file = storage.paths.as_ref().map(|p| p.disk_scan());
        let disk_key = storage.key.clone();
        let frecency = Arc::new(FrecencyStore::new(storage.db.clone())?);
        let index = new_shared_index();
        let weights = opts.settings.ranking;

        let roots = if opts.roots_override.is_empty() {
            opts.settings.index_roots.clone()
        } else {
            opts.roots_override.clone()
        };
        // Content indexing uses the real folders even when the name index is synthetic
        // (benchmark: a 1M-name index and a content batch at the same time).
        let content_roots = if opts.synthetic_files.is_some() && !opts.synthetic_uses_folders {
            Vec::new()
        } else {
            roots.clone()
        };
        let roots = if opts.synthetic_files.is_some() {
            Vec::new()
        } else {
            roots
        };
        let root_count = roots.len();
        let mut excludes = Excludes::default()
            .with_extra(opts.settings.excluded_names.clone())
            .with_folders(opts.settings.excluded_folders.clone());
        // A data folder somewhere else (developer flag) is skipped too, unless
        // the indexed folders themselves live inside it.
        if let Some(paths) = &storage.paths {
            let data = paths.root();
            if !content_roots.iter().any(|r| r.starts_with(data)) {
                excludes = excludes.with_folders([data.to_path_buf()]);
            }
        }
        // After the first crawl and after every rescan, lift files the user has opened before.
        let (hook_index, hook_store) = (index.clone(), frecency.clone());
        let extensions: Arc<Mutex<Vec<String>>> = Arc::default();
        let hook_extensions = extensions.clone();
        // True from loading a snapshot without sizes until a crawl has them.
        let sizes_pending: Arc<AtomicBool> = Arc::default();
        let hook_sizes = sizes_pending.clone();
        let last_crawl: Arc<AtomicU64> = Arc::default();
        let hook_crawled = last_crawl.clone();
        let on_reindexed: ReindexHook = Arc::new(move || {
            hook_crawled.store(u64::from(unix_now()), Ordering::Release);
            apply_frecency(&hook_index, &hook_store, i64::from(unix_now()));
            count_extensions(&hook_index, &hook_extensions);
            hook_sizes.store(false, Ordering::Release);
        });
        // The encrypted snapshot of the name index, if it was made for these folders.
        let snapshot_path = match (&storage.paths, &storage.key, opts.synthetic_files) {
            (Some(p), Some(k), None) => Some((p.names_file(), k.clone())),
            _ => None,
        };
        let excludes_key = names_store::excludes_key(
            &opts.settings.excluded_names,
            &opts.settings.excluded_folders,
        );
        let fingerprint = names_store::fingerprint(&roots, &excludes_key);
        // Entries a loaded snapshot held in places that are skipped now.
        let mut tidied = 0;
        let safe_mode = opts.settings.safe_mode;
        let mut notice = None;
        let preloaded = match &snapshot_path {
            Some((path, key)) if !roots.is_empty() => {
                let max = opts.settings.snapshot_max_entries;
                let first = names_store::try_load(path, key, &fingerprint, max);
                if first.is_err() {
                    // Derived data: removed, and the startup crawl makes it again.
                    crate::log::error("the name index was damaged and is being rebuilt");
                    let _ = std::fs::remove_file(path);
                    notice = Some(REPAIRED);
                }
                let loaded = first.ok().flatten().or_else(|| {
                    let old = with_appdata_roots(roots.clone());
                    let old = names_store::fingerprint(&old, &excludes_key);
                    names_store::load(path, key, &old, max)
                });
                match loaded {
                    Some(mut idx) => {
                        tidied = idx.prune_dirs(|dir, root| {
                            !roots.iter().any(|r| r == root)
                                || excludes.is_excluded_folder(dir)
                                || excludes.excludes_path(dir, root)
                        });
                        *write(&index) = idx;
                        apply_frecency(&index, &frecency, i64::from(unix_now()));
                        count_extensions(&index, &extensions);
                        sizes_pending.store(read(&index).has_unsized_files(), Ordering::Release);
                        true
                    }
                    None => {
                        if names_store::snapshot_entries(path, key, &fingerprint).is_some() {
                            crate::log::warn("the name snapshot is too large to load");
                        }
                        false
                    }
                }
            }
            _ => false,
        };
        // Safe mode only reads the snapshot: an index nothing fills must not replace it.
        let persister = match snapshot_path.filter(|_| !safe_mode) {
            Some((path, key)) => Some(
                Persister::start(index.clone(), path, key, fingerprint, pause.clone())
                    .map_err(StartError::Saver)?,
            ),
            None => None,
        };
        // Save the tidied snapshot even if nothing else changes.
        if let (Some(p), true) = (&persister, tidied > 0) {
            p.mark_dirty();
        }
        let on_changed: Option<ReindexHook> =
            persister.as_ref().map(Persister::dirty_flag).map(|d| {
                let hook: ReindexHook = Arc::new(move || d.store(true, Ordering::Release));
                hook
            });
        let plugin_host = opts.plugins.clone().filter(|_| !safe_mode).map(|mut cfg| {
            if cfg.master_key.is_none() {
                cfg.master_key = storage.key.clone();
            }
            PluginHost::start(cfg, pause.clone())
        });
        // Full-text index and its worker. A failure here only disables content search.
        let mut content_error: Option<String> = None;
        let store = if !safe_mode
            && opts.settings.index_contents
            && (opts.synthetic_files.is_none() || !content_roots.is_empty())
        {
            let opened = match (&storage.paths, &storage.key) {
                (Some(p), Some(k)) => {
                    open_content(&p.content_dir(), k).inspect(|(_, rebuilt)| {
                        if *rebuilt {
                            notice = Some(REPAIRED);
                        }
                    })
                }
                .map(|(store, _)| store),
                _ => ContentStore::open_in_ram(),
            };
            match opened {
                Ok(st) => {
                    st.set_roots(content_roots.clone());
                    Some(Arc::new(st))
                }
                Err(e) => {
                    crate::log::error("content search could not start");
                    content_error = Some(e.to_string());
                    None
                }
            }
        } else {
            None
        };
        let content = match store {
            Some(store) => {
                let svc = if content_roots.is_empty() {
                    None
                } else {
                    let cfg = ContentConfig {
                        roots: content_roots.clone(),
                        excludes: excludes.clone(),
                        store: store.clone(),
                        ocr: opts.settings.ocr.then(|| opts.ocr.clone()),
                        office: plugin_host
                            .clone()
                            .map(|h| Arc::new(PluginOffice(h)) as Arc<dyn OfficeText>),
                        idle: opts.idle.clone(),
                        idle_secs: opts.settings.idle_secs,
                        duty: opts.settings.index_speed.duty(opts.settings.crawl_duty),
                        commit_every: opts.settings.index_speed.commit_every(),
                        pause: pause.clone(),
                        max_bytes: opts.settings.content_index_max_bytes(),
                        fast: opts.settings.index_speed == crate::settings::IndexSpeed::Fast,
                    };
                    match ContentService::start(cfg) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            crate::log::error_io("content indexing could not start", &e);
                            None
                        }
                    }
                };
                let stats = svc.as_ref().map(ContentService::stats).unwrap_or_default();
                Some(ContentRuntime {
                    ocr: opts.settings.ocr.then(|| opts.ocr.clone()),
                    idle_secs: opts.settings.idle_secs,
                    store,
                    service: Mutex::new(svc),
                    stats,
                })
            }
            None => None,
        };
        let on_path: Option<crate::index_service::PathHook> = content
            .as_ref()
            .and_then(|c| {
                c.service
                    .lock()
                    .ok()?
                    .as_ref()
                    .map(ContentService::notifier)
            })
            .map(|n| {
                let hook: crate::index_service::PathHook = Arc::new(move |p| n.notify(p));
                hook
            });
        let hours = opts.settings.rescan_hours;
        let make_config = |watch| {
            let mut c = IndexConfig::new(roots.clone(), excludes.clone());
            c.crawl = !safe_mode;
            c.duty = opts.settings.crawl_duty;
            c.watch = watch;
            c.rescan_interval = (hours > 0).then(|| Duration::from_secs(u64::from(hours) * 3600));
            c.on_reindexed = Some(on_reindexed.clone());
            c.on_changed = on_changed.clone();
            c.on_path = on_path.clone();
            c.preloaded = preloaded;
            c.background = sizes_pending.load(Ordering::Acquire);
            c
        };
        // A watcher that cannot start (for example a network share) must not stop indexing.
        let service = match IndexService::start(index.clone(), make_config(true), pause.clone()) {
            Ok(s) => s,
            Err(ServiceError::Watch(..)) => {
                IndexService::start(index.clone(), make_config(false), pause.clone())?
            }
            Err(e) => return Err(e.into()),
        };

        let populating = Arc::new(AtomicBool::new(false));
        if let Some(files) = opts.synthetic_files {
            populating.store(true, Ordering::Release);
            let (idx, flag) = (index.clone(), populating.clone());
            let _ = std::thread::Builder::new()
                .name("lookout-synthetic".into())
                .spawn(move || {
                    let mut w = write(&idx);
                    let _ = synthetic::populate(&mut w, files, 42, unix_now());
                    w.shrink_to_fit();
                    drop(w);
                    flag.store(false, Ordering::Release);
                });
        }

        let apps_source = Arc::new(AppsSource::new(
            apps::scan_start_menu(&opts.start_menu_dirs),
            frecency.clone(),
            weights,
        ));
        let app_list = apps_source.shared();
        if opts.store_apps {
            // Asking Windows takes about a second, so start does not wait for it.
            let list = app_list.clone();
            let _ = std::thread::Builder::new()
                .name("lookout-store-apps".into())
                .spawn(move || {
                    let found = apps::list_apps_folder();
                    let mut list = list.write().unwrap_or_else(PoisonError::into_inner);
                    apps::add_store_apps(&mut list, found);
                });
        }
        let system = match opts.system.clone() {
            Some(s) => s,
            None => default_system(),
        };
        let theme: Arc<dyn quick_actions::ThemeBackend> = Arc::new(WindowsTheme);
        let quick_keys = Arc::new(crate::quickkeys::QuickKeys::new(storage.db.clone())?);
        let history = crate::history::SearchHistory::new(storage.db.clone())?;
        let timer_store = Arc::new(crate::timers::TimerStore::new(storage.db.clone())?);
        let timers = Arc::new(
            crate::timers::TimerService::start(
                timer_store.clone(),
                crate::notify::default_notifier(),
            )
            .map_err(StartError::Engine)?,
        );
        let files_source = Arc::new(FilesSource::new(index.clone(), weights));
        // Nothing is read or written until a note is asked for.
        let notes = Arc::new(NoteStore::new(storage.db.clone()));
        let sources: Vec<Arc<dyn Source>> = vec![
            Arc::new(CalculatorSource),
            Arc::new(AnswerSource),
            Arc::new(NoteSource::new(notes.clone(), weights)),
            Arc::new(crate::quickkeys::QuickKeySource::new(quick_keys.clone())),
            Arc::new(crate::timers::TimerSource::new(timer_store)),
            Arc::new(UnitsSource),
            Arc::new(crate::pathquery::PathSource),
            Arc::new(QuickSource::new(theme.clone())),
            Arc::new(PortSource::new(system.clone())),
            apps_source,
            files_source.clone(),
        ];
        let mut sources = sources;
        if let Some(host) = &plugin_host {
            let ids: Vec<String> = host.manifests().map(|m| m.id.clone()).collect();
            for id in ids {
                let wants_files = host
                    .manifest(&id)
                    .is_some_and(|m| !m.permissions.fs_read.is_empty());
                let files: Option<Arc<dyn Source>> =
                    wants_files.then(|| files_source.clone() as Arc<dyn Source>);
                if let Some(src) = PluginSource::new(host.clone(), &id, files) {
                    sources.push(Arc::new(src));
                }
            }
        }
        if let Some(c) = &content {
            let source = ContentSource::new(c.store.clone(), weights);
            sources.push(Arc::new(source.with_roots(content_roots.clone())));
        }
        let semantic = match (&plugin_host, &content, &storage.paths) {
            (Some(host), Some(c), Some(paths))
                if host.manifest(PLUGIN_ID).is_some() && !content_roots.is_empty() =>
            {
                let client = SemanticClient::new(host.clone(), PLUGIN_ID, paths.root());
                let progress = Arc::new(SemanticProgress::default());
                let triggers: Vec<String> = host
                    .manifests()
                    .flat_map(|m| m.triggers.iter().cloned())
                    .collect();
                let cfg_host = host.clone();
                let min: Box<dyn Fn() -> f32 + Send + Sync> = Box::new(move || {
                    cfg_host
                        .config_number(PLUGIN_ID, "min_similarity")
                        .unwrap_or(0.6) as f32
                });
                let source =
                    SemanticSource::new(client.clone(), c.store.clone(), weights, triggers, min);
                sources.push(Arc::new(source.with_roots(content_roots.clone())));
                let indexer = SemanticIndexer::start(SemanticConfig {
                    client: client.clone(),
                    roots: content_roots.clone(),
                    excludes: excludes.clone(),
                    store: c.store.clone(),
                    content: c.stats.clone(),
                    ocr: opts.settings.ocr.then(|| opts.ocr.clone()),
                    idle: opts.idle.clone(),
                    idle_secs: opts.settings.idle_secs,
                    duty: opts.settings.crawl_duty,
                    pause: pause.clone(),
                    frecency: frecency.clone(),
                    progress: progress.clone(),
                })
                .map_err(|e| crate::log::error_io("semantic indexing could not start", &e))
                .ok();
                Some(SemanticRuntime {
                    client,
                    progress,
                    indexer: Mutex::new(indexer),
                })
            }
            _ => None,
        };

        let log = UndoLog::new(storage.db.clone())?;
        let disk_hints = crate::diskusage::Hints::default();
        let tidy_store = Arc::new(crate::cleanup::BatchStore::new(
            crate::cleanup::Env::current(),
        ));
        let tidy = crate::cleanup::Tidy::new(tidy_store.clone(), pause.clone());
        // What may be dragged out or copied as a file: the indexer's own skip rules.
        let share_rules = Arc::new(Rules {
            excludes: excludes.clone(),
            roots: roots.clone(),
        });
        let action_list: Vec<Arc<dyn Action>> = vec![
            Arc::new(OpenAction::new(opts.launcher.clone())),
            Arc::new(KillPort::new(system)),
            Arc::new(OpenUrlAction::new(opts.launcher.clone())),
            Arc::new(PageAction::new(opts.launcher.clone())),
            Arc::new(ThemeAction::new(theme)),
            Arc::new(crate::timers::StartAction::new(timers.clone())),
            Arc::new(crate::timers::CancelAction::new(timers.clone())),
            Arc::new(RevealAction(Arc::new(ExplorerReveal))),
            Arc::new(CopyPathAction(Arc::new(WindowsClipboardWriter))),
            Arc::new(CopyFileAction(
                Arc::new(WindowsClipboardWriter),
                share_rules.clone(),
            )),
            Arc::new(RecycleAction::new(
                Arc::new(ShellRecycler),
                disk_hints.clone(),
            )),
            Arc::new(BatchRemoveAction::new(tidy_store, Arc::new(ShellRecycler))),
            Arc::new(CopyAnswer(Arc::new(WindowsClipboardWriter))),
            Arc::new(SaveNote(notes.clone())),
            Arc::new(CopyNote(notes.clone(), Arc::new(WindowsClipboardWriter))),
            Arc::new(DeleteNote(notes.clone())),
        ];
        let mut actions = ActionEngine::new(action_list, log, opts.gate);
        if let Some(host) = &plugin_host {
            actions = actions.with_provider(Arc::new(PluginActions(host.clone())));
        }

        // Remember which results can be acted on, so the UI can only name items we
        // produced. Actions never receive a path or port from the webview.
        let targets: Arc<Mutex<HashMap<String, Target>>> = Arc::default();
        let keyable: Arc<Mutex<HashMap<String, crate::item::Item>>> = Arc::default();
        let (map, inner, key_map) = (targets.clone(), sink, keyable.clone());
        let wrapped: Sink = Arc::new(move |update: Update| {
            let can_key: HashMap<String, crate::item::Item> = update
                .items
                .iter()
                .filter(|i| crate::quickkeys::eligible(i))
                .map(|i| (i.id.clone(), i.clone()))
                .collect();
            *key_map.lock().unwrap_or_else(PoisonError::into_inner) = can_key;
            let known: HashMap<String, Target> = update
                .items
                .iter()
                .filter_map(|i| target_of(i).map(|t| (i.id.clone(), t)))
                .collect();
            *map.lock().unwrap_or_else(PoisonError::into_inner) = known;
            inner(update);
        });
        // Notes are never handed to the command line.
        let cli_sources = sources
            .iter()
            .filter(|s| s.id() != "notes")
            .cloned()
            .collect();
        let home_roots = roots.clone();
        let engine =
            Engine::new(sources, wrapped, EngineConfig::default()).map_err(StartError::Engine)?;

        Ok(Self {
            engine,
            index,
            service: Mutex::new(Some(service)),
            frecency,
            pause,
            populating,
            targets,
            actions,
            roots: root_count,
            apps: app_list,
            needs_setup: opts.settings.needs_first_run(),
            storage,
            persister: Mutex::new(persister),
            wiped: AtomicBool::new(false),
            content,
            content_error,
            index_contents: opts.settings.index_contents,
            plugins: plugin_host,
            semantic,
            disk: crate::diskusage::DiskService::new(
                pause_for_disk,
                // The scan uses the Balanced duty, never less than the user's own setting.
                crate::settings::IndexSpeed::Balanced.duty(opts.settings.crawl_duty),
                disk_file,
                disk_key,
                disk_hints,
            ),
            tidy,
            quick_keys,
            history,
            notes,
            timers,
            keyable,
            cli_sources,
            home_roots,
            excludes,
            share_rules,
            extensions,
            sizes_pending,
            home_targets: Mutex::default(),
            safe_mode,
            last_crawl,
            notice: Mutex::new(notice),
            rebuild: Mutex::default(),
        })
    }

    /// The sentence about this start (a repaired index), once.
    pub fn take_notice(&self) -> Option<&'static str> {
        self.notice
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Reads the file names and file contents again from scratch. Search keeps
    /// answering meanwhile: the old names stay until the new ones are complete.
    /// Nothing else (history, notes, settings, saved scans) is touched.
    pub fn rebuild_index(&self) -> Result<(), &'static str> {
        if self.safe_mode {
            return Err("Turn everything back on first.");
        }
        if self.pause.is_paused() {
            return Err("Lookout is paused. Resume it from the tray icon, then rebuild.");
        }
        let mut mark = self.rebuild.lock().unwrap_or_else(PoisonError::into_inner);
        if mark.is_some() {
            return Ok(());
        }
        let service = self.service.lock().unwrap_or_else(PoisonError::into_inner);
        let service = service.as_ref().ok_or("Search is not running.")?;
        if service.status().crawling {
            return Err("Lookout is still reading your folders. Try again when it has finished.");
        }
        let names = service.can_rescan().then(|| service.rescan_count());
        let contents = self.content.as_ref().and_then(|c| {
            let guard = c.service.lock().unwrap_or_else(PoisonError::into_inner);
            let svc = guard.as_ref()?;
            let before = c.stats.rebuilt.load(Ordering::Acquire);
            svc.rebuild();
            Some(before)
        });
        if names.is_none() && contents.is_none() {
            return Err("There are no folders to read. Choose folders first.");
        }
        if names.is_some() {
            service.request_rescan();
        }
        *mark = Some(RebuildMark { names, contents });
        Ok(())
    }

    /// The state of the rebuild, if one was asked for. A finished one is
    /// reported once.
    pub fn rebuild_progress(&self) -> Option<RebuildProgress> {
        let mut mark = self.rebuild.lock().unwrap_or_else(PoisonError::into_inner);
        let m = (*mark)?;
        let names_done = m.names.is_none_or(|before| {
            self.service
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(|s| s.rescan_count() > before)
        });
        let stats = self.content.as_ref().map(|c| &c.stats);
        let contents_done = m
            .contents
            .is_none_or(|before| stats.is_some_and(|s| s.rebuilt.load(Ordering::Acquire) > before));
        let done = names_done && contents_done;
        let line = if done {
            *mark = None;
            "Search index rebuilt.".to_owned()
        } else if self.pause.is_paused() {
            "Rebuild paused. Resume from the tray icon.".to_owned()
        } else if !names_done {
            "Rebuilding: reading file names…".to_owned()
        } else {
            let sum = |a: &AtomicU64, b: &AtomicU64| {
                a.load(Ordering::Relaxed) + b.load(Ordering::Relaxed)
            };
            match stats.map(|s| {
                (
                    sum(&s.read_done, &s.ocr_done),
                    sum(&s.read_total, &s.ocr_total),
                )
            }) {
                Some((read, total)) if total > 0 => {
                    format!("Rebuilding: {read} of {total} files read…")
                }
                _ => "Rebuilding: reading file contents…".to_owned(),
            }
        };
        Some(RebuildProgress { line, done })
    }

    /// Numbers for the diagnostics text: no names, searches or paths.
    pub fn counts(&self) -> crate::diagnostics::Counts {
        let st = self.status();
        let crawled = self.last_crawl.load(Ordering::Acquire);
        crate::diagnostics::Counts {
            files: st.files,
            folders: st.folders,
            apps: st.apps,
            content_docs: st.content_docs,
            last_crawl: (crawled > 0).then_some(crawled),
            plugins: self.plugins.as_ref().map_or_else(Vec::new, |h| {
                h.list()
                    .into_iter()
                    .filter(|p| p.enabled)
                    .map(|p| p.id)
                    .collect()
            }),
        }
    }

    fn stop_semantic(&self) {
        if let Some(s) = &self.semantic {
            if let Some(mut i) = s
                .indexer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                i.stop();
            }
        }
    }

    /// State of semantic search for the Plugins panel (`None`: not available in this run).
    pub fn semantic_status(&self) -> Option<SemanticStatus> {
        let s = self.semantic.as_ref()?;
        Some(s.progress.snapshot(s.client.assets_dir()))
    }

    /// "Delete my index" for semantic search: removes every vector and turns
    /// the helper off, so nothing is rebuilt until the user turns it on again.
    pub fn delete_semantic_index(&self) -> Result<(), String> {
        let s = self
            .semantic
            .as_ref()
            .ok_or("semantic search is not available")?;
        if let Some(h) = &self.plugins {
            let _ = h.set_enabled(PLUGIN_ID, false, false);
        }
        s.client.delete_all().map_err(|e| e.to_string())
    }

    /// The plugin host, if plugins are available.
    pub fn plugins(&self) -> Option<&Arc<PluginHost>> {
        self.plugins.as_ref()
    }

    /// "Index now" for file contents: runs without waiting for idle and
    /// without the duty cap until the queue is empty. False if content indexing is off.
    pub fn index_now(&self, on: bool) -> bool {
        let Some(c) = &self.content else {
            return false;
        };
        let guard = c.service.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(svc) = guard.as_ref() else {
            return false;
        };
        if on {
            svc.index_now();
        } else {
            svc.stop_index_now();
        }
        true
    }

    /// Queues a content rescan (no idle bypass): files that were left alone,
    /// such as Office files while the Office helper was off, get read.
    pub fn rescan_contents(&self) {
        let Some(c) = &self.content else { return };
        let guard = c.service.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(svc) = guard.as_ref() {
            svc.request_rescan();
        }
    }

    /// What each feature is doing and why, from live state.
    pub fn health(&self) -> Health {
        use crate::content::service::{STATE_PAUSED, STATE_RUNNING, STATE_WAITING_IDLE};
        let st = self.status();
        let name_state = if st.paused {
            (
                "paused",
                "Paused. Files already found can still be searched. Resume from the tray icon."
                    .to_owned(),
            )
        } else if st.indexing {
            (
                "running",
                "Reading your folders for the first time.".to_owned(),
            )
        } else if st.roots == 0 {
            ("off", "No folders chosen.".to_owned())
        } else {
            (
                "up to date",
                format!("Watching {} folder(s) for changes.", st.roots),
            )
        };
        let name_index = NameIndexHealth {
            count: st.files as u64 + st.folders as u64,
            state: name_state.0.to_owned(),
            reason: name_state.1,
        };

        let stats = self.content.as_ref().map(|c| c.stats.clone());
        // Merges grow the files on disk after the last commit; re-read the
        // live size so "full" cannot stay unseen once the queue is empty.
        if let (Some(c), Some(s)) = (&self.content, &stats) {
            s.size_bytes.store(c.store.size_bytes(), Ordering::Relaxed);
        }
        let pending = stats.as_ref().map_or(0, |s| {
            s.queued.load(Ordering::Relaxed) + s.pending.load(Ordering::Relaxed)
        });
        let (cstate, creason) = match (&self.content, &stats) {
            _ if !self.index_contents => (
                "off",
                "Off in Settings.".to_owned(),
            ),
            (None, _) => (
                "off",
                match &self.content_error {
                    Some(_) => "Searching inside files could not start. Restart Lookout.".to_owned(),
                    None => "No folders are chosen to read.".to_owned(),
                },
            ),
            (Some(_), Some(s)) if s.full() => (
                "full",
                "Nothing new is added. Everything already read stays searchable, and file names are not affected."
                    .to_owned(),
            ),
            (Some(c), Some(s)) => {
                let running = s.busy.load(Ordering::Acquire);
                match s.state.load(Ordering::Acquire) {
                    STATE_PAUSED => (
                        "paused",
                        "Paused from the pause key or tray. Resume to continue."
                            .to_owned(),
                    ),
                    STATE_WAITING_IDLE => (
                        "waiting for idle",
                        format!(
                            "Large files and screenshots wait until you are idle for {} s. Small text files are read right away.",
                            c.idle_secs
                        ),
                    ),
                    STATE_RUNNING if running => (
                        "running",
                        if s.boost.load(Ordering::Acquire) {
                            "Reading files now, without waiting.".to_owned()
                        } else {
                            "Reading files in the background.".to_owned()
                        },
                    ),
                    _ => (
                        "up to date",
                        "Up to date. New changes are picked up as you save.".to_owned(),
                    ),
                }
            }
            (Some(_), None) => ("off", "No folders are chosen to read.".to_owned()),
        };
        let content = ContentHealth {
            state: cstate.to_owned(),
            reason: creason,
            files_done: self.content.as_ref().map_or(0, |c| c.store.doc_count()),
            files_queued: pending,
            read_done: stats
                .as_ref()
                .map_or(0, |s| s.read_done.load(Ordering::Relaxed)),
            read_total: stats
                .as_ref()
                .map_or(0, |s| s.read_total.load(Ordering::Relaxed)),
            ocr_done: stats
                .as_ref()
                .map_or(0, |s| s.ocr_done.load(Ordering::Relaxed)),
            ocr_total: stats
                .as_ref()
                .map_or(0, |s| s.ocr_total.load(Ordering::Relaxed)),
            last_error: stats.as_ref().and_then(|s| s.last_error()),
            index_now_running: stats
                .as_ref()
                .is_some_and(|s| s.boost.load(Ordering::Acquire)),
            size_bytes: self.content.as_ref().map_or(0, |c| c.store.size_bytes()),
            max_bytes: stats
                .as_ref()
                .map_or(0, |s| s.max_bytes.load(Ordering::Relaxed)),
            full: cstate == "full",
        };

        let (sstate, sreason) = match self.content.as_ref().map(|c| &c.ocr) {
            None => ("off", "File text is off, so pictures are not read.".to_owned()),
            Some(None) => ("off", "Off in Settings.".to_owned()),
            Some(Some(o)) if o.available() => (
                "available",
                "Ready to read text in pictures.".to_owned(),
            ),
            Some(Some(_)) => (
                "unavailable",
                "No text-reading language installed. Add one in Windows Settings > Time & language > Language & region.".to_owned(),
            ),
        };
        let screenshots = ScreenshotHealth {
            state: sstate.to_owned(),
            reason: sreason,
            last_error: None,
        };

        let plugins = self
            .plugins
            .as_ref()
            .map(|h| {
                h.list()
                    .into_iter()
                    .map(|p| {
                        let (state, reason) = if !p.enabled {
                            ("off", "Off in Add-ons.".to_owned())
                        } else if !p.approved {
                            ("off", "Waiting for you to allow it.".to_owned())
                        } else if p.running {
                            ("running", "Running.".to_owned())
                        } else if p.last_error.is_some() {
                            (
                                "failed",
                                "It stopped. Turn it off and on in Add-ons.".to_owned(),
                            )
                        } else {
                            ("ready", "Starts when it is first needed.".to_owned())
                        };
                        PluginHealth {
                            name: p.name,
                            state: state.to_owned(),
                            reason,
                            last_error: p.last_error,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Health {
            name_index,
            content,
            screenshots,
            plugins,
        }
    }

    /// Segments in the full-text index (for the benchmark).
    pub fn content_segments(&self) -> usize {
        self.content.as_ref().map_or(0, |c| c.store.segment_count())
    }

    /// Counters of the content worker (for tests and the benchmark).
    pub fn content_stats(&self) -> Option<Arc<ContentStats>> {
        self.content.as_ref().map(|c| c.stats.clone())
    }

    fn stop_content(&self) {
        if let Some(c) = &self.content {
            if let Some(mut svc) = c
                .service
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                svc.stop();
            }
        }
    }

    /// Stops indexing and writes the latest name snapshot. The handle must not
    /// be used for anything that writes afterwards (its `meta.db` writes are
    /// switched off so it cannot overwrite what a successor saved).
    pub fn shutdown(&self) {
        self.timers.stop();
        self.disk.close();
        self.stop_semantic();
        if let Some(h) = &self.plugins {
            h.shutdown();
        }
        self.stop_content();
        if let Some(c) = &self.content {
            c.store.close();
        }
        if let Some(service) = self
            .service
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            service.stop();
        }
        if let Some(p) = self
            .persister
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            p.stop(true);
        }
        self.storage.detach();
    }

    /// "Delete my index": stops indexing, then deletes the name snapshot,
    /// `meta.db` (frecency and the undo log) and the encryption key, and
    /// empties the in-memory index. Afterwards this handle finds nothing;
    /// start a new `Lookout` to index again. Settings are left alone.
    pub fn wipe_index(&self) -> Result<(), WipeError> {
        self.wiped.store(true, Ordering::Release);
        self.timers.stop();
        self.pause.set(true);
        self.disk.close();
        if let Some(h) = &self.plugins {
            h.shutdown();
        }
        // Stop everything that writes before deleting anything.
        self.stop_semantic();
        self.stop_content();
        if let Some(c) = &self.content {
            c.store.close();
        }
        if let Some(service) = self
            .service
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            service.stop();
        }
        if let Some(p) = self
            .persister
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            p.stop(false);
        }
        self.storage.detach();
        // Out of memory too; nothing is written any more.
        if self.notes.clear().is_err() {
            crate::log::warn("could not forget the notes");
        }
        *write(&self.index) = crate::name_index::NameIndex::new();
        // Pictures dragged out of the clipboard history.
        crate::dragtemp::clear(&crate::dragtemp::dir());
        if let Some(paths) = &self.storage.paths {
            paths.wipe()?;
            // What plugins stored (clipboard history) was encrypted with the key just deleted.
            match std::fs::remove_dir_all(paths.root().join("plugins")) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }

    /// Gives back memory that is only a cache: for when the bar has been hidden a while.
    pub fn trim_memory(&self) {
        if let Some(c) = &self.content {
            c.store.drop_caches();
        }
        crate::memory::release_free_heap();
    }

    /// Starts a query; see [`Engine::submit`].
    pub fn query(&self, generation: u64, text: &str) -> bool {
        self.engine.submit(generation, text)
    }

    /// Like [`query`](Self::query), ignoring the date or kind words in `skip`.
    pub fn query_with(&self, generation: u64, text: &str, skip: crate::query::Skip) -> bool {
        self.engine.submit_with(generation, text, skip)
    }

    /// A search for the command line: the same sources, ranking and date/kind
    /// words as the bar, but in its own short-lived engine, so it never
    /// replaces the bar's results or the actions that belong to them. Waits
    /// for every source (at most [`CLI_SEARCH_TIMEOUT`]).
    pub fn cli_search(
        &self,
        text: &str,
        kind: Option<lookout_cli::proto::KindFilter>,
        limit: usize,
    ) -> Vec<Item> {
        let (tx, rx) = std::sync::mpsc::channel::<Update>();
        let sink: Sink = Arc::new(move |u| {
            let _ = tx.send(u);
        });
        let cfg = EngineConfig {
            // With a kind filter, look further down the ranking before filtering.
            max_results: if kind.is_some() { 200 } else { 50 },
            ..EngineConfig::default()
        };
        let Ok(engine) = Engine::new(self.cli_sources.clone(), sink, cfg) else {
            return Vec::new();
        };
        engine.submit(1, text);
        let deadline = std::time::Instant::now() + CLI_SEARCH_TIMEOUT;
        let mut last: Option<Update> = None;
        while let Ok(u) =
            rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        {
            let done = u.done;
            last = Some(u);
            if done {
                break;
            }
        }
        drop(engine);
        last.map(|u| u.items)
            .unwrap_or_default()
            .into_iter()
            .filter(|i| !self.in_skipped_folder(i))
            .filter(|i| kind.is_none_or(|k| matches_kind(i, k)))
            .take(limit)
            .collect()
    }

    /// A search hit inside Lookout's own data or a folder the user excluded.
    fn in_skipped_folder(&self, item: &Item) -> bool {
        matches!(item.source.as_str(), "files" | "content" | "semantic")
            && item
                .uri
                .as_deref()
                .is_some_and(|u| self.excludes.is_excluded_folder(std::path::Path::new(u)))
    }

    /// Values to offer for `ext:` and `in:`, from what is already in memory.
    pub fn operator_values(&self) -> OperatorValues {
        OperatorValues {
            extensions: self
                .extensions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            folders: self
                .home_roots
                .iter()
                .map(|r| {
                    r.file_name()
                        .unwrap_or(r.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
        }
    }

    /// What the indexer skips: a file preview follows the same rules.
    pub fn file_rules(&self) -> Arc<Rules> {
        self.share_rules.clone()
    }

    /// The Home "Recent" files: recently changed, nothing skipped, hidden or
    /// temporary, the user's own kinds of files first.
    pub fn recent_files(&self, limit: usize) -> Vec<Item> {
        let items = crate::recent::files(
            &self.index,
            &self.home_roots,
            &self.excludes,
            unix_now(),
            limit,
        );
        let targets = items
            .iter()
            .filter_map(|i| target_of(i).map(|t| (i.id.clone(), t)))
            .collect();
        *self
            .home_targets
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = targets;
        items
    }

    /// Logs a command-line request like other actions: its kind and the length
    /// of the query, never the query itself.
    pub fn record_cli(&self, cmd: &str, query_len: usize, ok: bool) {
        self.actions.record_external(
            &format!("cli.{cmd}"),
            &serde_json::json!({ "query_len": query_len }),
            ok,
        );
    }

    pub fn status(&self) -> Status {
        let sem = self
            .semantic_status()
            .filter(|s| s.state == crate::semantic::SemanticState::Indexing);
        let s = self
            .service
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(IndexService::status);
        let stat = |f: fn(&ContentStats) -> &std::sync::atomic::AtomicU64| {
            self.content
                .as_ref()
                .map_or(0, |c| f(&c.stats).load(Ordering::Relaxed))
        };
        Status {
            files: s.map_or(0, |s| s.files),
            folders: s.map_or(0, |s| s.folders),
            apps: self
                .apps
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
            roots: self.roots,
            indexing: s.is_some_and(|s| s.crawling) || self.populating.load(Ordering::Acquire),
            paused: self.pause.is_paused(),
            sizes_pending: self.sizes_pending.load(Ordering::Acquire),
            needs_setup: self.needs_setup,
            content_docs: self.content.as_ref().map_or(0, |c| c.store.doc_count()),
            semantic_done: sem.as_ref().map_or(0, |s| s.done),
            semantic_total: sem.as_ref().map_or(0, |s| s.total),
            read_done: stat(|s| &s.read_done),
            read_total: stat(|s| &s.read_total),
            ocr_done: stat(|s| &s.ocr_done),
            ocr_total: stat(|s| &s.ocr_total),
            waiting_for_idle: self.content.as_ref().is_some_and(|c| {
                c.stats.state.load(Ordering::Acquire) == crate::content::service::STATE_WAITING_IDLE
            }),
            pauses_on_activity: self
                .content
                .as_ref()
                .is_some_and(|c| c.idle_secs > 0 && !c.stats.boost.load(Ordering::Acquire)),
        }
    }

    /// The target of a result from the most recent update or the Home list,
    /// if it can be acted on.
    pub fn target(&self, id: &str) -> Option<Target> {
        [&*self.targets, &self.home_targets]
            .into_iter()
            .find_map(|map| {
                map.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(id)
                    .cloned()
            })
    }

    /// The file a result stands for, if it may be dragged into another program.
    pub fn shareable_path(&self, item_id: &str) -> Result<PathBuf, Refusal> {
        let target = self.target(item_id).ok_or(Refusal::NotAFile)?;
        self.share_rules
            .path(&target.kind, target.uri.as_deref(), target.origin.is_some())
    }

    /// What dragging a result into another program carries: a clip's stored
    /// text, or a file.
    pub fn drag_payload(&self, item_id: &str) -> Result<Payload, Refusal> {
        let target = self.target(item_id).ok_or(Refusal::NotAFile)?;
        if target.kind != Kind::Clipboard {
            return self.shareable_path(item_id).map(Payload::File);
        }
        match self.clip_drag(&target, false)? {
            DragData::Text(text) => Ok(Payload::Text(text)),
            DragData::File(file) => Ok(Payload::TempFile(file)),
            DragData::Masked => Err(Refusal::MaskedClip),
            DragData::Picture => Err(Refusal::ClipGone),
        }
    }

    /// Why a result would not drag, found without writing anything.
    pub fn drag_refusal(&self, item_id: &str) -> Option<Refusal> {
        let target = self.target(item_id)?;
        if target.kind != Kind::Clipboard {
            return self.shareable_path(item_id).err();
        }
        match self.clip_drag(&target, true) {
            Ok(DragData::Masked) => Some(Refusal::MaskedClip),
            Ok(_) => None,
            Err(why) => Some(why),
        }
    }

    fn clip_drag(&self, target: &Target, check: bool) -> Result<DragData, Refusal> {
        let origin = target.origin.as_ref().ok_or(Refusal::NotAFile)?;
        let host = self.plugins.as_ref().ok_or(Refusal::ClipGone)?;
        host.drag_data(&origin.plugin_id, &origin.params, check)
            .map_err(|_| Refusal::ClipGone)
    }

    /// The small picture of an image clip on screen, as a `data:` URL.
    pub fn clip_thumbnail(&self, item_id: &str) -> Option<String> {
        let target = self.target(item_id)?;
        if target.kind != Kind::Clipboard {
            return None;
        }
        let origin = target.origin.as_ref()?;
        self.plugins
            .as_ref()?
            .thumbnail(&origin.plugin_id, &origin.params)
            .ok()
    }

    pub fn quick_keys(&self) -> &crate::quickkeys::QuickKeys {
        &self.quick_keys
    }

    /// Recent searches (the typed text only).
    pub fn history(&self) -> &crate::history::SearchHistory {
        &self.history
    }

    pub fn notes(&self) -> &NoteStore {
        &self.notes
    }

    /// Gives a result from the latest update a quick key. The result is
    /// looked up here; the webview names only its id and the alias.
    pub fn set_quick_key(
        &self,
        item_id: &str,
        alias: &str,
    ) -> Result<String, crate::quickkeys::KeyError> {
        let item = self
            .keyable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(item_id)
            .cloned()
            .ok_or(crate::quickkeys::KeyError::NotSupported)?;
        self.quick_keys.add(alias, &item)
    }

    /// The disk usage analyzer (on demand; idle until a scan is started).
    pub fn disk(&self) -> &crate::diskusage::DiskService {
        &self.disk
    }

    pub fn tidy(&self) -> &crate::cleanup::Tidy {
        &self.tidy
    }

    /// Step 1 for removing a stored list (Clean up item or chosen duplicates).
    pub fn prepare_batch(&self, batch_id: &str) -> Result<Prepared, ActionError> {
        let target = Target {
            item_id: format!("batch:{batch_id}"),
            kind: Kind::Folder,
            uri: Some(format!("batch:{batch_id}")),
            port: None,
            origin: None,
        };
        self.actions
            .prepare(crate::actions::disk::BATCH_REMOVE, target)
    }

    /// Step 1 for an action on a row of the disk view. The path comes from the
    /// scan held here; the webview only names a row.
    pub fn prepare_disk_action(&self, action_id: &str, node: u32) -> Result<Prepared, ActionError> {
        use crate::diskusage::NodeKind;
        let tree = self
            .disk
            .tree()
            .ok_or_else(|| ActionError::Stale("there is no scan open".into()))?;
        let kind = match tree.kind_of(node) {
            Some(NodeKind::Folder) => Kind::Folder,
            Some(NodeKind::File) => Kind::File,
            _ => {
                return Err(ActionError::Refused(
                    "that row is not a file or folder".into(),
                ))
            }
        };
        let path = self
            .disk
            .hint_for(node)
            .ok_or_else(|| ActionError::Stale("that row is no longer in the scan".into()))?;
        let target = Target {
            item_id: format!("disk:{node}"),
            kind,
            uri: Some(path.to_string_lossy().into_owned()),
            port: None,
            origin: None,
        };
        self.actions.prepare(action_id, target)
    }

    /// Like [`Self::prepare_disk_action`] for a row of the large-file tabs.
    pub fn prepare_disk_file_action(
        &self,
        action_id: &str,
        hit: u32,
    ) -> Result<Prepared, ActionError> {
        let path = self
            .disk
            .hint_for_hit(hit)
            .ok_or_else(|| ActionError::Stale("that file is no longer in the scan".into()))?;
        let target = Target {
            item_id: format!("disk-file:{hit}"),
            kind: Kind::File,
            uri: Some(path.to_string_lossy().into_owned()),
            port: None,
            origin: None,
        };
        self.actions.prepare(action_id, target)
    }

    /// Step 1 of every action: build the preview from the live state. The item
    /// must be one the backend sent in its latest update.
    pub fn prepare_action(&self, action_id: &str, item_id: &str) -> Result<Prepared, ActionError> {
        let target = self.target(item_id).ok_or_else(|| {
            ActionError::Stale("that result is no longer available; search again".into())
        })?;
        self.actions.prepare(action_id, target)
    }

    pub fn prepare_undo(&self) -> Result<Prepared, ActionError> {
        self.actions.prepare_undo()
    }

    /// Step 2: the interface reports that the preview is on screen.
    pub fn present_action(&self, ticket: &str) -> Result<(), ActionError> {
        self.actions.present(ticket)
    }

    pub fn cancel_action(&self, ticket: &str) {
        self.actions.cancel(ticket);
    }

    /// Step 3 for `safe` actions. Refused for anything that needs confirmation.
    pub fn run_action(&self, ticket: &str) -> Result<Done, ActionError> {
        let done = self.actions.run(ticket)?;
        self.after_run(&done);
        Ok(done)
    }

    /// Step 3 for any action: the user confirmed the preview that was shown.
    pub fn confirm_action(&self, ticket: &str) -> Result<Done, ActionError> {
        let done = self.actions.confirm(ticket)?;
        self.after_run(&done);
        Ok(done)
    }

    pub fn action_history(&self, limit: usize) -> Result<Vec<Entry>, ActionError> {
        self.actions.history(limit)
    }

    pub fn can_undo(&self) -> bool {
        self.actions.can_undo()
    }

    fn after_run(&self, done: &Done) {
        if done.action_id == open::ID && !done.was_undo {
            for id in &done.items {
                if self.record_open(id).is_err() {
                    crate::log::warn("could not record an open");
                }
            }
        }
    }

    /// Counts an open for ranking: persists it and lifts the file in the index.
    pub fn record_open(&self, id: &str) -> Result<(), FrecencyError> {
        let score = self.frecency.record_open(id, i64::from(unix_now()))?;
        if let Some(path) = id
            .strip_prefix("file:")
            .or_else(|| id.strip_prefix("folder:"))
        {
            write(&self.index)
                .set_frecency(std::path::Path::new(path), ranking::frecency_level(score));
        }
        Ok(())
    }
}

/// Opens the content index; one that fails to open is removed and made again,
/// once. The flag says that happened.
fn open_content(
    dir: &std::path::Path,
    key: &crate::crypto::Key,
) -> Result<(ContentStore, bool), crate::content::store::StoreError> {
    use crate::content::store::StoreError;
    let attempt = || {
        crate::guard::contain("opening the content index failed", || {
            ContentStore::open(dir, key)
        })
        .unwrap_or(Err(StoreError::Closed))
    };
    match attempt() {
        Ok(store) => {
            let rebuilt = store.was_rebuilt();
            Ok((store, rebuilt))
        }
        // Another copy is using it: nothing is wrong with the files.
        Err(StoreError::InUse) => Err(StoreError::InUse),
        Err(_) => {
            crate::log::error("the content index did not open and is being rebuilt");
            if let Err(e) = std::fs::remove_dir_all(dir) {
                crate::log::error_io("the damaged content index could not be removed", &e);
            }
            attempt().map(|store| (store, true))
        }
    }
}

/// What an action may act on for this result, if anything.
fn target_of(item: &crate::item::Item) -> Option<Target> {
    if item.source.starts_with("plugin:") {
        let meta = |k: &str| item.metadata.get(k);
        return Some(Target {
            item_id: item.id.clone(),
            kind: item.kind.clone(),
            uri: item.uri.clone(),
            port: None,
            origin: Some(PluginOrigin {
                plugin_id: meta("plugin")?.as_str()?.to_owned(),
                action_id: meta("plugin_action")?.as_str()?.to_owned(),
                also: meta("plugin_also")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default(),
                params: meta("plugin_params").cloned().unwrap_or_default(),
            }),
        });
    }
    match item.kind {
        Kind::File | Kind::Folder | Kind::App => Some(Target {
            item_id: item.id.clone(),
            kind: item.kind.clone(),
            uri: Some(item.uri.clone()?),
            port: None,
            origin: None,
        }),
        Kind::Port => {
            let port = item.metadata.get("port")?.as_u64()?;
            Some(Target {
                item_id: item.id.clone(),
                kind: Kind::Port,
                uri: None,
                port: Some(u16::try_from(port).ok()?),
                origin: None,
            })
        }
        Kind::Custom(ref name)
            if name == quick_actions::KIND_URL
                || name == quick_actions::KIND_SYSTEM
                || name == crate::timers::KIND_NEW
                || name == crate::timers::KIND_RUNNING
                || name == crate::answers::KIND_DATE
                || name == crate::answers::KIND_TIME
                || name == crate::answers::KIND_CALC
                || name == crate::answers::KIND_CONVERSION
                || name == crate::notes::KIND_NEW
                || name == crate::notes::KIND_NOTE =>
        {
            Some(Target {
                item_id: item.id.clone(),
                kind: item.kind.clone(),
                uri: Some(item.uri.clone()?),
                port: None,
                origin: None,
            })
        }
        _ => None,
    }
}

fn default_system() -> Arc<dyn ProcessBackend> {
    Arc::new(crate::actions::system::WindowsSystem)
}

/// The default action for each kind of result.
/// How long a command-line search waits for slow sources.
pub const CLI_SEARCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether a result belongs under a `--kind` of the command line.
pub fn matches_kind(item: &Item, kind: lookout_cli::proto::KindFilter) -> bool {
    use lookout_cli::proto::KindFilter as K;
    let ocr = item.metadata.get("ocr") == Some(&serde_json::Value::Bool(true));
    match kind {
        K::Files => matches!(item.kind, Kind::File | Kind::Folder) && item.source != "semantic",
        K::Apps => item.kind == Kind::App,
        K::Clipboard => item.kind == Kind::Clipboard,
        K::Screens => item.kind == Kind::Screenshot || ocr,
        K::Meaning => item.source == "semantic",
    }
}

pub fn default_action(kind: &Kind) -> Option<&'static str> {
    match kind {
        Kind::File | Kind::Folder | Kind::App => Some(open::ID),
        Kind::Port => Some(kill_port::ID),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::open::NoopLauncher;
    use std::fs;
    use std::sync::mpsc;
    use std::time::Instant;

    fn test_key() -> crate::crypto::Key {
        crate::crypto::Key::from_bytes(&[7u8; 32]).unwrap()
    }

    struct Fixture {
        lookout: Lookout,
        updates: Arc<Mutex<Vec<Update>>>,
        _dir: tempfile::TempDir,
    }

    fn make_files(dir: &std::path::Path) {
        let files = dir.join("files");
        fs::create_dir_all(&files).unwrap();
        fs::write(files.join("budget alpha.xlsx"), b"x").unwrap();
        fs::write(files.join("budget beta.xlsx"), b"x").unwrap();
        let start_menu = dir.join("start");
        fs::create_dir_all(&start_menu).unwrap();
        fs::write(start_menu.join("Notepad.lnk"), b"").unwrap();
    }

    fn start(extra: impl FnOnce(&std::path::Path)) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        extra(dir.path());
        let (lookout, updates) = launch(dir.path());
        Fixture {
            lookout,
            updates,
            _dir: dir,
        }
    }

    /// Starts Lookout over the files made by `make_files`; frecency goes to `meta.db` in `dir`.
    fn launch(dir: &std::path::Path) -> (Lookout, Arc<Mutex<Vec<Update>>>) {
        launch_with(dir, Settings::default(), PauseState::new())
    }

    fn launch_with(
        dir: &std::path::Path,
        settings: Settings,
        pause: PauseState,
    ) -> (Lookout, Arc<Mutex<Vec<Update>>>) {
        let files = dir.join("files");
        let start_menu = dir.join("start");
        let updates: Arc<Mutex<Vec<Update>>> = Arc::default();
        let u = updates.clone();
        let (tx, _rx) = mpsc::channel::<()>();
        let sink: Sink = Arc::new(move |update| {
            u.lock().unwrap().push(update);
            let _ = tx.send(());
        });
        let lookout = Lookout::start(
            StartOptions {
                settings,
                roots_override: vec![files],
                synthetic_files: None,
                synthetic_uses_folders: false,
                start_menu_dirs: vec![start_menu],
                store_apps: false,
                storage: crate::storage::Storage::with_key(
                    crate::storage::DataPaths::new(dir),
                    test_key(),
                )
                .unwrap(),
                idle: Arc::new(crate::content::idle::AlwaysIdle),
                ocr: Arc::new(crate::content::ocr::NoOcr),
                launcher: Arc::new(NoopLauncher),
                system: None,
                gate: GateConfig::default(),
                plugins: None,
            },
            sink,
            pause,
        )
        .unwrap();
        (lookout, updates)
    }

    impl Fixture {
        fn ask(&self, generation: u64, text: &str) -> Update {
            assert!(self.lookout.query(generation, text));
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(u) = self
                    .updates
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|u| u.generation == generation && u.done)
                {
                    return u.clone();
                }
                assert!(Instant::now() < deadline, "query never completed");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn wait_indexed(&self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.lookout.status().indexing {
                assert!(Instant::now() < deadline, "indexing never finished");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn command_line_search_returns_what_the_bar_returns_and_leaves_the_bar_alone() {
        use lookout_cli::proto::KindFilter;
        let f = start(|_| {});
        f.wait_indexed();
        for (g, q) in [
            (1, "budget"),
            (2, "notepad"),
            (3, "bud"),
            (4, "budget alpha"),
        ] {
            let bar: Vec<String> = f.ask(g, q).items.iter().map(|i| i.id.clone()).collect();
            let cli: Vec<String> = f
                .lookout
                .cli_search(q, None, 50)
                .iter()
                .map(|i| i.id.clone())
                .collect();
            assert!(!bar.is_empty(), "{q}");
            assert_eq!(bar, cli, "{q}");
        }
        // limit and kind
        assert_eq!(f.lookout.cli_search("budget", None, 1).len(), 1);
        let apps = f.lookout.cli_search("notepad", Some(KindFilter::Apps), 10);
        assert!(!apps.is_empty() && apps.iter().all(|i| i.kind == Kind::App));
        assert!(f
            .lookout
            .cli_search("budget", Some(KindFilter::Apps), 10)
            .is_empty());
        let files = f.lookout.cli_search("budget", Some(KindFilter::Files), 10);
        assert_eq!(files.len(), 2);
        assert!(f
            .lookout
            .cli_search("budget", Some(KindFilter::Clipboard), 10)
            .is_empty());
        // the bar's own results and generations were not touched
        let seen = f.updates.lock().unwrap().len();
        f.lookout.cli_search("budget", None, 5);
        assert_eq!(f.updates.lock().unwrap().len(), seen);
        assert!(f.lookout.query(5, "budget"));
    }

    #[test]
    fn command_line_requests_are_logged_without_the_query() {
        let f = start(|_| {});
        f.lookout.record_cli("search", 12, true);
        let rows = f.lookout.action_history(5).unwrap();
        let row = rows.iter().find(|r| r.action_id == "cli.search").unwrap();
        assert!(format!("{row:?}").contains("12"));
    }

    #[test]
    fn status_reports_counts_roots_and_apps() {
        let f = start(|_| {});
        f.wait_indexed();
        let s = f.lookout.status();
        assert_eq!((s.files, s.roots, s.apps), (2, 1, 1));
        assert!(!s.indexing && !s.paused);
    }

    #[test]
    fn only_results_from_the_latest_update_can_be_opened() {
        let f = start(|_| {});
        f.wait_indexed();
        let r = f.ask(1, "budget");
        let first = &r.items[0];
        let target = f.lookout.target(&first.id).unwrap();
        assert_eq!(target.kind, Kind::File);
        assert_eq!(target.uri.as_deref(), first.uri.as_deref());
        assert!(f
            .lookout
            .target("file:C:\\Windows\\system32\\cmd.exe")
            .is_none());
        // Calculator answers are copied, never opened.
        let calc = f.ask(2, "1+1");
        let id = &calc.items[0].id;
        assert!(f.lookout.prepare_action("open", id).is_err());
        assert!(f.lookout.prepare_action("answer_copy", id).is_ok());
        // After a new query the old ids are forgotten.
        assert!(f.lookout.target(&first.id).is_none());
    }

    #[test]
    fn opening_an_item_ranks_it_higher_next_time_and_survives_restart() {
        let f = start(|_| {});
        f.wait_indexed();
        let before = f.ask(1, "budget");
        // Equal matches: whichever is second can be promoted by use.
        let second = before.items[1].clone();
        f.lookout.record_open(&second.id).unwrap();
        let after = f.ask(2, "budget");
        assert_eq!(after.items[0].id, second.id);
        assert!(after.items[0].score_features.frecency > 0.0);
    }

    #[test]
    fn frecency_survives_a_restart_and_is_reapplied_to_the_new_index() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        let opened = {
            let f = {
                let (lookout, updates) = launch(dir.path());
                Fixture {
                    lookout,
                    updates,
                    _dir: tempfile::tempdir().unwrap(),
                }
            };
            f.wait_indexed();
            let before = f.ask(1, "budget");
            // The lower-ranked of two near-equal files is the one we "use".
            let second = before.items[1].clone();
            f.lookout.record_open(&second.id).unwrap();
            second.id
            // `f` is dropped here: the process-level state is gone, only meta.db remains.
        };
        let (lookout, updates) = launch(dir.path());
        let f = Fixture {
            lookout,
            updates,
            _dir: tempfile::tempdir().unwrap(),
        };
        f.wait_indexed(); // the reindex hook has applied frecency before indexing reports done
        let after = f.ask(1, "budget");
        assert_eq!(
            after.items[0].id, opened,
            "the file opened before the restart now leads"
        );
        assert!(after.items[0].score_features.frecency > 0.0);
    }

    #[test]
    fn names_with_shell_metacharacters_are_found_and_open_to_the_exact_path() {
        let names = ["R&D plan.txt", "a^b.txt", "100%.txt"];
        let f = start(|dir| {
            for n in names {
                fs::write(dir.join("files").join(n), b"x").unwrap();
            }
        });
        f.wait_indexed();
        let files = f._dir.path().join("files");
        let queries = ["r&d plan", "a^b", "100%"];
        for (i, (name, query)) in names.iter().zip(queries).enumerate() {
            let update = f.ask(i as u64 + 1, query);
            let titles: Vec<_> = update.items.iter().map(|it| it.title.as_str()).collect();
            let item = update
                .items
                .iter()
                .find(|it| it.title == *name)
                .unwrap_or_else(|| panic!("{name:?} not found for {query:?}: {titles:?}"));
            assert_eq!(
                item.kind,
                Kind::File,
                "{query:?} must not turn into a calculation"
            );
            let expected = files.join(name);
            assert_eq!(item.id, format!("file:{}", expected.display()));
            let target = f.lookout.target(&item.id).unwrap();
            assert_eq!(target.kind, Kind::File);
            assert_eq!(
                std::path::Path::new(target.uri.as_deref().unwrap()),
                expected,
                "open target must be the exact path, characters unchanged"
            );
        }
    }

    #[test]
    fn apps_and_files_both_answer_one_query() {
        let f = start(|_| {});
        f.wait_indexed();
        let r = f.ask(1, "notepad");
        assert_eq!(r.items[0].kind, Kind::App);
    }

    fn paths_in(dir: &std::path::Path) -> crate::storage::DataPaths {
        crate::storage::DataPaths::new(dir)
    }

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_name_index_survives_a_restart_and_is_served_before_the_new_crawl_finishes() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        {
            let (lookout, updates) = launch(dir.path());
            let f = Fixture {
                lookout,
                updates,
                _dir: tempfile::tempdir().unwrap(),
            };
            f.wait_indexed();
            // dropped here: the saver writes the snapshot on a normal stop
        }
        let snapshot = std::fs::read(paths_in(dir.path()).names_file()).unwrap();
        assert!(
            !snapshot.windows(6).any(|w| w == b"budget"),
            "file names must not be readable in names.idx"
        );
        let meta = std::fs::read(paths_in(dir.path()).meta_db()).unwrap();
        assert!(crate::crypto::looks_sealed(&meta));

        // Restart with indexing paused: the crawl cannot run, so anything found
        // came from the snapshot.
        let pause = PauseState::new();
        pause.set(true);
        let (lookout, updates) = launch_with(dir.path(), Settings::default(), pause);
        let f = Fixture {
            lookout,
            updates,
            _dir: tempfile::tempdir().unwrap(),
        };
        assert_eq!(f.lookout.status().files, 2, "snapshot loaded at start");
        let r = f.ask(1, "budget");
        assert_eq!(r.items.len(), 2);
    }

    #[test]
    fn a_snapshot_made_for_other_folders_or_exclusions_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        fs::create_dir_all(dir.path().join("files").join("private")).unwrap();
        fs::write(
            dir.path()
                .join("files")
                .join("private")
                .join("diary budget.txt"),
            b"x",
        )
        .unwrap();
        {
            let (lookout, updates) = launch(dir.path());
            let f = Fixture {
                lookout,
                updates,
                _dir: tempfile::tempdir().unwrap(),
            };
            f.wait_indexed();
            assert_eq!(f.lookout.status().files, 3);
        }
        // The user now excludes the folder; the old snapshot still lists it.
        let settings = Settings {
            excluded_folders: vec![dir.path().join("files").join("private")],
            ..Settings::default()
        };
        let pause = PauseState::new();
        pause.set(true);
        let (lookout, updates) = launch_with(dir.path(), settings.clone(), pause);
        let f = Fixture {
            lookout,
            updates,
            _dir: tempfile::tempdir().unwrap(),
        };
        assert_eq!(
            f.lookout.status().files,
            0,
            "stale snapshot ignored while paused"
        );
        drop(f);
        let (lookout, updates) = launch_with(dir.path(), settings, PauseState::new());
        let f = Fixture {
            lookout,
            updates,
            _dir: tempfile::tempdir().unwrap(),
        };
        f.wait_indexed();
        let r = f.ask(1, "diary");
        assert!(
            r.items.is_empty(),
            "excluded folder is never indexed: {r:?}"
        );
        assert_eq!(f.lookout.status().files, 2);
    }

    #[test]
    fn a_snapshot_without_sizes_says_so_until_the_crawl_has_them() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        let files = dir.path().join("files");
        let old = new_shared_index();
        {
            let mut w = write(&old);
            let root = w.add_root(&files, 0).unwrap();
            w.add_file(root, "budget alpha.xlsx", 0).unwrap();
        }
        let fingerprint = names_store::fingerprint(
            std::slice::from_ref(&files),
            &names_store::excludes_key(&[], &[]),
        );
        let names = paths_in(dir.path()).names_file();
        names_store::save(&old, &names, &test_key(), &fingerprint).unwrap();

        let pause = PauseState::new();
        pause.set(true);
        let (lookout, updates) = launch_with(dir.path(), Settings::default(), pause.clone());
        let f = Fixture {
            lookout,
            updates,
            _dir: tempfile::tempdir().unwrap(),
        };
        assert!(f.lookout.status().sizes_pending);
        // The snapshot is still served: only the sizes are missing.
        assert_eq!(f.ask(1, "budget").items.len(), 1);
        assert!(f.ask(2, "size:<1kb").items.is_empty());

        pause.set(false);
        f.wait_indexed();
        let deadline = Instant::now() + Duration::from_secs(10);
        while f.lookout.status().sizes_pending {
            assert!(Instant::now() < deadline, "the sizes never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(f.ask(3, "size:<1kb").items.len(), 2);
    }

    #[test]
    fn an_old_snapshot_is_pruned_of_skipped_places_without_a_reindex() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        let files = dir.path().join("files");
        let paths = paths_in(dir.path());
        let old = new_shared_index();
        {
            let mut w = write(&old);
            let root = w.add_root(&files, 0).unwrap();
            w.add_file(root, "kept budget.txt", 0).unwrap();
            let appdata = w.add_dir(root, "AppData", 0).unwrap();
            let roaming = w.add_dir(appdata, "Roaming", 0).unwrap();
            w.add_file(roaming, "meta.json", 0).unwrap();
            // A root this version no longer indexes.
            let gone = w.add_root(&dir.path().join("elsewhere"), 0).unwrap();
            w.add_file(gone, "BrowserMetrics", 0).unwrap();
        }
        let fingerprint = names_store::fingerprint(
            std::slice::from_ref(&files),
            &names_store::excludes_key(&[], &[]),
        );
        names_store::save(&old, &paths.names_file(), &test_key(), &fingerprint).unwrap();

        // Paused, so no crawl: the index is the pruned snapshot.
        let pause = PauseState::new();
        pause.set(true);
        let (lookout, updates) = launch_with(dir.path(), Settings::default(), pause);
        let f = Fixture {
            lookout,
            updates,
            _dir: tempfile::tempdir().unwrap(),
        };
        let s = f.lookout.status();
        assert_eq!((s.files, s.folders), (1, 1));
        assert!(f.ask(1, "meta").items.is_empty());
        assert_eq!(f.ask(2, "budget").items.len(), 1);
    }

    #[test]
    fn lookout_data_is_never_indexed_even_below_a_root() {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        let data = files.join(crate::exclude::APP_ID);
        fs::create_dir_all(data.join("content")).unwrap();
        fs::write(data.join("content").join("zebra meta.json"), b"x").unwrap();
        fs::write(files.join("zebra.txt"), b"x").unwrap();
        let sink: Sink = Arc::new(|_| {});
        let lookout = Lookout::start(
            StartOptions {
                settings: Settings::default(),
                roots_override: vec![files],
                synthetic_files: None,
                synthetic_uses_folders: false,
                start_menu_dirs: Vec::new(),
                store_apps: false,
                storage: Storage::with_key(crate::storage::DataPaths::new(&data), test_key())
                    .unwrap(),
                idle: Arc::new(crate::content::idle::AlwaysIdle),
                ocr: Arc::new(crate::content::ocr::NoOcr),
                launcher: Arc::new(NoopLauncher),
                system: None,
                gate: GateConfig::default(),
                plugins: None,
            },
            sink,
            PauseState::new(),
        )
        .unwrap();
        wait_until("indexed", || !lookout.status().indexing);
        let names = |items: Vec<Item>| items.into_iter().map(|i| i.title).collect::<Vec<_>>();
        assert_eq!(names(lookout.cli_search("zebra", None, 20)), ["zebra.txt"]);
        assert_eq!(names(lookout.recent_files(6)), ["zebra.txt"]);
        let recent = lookout.recent_files(6);
        assert!(lookout.target(&recent[0].id).is_some(), "can be opened");
    }

    #[test]
    fn delete_my_index_removes_every_index_file_and_the_key() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        let paths = paths_in(dir.path());
        let (lookout, updates) = launch(dir.path());
        let f = Fixture {
            lookout,
            updates,
            _dir: tempfile::tempdir().unwrap(),
        };
        f.wait_indexed();
        // Use it so frecency and the undo log have data.
        let first = f.ask(1, "budget").items[0].id.clone();
        f.lookout.record_open(&first).unwrap();
        // Wait for the saver to write the snapshot, so there is something to delete.
        wait_until("snapshot", || paths.names_file().exists());
        assert!(paths.meta_db().exists());
        // The key lives next to the data; Storage::with_key does not write one,
        // so make one the way the app does.
        crate::crypto::KeyStore::new(paths.key_file())
            .load_or_create()
            .unwrap();
        std::fs::write(paths.ocr_marker(), "3").unwrap();
        assert!(!paths.leftovers().is_empty());

        f.lookout.wipe_index().unwrap();
        assert!(!paths.ocr_marker().exists());

        assert!(
            paths.leftovers().is_empty(),
            "left behind: {:?}",
            paths.leftovers()
        );
        assert!(!paths.key_file().exists());
        // Nothing comes back, and the live index is empty.
        std::thread::sleep(Duration::from_millis(800));
        assert!(
            paths.leftovers().is_empty(),
            "re-created: {:?}",
            paths.leftovers()
        );
        assert_eq!(f.lookout.status().files, 0);
        assert!(f.ask(2, "budget").items.is_empty());
        // Writes after the wipe do not re-create meta.db.
        let _ = f.lookout.record_open(&first);
        assert!(!paths.meta_db().exists());
    }

    #[test]
    fn notes_stay_out_of_recent_files_and_the_command_line_and_are_wiped() {
        let f = start(|_| {});
        f.wait_indexed();
        let paths = paths_in(f._dir.path());
        let save = f.ask(1, "note: zebra crossing budget");
        // Only the notes see what follows "note:".
        assert_eq!(save.items.len(), 1, "{:?}", save.items);
        assert!(save.filters.operators.is_empty() && !save.filters.invalid_operator);
        let ready = f
            .lookout
            .prepare_action(crate::notes::SAVE, &save.items[0].id)
            .unwrap();
        f.lookout.run_action(&ready.ticket).unwrap();
        let in_bar = f.ask(2, "zebra");
        assert!(in_bar.items.iter().any(|i| i.source == "notes"));
        for text in ["zebra", "notes", "note: zebra", "budget"] {
            let cli = f.lookout.cli_search(text, None, 50);
            assert!(cli.iter().all(|i| i.source != "notes"), "{text}: {cli:?}");
        }
        assert!(f
            .lookout
            .recent_files(50)
            .iter()
            .all(|i| i.source != "notes" && !i.title.contains("zebra")));
        assert!(!f.lookout.history().record("note: zebra", true).unwrap());
        let log = serde_json::to_string(&f.lookout.action_history(10).unwrap()).unwrap();
        assert!(!log.contains("zebra"), "{log}");
        f.lookout.wipe_index().unwrap();
        assert!(paths.leftovers().is_empty(), "{:?}", paths.leftovers());
        assert!(f
            .ask(3, "notes")
            .items
            .iter()
            .all(|i| !i.title.contains("zebra")));
        assert!(!paths.meta_db().exists());
    }

    #[test]
    fn diagnostics_hold_no_names_searches_notes_or_paths() {
        let f = start(|d| {
            fs::write(
                d.join("files").join("zebrasecret plan.txt"),
                b"quokkabody text",
            )
            .unwrap();
        });
        crate::log::init(crate::log::folder(f._dir.path()));
        f.wait_indexed();
        f.ask(1, "wombatquery");
        f.lookout.history().record("wombatquery", true).unwrap();
        f.ask(2, "note narwhalnote");
        crate::log::error_io(
            "could not save a test file",
            &std::io::Error::other(format!("{}\\zebrasecret", f._dir.path().display())),
        );
        crate::log::panic_at("C:\\Users\\pangolin\\.cargo\\x\\src\\lib.rs", 3);

        let errors = crate::log::last_errors(
            &crate::log::folder(f._dir.path()),
            crate::diagnostics::ERROR_LINES,
        );
        let facts = crate::diagnostics::Facts {
            app_version: "1.2.3".into(),
            scales: vec![1.5],
            mode: "standard".into(),
            theme: f._dir.path().display().to_string(),
            safe_mode: false,
        };
        let text = crate::diagnostics::render(&facts, Some(&f.lookout.counts()), &errors);
        assert!(text.contains("Lookout 1.2.3"), "{text}");
        assert!(
            text.contains("could not save a test file (Other)"),
            "{text}"
        );
        assert!(text.contains("lib.rs line 3"), "{text}");
        assert!(text.contains("3 files"), "{text}");
        let folder = f._dir.path().file_name().unwrap().to_string_lossy();
        for secret in [
            "zebrasecret",
            "quokkabody",
            "wombatquery",
            "narwhalnote",
            "pangolin",
            "budget",
            "Notepad",
            folder.as_ref(),
            "\\",
            "/",
        ] {
            assert!(!text.contains(secret), "{secret} in:\n{text}");
        }
    }

    #[test]
    fn rebuild_reads_everything_again_and_keeps_saved_data() {
        let f = start(|_| {});
        f.wait_indexed();
        f.lookout.history().record("kept search", true).unwrap();
        wait_until("the rebuild to start", || f.lookout.rebuild_index().is_ok());
        wait_until("the rebuild to finish", || {
            f.lookout.rebuild_progress().is_some_and(|p| p.done)
        });
        assert!(f.lookout.rebuild_progress().is_none());
        assert_eq!(f.lookout.history().recent(5).unwrap(), ["kept search"]);
        assert_eq!(f.ask(5, "budget alpha").items.len(), 1);
    }

    #[test]
    fn a_damaged_index_is_rebuilt_and_said_once() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        fs::write(dir.path().join("names.idx"), b"not an index").unwrap();
        fs::create_dir_all(dir.path().join("content")).unwrap();
        fs::write(dir.path().join("content").join("meta.json"), b"junk").unwrap();
        let (lookout, _) = launch(dir.path());
        assert_eq!(lookout.take_notice(), Some(REPAIRED));
        assert_eq!(lookout.take_notice(), None);
        wait_until("the crawl", || lookout.status().files == 2);
        lookout.shutdown();
        let (again, _) = launch(dir.path());
        assert_eq!(again.take_notice(), None);
    }

    #[test]
    fn safe_mode_serves_the_snapshot_without_indexing_or_replacing_it() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        let (first, _) = launch(dir.path());
        wait_until("the crawl", || {
            let s = first.status();
            s.files == 2 && !s.indexing
        });
        first.shutdown();
        fs::write(dir.path().join("files").join("new.txt"), b"x").unwrap();
        let settings = Settings {
            safe_mode: true,
            ..Settings::default()
        };
        let (safe, _) = launch_with(dir.path(), settings, PauseState::new());
        assert_eq!(safe.status().files, 2);
        assert!(!safe.status().indexing);
        assert!(safe.rebuild_index().is_err());
        safe.shutdown();
        assert!(dir.path().join("names.idx").exists());
    }

    #[test]
    fn the_first_run_flag_reflects_settings() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        let (l, _) = launch_with(dir.path(), Settings::default(), PauseState::new());
        assert!(l.status().needs_setup);
        drop(l);
        let done = Settings {
            first_run_done: true,
            ..Settings::default()
        };
        let (l, _) = launch_with(dir.path(), done, PauseState::new());
        assert!(!l.status().needs_setup);
    }

    #[test]
    fn a_snapshot_above_snapshot_max_entries_is_not_loaded_and_the_crawl_fills_the_index() {
        let dir = tempfile::tempdir().unwrap();
        make_files(dir.path());
        {
            let (lookout, updates) = launch(dir.path());
            let f = Fixture {
                lookout,
                updates,
                _dir: tempfile::tempdir().unwrap(),
            };
            f.wait_indexed();
        }
        // 2 files + 1 root folder = 3 entries. With a limit of 2 the snapshot is ignored
        // (paused, so the crawl cannot fill it either: the index starts empty).
        let pause = PauseState::new();
        pause.set(true);
        let small = Settings {
            snapshot_max_entries: 2,
            ..Settings::default()
        };
        let (l, _) = launch_with(dir.path(), small, pause.clone());
        assert_eq!(l.status().files, 0, "search starts empty above the limit");
        drop(l);
        let (l, _) = launch_with(dir.path(), Settings::default(), pause);
        assert_eq!(l.status().files, 2, "below the limit it is loaded");
        drop(l);
        // Unpaused, the startup crawl fills the empty index.
        let small = Settings {
            snapshot_max_entries: 2,
            ..Settings::default()
        };
        let (lookout, updates) = launch_with(dir.path(), small, PauseState::new());
        let f = Fixture {
            lookout,
            updates,
            _dir: tempfile::tempdir().unwrap(),
        };
        f.wait_indexed();
        assert_eq!(f.lookout.status().files, 2);
    }

    #[test]
    fn synthetic_mode_never_reads_the_configured_folders_unless_told_to() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("private.txt"), "zebrafish secret plans").unwrap();
        let run = |uses_folders: bool| {
            let sink: Sink = Arc::new(|_| {});
            let settings = Settings {
                index_roots: vec![real.clone()],
                idle_secs: 0,
                ..Settings::default()
            };
            Lookout::start(
                StartOptions {
                    settings,
                    roots_override: Vec::new(),
                    synthetic_files: Some(100),
                    synthetic_uses_folders: uses_folders,
                    start_menu_dirs: Vec::new(),
                    store_apps: false,
                    storage: Storage::in_memory().unwrap(),
                    idle: Arc::new(crate::content::idle::AlwaysIdle),
                    ocr: Arc::new(crate::content::ocr::NoOcr),
                    launcher: Arc::new(NoopLauncher),
                    system: None,
                    gate: GateConfig::default(),
                    plugins: None,
                },
                sink,
                PauseState::new(),
            )
            .unwrap()
        };
        let l = run(false);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            l.content_stats().is_none(),
            "no content worker without the flag"
        );
        assert_eq!(l.status().content_docs, 0);
        assert_eq!(l.status().roots, 0);
        drop(l);
        let l = run(true);
        wait_until("content indexing with the explicit flag", || {
            l.content_stats()
                .is_some_and(|s| s.extractions.load(Ordering::Relaxed) > 0)
        });
    }

    #[test]
    fn synthetic_mode_builds_the_requested_index_without_crawling() {
        let dir = tempfile::tempdir().unwrap();
        let sink: Sink = Arc::new(|_| {});
        let lookout = Lookout::start(
            StartOptions {
                settings: Settings::default(),
                roots_override: vec![dir.path().to_path_buf()],
                synthetic_files: Some(5000),
                synthetic_uses_folders: false,
                start_menu_dirs: Vec::new(),
                store_apps: false,
                storage: Storage::in_memory().unwrap(),
                idle: Arc::new(crate::content::idle::AlwaysIdle),
                ocr: Arc::new(crate::content::ocr::NoOcr),
                launcher: Arc::new(NoopLauncher),
                system: None,
                gate: GateConfig::default(),
                plugins: None,
            },
            sink,
            PauseState::new(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while lookout.status().indexing {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let s = lookout.status();
        assert_eq!(s.files, 5000);
        assert_eq!(s.roots, 0, "the real root is ignored in synthetic mode");
    }
}
