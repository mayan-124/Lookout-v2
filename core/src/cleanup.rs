//! Disk page, stage B: the "Clean up" list and the shared pieces of "Duplicates".
//!
//! Nothing here deletes anything. A scan only *finds* things and keeps them in
//! the backend; removal always goes through a [`Batch`] that the delete action
//! (`actions::disk::BatchRemoveAction`) previews, asks the user to confirm and
//! then performs. The webview never sends a path: it names a result by id.
//!
//! The protected-folder rule lives here ([`is_protected`]) and is applied again
//! by the action right before anything is removed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::pause::PauseState;

pub const DAYS_OLD: u64 = 90;
const DAY: u64 = 86_400;

// ------------------------------------------------------------ places and rules

/// The folders the rules are about. `current()` reads the real ones; tests build
/// their own inside a temporary folder.
#[derive(Debug, Clone)]
pub struct Env {
    pub profile: PathBuf,
    pub local: PathBuf,
    pub roaming: PathBuf,
    pub temp: PathBuf,
    /// Windows itself, Program Files (both), ProgramData.
    pub system_dirs: Vec<PathBuf>,
}

impl Env {
    pub fn current() -> Self {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let profile = var("USERPROFILE").unwrap_or_default();
        let mut system_dirs: Vec<PathBuf> = [
            "SystemRoot",
            "windir",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
        ]
        .iter()
        .filter_map(|k| var(k))
        .collect();
        system_dirs.sort();
        system_dirs.dedup();
        Self {
            local: var("LOCALAPPDATA").unwrap_or_else(|| profile.join("AppData").join("Local")),
            roaming: var("APPDATA").unwrap_or_else(|| profile.join("AppData").join("Roaming")),
            temp: std::env::temp_dir(),
            profile,
            system_dirs,
        }
    }
}

/// Lower-cased components without `\\?\`, so paths compare the way Windows does.
fn parts(p: &Path) -> Vec<String> {
    use std::path::Component;
    p.components()
        .filter_map(|c| match c {
            Component::Prefix(x) => Some(match x.kind() {
                std::path::Prefix::Disk(d) | std::path::Prefix::VerbatimDisk(d) => {
                    format!("{}:", char::from(d).to_ascii_lowercase())
                }
                _ => x.as_os_str().to_string_lossy().to_lowercase(),
            }),
            Component::Normal(n) => Some(n.to_string_lossy().to_lowercase()),
            Component::ParentDir => Some("..".to_owned()),
            Component::RootDir | Component::CurDir => None,
        })
        .collect()
}

/// True for anything Lookout must never offer to remove: not a full path, a
/// path with `..`, Windows, Program Files, ProgramData, the recycle bin and
/// system-volume folders, and the user's profile folder itself (or anything
/// above it, such as `C:\Users` or a drive root).
pub fn is_protected(env: &Env, path: &Path) -> bool {
    if !path.is_absolute() {
        return true;
    }
    let p = parts(path);
    if p.iter().any(|c| c == "..") || p.len() < 2 {
        return true; // a drive root, or a path that climbs out of where it says it is
    }
    if p.iter()
        .any(|c| c == "$recycle.bin" || c == "system volume information")
    {
        return true;
    }
    if env
        .system_dirs
        .iter()
        .any(|d| !d.as_os_str().is_empty() && p.starts_with(&parts(d)))
    {
        return true;
    }
    let profile = parts(&env.profile);
    // The profile root and every folder above it.
    !profile.is_empty() && profile.starts_with(&p)
}

pub fn is_reparse(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_type().is_symlink() || md.file_attributes() & 0x400 != 0
}

pub fn is_cloud(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    let a = md.file_attributes();
    a & (0x1000 | 0x40_0000) != 0 || (a & 0x4_0000 != 0 && a & 0x400 != 0)
}

pub fn mtime_secs(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ------------------------------------------------------------ batches (what gets removed)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Move to the Windows Recycle Bin.
    Recycle,
    /// Delete directly (caches that are rebuilt).
    Delete,
    /// Empty the Recycle Bin itself.
    EmptyBin,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    /// Modified time when it was found; a file that changed since is left alone.
    pub stamp: u64,
}

#[derive(Debug, Clone)]
pub struct Batch {
    pub label: String,
    pub mode: Mode,
    pub entries: Vec<Entry>,
    /// Files that must stay (duplicates): never removed, and must still exist.
    pub keepers: Vec<PathBuf>,
    /// Items in the Recycle Bin (for `EmptyBin` only).
    pub bin_items: u64,
    pub bin_bytes: u64,
}

