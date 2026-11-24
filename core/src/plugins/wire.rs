//! One plugin process and the JSON-RPC 2.0 conversation with it.
//!
//! Framing: one JSON object per line on the plugin's stdin and stdout.
//! Nothing here can block the caller for longer than the timeout it passes:
//! writes go through a thread, reads come from a thread, and a plugin that
//! stops answering is killed by the caller.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Longest line accepted from a plugin; a longer one is treated as garbage.
pub const MAX_LINE: u64 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("could not start the plugin: {0}")]
    Spawn(String),
    #[error("the plugin did not answer in time and was stopped")]
    Timeout,
    #[error("the plugin stopped running")]
    Died,
    #[error("the add-on sent data that is not valid JSON-RPC and was stopped")]
    Garbage,
    #[error("{message}")]
    Rpc { code: i64, message: String },
}

enum Incoming {
    Response(u64, Result<Value, WireError>),
    Garbage,
}

/// Counters the reader thread updates.
#[derive(Default)]
pub struct Shared {
    /// Messages from the plugin that it is not allowed to send.
    pub violations: AtomicU32,
    /// Bumped by every `items_changed` notification.
    pub items_changed: AtomicU64,
}

/// What the operating system is asked to enforce on the process.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// May the process create child processes?
    pub spawn: bool,
    /// May the process use the clipboard?
    pub clipboard: bool,
    pub memory_bytes: usize,
}

pub struct PluginProcess {
    child: Child,
    writer: Sender<String>,
    rx: Receiver<Incoming>,
    next_id: u64,
    shared: Arc<Shared>,
    pub started: Instant,
    #[cfg(windows)]
    _job: job::Job,
}

impl PluginProcess {
    pub fn spawn(
        exe: &Path,
        args: &[String],
        dir: &Path,
        limits: Limits,
    ) -> Result<Self, WireError> {
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| WireError::Spawn(e.to_string()))?;
        #[cfg(windows)]
        let job = match job::Job::create(limits).and_then(|j| j.assign(&child).map(|()| j)) {
            Ok(j) => j,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(WireError::Spawn(e));
            }
        };
        #[cfg(not(windows))]
        let _ = limits;

        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(WireError::Spawn("no stdio".into()));
        };
        let shared = Arc::new(Shared::default());
        let (wtx, wrx) = channel::<String>();
        let (rtx, rrx) = channel::<Incoming>();
        std::thread::Builder::new()
            .name("lookout-plugin-write".into())
            .spawn(move || {
                let mut stdin = stdin;
                while let Ok(line) = wrx.recv() {
                    if stdin.write_all(line.as_bytes()).is_err()
                        || stdin.write_all(b"\n").is_err()
                        || stdin.flush().is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| WireError::Spawn(e.to_string()))?;
        let (reply, counters) = (wtx.clone(), shared.clone());
        std::thread::Builder::new()
            .name("lookout-plugin-read".into())
            .spawn(move || read_loop(stdout, &rtx, &reply, &counters))
            .map_err(|e| WireError::Spawn(e.to_string()))?;
        Ok(Self {
            child,
            writer: wtx,
            rx: rrx,
            next_id: 0,
            shared,
            started: Instant::now(),
            #[cfg(windows)]
            _job: job,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Sends a request and waits for its answer, at most `timeout`. On any
    /// error the caller should [`kill`](Self::kill) the process.
    pub fn call(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, WireError> {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.writer
            .send(line.to_string())
            .map_err(|_| WireError::Died)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Incoming::Response(rid, result)) if rid == id => return result,
                Ok(Incoming::Response(..)) => {}
                Ok(Incoming::Garbage) => return Err(WireError::Garbage),
                Err(RecvTimeoutError::Timeout) => return Err(WireError::Timeout),
                Err(RecvTimeoutError::Disconnected) => return Err(WireError::Died),
            }
        }
    }

    /// A JSON-RPC notification (no answer expected).
    pub fn notify(&self, method: &str, params: Value) {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let _ = self.writer.send(line.to_string());
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Asks the plugin to stop, then makes sure it did.
    pub fn stop(&mut self, grace: Duration) {
        let _ = self.call("shutdown", Value::Null, grace);
        let end = Instant::now() + grace;
        while self.is_alive() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read_loop(stdout: impl Read, tx: &Sender<Incoming>, reply: &Sender<String>, c: &Shared) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match (&mut reader).take(MAX_LINE + 1).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !buf.ends_with(b"\n") {
            if buf.len() as u64 > MAX_LINE {
                let _ = tx.send(Incoming::Garbage);
            }
            return;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&buf) else {
            c.violations.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(Incoming::Garbage);
            continue;
        };
        if msg.get("jsonrpc") != Some(&json!("2.0")) {
            c.violations.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(Incoming::Garbage);
            continue;
        }
        if let Some(method) = msg.get("method").and_then(Value::as_str) {
            // The plugin may only announce that its items changed. Anything
            // else (above all `execute`) is refused; it is never carried out.
            if msg.get("id").is_none() && method == "items_changed" {
                c.items_changed.fetch_add(1, Ordering::Relaxed);
            } else {
                c.violations.fetch_add(1, Ordering::Relaxed);
                if let Some(id) = msg.get("id") {
                    let err = json!({"jsonrpc": "2.0", "id": id, "error":
                        {"code": -32601, "message": "plugins cannot call the host"}});
                    let _ = reply.send(err.to_string());
                }
            }
            continue;
        }
        let Some(id) = msg.get("id").and_then(Value::as_u64) else {
            c.violations.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(Incoming::Garbage);
            continue;
        };
        let result = if let Some(err) = msg.get("error") {
            Err(WireError::Rpc {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(-32000),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .chars()
                    .take(300)
                    .collect(),
            })
        } else if let Some(r) = msg.get("result") {
            Ok(r.clone())
        } else {
            c.violations.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(Incoming::Garbage);
            continue;
        };
        if tx.send(Incoming::Response(id, result)).is_err() {
            return;
        }
    }
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_UILIMIT_READCLIPBOARD,
        JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };

    use super::Limits;

    /// A Windows Job Object. Closing the handle (when the host exits, even if
    /// it crashes) kills every process in the job.
    pub struct Job(HANDLE);

    // SAFETY: a job handle can be used from any thread.
    unsafe impl Send for Job {}

    impl Job {
        pub fn create(limits: Limits) -> Result<Self, String> {
            // SAFETY: plain Win32 calls with correctly sized, initialised structs.
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return Err("CreateJobObject failed".into());
                }
                let job = Job(h);
                let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                ext.BasicLimitInformation.LimitFlags =
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_MEMORY;
                ext.JobMemoryLimit = limits.memory_bytes;
                if !limits.spawn {
                    // The plugin itself is the one allowed process.
                    ext.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                    ext.BasicLimitInformation.ActiveProcessLimit = 1;
                }
                let ok = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(ext).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    return Err("SetInformationJobObject (limits) failed".into());
                }
                if !limits.clipboard {
                    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                        UIRestrictionsClass: JOB_OBJECT_UILIMIT_READCLIPBOARD
                            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
                    };
                    let ok = SetInformationJobObject(
                        h,
                        JobObjectBasicUIRestrictions,
                        std::ptr::addr_of!(ui).cast(),
                        std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
                    );
                    if ok == 0 {
                        return Err("SetInformationJobObject (clipboard) failed".into());
                    }
                }
                Ok(job)
            }
        }

        pub fn assign(&self, child: &Child) -> Result<(), String> {
            // SAFETY: both handles are valid for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) };
            if ok == 0 {
                Err("could not put the plugin into its job object".into())
            } else {
                Ok(())
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this value.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
