//! End-to-end tests of `kill port` against a real, harmless process.
//!
//! The victim is `examples/test_listener`, built by cargo for this test: it
//! only listens on a loopback port. Nothing else on the machine is touched:
//! every kill targets a pid this test started (or the restart of it), found
//! through the port the test picked.

// Test code: a failed setup step should fail the test loudly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lookout_core::actions::open::{Launcher, NoopLauncher};
use lookout_core::actions::system::{ProcessBackend, WindowsSystem};
use lookout_core::actions::undo_log::Status;
use lookout_core::actions::{ActionError, GateConfig, Risk};
use lookout_core::engine::{Sink, Update};
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::settings::Settings;

/// Builds `examples/test_listener` (a no-op when it is current) and returns the
/// path cargo reports for it, so no assumption is made about the target layout.
fn listener_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args([
                "build",
                "-p",
                "lookout-core",
                "--example",
                "test_listener",
                "--message-format=json",
            ])
            .stderr(Stdio::inherit())
            .output()
            .unwrap();
        assert!(out.status.success(), "building test_listener failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| {
                v["reason"] == "compiler-artifact" && v["target"]["name"] == "test_listener"
            })
            .find_map(|v| v["executable"].as_str().map(PathBuf::from))
            .expect("cargo did not report the test_listener executable")
    })
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn listening(port: u16) -> bool {
    !WindowsSystem.listeners(port).unwrap().is_empty()
}

/// Starts the harmless listener in `cwd` and waits until it is listening.
fn spawn_listener(port: u16, cwd: &Path) -> Child {
    let exe = listener_exe();
    let child = Command::new(exe)
        .arg(port.to_string())
        .arg("label with spaces")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the listener to listen", 15, || listening(port));
    child
}

/// Stops any test listener left on `port`, even when a test failed halfway.
struct Cleanup {
    port: u16,
    children: Vec<Child>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
        // The restarted copy is not a child of ours; stop it by port, but only
        // if it is the test listener.
        for pid in WindowsSystem.listeners(self.port).unwrap_or_default() {
            if let Some(info) = WindowsSystem.inspect(pid) {
                if info.name.eq_ignore_ascii_case("test_listener.exe") {
                    let _ = WindowsSystem.terminate(&info);
                }
            }
        }
    }
}

struct Recorder(Mutex<Vec<PathBuf>>);
impl Launcher for Recorder {
    fn open(&self, path: &Path) -> std::io::Result<()> {
        self.0.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

struct App {
    lookout: Lookout,
    updates: Arc<Mutex<Vec<Update>>>,
}

fn start(dir: &Path, launcher: Arc<dyn Launcher>) -> App {
    let updates: Arc<Mutex<Vec<Update>>> = Arc::default();
    let u = updates.clone();
    let sink: Sink = Arc::new(move |update| u.lock().unwrap().push(update));
    let lookout = Lookout::start(
        StartOptions {
            settings: Settings::default(),
            roots_override: vec![dir.join("files")],
            synthetic_files: None,
            synthetic_uses_folders: false,
            start_menu_dirs: vec![],
            storage: lookout_core::storage::Storage::with_key(
                lookout_core::storage::DataPaths::new(dir),
                lookout_core::crypto::Key::from_bytes(&[9u8; 32]).unwrap(),
            )
            .unwrap(),
            idle: Arc::new(lookout_core::content::idle::AlwaysIdle),
            ocr: Arc::new(lookout_core::content::ocr::NoOcr),
            launcher,
            system: None,
            gate: GateConfig {
                min_dwell: Duration::from_millis(60),
                ..GateConfig::default()
            },
            plugins: None,
        },
        sink,
        PauseState::new(),
    )
    .unwrap();
    App { lookout, updates }
}

impl App {
    fn ask(&self, generation: u64, text: &str) -> Update {
        assert!(self.lookout.query(generation, text));
        let mut found = None;
        wait_until("query results", 10, || {
            found = self
                .updates
                .lock()
                .unwrap()
                .iter()
                .find(|u| u.generation == generation && u.done)
                .cloned();
            found.is_some()
        });
        found.unwrap()
    }
}

fn pause_for_dwell() {
    std::thread::sleep(Duration::from_millis(120));
}

#[test]
fn kill_port_end_to_end_with_confirmation_and_restart_undo_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("files")).unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let port = free_port();
    let child = spawn_listener(port, workdir.path());
    let victim_pid = child.id();
    let mut cleanup = Cleanup {
        port,
        children: vec![child],
    };