/// Batches waiting for the user's confirmation, plus what has been reclaimed.
pub struct BatchStore {
    map: Mutex<HashMap<String, Batch>>,
    next: AtomicU64,
    reclaimed: AtomicU64,
    pub env: Env,
}

impl BatchStore {
    pub fn new(env: Env) -> Self {
        Self {
            map: Mutex::default(),
            next: AtomicU64::new(1),
            reclaimed: AtomicU64::new(0),
            env,
        }
    }
    pub fn put(&self, b: Batch) -> String {
        let id = format!("b{}", self.next.fetch_add(1, Ordering::Relaxed));
        let mut m = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        if m.len() > 32 {
            m.clear(); // old, never-confirmed previews
        }
        m.insert(id.clone(), b);
        id
    }
    pub fn get(&self, id: &str) -> Option<Batch> {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }
    pub fn forget(&self, id: &str) {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
    pub fn add_reclaimed(&self, bytes: u64) {
        self.reclaimed.fetch_add(bytes, Ordering::Relaxed);
    }
    pub fn reclaimed(&self) -> u64 {
        self.reclaimed.load(Ordering::Relaxed)
    }
}

// ------------------------------------------------------------ background tasks

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Idle,
    Running,
    Done,
    Cancelled,
    Failed,
}

/// Progress and result of one background scan, polled by the page.
pub struct Task<T> {
    pub cancel: AtomicBool,
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    state: AtomicU8,
    phase: Mutex<String>,
    error: Mutex<String>,
    result: Mutex<Option<T>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub state: TaskState,
    pub phase: String,
    pub files: u64,
    pub bytes: u64,
    pub error: String,
}

impl<T> Default for Task<T> {
    fn default() -> Self {
        Self {
            cancel: AtomicBool::new(false),
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            state: AtomicU8::new(0),
            phase: Mutex::default(),
            error: Mutex::default(),
            result: Mutex::new(None),
        }
    }
}

impl<T> Task<T> {
    pub fn reset(&self) {
        self.cancel.store(false, Ordering::Relaxed);
        self.files.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
        self.set_state(TaskState::Running);
        self.set_phase("Starting…");
        *self.error.lock().unwrap_or_else(PoisonError::into_inner) = String::new();
        *self.result.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    pub fn set_state(&self, s: TaskState) {
        self.state.store(s as u8, Ordering::Relaxed);
    }
    pub fn state(&self) -> TaskState {
        match self.state.load(Ordering::Relaxed) {
            1 => TaskState::Running,
            2 => TaskState::Done,
            3 => TaskState::Cancelled,
            4 => TaskState::Failed,
            _ => TaskState::Idle,
        }
    }
    pub fn set_phase(&self, s: &str) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = s.to_owned();
    }
    pub fn fail(&self, e: &str) {
        *self.error.lock().unwrap_or_else(PoisonError::into_inner) = e.to_owned();
        self.set_state(TaskState::Failed);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn finish(&self, r: T) {
        *self.result.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
        self.set_state(if self.cancelled() {
            TaskState::Cancelled
        } else {
            TaskState::Done
        });
    }
    pub fn view(&self) -> TaskView {
        TaskView {
            state: self.state(),
            phase: self
                .phase
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            files: self.files.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            error: self
                .error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }
    pub fn with_result<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(f)
    }
}

/// Keeps a background scan polite: in Gentle mode it works about a quarter of
/// the time, and it always stops while Lookout is paused.
pub struct Throttle {
    gentle: bool,
    pause: PauseState,
    since: Instant,
    n: u32,
}

impl Throttle {
    pub fn new(gentle: bool, pause: PauseState) -> Self {
        Self {
            gentle,
            pause,
            since: Instant::now(),
            n: 0,
        }
    }
    /// Call once per file or folder. Returns false when the task was cancelled.
    pub fn tick<T>(&mut self, task: &Task<T>) -> bool {
        self.n += 1;
        if !self.n.is_multiple_of(64) {
            return !task.cancelled();
        }
        while self.pause.is_paused() && !task.cancelled() {
            std::thread::sleep(Duration::from_millis(200));
            self.since = Instant::now();
        }
        if self.gentle {
            let worked = self.since.elapsed();
            std::thread::sleep(worked.mul_f32(3.0).min(Duration::from_millis(500)));
            self.since = Instant::now();
        }
        !task.cancelled()
    }
}

// ------------------------------------------------------------ the clean-up list

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Safe,
    CheckFirst,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: &'static str,
    pub title: String,
    pub why: String,
    pub risk: Risk,
    pub batch: Batch,
}

