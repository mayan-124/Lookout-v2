//! Idle memory of `lookout.exe` plus all of its `msedgewebview2.exe` descendants.
//!
//! Three metrics per process, all from Win32 APIs:
//! - working set and private bytes: `K32GetProcessMemoryInfo` (`WorkingSetSize`,
//!   `PrivateUsage`);
//! - private working set (the part of the working set not shared with other
//!   processes, what Task Manager shows as "Memory"): `K32QueryWorkingSet`, counting
//!   pages whose `Shared` bit is clear. The per-process counter
//!   "\Process V2(*)\Working Set - Private" reports the same quantity.

use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, K32QueryWorkingSet, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

const PAGE_BYTES: u64 = 4096;
const MB: f64 = 1024.0 * 1024.0;
/// Budget (brief section 8): combined private working set of lookout.exe and
/// its msedgewebview2.exe children, 2 minutes after launch with the bar hidden.
const IDLE_BUDGET_MB: f64 = 150.0;
const DEFAULT_WAIT_SECS: u64 = 120;
const DEFAULT_RUNS: usize = 3;

struct Row {
    name: String,
    pid: u32,
    working_set: u64,
    private_bytes: u64,
    private_ws: u64,
}

#[derive(Clone, Copy, Default)]
struct Totals {
    working_set: u64,
    private_bytes: u64,
    private_ws: u64,
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut exe = PathBuf::from("target/release/lookout.exe");
    let mut wait = DEFAULT_WAIT_SECS;
    let mut runs = DEFAULT_RUNS;
    let mut attach: Option<u32> = None;
    let mut app_args: Vec<String> = Vec::new();
    // A generated corpus for the content/OCR batch: the app indexes it right after launch.
    let mut fixture_text = 0usize;
    let mut fixture_images = 0usize;
    let mut startup_files = 0usize;
    let mut sample_ms = 1000u64;
    while let Some(a) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{what} needs a value"));
        match a.as_str() {
            "--exe" => exe = value("--exe")?.into(),
            "--wait" => wait = value("--wait")?.parse().map_err(|_| "bad --wait")?,
            "--runs" => runs = value("--runs")?.parse().map_err(|_| "bad --runs")?,
            // Measure an already running instance instead of launching one.
            "--pid" => attach = Some(value("--pid")?.parse().map_err(|_| "bad --pid")?),
            // Passed to the launched app, e.g. `--app-arg --synthetic-index --app-arg 1000000`.
            "--app-arg" => app_args.push(value("--app-arg")?),
            // Startup peak: index this many names, stop, relaunch and sample the second start.
            "--startup-files" => {
                startup_files = value("--startup-files")?
                    .parse()
                    .map_err(|_| "bad --startup-files")?;
            }
            "--sample-ms" => {
                sample_ms = value("--sample-ms")?
                    .parse()
                    .map_err(|_| "bad --sample-ms")?
            }
            "--fixture-text" => {
                fixture_text = value("--fixture-text")?
                    .parse()
                    .map_err(|_| "bad --fixture-text")?;
            }
            "--fixture-images" => {
                fixture_images = value("--fixture-images")?
                    .parse()
                    .map_err(|_| "bad --fixture-images")?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }

    if let Some(pid) = attach {
        let rows = measure_tree(pid)?;
        print_run(0, &rows);
        return Ok(within_budget(&totals(&rows)));
    }
    if !exe.exists() {
        return Err(format!(
            "{} not found; build it first with `npm --prefix ui run tauri build -- --no-bundle`",
            exe.display()
        ));
    }

    if startup_files > 0 {
        return run_startup(&exe, startup_files, wait, sample_ms);
    }
    let batch = fixture_text > 0 || fixture_images > 0;
    let mut all = Vec::new();
    let mut peaks = Vec::new();
    for run in 1..=runs {
        println!("run {run}/{runs}: launching, waiting {wait} s with the bar hidden...");
        let corpus = if batch {
            let c = crate::fixture::create(fixture_text, fixture_images)?;
            println!(
                "  corpus: {fixture_text} text files and {fixture_images} images in {}",
                c.root.display()
            );
            Some(c)
        } else {
            None
        };
        let mut launch_args = app_args.clone();
        if let Some(c) = &corpus {
            // The generated corpus is the only folder in that data dir's settings.
            if launch_args.iter().any(|a| a == "--synthetic-index") {
                launch_args.push("--synthetic-with-folders".into());
            }
            launch_args.push("--data-dir".into());
            launch_args.push(c.data_dir.to_string_lossy().into_owned());
        }
        let mut child = Command::new(&exe)
            .args(&launch_args)
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
        let peak = if batch {
            sample_during(child.id(), wait, sample_ms)
        } else {
            std::thread::sleep(Duration::from_secs(wait));
            0
        };
        peaks.push(peak);
        let result = measure_tree(child.id());
        // Kill the whole tree: WebView2 processes outlive their parent and the
        // next launch would attach to them, corrupting the next run.
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output();
        let _ = child.wait();
        if let Some(c) = &corpus {
            c.remove();
        }
        let rows = result?;
        print_run(run, &rows);
        all.push(totals(&rows));
        // Let the old webview processes exit before the next launch.
        std::thread::sleep(Duration::from_secs(3));
    }

    println!("summary of totals (MB)");
    println!(
        "{:<6}{:>14}{:>16}{:>20}",
        "run", "working set", "private bytes", "private working set"
    );
    for (i, t) in all.iter().enumerate() {
        println!(
            "{:<6}{:>14.1}{:>16.1}{:>20.1}",
            i + 1,
            t.working_set as f64 / MB,
            t.private_bytes as f64 / MB,
            t.private_ws as f64 / MB
        );
    }
    let mut ok = all.iter().all(within_budget);
    println!(
        "budget: combined private working set <= {IDLE_BUDGET_MB} MB in every run -> {}",
        if ok { "PASS" } else { "FAIL" }
    );
    if batch {
        let peak_ok = peaks.iter().all(|p| *p as f64 / MB <= IDLE_BUDGET_MB);
        println!(
            "peak during the content/OCR batch (MB): {}",
            peaks
                .iter()
                .map(|p| format!("{:.1}", *p as f64 / MB))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "budget: peak combined private working set during the batch <= {IDLE_BUDGET_MB} MB -> {}",
            if peak_ok { "PASS" } else { "FAIL" }
        );
        ok &= peak_ok;
    }
    Ok(ok)
}

/// Samples the app every second for `wait` seconds, printing a trace every
/// 5 s (elapsed, combined private working set, `lookout.exe` CPU in that
/// window as a percentage of one core). Returns the peak private working set in bytes.
fn sample_during(root: u32, wait: u64, sample_ms: u64) -> u64 {
    println!(
        "  {:>6} {:>12} {:>14}",
        "t (s)", "private WS MB", "lookout CPU %"
    );
    let start = std::time::Instant::now();
    let mut peak = 0u64;
    let mut last_cpu = cpu_seconds_of(root);
    let mut last_t = start.elapsed().as_secs_f64();
    let mut tick = 0u64;
    while start.elapsed() < Duration::from_secs(wait) {
        std::thread::sleep(Duration::from_millis(sample_ms));
        tick += 1;
        let Ok(rows) = measure_tree(root) else {
            continue; // WebView2 children are not up yet
        };
        let t = totals(&rows);
        peak = peak.max(t.private_ws);
        if tick.is_multiple_of(5000 / sample_ms.max(1)) {
            let now_t = start.elapsed().as_secs_f64();
            let cpu = cpu_seconds_of(root);
            let pct = (cpu - last_cpu) / (now_t - last_t).max(0.001) * 100.0;
            println!(
                "  {:>6.0} {:>12.1} {:>14.1}",
                now_t,
                t.private_ws as f64 / MB,
                pct
            );
            last_cpu = cpu;
            last_t = now_t;
        }
    }
    peak
}

/// Peak memory at startup while a persisted snapshot and the fresh startup crawl
/// both exist: launch once to build and save the snapshot, kill the app, launch
/// again and sample every `sample_ms`.
fn run_startup(
    exe: &std::path::Path,
    files: usize,
    wait: u64,
    sample_ms: u64,
) -> Result<bool, String> {
    println!("creating {files} empty files...");
    let corpus = crate::fixture::create_names_only(files)?;
    let data = corpus.data_dir.to_string_lossy().into_owned();
    let launch = || {
        Command::new(exe)
            .args(["--data-dir", &data])
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", exe.display()))
    };
    let kill = |c: &mut std::process::Child| {
        let _ = Command::new("taskkill")
            .args(["/PID", &c.id().to_string(), "/T", "/F"])
            .output();
        let _ = c.wait();
        std::thread::sleep(Duration::from_secs(3));
    };

    println!("first launch: crawling and saving the snapshot...");
    let mut first = launch()?;
    let snapshot = corpus.data_dir.join("names.idx");
    let begun = std::time::Instant::now();
    while !snapshot.exists() && begun.elapsed() < Duration::from_secs(400) {
        std::thread::sleep(Duration::from_secs(2));
    }
    std::thread::sleep(Duration::from_secs(15)); // let the saver finish
    kill(&mut first);
    let size = std::fs::metadata(&snapshot).map_or(0, |m| m.len());
    println!("snapshot written: {:.1} MB", size as f64 / MB);
    if size == 0 {
        corpus.remove();
        return Err("the first launch never wrote a snapshot".into());
    }

    println!("second launch: sampling every {sample_ms} ms for {wait} s (snapshot loaded, then fresh crawl swaps in)");
    let mut second = launch()?;
    let start = std::time::Instant::now();
    let mut peak = (0u64, 0.0f64);
    let mut series = Vec::new();
    while start.elapsed() < Duration::from_secs(wait) {
        std::thread::sleep(Duration::from_millis(sample_ms));
        if let Ok(rows) = measure_tree(second.id()) {
            let t = totals(&rows);
            let at = start.elapsed().as_secs_f64();
            if t.private_ws > peak.0 {
                peak = (t.private_ws, at);
            }
            series.push((at, t.private_ws));
        }
    }
    let last = series.last().map_or(0, |s| s.1);
    let first_sample = series.first().copied().unwrap_or((0.0, 0));
    kill(&mut second);
    corpus.remove();
    println!(
        "first sample {:.1} MB at {:.1} s; peak {:.1} MB at {:.1} s; at {wait} s {:.1} MB ({} samples)",
        first_sample.1 as f64 / MB,
        first_sample.0,
        peak.0 as f64 / MB,
        peak.1,
        last as f64 / MB,
        series.len()
    );
    let ok = peak.0 as f64 / MB <= IDLE_BUDGET_MB;
    println!(
        "budget: peak combined private working set during startup <= {IDLE_BUDGET_MB} MB -> {}",
        if ok { "PASS" } else { "FAIL" }
    );
    Ok(ok)
}

/// User plus kernel CPU seconds used so far by process `pid` (0 if unreadable).
fn cpu_seconds_of(pid: u32) -> f64 {
    // SAFETY: the handle is checked and closed; GetProcessTimes only writes the FILETIMEs.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
        if h.is_null() {
            return 0.0;
        }
        let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) =
            (zeroed(), zeroed(), zeroed(), zeroed());
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        CloseHandle(h);
        if ok == 0 {
            return 0.0;
        }
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        (ticks(k) + ticks(u)) as f64 / 1e7
    }
}

