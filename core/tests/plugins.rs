//! The plugin host against a deliberately badly behaved plugin
//! (`examples/test_plugin`). Everything happens in temp folders.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lookout_core::actions::open::NoopLauncher;
use lookout_core::actions::{ActionError, GateConfig, Risk};
use lookout_core::engine::{Sink, Update};
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::plugins::{Candidate, HostConfig, HostError, PluginHost, Timeouts};
use lookout_core::settings::Settings;
use lookout_core::storage::Storage;

const ID: &str = "test.plugin";

fn plugin_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args(["build", "-p", "lookout-core", "--example", "test_plugin"])
            .arg("--message-format=json")
            .stderr(Stdio::inherit())
            .output()
            .unwrap();
        assert!(out.status.success(), "building test_plugin failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "test_plugin")
            .find_map(|v| v["executable"].as_str().map(PathBuf::from))
            .expect("cargo did not report the test_plugin executable")
    })
}

struct Env {
    dir: tempfile::TempDir,
    marker: PathBuf,
}

/// A plugin folder holding the test plugin started in `mode`.
fn env(mode: &str, permissions: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("plugins").join("p");
    fs::create_dir_all(&plugin).unwrap();
    let exe = plugin.join("plugin.exe");
    if fs::hard_link(plugin_exe(), &exe).is_err() {
        fs::copy(plugin_exe(), &exe).unwrap();
    }
    let marker = dir.path().join("marker.txt");
    let manifest = serde_json::json!({
        "id": ID, "name": "Test plugin", "version": "0.0.1", "entry": "plugin.exe",
        "args": ["--mode", mode, "--marker", marker, "--state", dir.path().join("state")],
        "triggers": ["touch"],
        "permissions": serde_json::from_str::<serde_json::Value>(permissions).unwrap(),
    });
    fs::write(plugin.join("plugin.json"), manifest.to_string()).unwrap();
    Env { dir, marker }
}

fn config(e: &Env) -> HostConfig {
    let mut c = HostConfig::new(
        vec![e.dir.path().join("plugins")],
        e.dir.path().join("data"),
    );
    c.timeouts = Timeouts {
        init: Duration::from_secs(10),
        query: Duration::from_millis(700),
        preview: Duration::from_millis(700),
        execute: Duration::from_secs(10),
        undo: Duration::from_secs(10),
        shutdown: Duration::from_millis(300),
    };
    c.tick = Duration::from_millis(100);
    c
}

fn host(e: &Env) -> Arc<PluginHost> {
    let h = PluginHost::start(config(e), PauseState::new());
    h.set_enabled(ID, true, true).unwrap();
    h
}

fn no_files(_: &str) -> Vec<Candidate> {
    Vec::new()
}

const WRITES: &str = r#"{"fs_write": ["user_selected"]}"#;

fn pid_alive(pid: u32) -> bool {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(
        sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]),
        true,
    );
    sys.process(sysinfo::Pid::from_u32(pid)).is_some()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_plugin_that_crashes_is_restarted_on_the_next_use() {
    let e = env("crash_once", WRITES);
    let h = host(&e);
    assert!(h.query(ID, "touch", "touch", "", &no_files).is_err());
    assert!(!h.is_running(ID));
    let items = h.query(ID, "touch", "touch", "", &no_files).unwrap();
    assert_eq!(items.len(), 1);
    assert!(h.is_running(ID));
}

#[test]
fn a_plugin_that_never_crashes_the_host_even_when_it_crashes_at_startup() {
    let e = env("crash_init", WRITES);
    let h = host(&e);
    for _ in 0..5 {
        assert!(h.query(ID, "touch", "touch", "", &no_files).is_err());
    }
    assert!(!h.is_running(ID));
    assert!(h.last_error(ID).is_some());
}