impl Item {
    pub fn bytes(&self) -> u64 {
        if self.batch.mode == Mode::EmptyBin {
            self.batch.bin_bytes
        } else {
            self.batch.entries.iter().map(|e| e.size).sum()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemView {
    pub id: String,
    pub title: String,
    pub why: String,
    pub risk: Risk,
    pub mode: Mode,
    pub bytes: u64,
    pub count: u64,
}

pub fn views(items: &[Item]) -> Vec<ItemView> {
    items
        .iter()
        .map(|i| ItemView {
            id: i.id.to_owned(),
            title: i.title.clone(),
            why: i.why.clone(),
            risk: i.risk,
            mode: i.batch.mode,
            bytes: i.bytes(),
            count: if i.batch.mode == Mode::EmptyBin {
                i.batch.bin_items
            } else {
                i.batch.entries.len() as u64
            },
        })
        .collect()
}

/// Size of everything below `dir` (files only; links are not followed).
pub fn dir_size(dir: &Path, task: &Task<Vec<Item>>, th: &mut Throttle) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            if !th.tick(task) {
                return total;
            }
            let Ok(md) = e.metadata() else { continue };
            if is_reparse(&md) {
                continue;
            }
            if md.is_dir() {
                stack.push(e.path());
            } else {
                task.files.fetch_add(1, Ordering::Relaxed);
                total += md.len();
            }
        }
    }
    task.bytes.fetch_add(total, Ordering::Relaxed);
    total
}

fn entry_of(env: &Env, path: PathBuf, size: u64, md: &std::fs::Metadata) -> Option<Entry> {
    if is_protected(env, &path) {
        return None;
    }
    Some(Entry {
        path,
        size,
        stamp: mtime_secs(md),
    })
}

fn batch(label: &str, mode: Mode, entries: Vec<Entry>) -> Batch {
    Batch {
        label: label.to_owned(),
        mode,
        entries,
        keepers: Vec::new(),
        bin_items: 0,
        bin_bytes: 0,
    }
}

/// Folders (not files) directly below `dir` whose name satisfies `ok`.
fn child_dirs(dir: &Path, ok: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.metadata().is_ok_and(|m| m.is_dir() && !is_reparse(&m))
                && ok(&e.file_name().to_string_lossy())
        })
        .map(|e| e.path())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn folders_item(
    env: &Env,
    task: &Task<Vec<Item>>,
    th: &mut Throttle,
    id: &'static str,
    title: &str,
    why: &str,
    risk: Risk,
    mode: Mode,
    dirs: Vec<PathBuf>,
) -> Option<Item> {
    let mut entries = Vec::new();
    for d in dirs {
        if task.cancelled() {
            return None;
        }
        let Ok(md) = std::fs::symlink_metadata(&d) else {
            continue;
        };
        let size = dir_size(&d, task, th);
        if size == 0 {
            continue;
        }
        entries.extend(entry_of(env, d, size, &md));
    }
    (!entries.is_empty()).then(|| Item {
        id,
        title: title.to_owned(),
        why: why.to_owned(),
        risk,
        batch: batch(title, mode, entries),
    })
}

fn recycle_bin_item() -> Option<Item> {
    use windows_sys::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    // SAFETY: a zeroed SHQUERYRBINFO with cbSize set, and a null root (all drives).
    let (items, bytes) = unsafe {
        let mut info: SHQUERYRBINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
        if SHQueryRecycleBinW(std::ptr::null(), &mut info) != 0 {
            return None;
        }
        (info.i64NumItems.max(0) as u64, info.i64Size.max(0) as u64)
    };
    (items > 0).then(|| {
        let mut b = batch("Recycle Bin", Mode::EmptyBin, Vec::new());
        b.bin_items = items;
        b.bin_bytes = bytes;
        Item {
            id: "recycle_bin",
            title: "Recycle Bin".to_owned(),
            why: "Things you already deleted that still take space. Emptying it cannot be undone."
                .to_owned(),
            risk: Risk::CheckFirst,
            batch: b,
        }
    })
}