fn within_budget(t: &Totals) -> bool {
    t.private_ws as f64 / MB <= IDLE_BUDGET_MB
}

/// User plus kernel CPU seconds used so far by the calling process.
pub fn process_cpu_seconds() -> f64 {
    // SAFETY: GetProcessTimes only writes into the four FILETIMEs we pass.
    unsafe {
        let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) =
            (zeroed(), zeroed(), zeroed(), zeroed());
        if GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) == 0 {
            return 0.0;
        }
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        (ticks(k) + ticks(u)) as f64 / 1e7
    }
}

/// Memory of the calling process, with the same three metrics as the report.
pub struct Usage {
    pub working_set: u64,
    pub private_bytes: u64,
    pub private_ws: u64,
}

pub fn self_usage() -> Result<Usage, String> {
    let r = measure_process(std::process::id(), "bench")?;
    Ok(Usage {
        working_set: r.working_set,
        private_bytes: r.private_bytes,
        private_ws: r.private_ws,
    })
}

fn totals(rows: &[Row]) -> Totals {
    rows.iter().fold(Totals::default(), |mut t, r| {
        t.working_set += r.working_set;
        t.private_bytes += r.private_bytes;
        t.private_ws += r.private_ws;
        t
    })
}

fn print_run(run: usize, rows: &[Row]) {
    if run > 0 {
        println!("run {run} (MB)");
    }
    println!(
        "{:<18}{:>8}{:>14}{:>16}{:>20}",
        "process", "pid", "working set", "private bytes", "private working set"
    );
    for r in rows {
        println!(
            "{:<18}{:>8}{:>14.1}{:>16.1}{:>20.1}",
            r.name,
            r.pid,
            r.working_set as f64 / MB,
            r.private_bytes as f64 / MB,
            r.private_ws as f64 / MB
        );
    }
    let t = totals(rows);
    println!(
        "{:<18}{:>8}{:>14.1}{:>16.1}{:>20.1}\n",
        format!("TOTAL ({} procs)", rows.len()),
        "",
        t.working_set as f64 / MB,
        t.private_bytes as f64 / MB,
        t.private_ws as f64 / MB
    );
}