    let app = start(dir.path(), Arc::new(NoopLauncher));
    // 1. The typed request finds the real process on that port.
    let update = app.ask(1, &format!("kill port {port}"));
    let item = update
        .items
        .iter()
        .find(|i| i.id == format!("port:{port}"))
        .expect("port result");
    assert!(item
        .subtitle
        .as_deref()
        .unwrap()
        .contains("test_listener.exe"));
    assert_eq!(update.items[0].id, item.id, "the port result leads");

    // 2. The preview is built from that real process.
    let prepared = app.lookout.prepare_action("kill_port", &item.id).unwrap();
    assert_eq!(prepared.risk, Risk::Confirm);
    assert!(prepared.undoable);
    let text = prepared.preview.lines.join("\n");
    assert!(text.contains(&format!("PID {victim_pid}")), "{text}");
    assert!(text.contains("test_listener.exe"), "{text}");
    let folder = workdir.path().file_name().unwrap().to_string_lossy();
    assert!(text.contains(&*folder), "working folder missing: {text}");
    assert!(text.contains("label with spaces"), "{text}");

    // 3. Every way around the confirm step fails and the process lives.
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
    assert!(matches!(
        app.lookout.prepare_action("kill_port", "port:1"),
        Err(ActionError::Stale(_))
    ));
    assert!(matches!(
        app.lookout.prepare_action("open", &item.id),
        Err(ActionError::NotApplicable { .. })
    ));
    assert!(listening(port), "the process must still be running");
    assert!(cleanup.children[0].try_wait().unwrap().is_none());
    assert!(app.lookout.action_history(10).unwrap().is_empty());

    // 4. Confirmed: it is stopped, logged, and undoable.
    pause_for_dwell();
    let done = app.lookout.confirm_action(&prepared.ticket).unwrap();
    assert!(done.undoable && !done.was_undo);
    assert!(
        done.message.contains(&format!("PID {victim_pid}")),
        "{}",
        done.message
    );
    assert!(!listening(port));
    cleanup.children[0].wait().unwrap();
    let history = app.lookout.action_history(10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, Status::Executed);
    assert!(history[0].undo_token.is_some());
    assert!(matches!(
        app.lookout.confirm_action(&prepared.ticket),
        Err(ActionError::UnknownTicket)
    ));
    // Nothing is listening any more: a new preview says so instead of guessing.
    let update = app.ask(2, &format!("kill port {port}"));
    assert!(matches!(
        app.lookout.prepare_action("kill_port", &update.items[0].id),
        Err(ActionError::NothingToDo(_))
    ));

    // 5. "Restart Lookout": a new instance reads the same log.
    drop(app);
    let app = start(dir.path(), Arc::new(NoopLauncher));
    assert!(app.lookout.can_undo());
    let undo = app.lookout.prepare_undo().unwrap();
    assert!(undo.is_undo);
    assert_eq!(undo.risk, Risk::Confirm);
    let text = undo.preview.lines.join("\n");
    assert!(
        text.contains("test_listener.exe") && text.contains(&*folder),
        "{text}"
    );
    assert!(matches!(
        app.lookout.run_action(&undo.ticket),
        Err(ActionError::ConfirmationRequired)
    ));
    assert!(!listening(port), "undo must not run unconfirmed");
    app.lookout.present_action(&undo.ticket).unwrap();
    pause_for_dwell();
    let done = app.lookout.confirm_action(&undo.ticket).unwrap();
    assert!(done.was_undo);