/// Folders by kind, plus (kind, reason) for folders left alone because they are in use.
type DevFound = (
    HashMap<&'static str, Vec<PathBuf>>,
    Vec<(&'static str, String)>,
);

const BUILD_DIRS: [&str; 4] = ["node_modules", "target", ".gradle", "build"];
const ACTIVITY_DEPTH: u32 = 8;

/// True if any of the project's own files (build output excluded) changed after `cutoff`.
/// Recursive and depth-limited; stops at the first newer file.
fn project_active_since(dir: &Path, cutoff: u64, depth: u32) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue };
        if is_reparse(&md) {
            continue;
        }
        if md.is_dir() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if !BUILD_DIRS.contains(&n.as_str()) {
                subdirs.push(e.path());
            }
        } else if mtime_secs(&md) >= cutoff {
            return true;
        }
    }
    depth < ACTIVITY_DEPTH
        && subdirs
            .iter()
            .any(|d| project_active_since(d, cutoff, depth + 1))
}

/// Why a folder must be left alone right now: a program file inside it is open or running.
fn in_use_reason(dir: &Path) -> Option<String> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(md) = e.metadata() else { continue };
            if is_reparse(&md) {
                continue;
            }
            if md.is_dir() {
                stack.push(e.path());
                continue;
            }
            let name = e.file_name().to_string_lossy().to_lowercase();
            if !["exe", "dll", "node", "pdb", "sys"]
                .iter()
                .any(|x| name.ends_with(&format!(".{x}")))
            {
                continue;
            }
            // Share mode 0: fails if any process has the file open.
            match std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(e.path())
            {
                Err(err) if matches!(err.raw_os_error(), Some(32 | 33 | 5)) => {
                    return Some(format!("{} is open in another program", e.path().display()));
                }
                _ => {}
            }
        }
    }
    None
}

