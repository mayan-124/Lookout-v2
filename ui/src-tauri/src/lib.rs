//! Lookout daemon shell (Phase 0): pre-created hidden bar window, global
//! hotkey, tray icon (Pause / Quit) and hotkey-to-visible latency measurement.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use lookout_core::actions::open::Launcher;
use lookout_core::actions::{Done, GateConfig, Prepared};
use lookout_core::apps::default_start_menu_dirs;
use lookout_core::cli::{parse_dev_flags, parse_measure_args, MeasureArgs};
use lookout_core::content::idle::SystemIdle;
use lookout_core::content::ocr::WindowsOcr;
use lookout_core::engine::Sink;
use lookout_core::latency::{self, Sample};
use lookout_core::lookout::{Lookout, StartOptions, Status};
use lookout_core::pause::PauseState;
use lookout_core::settings::{monitor_key, position_on_monitor, Settings, DEFAULT_HOTKEY};
use lookout_core::storage::{DataPaths, Storage};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindow, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

const BAR: &str = "bar";

/// How long to wait for the frontend before telling the user it failed to load.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
enum BarError {
    #[error("bar window does not exist")]
    NoWindow,
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
}

/// A show request waiting for the frontend to report that it painted.
struct Pending {
    id: u64,
    t0: Instant,
    native_ms: f64,
}

struct Shared {
    pause: PauseState,
    pending: Mutex<Option<Pending>>,
    next_id: AtomicU64,
    frontend_ready: AtomicBool,
    started: Instant,
    /// Present only in `--measure-latency` mode.
    samples_tx: Mutex<Option<Sender<Sample>>>,
    /// The tray's "Pause indexing" item, kept in step with the pause hotkey.
    pause_item: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
    pause_shortcut: Mutex<Option<Shortcut>>,
    grab_shortcut: Mutex<Option<Shortcut>>,
    /// True while a screen-text grab is running, so the hotkey cannot start a second.
    grabbing: AtomicBool,
    /// A native folder dialog is open: the bar must not hide when it loses focus.
    dialog_open: AtomicBool,
    /// When the bar was last hidden (the frontend restores its state if that was recent).
    hidden_at: Mutex<Option<Instant>>,
    /// Actions currently running (the shell may take the foreground meanwhile).
    actions: ActionTracker,
    /// Pinned: stays on top and does not hide when it loses focus.
    pinned: AtomicBool,
    /// The position the shell last set itself, so its own moves are not saved as the user's.
    last_set: Mutex<Option<(i32, i32)>>,
    /// Counts user moves; only the latest one is written to disk.
    move_gen: AtomicU64,
}

/// Longest an action may hold the blur suspension; the action itself keeps running.
const ACTION_BLUR_LIMIT: Duration = Duration::from_secs(30);

/// Start times of running actions.
#[derive(Default)]
struct ActionTracker {
    running: Mutex<Vec<(u64, Instant)>>,
    next: AtomicU64,
}

impl ActionTracker {
    /// True while some action started less than `limit` before `now`.
    fn suspends_hide(&self, now: Instant, limit: Duration) -> bool {
        lock(&self.running)
            .iter()
            .any(|(_, t)| now.saturating_duration_since(*t) < limit)
    }
}

/// Held while an action runs: the bar must not hide on blur until it is dropped
/// (or the limit passes).
struct ActionGuard<'a> {
    tracker: &'a ActionTracker,
    id: u64,
}

impl<'a> ActionGuard<'a> {
    fn new(tracker: &'a ActionTracker, now: Instant) -> Self {
        let id = tracker.next.fetch_add(1, Ordering::Relaxed);
        lock(&tracker.running).push((id, now));
        Self { tracker, id }
    }
}

impl Drop for ActionGuard<'_> {
    fn drop(&mut self) {
        lock(&self.tracker.running).retain(|(id, _)| *id != self.id);
    }
}

/// Whether losing focus may hide the bar.
fn may_hide_on_blur(dialog_open: bool, action_suspends: bool) -> bool {
    !dialog_open && !action_suspends
}

/// A pinned window never hides on blur.
fn blur_hides(pinned: bool, dialog_open: bool, action_suspends: bool) -> bool {
    !pinned && may_hide_on_blur(dialog_open, action_suspends)
}

fn blur_hide_allowed(shared: &Shared) -> bool {
    blur_hides(
        shared.pinned.load(Ordering::Acquire),
        shared.dialog_open.load(Ordering::Acquire),
        shared
            .actions
            .suspends_hide(Instant::now(), ACTION_BLUR_LIMIT),
    )
}

/// Runs an action with blur-hiding suspended, then takes the focus back.
fn with_action<T>(app: &AppHandle, f: impl FnOnce() -> T) -> T {
    let result = {
        let shared = app.state::<Shared>();
        let _guard = ActionGuard::new(&shared.actions, Instant::now());
        f()
    };
    if let Ok(w) = bar_window(app) {
        if w.is_visible().unwrap_or(false) {
            let _ = w.set_focus();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_blur_hide_while_an_action_runs_or_a_dialog_is_open() {
        let t = ActionTracker::default();
        let now = Instant::now();
        let lim = ACTION_BLUR_LIMIT;
        assert!(may_hide_on_blur(false, t.suspends_hide(now, lim)));
        {
            let _a = ActionGuard::new(&t, now);
            let _b = ActionGuard::new(&t, now);
            assert!(!may_hide_on_blur(false, t.suspends_hide(now, lim)));
        }
        assert!(may_hide_on_blur(false, t.suspends_hide(now, lim)));
        assert!(!may_hide_on_blur(true, false));
    }

    #[test]
    fn a_pinned_window_never_hides_on_blur() {
        assert!(blur_hides(false, false, false));
        assert!(!blur_hides(true, false, false));
        assert!(!blur_hides(false, true, false));
        assert!(!blur_hides(false, false, true));
    }

    #[test]
    fn the_guard_is_released_on_error_and_on_panic() {
        let t = ActionTracker::default();
        let now = Instant::now();
        let r: Result<(), String> = {
            let _g = ActionGuard::new(&t, now);
            Err("failed".into())
        };
        assert!(r.is_err());
        assert!(!t.suspends_hide(now, ACTION_BLUR_LIMIT));
        let p = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = ActionGuard::new(&t, now);
            panic!("boom");
        }));
        assert!(p.is_err());
        assert!(!t.suspends_hide(now, ACTION_BLUR_LIMIT));
    }

    #[test]
    fn a_stuck_action_stops_suspending_hide_after_the_limit() {
        let t = ActionTracker::default();
        let start = Instant::now();
        let _g = ActionGuard::new(&t, start);
        let lim = ACTION_BLUR_LIMIT;
        assert!(t.suspends_hide(start + Duration::from_secs(29), lim));
        assert!(!t.suspends_hide(start + Duration::from_secs(30), lim));
        assert!(may_hide_on_blur(false, t.suspends_hide(start + lim, lim)));
    }
}

