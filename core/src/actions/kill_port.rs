//! `kill port 3000`: stop the process that is listening on a TCP port.
//!
//! The preview is built from the process found at that moment (pid, program,
//! command line, working folder). The confirmed run re-checks that exactly
//! those processes still own the port and are still the same processes. The
//! undo token holds what is needed to start them again, but only for
//! processes whose command line and working folder were both captured.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::system::{ProcInfo, ProcessBackend, SpawnSpec};
use super::{Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target, UndoPlan};
use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::sources::{QueryContext, Source, Tier};

pub const ID: &str = "kill_port";
/// Ranks below calculator answers (100) and above any file match.
pub const PORT_SCORE: f32 = 95.0;

/// Programs whose termination can bring Windows down. They are refused, not
/// just flagged, whatever the user confirms.
const NEVER_KILL: &[&str] = &[
    "system",
    "registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "fontdrvhost.exe",
    "dwm.exe",
];

const FREE_WAIT: Duration = Duration::from_secs(3);
const LISTEN_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Snapshot {
    port: u16,
    procs: Vec<ProcInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UndoToken {
    port: u16,
    restart: Vec<SpawnSpec>,
}

pub struct KillPort {
    system: Arc<dyn ProcessBackend>,
}

impl KillPort {
    pub fn new(system: Arc<dyn ProcessBackend>) -> Self {
        Self { system }
    }

    fn refusal(&self, p: &ProcInfo) -> Option<String> {
        let lower = p.name.to_lowercase();
        if p.pid == 0 || p.pid == 4 || NEVER_KILL.contains(&lower.as_str()) {
            return Some(format!(
                "{} (PID {}) is part of Windows; stopping it could crash the computer, so Lookout will not",
                p.name, p.pid
            ));
        }
        // Programs Lookout restarted for an undo are its children and stay stoppable;
        // only its own window process tree is protected.
        let own_webview = p.parent == Some(self.system.own_pid()) && lower == "msedgewebview2.exe";
        if p.pid == self.system.own_pid() || own_webview {
            return Some(format!(
                "{} (PID {}) belongs to Lookout itself",
                p.name, p.pid
            ));
        }
        None
    }

    fn in_system_folder(&self, p: &ProcInfo) -> bool {
        let root = self.system.system_root().to_string_lossy().to_lowercase();
        p.exe
            .as_ref()
            .is_some_and(|e| e.to_lowercase().starts_with(&root))
    }
}

/// Command line for display, quoting arguments that contain spaces.
fn display_command(cmd: &[String]) -> String {
    cmd.iter()
        .map(|a| {
            if a.is_empty() || a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn restart_spec(p: &ProcInfo) -> Option<SpawnSpec> {
    let (exe, cmd, cwd) = (p.exe.as_ref()?, p.cmd.as_ref()?, p.cwd.as_ref()?);
    Some(SpawnSpec {
        exe: exe.clone(),
        args: cmd.iter().skip(1).cloned().collect(),
        cwd: cwd.clone(),
    })
}

impl Action for KillPort {
    fn id(&self) -> &'static str {
        ID
    }

    fn label(&self) -> &'static str {
        "Stop the program using this port"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        *kind == Kind::Port
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let port = target
            .port
            .ok_or_else(|| ActionError::Refused("that result has no port".into()))?;
        let pids = self
            .system
            .listeners(port)
            .map_err(|e| ActionError::Failed(e.to_string()))?;
        let procs: Vec<ProcInfo> = pids
            .into_iter()
            .filter_map(|pid| self.system.inspect(pid))
            .collect();
        if procs.is_empty() {
            return Err(ActionError::NothingToDo(format!(
                "Nothing is listening on port {port}"
            )));
        }
        if let Some(reason) = procs.iter().find_map(|p| self.refusal(p)) {
            return Err(ActionError::Refused(reason));
        }

        let system_program = procs.iter().any(|p| self.in_system_folder(p));
        let unreadable = procs.iter().any(|p| p.exe.is_none());
        let risk = if system_program || unreadable {
            Risk::Dangerous
        } else {
            Risk::Confirm
        };

        let title = match procs.as_slice() {
            [p] => format!("Stop {} (PID {}) using port {port}", p.name, p.pid),
            many => format!("Stop {} programs using port {port}", many.len()),
        };
        let mut lines = Vec::new();
        for p in &procs {
            lines.push(format!("{} (PID {})", p.name, p.pid));
            lines.push(format!(
                "  Program: {}",
                p.exe.as_deref().unwrap_or("unknown (could not be read)")
            ));
            if let Some(cmd) = &p.cmd {
                lines.push(format!("  Command: {}", display_command(cmd)));
            }
            if let Some(cwd) = &p.cwd {
                lines.push(format!("  Folder: {cwd}"));
            }
        }
        let mut warnings = vec!["Unsaved work in the program will be lost.".to_owned()];
        if system_program {
            warnings.push(
                "This is a program from the Windows folder. Other things may depend on it.".into(),
            );
        }
        if unreadable {
            warnings.push(
                "Lookout could not read this program's details; it may belong to another user or need administrator rights.".into(),
            );
        }
        let restartable = procs.iter().filter(|p| restart_spec(p).is_some()).count();
        let undoable = restartable > 0;
        if undoable {
            lines.push(if restartable == procs.len() {
                "Undo: Lookout can start it again with the same command and folder.".to_owned()
            } else {
                format!(
                    "Undo: only {restartable} of {} can be started again.",
                    procs.len()
                )
            });
        } else {
            warnings.push(
                "This cannot be undone: its command line or folder could not be read.".into(),
            );
        }

        let fingerprint = {
            let mut s = format!("port={port}");
            for p in &procs {
                s.push_str(&format!(
                    "|{}:{}:{}:{}",
                    p.pid,
                    p.created,
                    p.exe.as_deref().unwrap_or(""),
                    p.cmd
                        .as_ref()
                        .map(|c| display_command(c))
                        .unwrap_or_default()
                ));
            }
            s
        };
        let snapshot = serde_json::to_value(Snapshot { port, procs })
            .map_err(|e| ActionError::Failed(e.to_string()))?;
        Ok(Plan {
            risk,
            preview: Preview {
                from: None,
                title,
                lines,
                warnings,
            },
            undoable,
            snapshot,
            fingerprint,
        })
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let snap: Snapshot = serde_json::from_value(plan.snapshot.clone())
            .map_err(|e| ActionError::Failed(format!("bad plan: {e}")))?;
        let mut stopped: Vec<&ProcInfo> = Vec::new();
        let mut problems: Vec<String> = Vec::new();
        for p in &snap.procs {
            match self.system.terminate(p) {
                Ok(()) => stopped.push(p),
                Err(e) => problems.push(e.to_string()),
            }
        }
        if stopped.is_empty() {
            return Err(ActionError::Failed(problems.join("; ")));
        }

        let deadline = Instant::now() + FREE_WAIT;
        let freed = loop {
            let still = self.system.listeners(snap.port).unwrap_or_default();
            if !stopped.iter().any(|p| still.contains(&p.pid)) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        let names: Vec<String> = stopped
            .iter()
            .map(|p| format!("{} (PID {})", p.name, p.pid))
            .collect();
        let mut message = format!("Stopped {}.", names.join(", "));
        message.push_str(&if freed {
            format!(" Port {} is free.", snap.port)
        } else {
            format!(" Port {} is still listed as in use.", snap.port)
        });
        for pr in &problems {
            message.push_str(&format!(" Not stopped: {pr}."));
        }

        let restart: Vec<SpawnSpec> = stopped.iter().filter_map(|p| restart_spec(p)).collect();
        let undo_token = if restart.is_empty() {
            None
        } else {
            serde_json::to_string(&UndoToken {
                port: snap.port,
                restart,
            })
            .ok()
        };
        Ok(Executed {
            message,
            undo_token,
        })
    }

    fn plan_undo(&self, token: &str) -> Result<UndoPlan, ActionError> {
        let t: UndoToken = serde_json::from_str(token)
            .map_err(|_| ActionError::Failed("the undo information is damaged".into()))?;
        let in_use = self
            .system
            .listeners(t.port)
            .map_err(|e| ActionError::Failed(e.to_string()))?;
        if !in_use.is_empty() {
            let who = in_use
                .iter()
                .map(|pid| {
                    self.system
                        .image_name(*pid)
                        .map_or_else(|| format!("PID {pid}"), |n| format!("{n} (PID {pid})"))
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ActionError::Refused(format!(
                "Port {} is in use again by {who}, so the program cannot be restarted on it",
                t.port
            )));
        }
        let mut lines = Vec::new();
        for s in &t.restart {
            if !Path::new(&s.exe).is_file() {
                return Err(ActionError::Refused(format!(
                    "{} no longer exists, so it cannot be restarted",
                    s.exe
                )));
            }
            if !Path::new(&s.cwd).is_dir() {
                return Err(ActionError::Refused(format!(
                    "the folder {} no longer exists, so the program cannot be restarted",
                    s.cwd
                )));
            }
            lines.push(format!("Program: {}", s.exe));
            lines.push(format!(
                "  Command: {}",
                display_command(&[vec![s.exe.clone()], s.args.clone()].concat())
            ));
            lines.push(format!("  Folder: {}", s.cwd));
        }
        Ok(UndoPlan {
            risk: Risk::Confirm,
            preview: Preview {
                from: None,
                title: format!("Start again what was using port {}", t.port),
                lines,
                warnings: vec![
                    "It starts with Lookout's environment, not the original one (environment variables are not saved).".into(),
                    "It starts without a console window.".into(),
                ],
            },
            fingerprint: format!("undo:{token}"),
        })
    }

    fn undo(
        &self,
        token: &str,
        _plan: &UndoPlan,
        _proof: &Authorized,
    ) -> Result<String, ActionError> {
        let t: UndoToken = serde_json::from_str(token)
            .map_err(|_| ActionError::Failed("the undo information is damaged".into()))?;
        let mut started = Vec::new();
        let mut problems = Vec::new();
        for spec in &t.restart {
            match self.system.spawn(spec) {
                Ok(pid) => started.push(pid),
                Err(e) => problems.push(e.to_string()),
            }
        }
        if started.is_empty() {
            return Err(ActionError::Failed(problems.join("; ")));
        }
        let deadline = Instant::now() + LISTEN_WAIT;
        let listening = loop {
            if !self.system.listeners(t.port).unwrap_or_default().is_empty() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let pids = started
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let mut message = if listening {
            format!("Started again (PID {pids}); port {} is listening.", t.port)
        } else {
            format!(
                "Started again (PID {pids}), but port {} is not listening yet.",
                t.port
            )
        };
        for pr in &problems {
            message.push_str(&format!(" Not started: {pr}."));
        }
        Ok(message)
    }
}

/// Reads `kill 3000`, `kill port 3000`, `stop :3000`, `free port :3000`.
pub fn parse_kill_request(raw: &str) -> Option<u16> {
    let mut words = raw.split_whitespace().map(str::to_lowercase);
    let verb = words.next()?;
    if !matches!(verb.as_str(), "kill" | "stop" | "free") {
        return None;
    }
    let mut word = words.next()?;
    if word == "port" {
        word = words.next()?;
    }
    if words.next().is_some() {
        return None;
    }
    let digits = word.strip_prefix(':').unwrap_or(&word);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || digits.len() > 5 {
        return None;
    }
    digits.parse::<u16>().ok().filter(|p| *p != 0)
}

/// Offers the `kill port N` result. It only names the program as a hint; the
/// preview is built again from the live process when the user presses Enter.
pub struct PortSource {
    system: Arc<dyn ProcessBackend>,
}

impl PortSource {
    pub fn new(system: Arc<dyn ProcessBackend>) -> Self {
        Self { system }
    }
}

impl Source for PortSource {
    fn id(&self) -> &'static str {
        "ports"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.kind.is_some() {
            return;
        }
        let Some(port) = parse_kill_request(&q.raw) else {
            return;
        };
        let hint = match self.system.listeners(port) {
            Ok(pids) if pids.is_empty() => format!("Nothing is listening on port {port}"),
            Ok(pids) => {
                let names: Vec<String> = pids
                    .iter()
                    .map(|pid| {
                        self.system
                            .image_name(*pid)
                            .map_or_else(|| format!("PID {pid}"), |n| format!("{n} (PID {pid})"))
                    })
                    .collect();
                format!(
                    "Listening: {}. Enter shows what will be stopped.",
                    names.join(", ")
                )
            }
            Err(e) => e.to_string(),
        };
        let mut item = Item::new(
            format!("port:{port}"),
            Kind::Port,
            format!("Kill port {port}"),
            self.id(),
        );
        item.subtitle = Some(hint);
        item.metadata
            .insert("port".into(), serde_json::Value::from(port));
        item.score = PORT_SCORE;
        item.score_features.match_quality = 1.0;
        emit(vec![item]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[test]
    fn parses_the_ways_people_type_it() {
        for ok in [
            "kill 3000",
            "kill port 3000",
            "KILL PORT 3000",
            "stop :3000",
            "free port :3000",
            "  kill   3000 ",
        ] {
            assert_eq!(parse_kill_request(ok), Some(3000), "{ok:?}");
        }
        for bad in [
            "kill",
            "kill port",
            "kill 0",
            "kill 65536",
            "kill 123456",
            "kill 30x0",
            "kill 3000 now",
            "kill -3000",
            "report 3000",
            "kill port port 3000",
            "kill ::3000",
        ] {
            assert_eq!(parse_kill_request(bad), None, "{bad:?}");
        }
        assert_eq!(parse_kill_request("kill 65535"), Some(65535));
    }

    /// A pretend machine: processes with ports, recording what was terminated.
    struct Fake {
        procs: Mutex<Vec<(u16, ProcInfo)>>,
        terminated: Mutex<Vec<u32>>,
        spawned: Mutex<Vec<SpawnSpec>>,
        spawn_count: AtomicUsize,
    }

    fn proc(pid: u32, name: &str, exe: &str, cmd: Option<&[&str]>, cwd: Option<&str>) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            exe: Some(exe.into()),
            cmd: cmd.map(|c| c.iter().map(|s| (*s).to_owned()).collect()),
            cwd: cwd.map(Into::into),
            created: 1000 + u64::from(pid),
            parent: Some(1),
        }
    }

    impl Fake {
        fn new(procs: Vec<(u16, ProcInfo)>) -> Arc<Self> {
            Arc::new(Self {
                procs: Mutex::new(procs),
                terminated: Mutex::default(),
                spawned: Mutex::default(),
                spawn_count: AtomicUsize::new(0),
            })
        }
    }

    impl ProcessBackend for Fake {
        fn listeners(&self, port: u16) -> Result<Vec<u32>, super::super::system::SystemError> {
            let g = self.procs.lock().unwrap();
            Ok(g.iter()
                .filter(|(p, _)| *p == port)
                .map(|(_, i)| i.pid)
                .collect())
        }
        fn inspect(&self, pid: u32) -> Option<ProcInfo> {
            let g = self.procs.lock().unwrap();
            g.iter().find(|(_, i)| i.pid == pid).map(|(_, i)| i.clone())
        }
        fn image_name(&self, pid: u32) -> Option<String> {
            self.inspect(pid).map(|i| i.name)
        }
        fn terminate(&self, p: &ProcInfo) -> Result<(), super::super::system::SystemError> {
            self.terminated.lock().unwrap().push(p.pid);
            self.procs.lock().unwrap().retain(|(_, i)| i.pid != p.pid);
            Ok(())
        }
        fn spawn(&self, spec: &SpawnSpec) -> Result<u32, super::super::system::SystemError> {
            self.spawned.lock().unwrap().push(spec.clone());
            Ok(9000 + self.spawn_count.fetch_add(1, Ordering::SeqCst) as u32)
        }
        fn own_pid(&self) -> u32 {
            777
        }
        fn system_root(&self) -> std::path::PathBuf {
            r"C:\Windows".into()
        }
    }

    fn target(port: u16) -> Target {
        Target {
            item_id: format!("port:{port}"),
            kind: Kind::Port,
            uri: None,
            port: Some(port),
            origin: None,
        }
    }

    fn node() -> ProcInfo {
        proc(
            4821,
            "node.exe",
            r"C:\Program Files\nodejs\node.exe",
            Some(&[
                r"C:\Program Files\nodejs\node.exe",
                "server.js",
                "--port",
                "3000",
            ]),
            Some(r"C:\proj\app"),
        )
    }

    #[test]
    fn preview_describes_the_real_process_and_asks_for_confirm() {
        let fake = Fake::new(vec![(3000, node())]);
        let plan = KillPort::new(fake.clone()).plan(&target(3000)).unwrap();
        assert_eq!(plan.risk, Risk::Confirm);
        assert!(plan.undoable);
        assert_eq!(
            plan.preview.title,
            "Stop node.exe (PID 4821) using port 3000"
        );
        let text = plan.preview.lines.join("\n");
        assert!(text.contains(r"C:\Program Files\nodejs\node.exe"), "{text}");
        assert!(
            text.contains(r#""C:\Program Files\nodejs\node.exe" server.js --port 3000"#),
            "{text}"
        );
        assert!(text.contains(r"Folder: C:\proj\app"));
        assert!(
            fake.terminated.lock().unwrap().is_empty(),
            "plan must not kill"
        );
    }

    #[test]
    fn nothing_listening_is_reported_not_guessed() {
        let fake = Fake::new(vec![(3000, node())]);
        let err = KillPort::new(fake).plan(&target(3001)).unwrap_err();
        assert!(matches!(err, ActionError::NothingToDo(m) if m.contains("3001")));
    }

    #[test]
    fn windows_and_own_processes_are_refused_outright() {
        let cases = [
            proc(4, "System", "", None, None),
            proc(
                600,
                "lsass.exe",
                r"C:\Windows\System32\lsass.exe",
                None,
                None,
            ),
            proc(777, "lookout.exe", r"C:\x\lookout.exe", None, None),
        ];
        for p in cases {
            let name = p.name.clone();
            let fake = Fake::new(vec![(445, p)]);
            let err = KillPort::new(fake.clone()).plan(&target(445)).unwrap_err();
            assert!(matches!(err, ActionError::Refused(_)), "{name}: {err:?}");
            assert!(fake.terminated.lock().unwrap().is_empty());
        }
        // Lookout's own WebView2 is refused; other children (a restarted server) are not.
        let mut webview = proc(
            9,
            "msedgewebview2.exe",
            r"C:\x\msedgewebview2.exe",
            None,
            None,
        );
        webview.parent = Some(777);
        let err = KillPort::new(Fake::new(vec![(9222, webview)]))
            .plan(&target(9222))
            .unwrap_err();
        assert!(matches!(err, ActionError::Refused(_)));
        let mut child = node();
        child.parent = Some(777);
        assert!(KillPort::new(Fake::new(vec![(3000, child)]))
            .plan(&target(3000))
            .is_ok());
    }

    #[test]
    fn programs_in_the_windows_folder_or_unreadable_ones_are_dangerous() {
        let svc = proc(
            1200,
            "svchost.exe",
            r"C:\WINDOWS\System32\svchost.exe",
            Some(&["svchost.exe"]),
            Some(r"C:\Windows\System32"),
        );
        let plan = KillPort::new(Fake::new(vec![(135, svc)]))
            .plan(&target(135))
            .unwrap();
        assert_eq!(plan.risk, Risk::Dangerous);

        let mut hidden = node();
        hidden.exe = None;
        hidden.cmd = None;
        hidden.cwd = None;
        let plan = KillPort::new(Fake::new(vec![(3000, hidden)]))
            .plan(&target(3000))
            .unwrap();
        assert_eq!(plan.risk, Risk::Dangerous);
        assert!(
            !plan.undoable,
            "cannot restart without command line and folder"
        );
    }

    #[test]
    fn undo_needs_both_command_line_and_working_folder() {
        for (cmd, cwd) in [(None, Some(r"C:\a")), (Some(&["x.exe"][..]), None)] {
            let p = proc(5, "x.exe", r"C:\a\x.exe", cmd, cwd);
            let plan = KillPort::new(Fake::new(vec![(1234, p)]))
                .plan(&target(1234))
                .unwrap();
            assert!(!plan.undoable);
        }
    }

    #[test]
    fn execute_stops_the_previewed_process_and_returns_a_restart_token() {
        let fake = Fake::new(vec![
            (3000, node()),
            (3001, proc(1, "other.exe", r"C:\o.exe", None, None)),
        ]);
        let action = KillPort::new(fake.clone());
        let plan = action.plan(&target(3000)).unwrap();
        let done = action.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(*fake.terminated.lock().unwrap(), [4821]);
        assert!(
            done.message.contains("Port 3000 is free"),
            "{}",
            done.message
        );
        let token: UndoToken = serde_json::from_str(&done.undo_token.unwrap()).unwrap();
        assert_eq!(token.port, 3000);
        assert_eq!(
            token.restart,
            [SpawnSpec {
                exe: r"C:\Program Files\nodejs\node.exe".into(),
                args: vec!["server.js".into(), "--port".into(), "3000".into()],
                cwd: r"C:\proj\app".into(),
            }]
        );
    }

    #[test]
    fn plan_undo_refuses_when_the_port_is_taken_or_the_files_are_gone() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("srv.exe");
        std::fs::write(&exe, b"x").unwrap();
        let token = |exe: &Path, cwd: &Path| {
            serde_json::to_string(&UndoToken {
                port: 3000,
                restart: vec![SpawnSpec {
                    exe: exe.to_string_lossy().into_owned(),
                    args: vec!["a b".into()],
                    cwd: cwd.to_string_lossy().into_owned(),
                }],
            })
            .unwrap()
        };
        let free = KillPort::new(Fake::new(vec![]));
        let ok = free.plan_undo(&token(&exe, dir.path())).unwrap();
        assert_eq!(ok.risk, Risk::Confirm);
        assert!(ok.preview.lines.join("\n").contains("\"a b\""));
        assert!(matches!(
            free.plan_undo(&token(&dir.path().join("gone.exe"), dir.path())),
            Err(ActionError::Refused(_))
        ));
        assert!(matches!(
            free.plan_undo(&token(&exe, &dir.path().join("gone"))),
            Err(ActionError::Refused(_))
        ));
        let busy = KillPort::new(Fake::new(vec![(3000, node())]));
        assert!(matches!(
            busy.plan_undo(&token(&exe, dir.path())),
            Err(ActionError::Refused(m)) if m.contains("node.exe")
        ));
        assert!(matches!(
            free.plan_undo("not json"),
            Err(ActionError::Failed(_))
        ));
    }

    #[test]
    fn port_source_offers_an_item_only_for_a_kill_request() {
        let fake = Fake::new(vec![(3000, node())]);
        let src = PortSource::new(fake);
        let run = |text: &str| {
            let ctx = QueryContext::new(1, Arc::new(std::sync::atomic::AtomicU64::new(1)));
            let q = crate::query::parse(text);
            let mut out = Vec::new();
            src.query(&q, &ctx, &mut |items| out = items);
            out
        };
        let items = run("kill port 3000");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "port:3000");
        assert_eq!(items[0].kind, Kind::Port);
        assert!(items[0]
            .subtitle
            .as_deref()
            .unwrap()
            .contains("node.exe (PID 4821)"));
        assert_eq!(items[0].metadata["port"], 3000);
        assert!(run("kill 3001")[0]
            .subtitle
            .as_deref()
            .unwrap()
            .contains("Nothing is listening"));
        assert!(run("budget report").is_empty());
        assert!(run("kind:file kill 3000").is_empty());
    }
}