/// Project folders that can be rebuilt, next to the file that shows they are one.
/// Also returns folders skipped because something is using them, with the reason.
fn dev_candidates(env: &Env, task: &Task<Vec<Item>>, th: &mut Throttle) -> DevFound {
    let mut found: HashMap<&'static str, Vec<PathBuf>> = HashMap::new();
    let mut skipped: Vec<(&'static str, String)> = Vec::new();
    let cutoff = now_secs().saturating_sub(DAYS_OLD * DAY);
    let mut stack: Vec<(PathBuf, u32)> = vec![(env.profile.clone(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if !th.tick(task) {
            break;
        }
        task.set_phase(&format!(
            "Looking for old project folders: {}",
            dir.display()
        ));
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: HashMap<String, (bool, u64)> = HashMap::new();
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if is_reparse(&md) {
                continue;
            }
            names.insert(
                e.file_name().to_string_lossy().to_lowercase(),
                (md.is_dir(), mtime_secs(&md)),
            );
        }
        let has = |n: &str| names.contains_key(n);
        let rules: [(&str, bool, &'static str); 4] = [
            ("node_modules", has("package.json"), "node"),
            ("target", has("cargo.toml"), "rust"),
            (
                ".gradle",
                has("build.gradle") || has("build.gradle.kts") || has("settings.gradle"),
                "gradle",
            ),
            (
                "build",
                has("build.gradle") || has("build.gradle.kts"),
                "gradle",
            ),
        ];
        for (name, marker, kind) in rules {
            if marker && names.get(name).is_some_and(|(d, _)| *d) {
                // Old = no project file anywhere below (build output excluded) changed in 90 days.
                let top_new = names
                    .iter()
                    .filter(|(n, _)| !BUILD_DIRS.contains(&n.as_str()))
                    .any(|(_, (_, m))| *m >= cutoff);
                if top_new || project_active_since(&dir, cutoff, 0) {
                    continue;
                }
                let folder = dir.join(name_case(&dir, name));
                if let Some(why) = in_use_reason(&folder) {
                    skipped.push((kind, format!("{}: {why}", folder.display())));
                } else {
                    found.entry(kind).or_default().push(folder);
                }
            }
        }
        if depth < 6 {
            for d in child_dirs(&dir, |n| {
                !matches!(
                    n.to_lowercase().as_str(),
                    "appdata"
                        | "node_modules"
                        | ".git"
                        | "target"
                        | ".gradle"
                        | "build"
                        | "$recycle.bin"
                        | ".cargo"
                        | ".rustup"
                        | ".npm"
                        | ".nuget"
                )
            }) {
                stack.push((d, depth + 1));
            }
        }
    }
    (found, skipped)
}

/// The folder's real spelling (names were lower-cased for the lookups).
fn name_case(dir: &Path, lower: &str) -> String {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.to_lowercase() == lower)
        .unwrap_or_else(|| lower.to_owned())
}

/// Finds the known space-wasters. Nothing is changed.
pub fn scan(env: &Env, task: &Task<Vec<Item>>, pause: &PauseState, gentle: bool) -> Vec<Item> {
    let mut th = Throttle::new(gentle, pause.clone());
    let mut out: Vec<Item> = Vec::new();
    let mut push = |i: Option<Item>| out.extend(i);

    task.set_phase("Recycle Bin");
    push(recycle_bin_item());

    task.set_phase("Temp folder");
    // Files from the last day may still be in use; they stay.
    let day_ago = now_secs().saturating_sub(DAY);
    let mut temp_entries = Vec::new();
    if env.temp.starts_with(&env.profile) {
        for e in std::fs::read_dir(&env.temp).into_iter().flatten().flatten() {
            if !th.tick(task) {
                break;
            }
            let Ok(md) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if is_reparse(&md) || mtime_secs(&md) > day_ago {
                continue;
            }
            let size = if md.is_dir() {
                dir_size(&e.path(), task, &mut th)
            } else {
                md.len()
            };
            temp_entries.extend(entry_of(env, e.path(), size, &md));
        }
    }
    if !temp_entries.is_empty() {
        push(Some(Item {
            id: "temp",
            title: "Temporary files".to_owned(),
            why: "Leftovers in your Temp folder from the last day or older. Files in use are skipped."
                .to_owned(),
            risk: Risk::Safe,
            batch: batch("Temporary files", Mode::Delete, temp_entries),
        }));
    }

    let browsers: [(&'static str, &str, PathBuf, &[&str]); 3] = [
        (
            "chrome",
            "Chrome cache",
            env.local.join(r"Google\Chrome\User Data"),
            &["Cache", "Code Cache", "GPUCache"],
        ),
        (
            "edge",
            "Edge cache",
            env.local.join(r"Microsoft\Edge\User Data"),
            &["Cache", "Code Cache", "GPUCache"],
        ),
        (
            "firefox",
            "Firefox cache",
            env.local.join(r"Mozilla\Firefox\Profiles"),
            &["cache2"],
        ),
    ];
    for (id, title, base, subs) in browsers {
        if task.cancelled() {
            break;
        }
        task.set_phase(title);
        let mut dirs = Vec::new();
        for profile in child_dirs(&base, |_| true) {
            for s in subs {
                let c = profile.join(s);
                if c.is_dir() {
                    dirs.push(c);
                }
            }
        }
        push(folders_item(
            env,
            task,
            &mut th,
            id,
            title,
            "Saved copies of web pages; the browser rebuilds them. Close the browser first, or files in use are skipped.",
            Risk::Safe,
            Mode::Delete,
            dirs,
        ));
    }

    task.set_phase("Old installers in Downloads");
    let downloads = env.profile.join("Downloads");
    let cutoff = now_secs().saturating_sub(DAYS_OLD * DAY);
    let mut inst = Vec::new();
    for e in std::fs::read_dir(&downloads)
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(md) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        let name = e.file_name().to_string_lossy().to_lowercase();
        let ext_ok = [".exe", ".msi", ".iso"].iter().any(|x| name.ends_with(x));
        if md.is_file() && !is_reparse(&md) && ext_ok && mtime_secs(&md) < cutoff {
            inst.extend(entry_of(env, e.path(), md.len(), &md));
        }
    }
    if !inst.is_empty() {
        push(Some(Item {
            id: "installers",
            title: "Old installers in Downloads".to_owned(),
            why: "Setup files (.exe, .msi, .iso) older than 90 days. Check you do not need them again."
                .to_owned(),
            risk: Risk::CheckFirst,
            batch: batch("Old installers", Mode::Recycle, inst),
        }));
    }

    let (dev, skipped) = dev_candidates(env, task, &mut th);
    for (kind, id, title, why) in [
        (
            "node",
            "dev_node",
            "Old node_modules folders",
            "Next to a package.json, nothing in the project changed for 90 days. `npm install` brings them back.",
        ),
        (
            "rust",
            "dev_rust",
            "Old Rust target folders",
            "Next to a Cargo.toml, nothing in the project changed for 90 days. `cargo build` makes them again (slowly).",
        ),
        (
            "gradle",
            "dev_gradle",
            "Old Gradle build folders",
            "`.gradle` and `build` next to a build.gradle, nothing in the project changed for 90 days.",
        ),
    ] {
        let dirs = dev.get(kind).cloned().unwrap_or_default();
        let notes: Vec<&str> = skipped
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, n)| n.as_str())
            .collect();
        let why = if notes.is_empty() {
            why.to_owned()
        } else {
            format!("{why} Skipped, in use: {}.", notes.join("; "))
        };
        let item = folders_item(
            env,
            task,
            &mut th,
            id,
            title,
            &why,
            Risk::CheckFirst,
            Mode::Recycle,
            dirs,
        );
        match item {
            Some(i) => push(Some(i)),
            // Nothing to offer, but say why a folder was left out.
            None if !notes.is_empty() => push(Some(Item {
                id,
                title: title.to_owned(),
                why,
                risk: Risk::CheckFirst,
                batch: batch(title, Mode::Recycle, Vec::new()),
            })),
            None => {}
        }
    }

    let caches: [(&'static str, &str, PathBuf, &str, Risk); 3] = [
        (
            "npm",
            "npm cache",
            env.local.join("npm-cache"),
            "Downloaded packages npm keeps for speed; it fetches them again when needed.",
            Risk::Safe,
        ),
        (
            "pip",
            "pip cache",
            env.local.join(r"pip\Cache"),
            "Downloaded Python packages pip keeps for speed.",
            Risk::Safe,
        ),
        (
            "cargo",
            "Cargo registry cache",
            env.profile.join(r".cargo\registry"),
            "Downloaded crate sources. Cargo downloads them again, so offline builds stop working until then.",
            Risk::CheckFirst,
        ),
    ];
    for (id, title, dir, why, risk) in caches {
        if task.cancelled() {
            break;
        }
        task.set_phase(title);
        let dirs = if dir.is_dir() {
            if id == "cargo" {
                child_dirs(&dir, |n| matches!(n, "cache" | "src"))
            } else {
                vec![dir]
            }
        } else {
            Vec::new()
        };
        push(folders_item(
            env,
            task,
            &mut th,
            id,
            title,
            why,
            risk,
            Mode::Delete,
            dirs,
        ));
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.bytes()));
    out
}

