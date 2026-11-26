//! The plugin host: finds plugins, starts them when needed, stops them when
//! idle or broken, and talks to them with a time limit on every call.
//!
//! A plugin is untrusted input. Whatever it sends is parsed defensively and
//! cut to size; whatever goes wrong with it (crash, hang, garbage) ends with
//! its process killed and an error for the caller. The core never waits for a
//! plugin longer than the timeout of the call.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::manifest::{ClipboardAccess, Manifest};
use super::wire::{Limits, PluginProcess, WireError};
use crate::actions::gate::Authorized;
use crate::actions::Risk;
use crate::item::{Item, Kind};
use crate::pause::PauseState;
pub use serde_json::Value as ConfigValue;

pub const PROTOCOL: u32 = 1;
const MAX_ITEMS: usize = 20;
const MAX_PARAMS_BYTES: usize = 8 * 1024;
const MAX_TOKEN_BYTES: usize = 1024 * 1024;
const MAX_LINES: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("no such add-on")]
    Unknown,
    #[error("the add-on is turned off or has not been approved")]
    Disabled,
    #[error("the add-on is busy")]
    Busy,
    #[error("the add-on keeps failing; trying again later")]
    Backoff,
    #[error("the add-on's permissions have not been accepted")]
    NotApproved,
    #[error("the add-on sent an answer that does not make sense: {0}")]
    BadReply(String),
    #[error("{0}")]
    Wire(#[from] WireError),
    #[error("could not save add-on settings: {0}")]
    Save(String),
    #[error("{0} is not a method the host calls on add-ons")]
    NotAllowed(String),
}

/// Methods the core calls on a plugin outside the query and action flow (the
/// semantic-search helper's index API; see `docs/plugins.md`). Nothing else
/// can be sent through [`PluginHost::service_call`], and none of them can run
/// an action: actions only go through the gate.
pub const SERVICE_METHODS: &[&str] = &[
    "index_needs",
    "index_batch",
    "index_remove",
    "index_list",
    "index_status",
    "index_delete",
    "index_search",
    "extract_office",
];

#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub init: Duration,
    pub query: Duration,
    pub preview: Duration,
    pub execute: Duration,
    pub undo: Duration,
    pub shutdown: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            init: Duration::from_secs(5),
            query: Duration::from_millis(1500),
            preview: Duration::from_secs(5),
            execute: Duration::from_secs(60),
            undo: Duration::from_secs(30),
            shutdown: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Folders that contain one sub-folder per plugin, each with a `plugin.json`.
    pub dirs: Vec<PathBuf>,
    /// Lookout's data folder; plugin state and each plugin's own folder live here.
    pub data_dir: PathBuf,
    pub timeouts: Timeouts,
    /// A plugin that is not `background` is stopped after this long without a call.
    pub idle_timeout: Duration,
    /// How often idle plugins are stopped, crashed background plugins restarted
    /// and pause changes passed on.
    pub tick: Duration,
    pub memory_limit: usize,
    /// The master key. Plugins never get it: each is given a key derived from it and
    /// its id, at `initialize` only.
    pub master_key: Option<crate::crypto::Key>,
}