    // 6. The program is running again, on the same port, in the same folder.
    assert!(listening(port), "{}", done.message);
    let pid = WindowsSystem.listeners(port).unwrap()[0];
    assert_ne!(pid, victim_pid);
    let again = WindowsSystem.inspect(pid).unwrap();
    assert_eq!(again.name.to_lowercase(), "test_listener.exe");
    assert_eq!(
        again.cmd.unwrap()[1..],
        [port.to_string(), "label with spaces".to_string()]
    );
    assert!(again.cwd.unwrap().contains(&*folder));
    assert_eq!(
        app.lookout.action_history(1).unwrap()[0].status,
        Status::Undone
    );
    assert!(!app.lookout.can_undo());
    assert!(matches!(
        app.lookout.prepare_undo(),
        Err(ActionError::NothingToUndo)
    ));
}

#[test]
fn a_confirmation_cannot_stop_a_different_process_that_took_the_port() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("files")).unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let port = free_port();
    let first = spawn_listener(port, workdir.path());
    let mut cleanup = Cleanup {
        port,
        children: vec![first],
    };
    let app = start(dir.path(), Arc::new(NoopLauncher));
    let update = app.ask(1, &format!("kill {port}"));
    let prepared = app
        .lookout
        .prepare_action("kill_port", &update.items[0].id)
        .unwrap();
    app.lookout.present_action(&prepared.ticket).unwrap();

    // Between the preview and the confirmation the program is replaced.
    cleanup.children[0].kill().unwrap();
    cleanup.children[0].wait().unwrap();
    wait_until("the port to free up", 10, || !listening(port));
    cleanup.children.push(spawn_listener(port, workdir.path()));
    let second_pid = cleanup.children[1].id();

    pause_for_dwell();
    let r = app.lookout.confirm_action(&prepared.ticket);
    assert!(matches!(r, Err(ActionError::Stale(_))), "{r:?}");
    assert!(
        listening(port),
        "the new process must not have been stopped"
    );
    assert!(cleanup.children[1].try_wait().unwrap().is_none());
    assert_eq!(WindowsSystem.listeners(port).unwrap(), [second_pid]);
    assert!(app.lookout.action_history(10).unwrap().is_empty());
}

#[test]
fn opening_files_goes_through_the_gate_by_risk() {
    let dir = tempfile::tempdir().unwrap();
    let files = dir.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    for n in ["notes alpha.txt", "tool alpha.exe", "run alpha.bat"] {
        std::fs::write(files.join(n), b"x").unwrap();
    }
    let rec = Arc::new(Recorder(Mutex::default()));
    let app = start(dir.path(), rec.clone());
    wait_until("indexing", 10, || !app.lookout.status().indexing);
    let update = app.ask(1, "alpha");
    let find = |name: &str| {
        update
            .items
            .iter()
            .find(|i| i.title == name)
            .unwrap()
            .id
            .clone()
    };

    // A document opens with one step.
    let doc = app
        .lookout
        .prepare_action("open", &find("notes alpha.txt"))
        .unwrap();
    assert_eq!(doc.risk, Risk::Safe);
    app.lookout.run_action(&doc.ticket).unwrap();
    assert_eq!(rec.0.lock().unwrap().len(), 1);

    // Executable types cannot be opened without confirming, by any route.
    for name in ["tool alpha.exe", "run alpha.bat"] {
        let p = app.lookout.prepare_action("open", &find(name)).unwrap();
        assert_eq!(p.risk, Risk::Confirm, "{name}");
        assert!(matches!(
            app.lookout.run_action(&p.ticket),
            Err(ActionError::ConfirmationRequired)
        ));
        assert!(matches!(
            app.lookout.confirm_action(&p.ticket),
            Err(ActionError::NotPresented)
        ));
        app.lookout.present_action(&p.ticket).unwrap();
        assert!(matches!(
            app.lookout.confirm_action(&p.ticket),
            Err(ActionError::TooSoon)
        ));
        assert_eq!(
            rec.0.lock().unwrap().len(),
            1,
            "{name} must not have opened"
        );
    }
    // Confirmed once, it opens.
    let p = app
        .lookout
        .prepare_action("open", &find("tool alpha.exe"))
        .unwrap();
    app.lookout.present_action(&p.ticket).unwrap();
    pause_for_dwell();
    app.lookout.confirm_action(&p.ticket).unwrap();
    assert_eq!(rec.0.lock().unwrap().len(), 2);
    assert!(rec.0.lock().unwrap()[1].ends_with("tool alpha.exe"));
    // Every execution is in the log, none of them undoable.
    let h = app.lookout.action_history(10).unwrap();
    assert_eq!(h.len(), 2);
    assert!(h
        .iter()
        .all(|e| e.action_id == "open" && e.undo_token.is_none()));
}