// ------------------------------------------------------------ the service

/// The Clean up list and the duplicate finder: on demand, one of each at a time.
pub struct Tidy {
    pub store: Arc<BatchStore>,
    cleanup: Arc<Task<Vec<Item>>>,
    dupes: Arc<Task<Vec<crate::dupes::Group>>>,
    pause: PauseState,
}

impl Tidy {
    pub fn new(store: Arc<BatchStore>, pause: PauseState) -> Self {
        Self {
            store,
            cleanup: Arc::default(),
            dupes: Arc::default(),
            pause,
        }
    }

    pub fn start_cleanup(&self, gentle: bool) -> Result<(), String> {
        if self.pause.is_paused() {
            return Err("Lookout is paused; resume it to scan.".to_owned());
        }
        if self.cleanup.state() == TaskState::Running {
            return Err("A scan is already running.".to_owned());
        }
        self.cleanup.reset();
        let (task, pause, env) = (
            self.cleanup.clone(),
            self.pause.clone(),
            self.store.env.clone(),
        );
        std::thread::spawn(move || {
            let items = scan(&env, &task, &pause, gentle);
            task.finish(items);
        });
        Ok(())
    }

    pub fn cleanup_progress(&self) -> TaskView {
        self.cleanup.view()
    }
    pub fn cancel_cleanup(&self) {
        self.cleanup.cancel.store(true, Ordering::Relaxed);
    }
    pub fn cleanup_items(&self) -> Vec<ItemView> {
        self.cleanup.with_result(|v| views(v)).unwrap_or_default()
    }
    /// Exactly what Clean would remove, for the Preview button.
    pub fn cleanup_preview(&self, id: &str) -> Option<(Vec<(String, u64)>, u64)> {
        self.cleanup.with_result(|v| {
            let i = v.iter().find(|i| i.id == id)?;
            let all: Vec<(String, u64)> = i
                .batch
                .entries
                .iter()
                .map(|e| (e.path.to_string_lossy().into_owned(), e.size))
                .collect();
            let total = all.len() as u64;
            Some((all.into_iter().take(2000).collect(), total))
        })?
    }
    /// Stores the item's removal list and returns the id the action takes.
    pub fn cleanup_batch(&self, id: &str) -> Option<String> {
        let b = self
            .cleanup
            .with_result(|v| v.iter().find(|i| i.id == id).map(|i| i.batch.clone()))??;
        Some(self.store.put(b))
    }