impl HostConfig {
    pub fn new(dirs: Vec<PathBuf>, data_dir: PathBuf) -> Self {
        Self {
            dirs,
            data_dir,
            timeouts: Timeouts::default(),
            idle_timeout: Duration::from_secs(60),
            tick: Duration::from_millis(250),
            memory_limit: 1 << 30,
            master_key: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UserState {
    enabled: bool,
    /// `Manifest::approval_key` the user accepted.
    approved: Option<String>,
    config: Value,
}

struct Runtime {
    proc: Option<PluginProcess>,
    last_used: Instant,
    last_error: Option<String>,
    failures: u32,
    retry_at: Option<Instant>,
    sent_paused: bool,
}

struct Plugin {
    manifest: Manifest,
    dir: PathBuf,
    key: String,
    source_id: &'static str,
    user: Mutex<UserState>,
    run: Mutex<Runtime>,
    labels: Mutex<HashMap<String, String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the settings panel shows for one plugin.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub summary: String,
    pub permissions: Vec<String>,
    pub risk_floor: Risk,
    pub enabled: bool,
    /// The user accepted exactly these permissions.
    pub approved: bool,
    pub running: bool,
    pub last_error: Option<String>,
    pub config: Value,
}

/// A file or folder the host offers a plugin to work on (found by name by the
/// user's own query). A plugin may only return items about these.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: String,
    pub is_folder: bool,
}

/// What a plugin says about an action before anything runs.
#[derive(Debug, Clone)]
pub struct PlanReply {
    /// Already raised to at least the manifest's floor.
    pub risk: Risk,
    pub title: String,
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
    pub undoable: bool,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecReply {
    pub message: String,
    pub undo_token: Option<String>,
}

pub struct PluginHost {
    plugins: Vec<Arc<Plugin>>,
    cfg: HostConfig,
    pause: PauseState,
    state_path: PathBuf,
    stop: AtomicBool,
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn parse_risk(v: Option<&Value>) -> Option<Risk> {
    match v.and_then(Value::as_str)? {
        "safe" => Some(Risk::Safe),
        "confirm" => Some(Risk::Confirm),
        "dangerous" => Some(Risk::Dangerous),
        _ => None,
    }
}

/// The plugin's saved settings laid over the defaults from its manifest.
fn effective_config(defaults: &Value, saved: &Value) -> Value {
    let mut out = defaults.as_object().cloned().unwrap_or_default();
    if let Some(s) = saved.as_object() {
        for (k, v) in s {
            if out.contains_key(k) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

fn leak(s: String) -> &'static str {
    // One short string per plugin per start-up, bounded by the number of manifests.
    Box::leak(s.into_boxed_str())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// Temp file, flush to disk, rename over the old file, then a `.bak` copy that
/// `start` falls back to when the main file is damaged.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = sibling(path, ".tmp");
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let _ = std::fs::copy(path, sibling(path, ".bak"));
    Ok(())
}

impl PluginHost {
    pub fn start(cfg: HostConfig, pause: PauseState) -> Arc<Self> {
        let state_path = cfg.data_dir.join("plugins.json");
        let read = |p: &Path| -> Option<HashMap<String, UserState>> {
            serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
        };
        let saved: HashMap<String, UserState> = read(&state_path)
            .or_else(|| read(&sibling(&state_path, ".bak")))
            .unwrap_or_default();
        let mut plugins: Vec<Arc<Plugin>> = Vec::new();
        for dir in &cfg.dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut subdirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            subdirs.sort();
            for sub in subdirs {
                let Ok(text) = std::fs::read_to_string(sub.join("plugin.json")) else {
                    continue;
                };
                let manifest = match Manifest::parse(&text) {
                    Ok(m) => m,
                    Err(_) => {
                        crate::log::warn("a plugin with an unreadable description was ignored");
                        continue;
                    }
                };
                if plugins.iter().any(|p| p.manifest.id == manifest.id) {
                    continue;
                }
                let user = saved.get(&manifest.id).cloned().unwrap_or_default();
                plugins.push(Arc::new(Plugin {
                    key: manifest.approval_key(),
                    source_id: leak(format!("plugin:{}", manifest.id)),
                    manifest,
                    dir: sub,
                    user: Mutex::new(user),
                    run: Mutex::new(Runtime {
                        proc: None,
                        last_used: Instant::now(),
                        last_error: None,
                        failures: 0,
                        retry_at: None,
                        sent_paused: false,
                    }),
                    labels: Mutex::new(HashMap::new()),
                }));
            }
        }
        let host = Arc::new(Self {
            plugins,
            cfg,
            pause,
            state_path,
            stop: AtomicBool::new(false),
        });
        let weak: Weak<PluginHost> = Arc::downgrade(&host);
        let tick = host.cfg.tick;
        let spawned = std::thread::Builder::new()
            .name("lookout-plugin-supervisor".into())
            .spawn(move || loop {
                std::thread::sleep(tick);
                let Some(h) = weak.upgrade() else { return };
                if h.stop.load(Ordering::Acquire) {
                    return;
                }
                crate::guard::contain("checking on the plugins failed", || h.supervise());
            });
        if let Err(e) = spawned {
            crate::log::error_io("plugin supervisor did not start", &e);
        }
        host
    }

    fn find(&self, id: &str) -> Result<&Arc<Plugin>, HostError> {
        self.plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .ok_or(HostError::Unknown)
    }

    fn usable(p: &Plugin) -> bool {
        let u = lock(&p.user);
        u.enabled && u.approved.as_deref() == Some(p.key.as_str())
    }

    pub fn is_usable(&self, id: &str) -> bool {
        self.find(id).is_ok_and(|p| Self::usable(p))
    }

    pub fn manifest(&self, id: &str) -> Option<&Manifest> {
        self.find(id).ok().map(|p| &p.manifest)
    }

    /// One numeric setting of a plugin (the user's value, else the default).
    pub fn config_number(&self, id: &str, key: &str) -> Option<f64> {
        let p = self.find(id).ok()?;
        let cfg = effective_config(&p.manifest.config_defaults, &lock(&p.user).config);
        cfg.get(key)?.as_f64()
    }

    pub fn manifests(&self) -> impl Iterator<Item = &Manifest> {
        self.plugins.iter().map(|p| &p.manifest)
    }

    pub fn source_id(&self, id: &str) -> Option<&'static str> {
        self.find(id).ok().map(|p| p.source_id)
    }

    /// Plugins for the settings panel.
    pub fn list(&self) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .map(|p| {
                let u = lock(&p.user).clone();
                let (running, last_error) = match p.run.try_lock() {
                    Ok(mut rt) => (
                        rt.proc.as_mut().is_some_and(PluginProcess::is_alive),
                        rt.last_error.clone(),
                    ),
                    Err(_) => (true, None),
                };
                PluginInfo {
                    id: p.manifest.id.clone(),
                    name: p.manifest.name.clone(),
                    version: p.manifest.version.clone(),
                    description: p.manifest.description.clone(),
                    summary: p.manifest.summary.clone(),
                    permissions: p.manifest.describe_permissions(),
                    risk_floor: p.manifest.risk_floor(),
                    enabled: u.enabled,
                    approved: u.approved.as_deref() == Some(p.key.as_str()),
                    running,
                    last_error,
                    config: effective_config(&p.manifest.config_defaults, &u.config),
                }
            })
            .collect()
    }

    fn save(&self) -> Result<(), HostError> {
        let all: HashMap<String, UserState> = self
            .plugins
            .iter()
            .map(|p| (p.manifest.id.clone(), lock(&p.user).clone()))
            .collect();
        let text =
            serde_json::to_string_pretty(&all).map_err(|e| HostError::Save(e.to_string()))?;
        std::fs::create_dir_all(&self.cfg.data_dir).map_err(|e| HostError::Save(e.to_string()))?;
        write_atomic(&self.state_path, text.as_bytes()).map_err(|e| HostError::Save(e.to_string()))
    }

    /// Turns a plugin on or off. Turning it on needs `accept_permissions`
    /// unless the user already accepted exactly the current permissions.
    pub fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
        accept_permissions: bool,
    ) -> Result<(), HostError> {
        let p = self.find(id)?;
        {
            let mut u = lock(&p.user);
            if enabled {
                if u.approved.as_deref() != Some(p.key.as_str()) {
                    if !accept_permissions {
                        return Err(HostError::NotApproved);
                    }
                    u.approved = Some(p.key.clone());
                }
                u.enabled = true;
            } else {
                u.enabled = false;
            }
        }
        if !enabled {
            self.stop_plugin(p);
        } else {
            // A fresh start by the user: old failures no longer hold it back.
            Self::forget_failures(&mut lock(&p.run));
        }
        self.save()
    }

    fn forget_failures(rt: &mut Runtime) {
        rt.failures = 0;
        rt.retry_at = None;
    }

    /// Stores new settings for a plugin and restarts it so it picks them up.
    pub fn set_config(&self, id: &str, config: Value) -> Result<(), HostError> {
        let p = self.find(id)?;
        lock(&p.user).config = config;
        self.stop_plugin(p);
        Self::forget_failures(&mut lock(&p.run));
        self.save()
    }

    fn stop_plugin(&self, p: &Plugin) {
        let mut rt = lock(&p.run);
        if let Some(mut proc) = rt.proc.take() {
            proc.stop(self.cfg.timeouts.shutdown);
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.find(id).is_ok_and(|p| {
            lock(&p.run)
                .proc
                .as_mut()
                .is_some_and(PluginProcess::is_alive)
        })
    }

    pub fn pid(&self, id: &str) -> Option<u32> {
        self.find(id)
            .ok()
            .and_then(|p| lock(&p.run).proc.as_ref().map(PluginProcess::pid))
    }

    /// Messages the plugin sent that it may not send (tests, diagnostics).
    pub fn violations(&self, id: &str) -> u32 {
        self.find(id).map_or(0, |p| {
            lock(&p.run)
                .proc
                .as_ref()
                .map_or(0, |pr| pr.shared().violations.load(Ordering::Relaxed))
        })
    }

    pub fn last_error(&self, id: &str) -> Option<String> {
        self.find(id)
            .ok()
            .and_then(|p| lock(&p.run).last_error.clone())
    }

    /// Stops every plugin (application exit).
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        for p in &self.plugins {
            self.stop_plugin(p);
        }
    }

    // ----------------------------------------------------------- lifecycle

    fn fail(rt: &mut Runtime, why: String) {
        crate::log::error("an add-on failed");
        if let Some(mut proc) = rt.proc.take() {
            proc.kill();
        }
        rt.last_error = Some(why);
        rt.failures += 1;
        // The first two failures retry at once; a plugin that keeps failing is
        // left alone for longer each time (at most a minute).
        rt.retry_at = (rt.failures >= 3).then(|| {
            Instant::now()
                + Duration::from_secs(1u64 << (rt.failures - 2).min(6)).min(Duration::from_secs(60))
        });
    }

    fn ensure(&self, p: &Plugin, rt: &mut Runtime) -> Result<(), HostError> {
        if let Some(proc) = rt.proc.as_mut() {
            if proc.is_alive() {
                return Ok(());
            }
            Self::fail(rt, "the add-on stopped unexpectedly".into());
        }
        if rt.retry_at.is_some_and(|t| Instant::now() < t) {
            return Err(HostError::Backoff);
        }
        let exe = p.dir.join(&p.manifest.entry);
        let perms = &p.manifest.permissions;
        let limits = Limits {
            spawn: perms.spawn,
            clipboard: perms.clipboard != ClipboardAccess::None,
            memory_bytes: self.cfg.memory_limit,
        };
        let mut proc = match PluginProcess::spawn(&exe, &p.manifest.args, &p.dir, limits) {
            Ok(pr) => pr,
            Err(e) => {
                Self::fail(rt, e.to_string());
                return Err(e.into());
            }
        };
        let own_dir = self.cfg.data_dir.join("plugins").join(&p.manifest.id);
        let _ = std::fs::create_dir_all(&own_dir);
        let paused = self.pause.is_paused();
        let init = json!({
            "protocol": PROTOCOL,
            "data_dir": own_dir,
            "key": self.cfg.master_key.as_ref().map(|m| crate::crypto::derive_plugin_key(m, &p.manifest.id).to_hex()),
            "config": effective_config(&p.manifest.config_defaults, &lock(&p.user).config),
            "paused": paused,
        });
        match proc.call("initialize", init, self.cfg.timeouts.init) {
            Ok(_) => {
                rt.proc = Some(proc);
                rt.sent_paused = paused;
                rt.last_used = Instant::now();
                rt.last_error = None;
                rt.failures = 0;
                rt.retry_at = None;
                Ok(())
            }
            Err(e) => {
                proc.kill();
                Self::fail(rt, e.to_string());
                Err(e.into())
            }
        }
    }

    /// One call to a plugin, starting it if needed. A failure that is not an
    /// error answer kills the process; the next call starts a new one.
    fn call(
        &self,
        p: &Plugin,
        method: &str,
        params: Value,
        timeout: Duration,
        wait: bool,
    ) -> Result<Value, HostError> {
        let mut rt = if wait {
            lock(&p.run)
        } else {
            match p.run.try_lock() {
                Ok(g) => g,
                Err(TryLockError::Poisoned(e)) => e.into_inner(),
                Err(TryLockError::WouldBlock) => return Err(HostError::Busy),
            }
        };
        if !Self::usable(p) {
            return Err(HostError::Disabled);
        }
        self.ensure(p, &mut rt)?;
        let result = match rt.proc.as_mut() {
            Some(proc) => proc.call(method, params, timeout),
            None => Err(WireError::Died),
        };
        rt.last_used = Instant::now();
        match result {
            Ok(v) => Ok(v),
            Err(e @ WireError::Rpc { .. }) => Err(e.into()),
            Err(e) => {
                Self::fail(&mut rt, e.to_string());
                Err(e.into())
            }
        }
    }

    /// One call of the index API (see [`SERVICE_METHODS`]) to a plugin the user
    /// has turned on. Starts the plugin if needed. While another call is using
    /// the plugin this waits, but gives up with `Busy` once `cancelled`
    /// returns true (the next keystroke, or a shutdown).
    pub fn service_call(
        &self,
        id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, HostError> {
        if !SERVICE_METHODS.contains(&method) {
            return Err(HostError::NotAllowed(method.to_owned()));
        }
        let p = self.find(id)?;
        loop {
            match self.call(p, method, params.clone(), timeout, false) {
                Err(HostError::Busy) => {
                    if cancelled() || self.stop.load(Ordering::Acquire) {
                        return Err(HostError::Busy);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                other => return other,
            }
        }
    }

    fn supervise(&self) {
        let paused = self.pause.is_paused();
        for p in &self.plugins {
            let Ok(mut rt) = p.run.try_lock() else {
                continue;
            };
            if !Self::usable(p) {
                if let Some(mut proc) = rt.proc.take() {
                    proc.stop(self.cfg.timeouts.shutdown);
                }
                continue;
            }
            let alive = rt.proc.as_mut().map(PluginProcess::is_alive);
            match alive {
                Some(true) => {
                    if !p.manifest.background && rt.last_used.elapsed() > self.cfg.idle_timeout {
                        if let Some(mut proc) = rt.proc.take() {
                            proc.stop(self.cfg.timeouts.shutdown);
                        }
                    } else if rt.sent_paused != paused {
                        if let Some(proc) = rt.proc.as_ref() {
                            proc.notify("pause_changed", json!({"paused": paused}));
                        }
                        rt.sent_paused = paused;
                    }
                }
                Some(false) => Self::fail(&mut rt, "the add-on stopped unexpectedly".into()),
                None => {
                    if p.manifest.background {
                        let _ = self.ensure(p, &mut rt);
                    }
                }
            }
        }
    }

    // --------------------------------------------------------------- query

    /// Asks a plugin for items. `search` finds files and folders by name for
    /// plugins that work on files the user picks; whatever it returns is the
    /// only set of paths the plugin may answer about.
    pub fn query(
        &self,
        id: &str,
        raw: &str,
        verb: &str,
        rest: &str,
        search: &dyn Fn(&str) -> Vec<Candidate>,
    ) -> Result<Vec<Item>, HostError> {
        let p = self.find(id)?;
        let timeout = self.cfg.timeouts.query;
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut reply = Value::Null;
        for round in 0..2 {
            let cands: Vec<Value> = candidates
                .iter()
                .map(|c| json!({"path": c.path, "kind": if c.is_folder {"folder"} else {"file"}}))
                .collect();
            let params = json!({"raw": raw, "verb": verb, "rest": rest, "candidates": cands});
            reply = self.call(p, "query", params, timeout, false)?;
            let wants = reply
                .get("select")
                .and_then(|s| s.get("text"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            match wants {
                Some(text) if round == 0 && candidates.is_empty() => {
                    candidates = search(&clean(&text, 200));
                    candidates.truncate(8);
                    if candidates.is_empty() {
                        return Ok(Vec::new());
                    }
                }
                _ => break,
            }
        }
        let grants: HashSet<String> = candidates.iter().map(|c| c.path.to_lowercase()).collect();
        let list = reply
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| HostError::BadReply("no items".into()))?;
        let mut out = Vec::new();
        for raw_item in list.iter().take(MAX_ITEMS) {
            if let Some(item) = self.make_item(p, raw_item, &grants) {
                out.push(item);
            }
        }
        Ok(out)
    }

    fn make_item(&self, p: &Plugin, v: &Value, grants: &HashSet<String>) -> Option<Item> {
        let s = |k: &str| v.get(k).and_then(Value::as_str);
        let (id, title, action) = (s("id")?, s("title")?, s("action")?);
        if id.is_empty() || action.is_empty() || action.len() > 64 {
            return None;
        }
        let uri = s("uri").map(str::to_owned);
        if let Some(u) = &uri {
            // "user_selected": only paths the host handed to the plugin.
            if !grants.contains(&u.to_lowercase()) {
                return None;
            }
        }
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        if params.to_string().len() > MAX_PARAMS_BYTES {
            return None;
        }
        let kind = match s("kind").unwrap_or("custom") {
            "file" => Kind::File,
            "folder" => Kind::Folder,
            "clipboard" => Kind::Clipboard,
            other => Kind::Custom(clean(other, 32)),
        };
        let mut item = Item::new(
            format!("plugin:{}:{}", p.manifest.id, clean(id, 128)),
            kind,
            clean(title, 200),
            p.source_id,
        );
        item.subtitle = s("subtitle").map(|t| clean(t, 300));
        item.uri = uri;
        item.score = v
            .get("score")
            .and_then(Value::as_f64)
            .map_or(5.0, |f| f as f32);
        item.score = item.score.clamp(0.0, 50.0);
        item.score_features.match_quality = 1.0;
        item.metadata.insert("plugin".into(), json!(p.manifest.id));
        item.metadata.insert(
            "default_action".into(),
            json!(action_id(&p.manifest.id, action)),
        );
        item.metadata.insert("plugin_action".into(), json!(action));
        item.metadata.insert("plugin_params".into(), params);
        Some(item)
    }

    // ------------------------------------------------------------- actions

    /// Preview of an action on one item, with the risk raised to the floor.
    pub fn plan(
        &self,
        id: &str,
        action: &str,
        item: &Value,
        params: &Value,
    ) -> Result<PlanReply, HostError> {
        let p = self.find(id)?;
        let t = self.cfg.timeouts.preview;
        let offered = self.call(p, "get_actions", json!({"items": [item]}), t, true)?;
        let entry = offered
            .get("actions")
            .and_then(Value::as_array)
            .and_then(|a| {
                a.iter()
                    .find(|a| a.get("id").and_then(Value::as_str) == Some(action))
            })
            .ok_or_else(|| HostError::BadReply("the plugin does not offer that action".into()))?;
        let label = entry
            .get("label")
            .and_then(Value::as_str)
            .map(|l| clean(l, 80));
        if let Some(l) = &label {
            lock(&p.labels).insert(action.to_owned(), l.clone());
        }
        let asked = parse_risk(entry.get("risk")).unwrap_or(Risk::Confirm);
        let reply = self.call(
            p,
            "preview",
            json!({"action_id": action, "items": [item], "params": params}),
            t,
            true,
        )?;
        let mut parsed = parse_preview(&reply)?;
        parsed.risk = parsed.risk.max(asked).max(p.manifest.risk_floor());
        parsed.label = label;
        Ok(parsed)
    }

    /// Only the gate can call this: it needs an [`Authorized`].
    pub fn execute(
        &self,
        id: &str,
        action: &str,
        item: &Value,
        params: &Value,
        _proof: &Authorized,
    ) -> Result<ExecReply, HostError> {
        let p = self.find(id)?;
        let reply = self.call(
            p,
            "execute",
            json!({"action_id": action, "items": [item], "params": params}),
            self.cfg.timeouts.execute,
            true,
        )?;
        let message = reply
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| "Done".to_owned(), |m| clean(m, 500));
        let undo_token = reply
            .get("undo_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.len() <= MAX_TOKEN_BYTES)
            .map(str::to_owned);
        Ok(ExecReply {
            message,
            undo_token,
        })
    }

    pub fn plan_undo(&self, id: &str, token: &str) -> Result<PlanReply, HostError> {
        let p = self.find(id)?;
        let reply = self.call(
            p,
            "undo",
            json!({"undo_token": token, "dry_run": true}),
            self.cfg.timeouts.preview,
            true,
        )?;
        let mut parsed = parse_preview(&reply)?;
        parsed.risk = parsed.risk.max(p.manifest.risk_floor());
        Ok(parsed)
    }

    pub fn undo(&self, id: &str, token: &str, _proof: &Authorized) -> Result<String, HostError> {
        let p = self.find(id)?;
        let reply = self.call(
            p,
            "undo",
            json!({"undo_token": token, "dry_run": false}),
            self.cfg.timeouts.undo,
            true,
        )?;
        Ok(reply
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| "Undone".to_owned(), |m| clean(m, 500)))
    }

    /// The line the card shows above anything the plugin wrote.
    pub fn from_line(&self, id: &str) -> String {
        let name = self.find(id).map_or(id, |p| p.manifest.name.as_str());
        format!("From plugin: {}", clean(name, 60))
    }

    pub fn label_of(&self, id: &str, action: &str) -> Option<String> {
        self.find(id)
            .ok()
            .and_then(|p| lock(&p.labels).get(action).cloned())
    }

    pub fn plugin_dir(&self, id: &str) -> Option<&Path> {
        self.find(id).ok().map(|p| p.dir.as_path())
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// The action id the gate uses for a plugin action.
pub fn action_id(plugin: &str, action: &str) -> String {
    format!("plugin/{plugin}/{action}")
}

/// Splits `plugin/<id>/<action>`.
pub fn split_action_id(full: &str) -> Option<(&str, &str)> {
    let rest = full.strip_prefix("plugin/")?;
    let (plugin, action) = rest.split_once('/')?;
    (!plugin.is_empty() && !action.is_empty()).then_some((plugin, action))
}

fn parse_preview(v: &Value) -> Result<PlanReply, HostError> {
    let strings = |k: &str, max_len: usize| -> Vec<String> {
        v.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .take(MAX_LINES)
                    .map(|s| clean(s, max_len))
                    .collect()
            })
            .unwrap_or_default()
    };
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| HostError::BadReply("the preview has no title".into()))?;
    let mut lines = strings("lines", 500);
    if v.get("lines")
        .and_then(Value::as_array)
        .is_some_and(|a| a.len() > MAX_LINES)
    {
        lines.push("... and more".into());
    }
    Ok(PlanReply {
        risk: parse_risk(v.get("risk")).unwrap_or(Risk::Safe),
        title: clean(title, 300),
        lines,
        warnings: strings("warnings", 500),
        undoable: v.get("undoable").and_then(Value::as_bool).unwrap_or(false),
        label: None,
    })
}