/// What the frontend gets with `bar-shown`.
#[derive(Clone, serde::Serialize)]
struct Shown {
    id: u64,
    /// Seconds since the bar was hidden; none if it never was.
    hidden_secs: Option<u64>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn bar_window(app: &AppHandle) -> Result<WebviewWindow, BarError> {
    app.get_webview_window(BAR).ok_or(BarError::NoWindow)
}

/// Centers the bar horizontally in the upper third of the monitor under the cursor.
fn position_bar(app: &AppHandle, window: &WebviewWindow) {
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let (Some(monitor), Ok(size)) = (monitor, window.outer_size()) else {
        return;
    };
    let origin = monitor.position();
    let area = monitor.size();
    let mut x = origin.x + (area.width as i32 - size.width as i32) / 2;
    let mut y = origin.y + (f64::from(area.height) * 0.18) as i32;
    // The place the user last dragged it to on this monitor, if still on it.
    let key = monitor_key_of(&monitor);
    let saved = app
        .try_state::<Services>()
        .and_then(|s| lock(&s.settings).window_positions.get(&key).copied());
    if let Some(p) = saved {
        let mon = (origin.x, origin.y, area.width as i32, area.height as i32);
        if position_on_monitor(p, size.width as i32, mon) {
            (x, y) = p;
        }
    }
    *lock(&app.state::<Shared>().last_set) = Some((x, y));
    if let Err(e) = window.set_position(PhysicalPosition::new(x, y)) {
        eprintln!("lookout: could not position bar: {e}");
    }
}

fn monitor_key_of(m: &tauri::Monitor) -> String {
    let (p, s) = (m.position(), m.size());
    monitor_key(
        m.name().map_or("", |n| n.as_str()),
        p.x,
        p.y,
        s.width,
        s.height,
    )
}

/// Saves the pin state and window positions with the other settings (atomic write).
fn persist_window_state(app: &AppHandle, pos: Option<(i32, i32)>) {
    let Some(services) = app.try_state::<Services>() else {
        return;
    };
    let key = bar_window(app)
        .ok()
        .and_then(|w| w.current_monitor().ok().flatten())
        .map(|m| monitor_key_of(&m));
    let mut s = lock(&services.settings);
    s.pinned = app.state::<Shared>().pinned.load(Ordering::Acquire);
    if let (Some(key), Some(pos)) = (key, pos) {
        s.window_positions.insert(key, pos);
    }
    if let Err(e) = s.save(&services.paths.settings()) {
        eprintln!("lookout: could not save window state: {e}");
    }
}

/// The user moved the window: remember the place once they stop dragging.
fn on_bar_moved(app: &AppHandle, pos: PhysicalPosition<i32>) {
    let shared = app.state::<Shared>();
    if lock(&shared.last_set).is_some_and(|p| p == (pos.x, pos.y)) {
        return; // our own placement
    }
    let n = shared.move_gen.fetch_add(1, Ordering::AcqRel) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        if app.state::<Shared>().move_gen.load(Ordering::Acquire) == n {
            persist_window_state(&app, Some((pos.x, pos.y)));
        }
    });
}

#[derive(serde::Serialize)]
struct WindowState {
    pinned: bool,
}

#[tauri::command]
fn window_state(shared: State<Shared>) -> WindowState {
    WindowState {
        pinned: shared.pinned.load(Ordering::Acquire),
    }
}

#[tauri::command]
fn set_pinned(pinned: bool, app: AppHandle) {
    app.state::<Shared>()
        .pinned
        .store(pinned, Ordering::Release);
    if pinned {
        if let Ok(w) = bar_window(&app) {
            let _ = w.set_always_on_top(true);
        }
    }
    persist_window_state(&app, None);
}

/// Tab in a path query: the next folder name, or `None`.
#[tauri::command]
fn complete_path(text: String) -> Option<String> {
    lookout_core::pathquery::complete(&text)
}

/// The built-in skip list, for the Settings screen.
#[tauri::command]
fn skip_list() -> Vec<String> {
    lookout_core::exclude::skip_list()
}

fn show_bar(app: &AppHandle) -> Result<(), BarError> {
    let t0 = Instant::now();
    let window = bar_window(app)?;
    position_bar(app, &window);
    window.show()?;
    window.set_focus()?;
    let native_ms = ms(t0.elapsed());

    let shared = app.state::<Shared>();
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    *lock(&shared.pending) = Some(Pending { id, t0, native_ms });
    let hidden_secs = lock(&shared.hidden_at).map(|t| t.elapsed().as_secs());
    app.emit_to(BAR, "bar-shown", Shown { id, hidden_secs })?;
    Ok(())
}

fn hide_bar_window(app: &AppHandle) {
    *lock(&app.state::<Shared>().hidden_at) = Some(Instant::now());
    if let Ok(window) = bar_window(app) {
        if let Err(e) = window.hide() {
            eprintln!("lookout: could not hide bar: {e}");
        }
    }
}

fn toggle_bar(app: &AppHandle) {
    // Before the frontend has its listener registered a show would be lost
    // (and measured wrongly), so the hotkey is ignored until it reports ready.
    if !app.state::<Shared>().frontend_ready.load(Ordering::Acquire) {
        return;
    }
    let visible = bar_window(app)
        .and_then(|w| w.is_visible().map_err(BarError::from))
        .unwrap_or(false);
    if visible {
        hide_bar_window(app);
    } else if let Err(e) = show_bar(app) {
        eprintln!("lookout: could not show bar: {e}");
    }
}

#[tauri::command]
fn hide_bar(app: AppHandle) {
    hide_bar_window(&app);
}

#[tauri::command]
fn frontend_ready(shared: State<Shared>) {
    if !shared.frontend_ready.swap(true, Ordering::AcqRel) {
        eprintln!(
            "lookout: frontend ready {:.0} ms after launch",
            ms(shared.started.elapsed())
        );
    }
}

/// Called by the frontend two animation frames after `bar-shown`.
#[tauri::command]
fn bar_painted(id: u64, shared: State<Shared>) {
    let pending = {
        let mut slot = lock(&shared.pending);
        match slot.take() {
            Some(p) if p.id == id => p,
            // A newer show superseded this one (or the bar was toggled fast).
            other => {
                *slot = other;
                return;
            }
        }
    };
    let sample = Sample {
        native_ms: pending.native_ms,
        painted_ms: ms(pending.t0.elapsed()),
    };
    eprintln!(
        "lookout: hotkey-to-visible {:.1} ms (native {:.1} ms)",
        sample.painted_ms, sample.native_ms
    );
    if let Some(tx) = lock(&shared.samples_tx).as_ref() {
        let _ = tx.send(sample);
    }
}