    pub fn start_dupes(&self, root: &Path, min_size: u64, gentle: bool) -> Result<(), String> {
        if self.pause.is_paused() {
            return Err("Lookout is paused; resume it to scan.".to_owned());
        }
        if self.dupes.state() == TaskState::Running {
            return Err("A scan is already running.".to_owned());
        }
        let env = self.store.env.clone();
        if !root.is_dir() {
            return Err("That is not a folder.".to_owned());
        }
        if env
            .system_dirs
            .iter()
            .any(|d| !d.as_os_str().is_empty() && root.starts_with(d))
        {
            return Err("System and program folders are not scanned for duplicates.".to_owned());
        }
        self.dupes.reset();
        let (task, pause, root) = (self.dupes.clone(), self.pause.clone(), root.to_path_buf());
        std::thread::spawn(move || {
            let g = crate::dupes::find(&env, &root, min_size, &task, &pause, gentle);
            task.finish(g);
        });
        Ok(())
    }
    pub fn dupes_progress(&self) -> TaskView {
        self.dupes.view()
    }
    pub fn cancel_dupes(&self) {
        self.dupes.cancel.store(true, Ordering::Relaxed);
    }
    /// The biggest groups (at most `limit`) and how many there are.
    pub fn dupe_groups(&self, limit: usize) -> (Vec<crate::dupes::Group>, usize) {
        self.dupes
            .with_result(|g| (g.iter().take(limit).cloned().collect(), g.len()))
            .unwrap_or_default()
    }
    /// A removal list for the chosen groups: everything but one keeper each.
    pub fn dupe_batch(
        &self,
        strategy: &str,
        manual: &HashMap<u32, usize>,
        groups: &[u32],
    ) -> Option<String> {
        let b = self.dupes.with_result(|all| {
            let chosen: Vec<&crate::dupes::Group> =
                all.iter().filter(|g| groups.contains(&g.id)).collect();
            crate::dupes::build_batch(&self.store.env, &chosen, &|g| match manual.get(&g.id) {
                Some(&i) if strategy == "manual" => crate::dupes::Keep::Index(i),
                _ => crate::dupes::Keep::parse(strategy),
            })
        })??;
        Some(self.store.put(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_in(root: &Path) -> Env {
        let profile = root.join("Users").join("me");
        std::fs::create_dir_all(profile.join("AppData").join("Local").join("Temp")).unwrap();
        Env {
            local: profile.join("AppData").join("Local"),
            roaming: profile.join("AppData").join("Roaming"),
            temp: profile.join("AppData").join("Local").join("Temp"),
            system_dirs: vec![
                root.join("Windows"),
                root.join("Program Files"),
                root.join("Program Files (x86)"),
            ],
            profile,
        }
    }

    #[test]
    fn protected_folder_rule() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let r = d.path();
        for p in [
            r.join("Windows"),
            r.join("Windows").join("Temp").join("x.tmp"),
            r.join("Program Files").join("App"),
            r.join("Program Files (x86)").join("App").join("a.dll"),
            env.profile.clone(),
            r.join("Users"),
            r.to_path_buf(),
            env.profile
                .join("AppData")
                .join("..")
                .join("..")
                .join("Windows"),
            r.join("$Recycle.Bin").join("S-1").join("x"),
            PathBuf::from("relative\\file.txt"),
        ] {
            assert!(is_protected(&env, &p), "{} must be protected", p.display());
        }
        // Case does not matter on Windows.
        assert!(is_protected(&env, &r.join("WINDOWS").join("x")));
        for p in [
            env.temp.join("junk"),
            env.profile.join("Downloads").join("setup.exe"),
            env.profile
                .join("Projects")
                .join("app")
                .join("node_modules"),
            env.profile.join("AppData").join("Local").join("npm-cache"),
        ] {
            assert!(!is_protected(&env, &p), "{} should be allowed", p.display());
        }
    }

    #[test]
    fn scan_never_offers_protected_paths_and_respects_age_and_markers() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let old = filetime_days_ago(120);
        // An old Node project and a fresh one.
        for (name, age) in [("old", 120), ("fresh", 1)] {
            let p = env.profile.join("proj").join(name);
            std::fs::create_dir_all(p.join("node_modules").join("x")).unwrap();
            std::fs::write(
                p.join("node_modules").join("x").join("f.js"),
                vec![1u8; 2000],
            )
            .unwrap();
            std::fs::write(p.join("package.json"), "{}").unwrap();
            if age > 1 {
                set_old(&p.join("package.json"), old);
                set_old(&p.join("node_modules"), old);
                set_old(&p, old);
            }
        }
        // node_modules without a package.json is not a project: not offered.
        let stray = env.profile.join("stray").join("node_modules");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("a"), vec![1u8; 100]).unwrap();
        // An old installer and a new one.
        let dl = env.profile.join("Downloads");
        std::fs::create_dir_all(&dl).unwrap();
        std::fs::write(dl.join("old.exe"), vec![0u8; 500]).unwrap();
        set_old(&dl.join("old.exe"), old);
        std::fs::write(dl.join("new.exe"), vec![0u8; 500]).unwrap();
        std::fs::write(dl.join("notes.txt"), "x").unwrap();
        set_old(&dl.join("notes.txt"), old);

        let task = Task::default();
        let items = scan(&env, &task, &PauseState::new(), false);
        let all: Vec<&Entry> = items.iter().flat_map(|i| &i.batch.entries).collect();
        assert!(all.iter().all(|e| !is_protected(&env, &e.path)));
        let paths: Vec<String> = all
            .iter()
            .map(|e| e.path.to_string_lossy().into_owned())
            .collect();
        assert!(
            paths.iter().any(|p| p.ends_with(r"old\node_modules")),
            "{paths:?}"
        );
        assert!(!paths.iter().any(|p| p.contains("fresh")), "{paths:?}");
        assert!(!paths.iter().any(|p| p.contains("stray")), "{paths:?}");
        assert!(paths.iter().any(|p| p.ends_with("old.exe")));
        assert!(!paths
            .iter()
            .any(|p| p.ends_with("new.exe") || p.ends_with("notes.txt")));
    }