#[test]
fn turning_a_failing_plugin_off_and_on_clears_its_backoff() {
    let e = env("crash_init", WRITES);
    let h = host(&e);
    for _ in 0..5 {
        assert!(h.query(ID, "touch", "touch", "", &no_files).is_err());
    }
    assert!(matches!(
        h.query(ID, "touch", "touch", "", &no_files),
        Err(HostError::Backoff)
    ));
    h.set_enabled(ID, false, false).unwrap();
    h.set_enabled(ID, true, true).unwrap();
    assert!(
        !matches!(
            h.query(ID, "touch", "touch", "", &no_files),
            Err(HostError::Backoff)
        ),
        "still waiting out the old failures after the user turned it back on"
    );
}

#[test]
fn a_plugin_that_hangs_costs_one_timeout_and_is_killed() {
    let e = env("hang", WRITES);
    let h = host(&e);
    // Prime the pid: initialize works, the query hangs.
    let started = Instant::now();
    let err = h.query(ID, "touch", "touch", "", &no_files).unwrap_err();
    assert!(matches!(err, HostError::Wire(_)), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert!(!h.is_running(ID));
    assert!(h.pid(ID).is_none());
}

#[test]
fn a_hanging_initialize_is_bounded_too() {
    let e = env("hang_init", WRITES);
    let mut c = config(&e);
    c.timeouts.init = Duration::from_millis(500);
    let h = PluginHost::start(c, PauseState::new());
    h.set_enabled(ID, true, true).unwrap();
    let started = Instant::now();
    assert!(h.query(ID, "touch", "touch", "", &no_files).is_err());
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(!h.is_running(ID));
}

#[test]
fn garbage_and_oversized_output_get_the_plugin_killed() {
    for mode in ["garbage", "huge"] {
        let e = env(mode, WRITES);
        let h = host(&e);
        let err = h.query(ID, "touch", "touch", "", &no_files).unwrap_err();
        assert!(matches!(err, HostError::Wire(_)), "{mode}: {err:?}");
        assert!(!h.is_running(ID), "{mode}");
    }
}

#[test]
fn a_plugin_may_only_return_paths_the_host_gave_it() {
    let e = env("foreign_path", WRITES);
    let h = host(&e);
    assert!(h
        .query(ID, "touch", "touch", "", &no_files)
        .unwrap()
        .is_empty());

    let e = env("select", WRITES);
    let h = host(&e);
    let picked = e.dir.path().join("picked.txt");
    fs::write(&picked, "x").unwrap();
    let path = picked.to_string_lossy().into_owned();
    let items = h
        .query(ID, "touch picked", "touch", "picked", &|_| {
            vec![Candidate {
                path: path.clone(),
                is_folder: false,
            }]
        })
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].uri.as_deref(), Some(path.as_str()));
}