/// Read-only: plans (never confirms, never executes) `kill port` against a
/// real listener owned by the System process or a core Windows service
/// process, and expects a refusal.
#[test]
fn a_real_windows_system_listener_is_refused_at_plan_time() {
    use lookout_core::actions::kill_port::KillPort;
    use lookout_core::actions::{Action, Target};
    use lookout_core::item::Kind;

    let out = Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let sys = Arc::new(WindowsSystem);
    let mut found = None;
    for line in text.lines().filter(|l| l.contains("LISTENING")) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (Some(addr), Some(pid)) = (cols.get(1), cols.last()) else {
            continue;
        };
        let (Some(port), Ok(pid)) = (
            addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()),
            pid.parse::<u32>(),
        ) else {
            continue;
        };
        let protected = pid == 4
            || sys.inspect(pid).is_some_and(|p| {
                ["lsass.exe", "services.exe", "wininit.exe", "smss.exe"]
                    .contains(&p.name.to_lowercase().as_str())
            });
        if protected {
            found = Some((port, pid));
            break;
        }
    }
    let Some((port, pid)) = found else {
        eprintln!(
            "SKIPPED: no listener owned by System, lsass, services or wininit on this machine"
        );
        return;
    };
    eprintln!("planning kill-port against port {port} owned by PID {pid} (plan only)");
    let action = KillPort::new(sys);
    let target = Target {
        item_id: format!("port:{port}"),
        kind: Kind::Port,
        uri: None,
        port: Some(port),
        origin: None,
    };
    let r = action.plan(&target);
    assert!(matches!(r, Err(ActionError::Refused(_))), "{r:?}");
}

#[test]
fn the_program_exiting_before_the_confirmation_gives_a_clear_answer_and_stops_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("files")).unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let port = free_port();
    let first = spawn_listener(port, workdir.path());
    let mut cleanup = Cleanup {
        port,
        children: vec![first],
    };
    let app = start(dir.path(), Arc::new(NoopLauncher));
    let update = app.ask(1, &format!("kill {port}"));
    let prepared = app
        .lookout
        .prepare_action("kill_port", &update.items[0].id)
        .unwrap();
    app.lookout.present_action(&prepared.ticket).unwrap();

    // The program stops on its own while the confirmation card is open.
    cleanup.children[0].kill().unwrap();
    cleanup.children[0].wait().unwrap();
    wait_until("the port to free up", 10, || !listening(port));

    pause_for_dwell();
    let r = app.lookout.confirm_action(&prepared.ticket);
    eprintln!("exited-before-confirm result: {r:?}");
    let msg = match &r {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(
        r.is_err(),
        "nothing was running, nothing can have been stopped"
    );
    assert!(
        msg.to_lowercase().contains("nothing")
            || msg.to_lowercase().contains("changed")
            || msg.to_lowercase().contains("no longer"),
        "unclear message: {msg}"
    );
    assert!(app.lookout.action_history(10).unwrap().is_empty());
}