    #[test]
    fn old_top_level_with_recent_nested_file_is_not_offered() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let old = filetime_days_ago(200);
        let mk = |name: &str, nested_age: u64| {
            let p = env.profile.join("proj").join(name);
            std::fs::create_dir_all(p.join("node_modules")).unwrap();
            std::fs::write(p.join("node_modules").join("f.js"), vec![1u8; 100]).unwrap();
            std::fs::create_dir_all(p.join("src").join("deep")).unwrap();
            std::fs::write(p.join("package.json"), "{}").unwrap();
            let nested = p.join("src").join("deep").join("a.ts");
            std::fs::write(&nested, "x").unwrap();
            set_old(&p.join("package.json"), old);
            set_old(&p.join("node_modules"), old);
            if nested_age > 0 {
                set_old(&nested, filetime_days_ago(nested_age));
            }
            set_old(&p.join("src").join("deep"), old);
            set_old(&p.join("src"), old);
            set_old(&p, old);
        };
        mk("recent_nested", 0);
        mk("all_old", 150);
        let task = Task::default();
        let items = scan(&env, &task, &PauseState::new(), false);
        let paths: Vec<String> = items
            .iter()
            .flat_map(|i| &i.batch.entries)
            .map(|e| e.path.to_string_lossy().into_owned())
            .collect();
        assert!(
            !paths.iter().any(|p| p.contains("recent_nested")),
            "{paths:?}"
        );
        assert!(paths.iter().any(|p| p.contains("all_old")), "{paths:?}");
        let node = items.iter().find(|i| i.id == "dev_node").unwrap();
        assert_eq!(node.risk, Risk::CheckFirst);
    }

    #[test]
    fn folder_with_open_program_file_is_skipped_with_reason() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let old = filetime_days_ago(200);
        let p = env.profile.join("proj").join("busy");
        std::fs::create_dir_all(p.join("node_modules").join(".bin")).unwrap();
        let exe = p.join("node_modules").join(".bin").join("tool.exe");
        std::fs::write(&exe, vec![1u8; 100]).unwrap();
        std::fs::write(p.join("package.json"), "{}").unwrap();
        set_old(&p.join("package.json"), old);
        set_old(&p.join("node_modules"), old);
        set_old(&p, old);
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&exe)
            .unwrap();
        let task = Task::default();
        let items = scan(&env, &task, &PauseState::new(), false);
        let node = items.iter().find(|i| i.id == "dev_node").unwrap();
        assert!(node.batch.entries.is_empty());
        assert!(node.why.contains("Skipped, in use"), "{}", node.why);
    }

    fn filetime_days_ago(days: u64) -> SystemTime {
        SystemTime::now() - Duration::from_secs(days * DAY)
    }

    fn set_old(p: &Path, t: SystemTime) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(p)
            .or_else(|_| {
                use std::os::windows::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS: open a folder
                    .open(p)
            })
            .unwrap();
        f.set_modified(t).unwrap();
    }
}