#[test]
fn the_job_object_stops_a_plugin_from_starting_programs_unless_allowed() {
    for (perms, expected) in [(WRITES, "refused"), (r#"{"spawn": true}"#, "ok")] {
        let e = env("info", perms);
        let h = host(&e);
        let items = h.query(ID, "touch", "touch", "", &no_files).unwrap();
        let title = items[0].title.clone();
        assert!(
            title.ends_with(&format!("child={expected}")),
            "{perms}: {title}"
        );
        let pid = h.pid(ID).unwrap();
        assert!(pid_alive(pid));
        h.shutdown();
        wait_until("the plugin process to end", || !pid_alive(pid));
    }
}

#[test]
fn turning_on_in_one_step_starts_a_background_plugin_or_leaves_it_off() {
    for (mode, starts) in [("good", true), ("crash_init", false)] {
        let e = env(mode, WRITES);
        let path = e.dir.path().join("plugins").join("p").join("plugin.json");
        let mut m: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        m["background"] = true.into();
        fs::write(&path, m.to_string()).unwrap();
        let h = PluginHost::start(config(&e), PauseState::new());
        assert_eq!(h.turn_on(ID).is_ok(), starts, "{mode}");
        assert_eq!(h.is_usable(ID), starts, "{mode}");
        assert_eq!(h.is_running(ID), starts, "{mode}");
    }
}

#[test]
fn a_disabled_or_unapproved_plugin_is_never_started() {
    let e = env("good", WRITES);
    let h = PluginHost::start(config(&e), PauseState::new());
    assert!(matches!(
        h.set_enabled(ID, true, false),
        Err(HostError::NotApproved)
    ));
    assert!(h.query(ID, "touch", "touch", "", &no_files).is_err());
    assert!(!h.is_running(ID));
    h.set_enabled(ID, true, true).unwrap();
    assert_eq!(
        h.query(ID, "touch", "touch", "", &no_files).unwrap().len(),
        1
    );
    assert!(h.is_running(ID));
    let pid = h.pid(ID).unwrap();
    h.set_enabled(ID, false, false).unwrap();
    assert!(!h.is_running(ID));
    wait_until("the plugin process to end", || !pid_alive(pid));
    assert!(h.query(ID, "touch", "touch", "", &no_files).is_err());
    // The approval is remembered across a restart of the host, so it asks once.
    drop(h);
    let again = PluginHost::start(config(&e), PauseState::new());
    again.set_enabled(ID, true, false).unwrap();
}

#[test]
fn an_idle_plugin_is_stopped() {
    let e = env("good", WRITES);
    let mut c = config(&e);
    c.idle_timeout = Duration::from_millis(300);
    let h = PluginHost::start(c, PauseState::new());
    h.set_enabled(ID, true, true).unwrap();
    h.query(ID, "touch", "touch", "", &no_files).unwrap();
    assert!(h.is_running(ID));
    wait_until("the idle plugin to stop", || !h.is_running(ID));
    // And it comes back on the next use.
    assert_eq!(
        h.query(ID, "touch", "touch", "", &no_files).unwrap().len(),
        1
    );
}

// ----------------------------------------------------------- the gate

struct App {
    lookout: Lookout,
    updates: Arc<Mutex<Vec<Update>>>,
    next: u64,
}

fn app(e: &Env) -> App {
    let updates: Arc<Mutex<Vec<Update>>> = Arc::default();
    let u = updates.clone();
    let sink: Sink = Arc::new(move |update| u.lock().unwrap().push(update));
    let lookout = Lookout::start(
        StartOptions {
            settings: Settings {
                index_contents: false,
                ocr: false,
                first_run_done: true,
                ..Settings::default()
            },
            roots_override: vec![],
            synthetic_files: None,
            synthetic_uses_folders: false,
            start_menu_dirs: vec![],
            store_apps: false,
            windows: None,
            storage: Storage::in_memory().unwrap(),
            idle: Arc::new(lookout_core::content::idle::AlwaysIdle),
            ocr: Arc::new(lookout_core::content::ocr::NoOcr),
            launcher: Arc::new(NoopLauncher),
            system: None,
            gate: GateConfig {
                min_dwell: Duration::from_millis(60),
                ..GateConfig::default()
            },
            plugins: Some(config(e)),
        },
        sink,
        PauseState::new(),
    )
    .unwrap();
    lookout
        .plugins()
        .unwrap()
        .set_enabled(ID, true, true)
        .unwrap();
    App {
        lookout,
        updates,
        next: 0,
    }
}

impl App {
    fn ask(&mut self, text: &str) -> Update {
        self.next += 1;
        let generation = self.next;
        assert!(self.lookout.query(generation, text));
        let end = Instant::now() + Duration::from_secs(10);
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
            assert!(Instant::now() < end, "query never completed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The plugin's result and the action it names.
    fn plugin_result(&mut self) -> (String, String) {
        let r = self.ask("touch");
        let item = r
            .items
            .iter()
            .find(|i| i.source.starts_with("plugin:"))
            .expect("no plugin result");
        let action = item.metadata["default_action"].as_str().unwrap().to_owned();
        (item.id.clone(), action)
    }
}

#[test]
fn a_plugin_cannot_run_an_action_without_the_confirm_step_and_cannot_lower_its_risk() {
    let e = env("rogue", WRITES);
    let mut app = app(&e);
    let (id, action) = app.plugin_result();
    // The rogue plugin sent `execute` requests and notifications to the host.
    assert!(app.lookout.plugins().unwrap().violations(ID) >= 3);
    assert!(!e.marker.exists(), "the plugin executed by itself");

    // It asked for `safe`; its permissions (fs_write) make it `confirm`.
    let prepared = app.lookout.prepare_action(&action, &id).unwrap();
    assert_eq!(prepared.risk, Risk::Confirm);
    // The card says where the text comes from, in words the plugin did not write.
    assert_eq!(
        prepared.preview.from.as_deref(),
        Some("From add-on: Test plugin")
    );
    assert!(!e.marker.exists());

    assert!(matches!(
        app.lookout.run_action(&prepared.ticket),
        Err(ActionError::ConfirmationRequired)
    ));
    assert!(matches!(
        app.lookout.confirm_action(&prepared.ticket),
        Err(ActionError::NotPresented)
    ));
    app.lookout.present_action(&prepared.ticket).unwrap();
    assert!(matches!(
        app.lookout.confirm_action(&prepared.ticket),
        Err(ActionError::TooSoon)
    ));
    assert!(!e.marker.exists(), "ran without a valid confirmation");

    std::thread::sleep(Duration::from_millis(120));
    let done = app.lookout.confirm_action(&prepared.ticket).unwrap();
    assert!(done.undoable);
    assert!(e.marker.exists());
    // A used ticket is dead.
    assert!(app.lookout.confirm_action(&prepared.ticket).is_err());

    // Undo goes through the gate as well.
    let undo = app.lookout.prepare_undo().unwrap();
    assert!(undo.is_undo);
    assert_eq!(
        undo.preview.from.as_deref(),
        Some("From add-on: Test plugin")
    );
    assert!(e.marker.exists());
    app.lookout.present_action(&undo.ticket).unwrap();
    std::thread::sleep(Duration::from_millis(120));
    app.lookout.confirm_action(&undo.ticket).unwrap();
    assert!(!e.marker.exists());
}

#[test]
fn the_webview_cannot_point_a_plugin_action_at_other_things() {
    let e = env("good", WRITES);
    let mut app = app(&e);
    let (id, _) = app.plugin_result();
    for other in [
        "plugin/test.plugin/other",
        "plugin/nobody/touch",
        "open",
        "kill_port",
    ] {
        let r = app.lookout.prepare_action(other, &id);
        assert!(r.is_err(), "{other}");
    }
    assert!(!e.marker.exists());
}

#[test]
fn a_plugin_that_asks_for_safe_without_write_permissions_runs_in_one_step() {
    let e = env("good", "{}");
    let mut app = app(&e);
    let (id, action) = app.plugin_result();
    let prepared = app.lookout.prepare_action(&action, &id).unwrap();
    assert_eq!(prepared.risk, Risk::Safe);
    app.lookout.run_action(&prepared.ticket).unwrap();
    assert!(e.marker.exists());
}

#[test]
fn a_plugin_that_changes_its_preview_makes_the_confirmation_stale() {
    // The gate plans again before running; a different preview means nothing runs.
    // (Here the permissions change underneath the ticket instead: disabling the
    // plugin between preview and confirm must stop the action.)
    let e = env("good", WRITES);
    let mut app = app(&e);
    let (id, action) = app.plugin_result();
    let prepared = app.lookout.prepare_action(&action, &id).unwrap();
    app.lookout.present_action(&prepared.ticket).unwrap();
    app.lookout
        .plugins()
        .unwrap()
        .set_enabled(ID, false, false)
        .unwrap();
    std::thread::sleep(Duration::from_millis(120));
    assert!(app.lookout.confirm_action(&prepared.ticket).is_err());
    assert!(!e.marker.exists());
}

#[test]
fn a_hanging_plugin_does_not_stop_the_other_results_or_the_core() {
    let e = env("hang", WRITES);
    let mut app = app(&e);
    let started = Instant::now();
    let r = app.ask("touch");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(r.items.iter().all(|i| !i.source.starts_with("plugin:")));
    // The core still answers.
    let calc = app.ask("2+2");
    assert!(calc.items.iter().any(|i| i.title == "4"));
}

// ------------------------------------------------------------ index API

mod index_api {
    use super::*;
    use lookout_core::semantic::SemanticClient;

    fn client(e: &Env, memory: Option<usize>) -> (Arc<PluginHost>, SemanticClient) {
        let mut c = config(e);
        c.timeouts.query = Duration::from_millis(700);
        if let Some(m) = memory {
            c.memory_limit = m;
        }
        let h = PluginHost::start(c, PauseState::new());
        h.set_enabled(ID, true, true).unwrap();
        let cl = SemanticClient::new(h.clone(), ID, &e.dir.path().join("data"));
        (h, cl)
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn the_index_methods_work_and_nothing_else_can_be_sent() {
        let e = env("index_ok", "{}");
        let (h, cl) = client(&e, None);
        let hits = cl.search("two words", 5, &never).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(!cl.status(&never).unwrap().ready);
        let err = h
            .service_call(
                ID,
                "execute",
                serde_json::json!({}),
                Duration::from_secs(1),
                &never,
            )
            .unwrap_err();
        assert!(matches!(err, HostError::NotAllowed(_)), "{err}");
    }

    #[test]
    fn a_helper_that_crashes_hangs_or_blows_the_memory_cap_costs_one_call() {
        for (mode, memory) in [
            ("index_crash", None),
            ("index_hang", None),
            ("index_hog", Some(64 << 20)),
        ] {
            let e = env(mode, "{}");
            let (h, cl) = client(&e, memory);
            let started = Instant::now();
            assert!(cl.search("two words", 5, &never).is_err(), "{mode}");
            // The hang is bounded by the call's timeout (8 s for a search), not forever.
            assert!(started.elapsed() < Duration::from_secs(12), "{mode}");
            // The core still answers, and the next use starts a fresh process.
            let again = cl.status(&never);
            assert!(again.is_ok(), "{mode}: {again:?}");
            h.shutdown();
        }
    }

    #[test]
    fn a_turned_off_helper_is_never_started_and_the_core_keeps_answering() {
        let e = env("index_ok", "{}");
        let (h, cl) = client(&e, None);
        h.set_enabled(ID, false, false).unwrap();
        assert!(matches!(
            cl.search("two words", 5, &never),
            Err(lookout_core::semantic::SemanticError::Host(
                HostError::Disabled
            ))
        ));
        assert!(h.pid(ID).is_none());
    }
}

#[test]
fn turning_a_busy_plugin_off_never_blocks_the_list_and_finishes_in_bounded_time() {
    let e = env("hang", WRITES);
    let h = host(&e);
    let busy = {
        let h = h.clone();
        std::thread::spawn(move || h.query(ID, "touch", "touch", "", &no_files))
    };
    std::thread::sleep(Duration::from_millis(300)); // the query is now hanging, lock held
    let t = Instant::now();
    let _ = h.list();
    assert!(
        t.elapsed() < Duration::from_millis(250),
        "list() waited {:?}",
        t.elapsed()
    );
    let t = Instant::now();
    h.set_enabled(ID, false, false).unwrap();
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "turning off took {:?}",
        t.elapsed()
    );
    let _ = busy.join();
    assert!(!h.is_running(ID));
}

#[test]
fn add_on_state_survives_a_damaged_file_and_a_failed_save_keeps_the_old_one() {
    let e = env("crash_init", WRITES);
    let h = host(&e);
    let state = e.dir.path().join("data").join("plugins.json");
    assert!(h.list()[0].enabled);
    drop(h);

    // A damaged main file falls back to the last good copy.
    fs::write(&state, "{ half a fi").unwrap();
    let h = PluginHost::start(config(&e), PauseState::new());
    assert!(h.list()[0].enabled, "the .bak copy was not used");

    // A save that cannot finish leaves the file as it was.
    fs::write(&state, fs::read(state.with_extension("json.bak")).unwrap()).unwrap();
    let before = fs::read(&state).unwrap();
    fs::create_dir(state.with_extension("json.tmp")).unwrap();
    assert!(h.set_enabled(ID, false, false).is_err());
    assert_eq!(fs::read(&state).unwrap(), before);
}