/// The root process plus every descendant named `msedgewebview2.exe`.
fn measure_tree(root: u32) -> Result<Vec<Row>, String> {
    let procs = snapshot()?;
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        tree.extend(procs.iter().filter(|p| p.1 == parent).map(|p| p.0));
        i += 1;
    }
    let mut rows = Vec::new();
    for pid in tree {
        let Some((_, _, name)) = procs.iter().find(|p| p.0 == pid) else {
            continue;
        };
        // The WebView2 processes and any plugin processes (`lookout-*.exe`) count.
        let ours = name.eq_ignore_ascii_case("msedgewebview2.exe")
            || name.to_lowercase().starts_with("lookout-");
        if pid != root && !ours {
            continue;
        }
        rows.push(measure_process(pid, name)?);
    }
    if rows.len() < 2 {
        return Err(format!(
            "found no msedgewebview2.exe children under pid {root}; \
             a stale instance may still be running"
        ));
    }
    Ok(rows)
}

/// (pid, parent pid, exe name) for every process.
fn snapshot() -> Result<Vec<(u32, u32, String)>, String> {
    // SAFETY: plain Win32 calls; the entry struct is zeroed with dwSize set as required.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(format!("process snapshot failed ({})", GetLastError()));
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut out = Vec::new();
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            out.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..len]),
            ));
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
        Ok(out)
    }
}