/// Search state. `lookout` is `None` if it failed to start (the user was told
/// by a dialog) and is replaced whenever the user changes folders or deletes
/// the index.
struct Services {
    lookout: RwLock<Option<Arc<Lookout>>>,
    settings: Mutex<Settings>,
    paths: DataPaths,
    flags: lookout_core::cli::DevFlags,
}

impl Services {
    fn current(&self) -> Option<Arc<Lookout>> {
        self.lookout
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn replace(&self, next: Option<Arc<Lookout>>) -> Option<Arc<Lookout>> {
        std::mem::replace(
            &mut *self.lookout.write().unwrap_or_else(PoisonError::into_inner),
            next,
        )
    }
}

/// Starts a query. The frontend numbers its own queries so that it can drop
/// any update that is not for its latest one.
#[tauri::command]
fn query(generation: u64, text: String, services: State<Services>) -> bool {
    services
        .current()
        .is_some_and(|l| l.query(generation, &text))
}

/// Opens a path the way the `open` crate does. The action engine decides
/// whether it may be opened; this only does it.
struct ShellLauncher;

impl Launcher for ShellLauncher {
    fn open(&self, path: &std::path::Path) -> std::io::Result<()> {
        open::that_detached(path)
    }
}

fn services_of(services: &State<'_, Services>) -> Result<Arc<Lookout>, String> {
    services
        .current()
        .ok_or_else(|| "search is not running".to_owned())
}

/// Actions are a short protocol (the slow ones are `async` so a stop or restart
/// that takes seconds does not freeze the window); every step is a separate command and the
/// backend enforces the order (see `lookout_core::actions::gate`):
/// `prepare_action` -> `present_action` -> `run_action` (safe) or
/// `confirm_action`. The webview only ever names an action and a result id
/// from the latest update; paths and ports come from the backend.
#[tauri::command]
async fn prepare_action(
    action: String,
    id: String,
    services: State<'_, Services>,
) -> Result<Prepared, String> {
    services_of(&services)?
        .prepare_action(&action, &id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn prepare_undo(services: State<'_, Services>) -> Result<Prepared, String> {
    services_of(&services)?
        .prepare_undo()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn present_action(ticket: String, services: State<Services>) -> Result<(), String> {
    services_of(&services)?
        .present_action(&ticket)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn cancel_action(ticket: String, services: State<Services>) {
    if let Ok(l) = services_of(&services) {
        l.cancel_action(&ticket);
    }
}

/// Runs a ticket whose risk is `safe`; the backend refuses any other.
#[tauri::command]
async fn run_action(
    app: AppHandle,
    ticket: String,
    services: State<'_, Services>,
) -> Result<Done, String> {
    let l = services_of(&services)?;
    with_action(&app, || l.run_action(&ticket).map_err(|e| e.to_string()))
}

#[tauri::command]
async fn confirm_action(
    app: AppHandle,
    ticket: String,
    services: State<'_, Services>,
) -> Result<Done, String> {
    let l = services_of(&services)?;
    with_action(&app, || {
        l.confirm_action(&ticket).map_err(|e| e.to_string())
    })
}

#[tauri::command]
fn can_undo(services: State<Services>) -> bool {
    services.current().is_some_and(|l| l.can_undo())
}

#[tauri::command]
fn index_status(services: State<Services>) -> Option<Status> {
    services.current().map(|l| l.status())
}

// ---------------------------------------------------------------- disk usage
// On demand only: nothing here runs until the user presses Scan, and closing
// the view drops the scan (see lookout_core::diskusage).

#[tauri::command]
fn disk_locations() -> Vec<lookout_core::diskusage::Location> {
    lookout_core::diskusage::locations()
}

#[tauri::command]
async fn disk_start(
    path: String,
    low_impact: bool,
    services: State<'_, Services>,
) -> Result<(), String> {
    services_of(&services)?
        .disk()
        .start(std::path::Path::new(&path), low_impact)
        .map_err(|e| e.to_string())
}

/// Whether Fast scan can read this place (an NTFS drive), and why not if it can't.
#[tauri::command]
fn disk_fast_info(path: String) -> lookout_core::diskusage::FastInfo {
    lookout_core::diskusage::fast_info(std::path::Path::new(&path))
}

/// Fast scan: the read-only helper next to `lookout.exe` reads the drive's file
/// table after Windows' permission prompt. Lookout itself is never elevated.
#[tauri::command]
async fn disk_start_fast(path: String, services: State<'_, Services>) -> Result<(), String> {
    let helper = std::env::current_exe()
        .ok()
        .and_then(|e| {
            e.parent().map(|d| {
                d.join("helpers")
                    .join("fastscan")
                    .join("lookout-fastscan.exe")
            })
        })
        .ok_or_else(|| "Fast scan is not available".to_owned())?;
    services_of(&services)?
        .disk()
        .start_fast(std::path::Path::new(&path), &helper)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn disk_progress(services: State<Services>) -> Option<lookout_core::diskusage::ProgressView> {
    services.current().map(|l| l.disk().progress())
}

#[tauri::command]
fn disk_cancel(services: State<Services>) {
    if let Some(l) = services.current() {
        l.disk().cancel();
    }
}

#[tauri::command]
async fn disk_close(services: State<'_, Services>) -> Result<(), String> {
    if let Some(l) = services.current() {
        l.disk().close();
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct DiskView {
    node: u32,
    crumbs: Vec<lookout_core::diskusage::Crumb>,
    rows: Vec<lookout_core::diskusage::Row>,
    total_disk: u64,
    total_logical: u64,
    items: u32,
    stats: lookout_core::diskusage::TreeStats,
    scanned_at: u64,
}

#[tauri::command]
fn disk_view(
    node: Option<u32>,
    sort: String,
    descending: bool,
    services: State<Services>,
) -> Result<DiskView, String> {
    let lookout = services_of(&services)?;
    let tree = lookout
        .disk()
        .tree()
        .ok_or_else(|| "there is no finished scan".to_owned())?;
    let node = node.unwrap_or(0);
    let (total_disk, total_logical, items) = tree.total();
    Ok(DiskView {
        node,
        crumbs: tree.breadcrumb(node),
        rows: tree.children(
            node,
            lookout_core::diskusage::SortBy::parse(&sort),
            descending,
        ),
        total_disk,
        total_logical,
        items,
        stats: tree.stats,
        scanned_at: tree.scanned_at,
    })
}

#[tauri::command]
async fn disk_save(services: State<'_, Services>) -> Result<(), String> {
    services_of(&services)?
        .disk()
        .save()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn disk_saved_info(services: State<Services>) -> Option<lookout_core::diskusage::SavedInfo> {
    services.current().and_then(|l| l.disk().saved_info())
}

#[tauri::command]
async fn disk_load_saved(services: State<'_, Services>) -> Result<(), String> {
    services_of(&services)?
        .disk()
        .load_saved()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn disk_delete_saved(services: State<Services>) {
    if let Some(l) = services.current() {
        l.disk().delete_saved();
    }
}

#[tauri::command]
fn disk_chart(
    node: u32,
    levels: u32,
    services: State<Services>,
) -> Result<lookout_core::diskusage::ChartNode, String> {
    let tree = services_of(&services)?
        .disk()
        .tree()
        .ok_or_else(|| "there is no finished scan".to_owned())?;
    tree.chart(node, levels.min(5))
        .ok_or_else(|| "that row is not in the scan".to_owned())
}

#[derive(serde::Serialize)]
struct FilesView {
    rows: Vec<lookout_core::diskusage::BigRow>,
    /// Why the list may be incomplete, if it may be.
    note: String,
}

/// The "largest files" tab (`kind` = "largest") and the "old large files" tab
/// (`kind` = "old": at least `min_mb` and not modified for `older_days`).
#[tauri::command]
fn disk_files(
    kind: String,
    min_mb: u64,
    older_days: u32,
    services: State<Services>,
) -> Result<FilesView, String> {
    let tree = services_of(&services)?
        .disk()
        .tree()
        .ok_or_else(|| "there is no finished scan".to_owned())?;
    if kind == "largest" {
        let rows = tree.big_files(0, None, 100);
        let note = if rows.len() < 100 {
            "Only files of 1 MB and larger are tracked, so fewer than 100 are listed.".to_owned()
        } else {
            String::new()
        };
        return Ok(FilesView { rows, note });
    }
    let min = min_mb.saturating_mul(1 << 20);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let cutoff = u32::try_from(now.saturating_sub(u64::from(older_days) * 86_400)).unwrap_or(0);
    let rows = tree.big_files(min, Some(cutoff), 2000);
    let note = if tree.big_complete_for(min) {
        String::new()
    } else {
        "This scan kept only the largest 20,000 files, so some files at this size may be missing."
            .to_owned()
    };
    Ok(FilesView { rows, note })
}

#[tauri::command]
async fn disk_prepare_file(
    action: String,
    hit: u32,
    services: State<'_, Services>,
) -> Result<Prepared, String> {
    services_of(&services)?
        .prepare_disk_file_action(&action, hit)
        .map_err(|e| e.to_string())
}

/// Makes the bar window wide enough for the disk view, or puts it back.
#[tauri::command]
fn bar_resize(wide: bool, tall: Option<bool>, app: AppHandle) {
    let Ok(window) = bar_window(&app) else {
        return;
    };
    let (w, h): (f64, f64) = if wide {
        (1180.0, 700.0)
    } else if tall == Some(true) {
        (760.0, 620.0)
    } else {
        (760.0, 440.0)
    };
    // Never larger than the screen the bar is on.
    let (w, h) = match window.current_monitor() {
        Ok(Some(m)) => {
            let scale = m.scale_factor();
            let size = m.size();
            (
                w.min(f64::from(size.width) / scale - 40.0),
                h.min(f64::from(size.height) / scale - 80.0),
            )
        }
        _ => (w, h),
    };
    let _ = window.set_size(tauri::LogicalSize::new(w, h));
    position_bar(&app, &window);
}

/// Step 1 for an action on a row of the disk view; the rest of the protocol is
/// the ordinary `present_action` / `run_action` / `confirm_action`.
#[tauri::command]
async fn disk_prepare(
    action: String,
    node: u32,
    services: State<'_, Services>,
) -> Result<Prepared, String> {
    services_of(&services)?
        .prepare_disk_action(&action, node)
        .map_err(|e| e.to_string())
}

// ------------------------------------------------------------ clean up / duplicates

#[tauri::command]
async fn cleanup_start(gentle: bool, services: State<'_, Services>) -> Result<(), String> {
    services_of(&services)?.tidy().start_cleanup(gentle)
}

#[tauri::command]
fn cleanup_progress(services: State<Services>) -> Option<lookout_core::cleanup::TaskView> {
    services.current().map(|l| l.tidy().cleanup_progress())
}

#[tauri::command]
fn cleanup_cancel(services: State<Services>) {
    if let Some(l) = services.current() {
        l.tidy().cancel_cleanup();
    }
}

#[derive(serde::Serialize)]
struct CleanupList {
    items: Vec<lookout_core::cleanup::ItemView>,
    reclaimed: u64,
}

#[tauri::command]
fn cleanup_items(services: State<Services>) -> CleanupList {
    services
        .current()
        .map(|l| CleanupList {
            items: l.tidy().cleanup_items(),
            reclaimed: l.tidy().store.reclaimed(),
        })
        .unwrap_or(CleanupList {
            items: Vec::new(),
            reclaimed: 0,
        })
}

#[derive(serde::Serialize)]
struct PathSize {
    path: String,
    size: u64,
}

#[derive(serde::Serialize)]
struct PreviewList {
    rows: Vec<PathSize>,
    total: u64,
}

/// What Clean would remove for one item, listed exactly.
#[tauri::command]
fn cleanup_preview(id: String, services: State<Services>) -> Result<PreviewList, String> {
    let l = services.current().ok_or("Lookout is not ready")?;
    let (rows, total) = l
        .tidy()
        .cleanup_preview(&id)
        .ok_or("that item is no longer in the list; scan again")?;
    Ok(PreviewList {
        rows: rows
            .into_iter()
            .map(|(path, size)| PathSize { path, size })
            .collect(),
        total,
    })
}

/// Step 1 of Clean: the ordinary confirm flow follows (`present_action`, `confirm_action`).
#[tauri::command]
async fn cleanup_prepare(id: String, services: State<'_, Services>) -> Result<Prepared, String> {
    let l = services_of(&services)?;
    let batch = l
        .tidy()
        .cleanup_batch(&id)
        .ok_or("that item is no longer in the list; scan again")?;
    l.prepare_batch(&batch).map_err(|e| e.to_string())
}

#[tauri::command]
async fn dupes_start(
    path: String,
    min_size: u64,
    gentle: bool,
    services: State<'_, Services>,
) -> Result<(), String> {
    services_of(&services)?
        .tidy()
        .start_dupes(std::path::Path::new(&path), min_size, gentle)
}

#[tauri::command]
fn dupes_progress(services: State<Services>) -> Option<lookout_core::cleanup::TaskView> {
    services.current().map(|l| l.tidy().dupes_progress())
}

#[tauri::command]
fn dupes_cancel(services: State<Services>) {
    if let Some(l) = services.current() {
        l.tidy().cancel_dupes();
    }
}

#[derive(serde::Serialize)]
struct DupList {
    groups: Vec<lookout_core::dupes::Group>,
    total_groups: usize,
    reclaimed: u64,
}

#[tauri::command]
fn dupes_groups(services: State<Services>) -> DupList {
    services
        .current()
        .map(|l| {
            let (groups, total_groups) = l.tidy().dupe_groups(500);
            DupList {
                groups,
                total_groups,
                reclaimed: l.tidy().store.reclaimed(),
            }
        })
        .unwrap_or(DupList {
            groups: Vec::new(),
            total_groups: 0,
            reclaimed: 0,
        })
}

/// Step 1 of removing duplicates: `strategy` is newest, oldest, shortest or
/// manual; `manual` gives (group, position of the file to keep) pairs.
#[tauri::command]
async fn dupes_prepare(
    strategy: String,
    manual: Vec<(u32, usize)>,
    groups: Vec<u32>,
    services: State<'_, Services>,
) -> Result<Prepared, String> {
    let l = services_of(&services)?;
    let manual: std::collections::HashMap<u32, usize> = manual.into_iter().collect();
    let batch = l
        .tidy()
        .dupe_batch(&strategy, &manual, &groups)
        .ok_or("nothing to remove for those groups; scan again")?;
    l.prepare_batch(&batch).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ plugins

#[tauri::command]
fn plugins_list(services: State<Services>) -> Vec<lookout_core::plugins::PluginInfo> {
    services
        .current()
        .and_then(|l| l.plugins().map(|h| h.list()))
        .unwrap_or_default()
}

/// Turning a plugin on needs `accept` (the user saw its permissions).
#[tauri::command]
async fn plugin_set_enabled(
    id: String,
    enabled: bool,
    accept: bool,
    services: State<'_, Services>,
) -> Result<Vec<lookout_core::plugins::PluginInfo>, String> {
    let lookout = services_of(&services)?;
    let host = lookout.plugins().ok_or("plugins are not available")?;
    host.set_enabled(&id, enabled, accept)
        .map_err(|e| e.to_string())?;
    // Turned on: files the helper could not read before are queued now.
    if enabled && id == lookout_core::content::office::PLUGIN_ID {
        lookout.rescan_contents();
    }
    Ok(host.list())
}

#[tauri::command]
async fn plugin_set_config(
    id: String,
    config: lookout_core::plugins::host::ConfigValue,
    services: State<'_, Services>,
) -> Result<Vec<lookout_core::plugins::PluginInfo>, String> {
    let lookout = services_of(&services)?;
    let host = lookout.plugins().ok_or("plugins are not available")?;
    host.set_config(&id, config).map_err(|e| e.to_string())?;
    Ok(host.list())
}

// ------------------------------------------------------------------ health

#[tauri::command]
fn health(services: State<Services>) -> Option<lookout_core::lookout::Health> {
    services.current().map(|l| l.health())
}

/// "Index now" on or off (off stops a run in progress).
#[tauri::command]
fn index_now(on: bool, services: State<Services>) -> bool {
    services.current().is_some_and(|l| l.index_now(on))
}

// ------------------------------------------------------------------ semantic search

#[tauri::command]
fn semantic_status(services: State<Services>) -> Option<lookout_core::semantic::SemanticStatus> {
    services.current().and_then(|l| l.semantic_status())
}

/// "Delete my index" for semantic search: removes the vectors and turns it off.
#[tauri::command]
async fn semantic_delete_index(services: State<'_, Services>) -> Result<(), String> {
    services_of(&services)?.delete_semantic_index()
}

/// What the confirmation shows before anything is downloaded. No network.
#[tauri::command]
fn semantic_setup_info() -> lookout_core::semantic::setup::SetupInfo {
    lookout_core::semantic::setup::info()
}

#[tauri::command]
fn semantic_setup_status(
    setup: State<Arc<lookout_core::semantic::setup::SemanticSetup>>,
) -> lookout_core::semantic::setup::SetupStatus {
    setup.status()
}

/// The only place Lookout starts a download: the user clicked "Set up semantic search".
#[tauri::command]
fn semantic_setup_start(
    setup: State<Arc<lookout_core::semantic::setup::SemanticSetup>>,
) -> Result<(), String> {
    use lookout_core::semantic::setup::{Plan, WinHttp};
    setup.inner().start(Plan::pinned(), Arc::new(WinHttp))
}

#[tauri::command]
fn semantic_setup_cancel(setup: State<Arc<lookout_core::semantic::setup::SemanticSetup>>) {
    setup.cancel();
}

/// Removes only the three downloaded files. The helper is stopped first (plugin off).
#[tauri::command]
async fn semantic_remove_files(
    services: State<'_, Services>,
    setup: State<'_, Arc<lookout_core::semantic::setup::SemanticSetup>>,
) -> Result<(), String> {
    if let Some(h) = services.current().and_then(|l| l.plugins().cloned()) {
        let _ = h.set_enabled(lookout_core::semantic::PLUGIN_ID, false, false);
    }
    setup.remove_files()
}

// ------------------------------------------------------------------ privacy

/// What the privacy panel shows and edits.
#[derive(serde::Serialize)]
struct PrivacyState {
    roots: Vec<String>,
    excluded_folders: Vec<String>,
    paused: bool,
    needs_setup: bool,
    pause_hotkey: String,
    grab_hotkey: String,
    open_hotkey: String,
    index_contents: bool,
    ocr: bool,
    idle_secs: u32,
    rescan_hours: u32,
    index_speed: lookout_core::settings::IndexSpeed,
    content_index_max_mb: u64,
    theme: lookout_core::settings::Theme,
}

fn privacy_of(services: &Services, shared: &Shared) -> PrivacyState {
    let s = lock(&services.settings);
    let show = |v: &[std::path::PathBuf]| {
        v.iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    PrivacyState {
        roots: show(&s.index_roots),
        excluded_folders: show(&s.excluded_folders),
        paused: shared.pause.is_paused(),
        needs_setup: s.needs_first_run(),
        pause_hotkey: s.pause_hotkey.clone(),
        grab_hotkey: s.grab_hotkey.clone(),
        open_hotkey: s.hotkey.clone(),
        index_contents: s.index_contents,
        ocr: s.ocr,
        idle_secs: s.idle_secs,
        rescan_hours: s.rescan_hours,
        index_speed: s.index_speed,
        content_index_max_mb: s.content_index_max_mb,
        theme: s.theme,
    }
}

#[tauri::command]
fn privacy_state(services: State<Services>, shared: State<Shared>) -> PrivacyState {
    privacy_of(&services, &shared)
}

/// Native folder picker. The bar hides when it loses focus, so it is told not
/// to while the dialog is up.
#[tauri::command]
async fn pick_folder(app: AppHandle, shared: State<'_, Shared>) -> Result<Option<String>, String> {
    shared.dialog_open.store(true, Ordering::Release);
    let picked = app.dialog().file().blocking_pick_folder();
    shared.dialog_open.store(false, Ordering::Release);
    if let Ok(w) = bar_window(&app) {
        let _ = w.set_focus();
    }
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

/// Saves the user's folder choice and restarts indexing with it. An empty
/// list is a valid choice ("index nothing"); the first-run question is then answered.
#[tauri::command]
async fn apply_folders(
    roots: Vec<String>,
    excluded: Vec<String>,
    app: AppHandle,
    services: State<'_, Services>,
    shared: State<'_, Shared>,
) -> Result<PrivacyState, String> {
    let roots: Vec<std::path::PathBuf> = roots
        .into_iter()
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .collect();
    {
        let mut s = lock(&services.settings);
        s.index_roots = roots;
        s.excluded_folders = excluded.into_iter().map(std::path::PathBuf::from).collect();
        s.first_run_done = true;
        s.save(&services.paths.settings())
            .map_err(|e| format!("could not save settings: {e}"))?;
    }
    restart_lookout(&app);
    Ok(privacy_of(&services, &shared))
}

/// The Settings Save: checks every value, saves once, then restarts indexing
/// once. If anything is wrong nothing is applied.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveArgs {
    roots: Vec<String>,
    excluded: Vec<String>,
    idle_secs: i64,
    rescan_hours: i64,
    speed: lookout_core::settings::IndexSpeed,
    content_index_max_mb: i64,
    theme: lookout_core::settings::Theme,
}

#[tauri::command]
async fn save_settings(
    args: SaveArgs,
    app: AppHandle,
    services: State<'_, Services>,
    shared: State<'_, Shared>,
) -> Result<PrivacyState, String> {
    let changes = lookout_core::settings::Changes {
        roots: args
            .roots
            .into_iter()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .collect(),
        excluded_folders: args
            .excluded
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect(),
        idle_secs: args.idle_secs,
        rescan_hours: args.rescan_hours,
        index_speed: args.speed,
        content_index_max_mb: args.content_index_max_mb,
        theme: args.theme,
    };
    lock(&services.settings).apply(changes, &services.paths.settings())?;
    restart_lookout(&app);
    Ok(privacy_of(&services, &shared))
}

/// Changes the grab-text hotkey without a restart. If the new combo has no
/// modifier or cannot be registered, the old hotkey stays active and saved.
#[tauri::command]
fn set_grab_hotkey(
    wanted: String,
    app: AppHandle,
    services: State<'_, Services>,
    shared: State<'_, Shared>,
) -> Result<PrivacyState, String> {
    let shortcut = Shortcut::from_str(wanted.trim())
        .map_err(|e| format!("'{wanted}' is not a valid key combination: {e}"))?;
    if shortcut.mods.is_empty() {
        return Err("Include Ctrl, Alt, Shift or Win in the combination.".to_owned());
    }
    let gs = app.global_shortcut();
    let old = *lock(&shared.grab_shortcut);
    if old == Some(shortcut) {
        return Ok(privacy_of(&services, &shared));
    }
    if let Err(e) = gs.register(shortcut) {
        return Err(lookout_core::screengrab::grab_hotkey_message(
            &wanted,
            &e.to_string(),
        ));
    }
    if let Some(old) = old {
        let _ = gs.unregister(old);
    }
    *lock(&shared.grab_shortcut) = Some(shortcut);
    {
        let mut s = lock(&services.settings);
        s.grab_hotkey = wanted.trim().to_owned();
        s.save(&services.paths.settings())
            .map_err(|e| format!("could not save settings: {e}"))?;
    }
    Ok(privacy_of(&services, &shared))
}

/// Central pause switch for the tray item, the hotkey and the panel.
fn apply_pause(app: &AppHandle, paused: bool) {
    let shared = app.state::<Shared>();
    shared.pause.set(paused);
    let item = lock(&shared.pause_item).clone();
    if let Some(item) = item {
        let _ = item.set_checked(paused);
    }
}

#[tauri::command]
fn set_paused(paused: bool, app: AppHandle) {
    apply_pause(&app, paused);
}

/// "Delete my index": removes the name snapshot, `meta.db` and the key, and
/// forgets the folder choice so the chooser comes back. Settings otherwise stay.
#[tauri::command]
async fn delete_index(
    app: AppHandle,
    services: State<'_, Services>,
    shared: State<'_, Shared>,
) -> Result<PrivacyState, String> {
    let wiped = match services.replace(None) {
        Some(old) => old.wipe_index().map_err(|e| e.to_string()),
        None => services.paths.wipe().map_err(|e| e.to_string()),
    };
    {
        let mut s = lock(&services.settings);
        s.index_roots.clear();
        s.first_run_done = false;
        if let Err(e) = s.save(&services.paths.settings()) {
            eprintln!("lookout: could not save settings: {e}");
        }
    }
    restart_lookout(&app);
    wiped?;
    Ok(privacy_of(&services, &shared))
}

/// Stops the running search, opens the (encrypted) storage and starts a new one
/// from the current settings.
fn restart_lookout(app: &AppHandle) {
    let services = app.state::<Services>();
    if let Some(old) = services.replace(None) {
        old.shutdown();
    }
    let next = start_lookout(app, &services).map(Arc::new);
    services.replace(next);
}

/// Plugins live in `plugins/<name>/plugin.json` next to `lookout.exe`.
fn plugin_config(paths: &DataPaths) -> lookout_core::plugins::HostConfig {
    let dirs = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("plugins")))
        .into_iter()
        .collect();
    lookout_core::plugins::HostConfig::new(dirs, paths.root().to_path_buf())
}

fn start_lookout(app: &AppHandle, services: &Services) -> Option<Lookout> {
    let report = |what: &str, e: &dyn std::fmt::Display| {
        eprintln!("lookout: {what}: {e}");
        app.dialog()
            .message(format!(
                "{what}:

{e}"
            ))
            .title("Lookout: search unavailable")
            .kind(MessageDialogKind::Error)
            .show(|_| {});
    };
    let (storage, reset) = match Storage::open(services.paths.clone()) {
        Ok(s) => s,
        Err(e) => {
            report("Lookout could not open its encrypted storage", &e);
            return None;
        }
    };
    if reset {
        eprintln!(
            "lookout: the stored key or database could not be read; set aside as *.unreadable"
        );
        app.dialog()
            .message(
                "Lookout could not unlock its index (the key does not belong to this Windows \
                 account, or the files are damaged).\n\nThe old index was set aside as \
                 *.unreadable files in Lookout's data folder and a new, empty index was started. \
                 The set-aside files are deleted the next time Lookout starts normally, or when \
                 you use \"Delete my index\".",
            )
            .title("Lookout: index could not be unlocked")
            .kind(MessageDialogKind::Warning)
            .show(|_| {});
    }
    let sink_handle = app.clone();
    let sink: Sink = Arc::new(move |update| {
        if let Err(e) = sink_handle.emit_to(BAR, "query-results", &update) {
            eprintln!("lookout: could not deliver results: {e}");
        }
    });
    let options = StartOptions {
        settings: lock(&services.settings).clone(),
        roots_override: services.flags.index_roots.clone(),
        synthetic_files: services.flags.synthetic_index,
        synthetic_uses_folders: services.flags.synthetic_with_folders,
        start_menu_dirs: default_start_menu_dirs(),
        storage,
        idle: Arc::new(SystemIdle),
        ocr: Arc::new(WindowsOcr),
        launcher: Arc::new(ShellLauncher),
        system: None,
        gate: GateConfig::default(),
        plugins: Some(plugin_config(&services.paths)),
    };
    let pause = app.state::<Shared>().pause.clone();
    match Lookout::start(options, sink, pause) {
        Ok(l) => Some(l),
        Err(e) => {
            report("Lookout could not start search", &e);
            None
        }
    }
}

/// Registers the global shortcut that pauses and resumes indexing. A failure
/// is logged only: the tray item still works.
fn register_pause_hotkey(app: &AppHandle, wanted: &str) {
    if wanted.trim().is_empty() {
        return;
    }
    let shortcut = match Shortcut::from_str(wanted) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lookout: bad pause_hotkey '{wanted}': {e}");
            return;
        }
    };
    match app.global_shortcut().register(shortcut) {
        Ok(()) => *lock(&app.state::<Shared>().pause_shortcut) = Some(shortcut),
        Err(e) => eprintln!("lookout: cannot register pause hotkey '{wanted}': {e}"),
    }
}

/// Registers the "grab text from screen" hotkey. A failure is logged and shown.
fn register_grab_hotkey(app: &AppHandle, wanted: &str) {
    if wanted.trim().is_empty() {
        return;
    }
    let shortcut = match Shortcut::from_str(wanted) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lookout: bad grab_hotkey '{wanted}': {e}");
            return;
        }
    };
    match app.global_shortcut().register(shortcut) {
        Ok(()) => *lock(&app.state::<Shared>().grab_shortcut) = Some(shortcut),
        Err(e) => {
            eprintln!("lookout: cannot register grab hotkey '{wanted}': {e}");
            app.dialog()
                .message(lookout_core::screengrab::grab_hotkey_message(
                    wanted,
                    &e.to_string(),
                ))
                .title("Lookout: grab hotkey unavailable")
                .kind(MessageDialogKind::Warning)
                .show(|_| {});
        }
    }
}

/// Runs one screen-text grab on its own thread (it waits for the user's drag).
fn start_grab(app: &AppHandle) {
    #[cfg(windows)]
    {
        let shared = app.state::<Shared>();
        if shared.grabbing.swap(true, Ordering::AcqRel) {
            return;
        }
        let (app, pause) = (app.clone(), shared.pause.clone());
        std::thread::spawn(move || {
            let outcome = lookout_core::screengrab::run(&pause);
            if let lookout_core::screengrab::Outcome::Failed(why) = &outcome {
                eprintln!("lookout: grab failed: {why}");
            }
            app.state::<Shared>()
                .grabbing
                .store(false, Ordering::Release);
        });
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Registers the configured hotkey, falling back to the default. Returns a
/// human-readable status for the tray tooltip and whether any hotkey is active.
fn register_hotkey(app: &AppHandle, wanted: &str) -> (String, bool) {
    let gs = app.global_shortcut();
    match gs.register(wanted) {
        Ok(()) => return (format!("Lookout - press {wanted}"), true),
        Err(e) => eprintln!("lookout: cannot register hotkey '{wanted}': {e}"),
    }
    if wanted != DEFAULT_HOTKEY {
        match gs.register(DEFAULT_HOTKEY) {
            Ok(()) => {
                return (
                    format!("Lookout - '{wanted}' unavailable, using {DEFAULT_HOTKEY}"),
                    true,
                )
            }
            Err(e) => eprintln!("lookout: cannot register fallback hotkey: {e}"),
        }
    }
    (
        format!("Lookout - hotkey '{wanted}' is unavailable (already used by another app?)"),
        false,
    )
}

fn run_measurement(app: AppHandle, args: MeasureArgs, rx: Receiver<Sample>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !app.state::<Shared>().frontend_ready.load(Ordering::Acquire) && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    // Let the freshly loaded page settle so cycle 0 is the first show, not load jitter.
    std::thread::sleep(Duration::from_millis(500));

    let mut samples = Vec::with_capacity(args.cycles);
    for _ in 0..args.cycles {
        if let Err(e) = show_bar(&app) {
            eprintln!("lookout: measurement show failed: {e}");
            break;
        }
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(s) => samples.push(s),
            Err(_) => eprintln!("lookout: no paint report within 3 s"),
        }
        std::thread::sleep(Duration::from_millis(150));
        hide_bar_window(&app);
        std::thread::sleep(Duration::from_millis(150));
    }
    let complete = samples.len() == args.cycles;
    if let Err(e) = std::fs::write(&args.out, latency::to_csv(&samples)) {
        eprintln!("lookout: could not write {}: {e}", args.out.display());
        app.exit(2);
        return;
    }
    app.exit(if complete { 0 } else { 1 });
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();

    let flags = parse_dev_flags(std::env::args());
    // `--data-dir` keeps tests and benchmarks away from the user's real settings and logs.
    let data_dir = match &flags.data_dir {
        Some(d) => d.clone(),
        None => app.path().app_config_dir()?,
    };

    // Settings: a missing file is created so the hotkey is easy to find and edit.
    let settings_path = data_dir.join("settings.json");
    let mut settings = match Settings::load(&settings_path) {
        Ok(s) => {
            if !settings_path.exists() {
                if let Err(e) = s.save(&settings_path) {
                    eprintln!("lookout: could not write default settings: {e}");
                }
            }
            s
        }
        Err(e) => {
            eprintln!("lookout: ignoring unreadable settings ({e}); using defaults");
            Settings::default()
        }
    };

    // Developer flags that name folders (or generate an index) stand in for the first-run choice.
    if !flags.index_roots.is_empty() || flags.synthetic_index.is_some() {
        settings.first_run_done = true;
    }

    let (tooltip, hotkey_ok) = register_hotkey(&handle, &settings.hotkey);
    if !hotkey_ok {
        let message = format!(
            "Lookout could not register the hotkey '{}' (and the fallback '{DEFAULT_HOTKEY}'). \
             Another app may be using it.\n\n\
             Edit \"hotkey\" in:\n{}\n\nthen restart Lookout. Lookout stays in the tray meanwhile.",
            settings.hotkey,
            settings_path.display()
        );
        handle
            .dialog()
            .message(message)
            .title("Lookout: hotkey unavailable")
            .kind(MessageDialogKind::Warning)
            .show(|_| {});
    }

    register_pause_hotkey(&handle, &settings.pause_hotkey);
    register_grab_hotkey(&handle, &settings.grab_hotkey);

    // Hide when the bar loses focus, like a launcher (except while a folder
    // dialog, which takes focus, is open).
    let window = bar_window(&handle)?;
    handle
        .state::<Shared>()
        .pinned
        .store(settings.pinned, Ordering::Release);
    let blur_handle = handle.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(pos) = event {
            on_bar_moved(&blur_handle, *pos);
        }
        if let WindowEvent::Focused(false) = event {
            let shared = blur_handle.state::<Shared>();
            if !blur_hide_allowed(&shared) {
                return;
            }
            // Focus may have moved to another window of Lookout (a file dialog,
            // a message box): then the bar stays, with Settings, Disk or the
            // confirm card as they are, and hides once focus leaves Lookout.
            let h = blur_handle.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                let deadline = Instant::now() + Duration::from_secs(600);
                while lookout_core::winfocus::foreground_is_ours() && Instant::now() < deadline {
                    if bar_window(&h)
                        .and_then(|w| w.is_focused().map_err(BarError::from))
                        .unwrap_or(false)
                    {
                        return; // back in the bar: its next blur is handled again
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                let focused = bar_window(&h)
                    .and_then(|w| w.is_focused().map_err(BarError::from))
                    .unwrap_or(false);
                if !focused && blur_hide_allowed(&h.state::<Shared>()) {
                    hide_bar_window(&h);
                }
            });
        }
    });

    // Tray: Pause / Quit.
    let pause_item =
        CheckMenuItem::with_id(app, "pause", "Pause indexing", true, false, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit Lookout", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&pause_item, &quit_item])?;
    let pause_for_menu = pause_item.clone();
    let mut tray = TrayIconBuilder::with_id("lookout")
        .tooltip(&tooltip)
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "pause" => {
                // The check item flips its own state before this fires.
                let paused = pause_for_menu.is_checked().unwrap_or(false);
                apply_pause(app, paused);
            }
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    *lock(&handle.state::<Shared>().pause_item) = Some(pause_item);

    app.manage(Arc::new(lookout_core::semantic::setup::SemanticSetup::new(
        &data_dir,
        lookout_core::semantic::PLUGIN_ID,
    )));

    // Search: index, sources and query engine all live in lookout-core.
    app.manage(Services {
        lookout: RwLock::new(None),
        settings: Mutex::new(settings),
        paths: DataPaths::new(data_dir),
        flags,
    });
    restart_lookout(&handle);

    // If the page never reports ready the hotkey would silently do nothing forever.
    let watchdog = handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(READY_TIMEOUT);
        if watchdog
            .state::<Shared>()
            .frontend_ready
            .load(Ordering::Acquire)
        {
            return;
        }
        eprintln!(
            "lookout: ERROR frontend not ready {} s after launch",
            READY_TIMEOUT.as_secs()
        );
        watchdog
            .dialog()
            .message(
                "Lookout's interface failed to load, so the hotkey will not open the bar.\n\n\
                 Try restarting Lookout. If it keeps happening, reinstall it or check that \
                 the WebView2 runtime is installed.",
            )
            .title("Lookout: interface failed to load")
            .kind(MessageDialogKind::Error)
            .show(|_| {});
    });

    if let Some(args) = parse_measure_args(std::env::args()) {
        let (tx, rx) = mpsc::channel();
        *lock(&handle.state::<Shared>().samples_tx) = Some(tx);
        std::thread::spawn(move || run_measurement(handle, args, rx));
    }
    Ok(())
}

pub fn run() {
    let result = tauri::Builder::default()
        .manage(Shared {
            pause: PauseState::new(),
            pending: Mutex::new(None),
            next_id: AtomicU64::new(1),
            frontend_ready: AtomicBool::new(false),
            started: Instant::now(),
            samples_tx: Mutex::new(None),
            pause_item: Mutex::new(None),
            pause_shortcut: Mutex::new(None),
            grab_shortcut: Mutex::new(None),
            grabbing: AtomicBool::new(false),
            dialog_open: AtomicBool::new(false),
            actions: ActionTracker::default(),
            hidden_at: Mutex::new(None),
            pinned: AtomicBool::new(false),
            last_set: Mutex::new(None),
            move_gen: AtomicU64::new(0),
        })
        .plugin(tauri_plugin_dialog::init())
        // Only used when the user clicks "Check for updates" in Settings.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let is_pause = lock(&app.state::<Shared>().pause_shortcut)
                        .as_ref()
                        .is_some_and(|s| s == shortcut);
                    let is_grab = lock(&app.state::<Shared>().grab_shortcut)
                        .as_ref()
                        .is_some_and(|s| s == shortcut);
                    if is_pause {
                        let paused = app.state::<Shared>().pause.is_paused();
                        apply_pause(app, !paused);
                    } else if is_grab {
                        start_grab(app);
                    } else {
                        toggle_bar(app);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            hide_bar,
            frontend_ready,
            bar_painted,
            query,
            prepare_action,
            prepare_undo,
            present_action,
            cancel_action,
            run_action,
            confirm_action,
            can_undo,
            index_status,
            privacy_state,
            pick_folder,
            apply_folders,
            set_paused,
            save_settings,
            set_grab_hotkey,
            delete_index,
            plugins_list,
            plugin_set_enabled,
            plugin_set_config,
            health,
            index_now,
            semantic_status,
            semantic_delete_index,
            semantic_setup_info,
            semantic_setup_status,
            semantic_setup_start,
            semantic_setup_cancel,
            semantic_remove_files,
            disk_locations,
            disk_start,
            disk_fast_info,
            disk_start_fast,
            disk_progress,
            disk_cancel,
            disk_close,
            disk_view,
            disk_save,
            disk_saved_info,
            disk_load_saved,
            disk_delete_saved,
            disk_prepare,
            disk_chart,
            disk_files,
            disk_prepare_file,
            cleanup_start,
            cleanup_progress,
            cleanup_cancel,
            cleanup_items,
            cleanup_preview,
            cleanup_prepare,
            dupes_start,
            dupes_progress,
            dupes_cancel,
            dupes_groups,
            dupes_prepare,
            bar_resize,
            window_state,
            set_pinned,
            complete_path,
            skip_list
        ])
        .setup(setup)
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("lookout: fatal: {e}");
        std::process::exit(1);
    }
}