fn measure_process(pid: u32, name: &str) -> Result<Row, String> {
    // SAFETY: the handle is checked and closed; buffers are sized by the callee contract.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if h.is_null() {
            return Err(format!("OpenProcess({pid}) failed ({})", GetLastError()));
        }
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok = K32GetProcessMemoryInfo(
            h,
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        );
        if ok == 0 {
            let e = GetLastError();
            CloseHandle(h);
            return Err(format!("GetProcessMemoryInfo({pid}) failed ({e})"));
        }
        let private_ws = private_working_set(h);
        CloseHandle(h);
        Ok(Row {
            name: name.to_owned(),
            pid,
            working_set: counters.WorkingSetSize as u64,
            private_bytes: counters.PrivateUsage as u64,
            private_ws: private_ws.map_err(|e| format!("QueryWorkingSet({pid}) failed ({e})"))?,
        })
    }
}

/// Counts working-set pages whose `Shared` bit (bit 8 of each entry) is clear.
unsafe fn private_working_set(h: HANDLE) -> Result<u64, u32> {
    let mut capacity = 1usize << 16;
    for _ in 0..8 {
        // Layout: [entry count, entry, entry, ...], all usize.
        let mut buf = vec![0usize; capacity + 1];
        let ok = K32QueryWorkingSet(
            h,
            buf.as_mut_ptr().cast(),
            (buf.len() * size_of::<usize>()) as u32,
        );
        if ok != 0 {
            let n = buf[0].min(capacity);
            let private = buf[1..=n].iter().filter(|e| (**e >> 8) & 1 == 0).count();
            return Ok(private as u64 * PAGE_BYTES);
        }
        // On ERROR_BAD_LENGTH the first slot holds the entry count that is needed.
        let needed = buf[0];
        if needed <= capacity {
            return Err(GetLastError());
        }
        capacity = needed + 4096;
    }
    Err(GetLastError())
}
