//! Budget benchmarks. Phase 0 measures hotkey-to-visible latency by launching
//! the app in `--measure-latency` mode and reading back its CSV.
//!
//! Usage: `bench latency [--exe PATH] [--cycles N]`
//! Exit code is non-zero when a budget is exceeded.

mod content;
mod crawl;
mod fixture;
mod memory;
mod query;

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use lookout_core::latency::{self, Stats, HOTKEY_BUDGET_MS};

const DEFAULT_CYCLES: usize = 30;

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(msg) => {
            eprintln!("bench: {msg}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("latency") => {}
        Some("memory") => return memory::run(args),
        Some("query") => return query::run(args),
        Some("content") => return content::run(args),
        Some("crawl") => return crawl::run_crawl(args),
        Some("locks") => return crawl::run_locks(args),
        Some("rescan-memory") => return crawl::run_rescan_memory(args),
        Some("watch") => return crawl::run_watch(args),
        _ => {
            return Err("usage: bench latency [--exe PATH] [--cycles N]\n       \
                        bench memory [--exe PATH] [--wait SECS] [--runs N] [--pid PID] [--app-arg ARG]...\n       \
                        bench query [--files N] [--reps N] [--seed N] [--threads N]\n       \
                        bench crawl [--files N] [--duty D]...\n       \
                        bench locks [--files N]\n       \
                        bench rescan-memory [--files N]\n       \
                        bench watch [--files N]"
                .into())
        }
    }
    let mut exe = PathBuf::from("target/release/lookout.exe");
    let mut cycles = DEFAULT_CYCLES;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--exe" => exe = args.next().ok_or("--exe needs a path")?.into(),
            "--cycles" => {
                cycles = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--cycles needs a number")?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if !exe.exists() {
        return Err(format!(
            "{} not found; build it first with `npm --prefix ui run tauri build -- --no-bundle`",
            exe.display()
        ));
    }

    let out = std::env::temp_dir().join(format!("lookout-latency-{}.csv", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let status = Command::new(&exe)
        .arg("--measure-latency")
        .arg(cycles.to_string())
        .arg("--out")
        .arg(&out)
        .status()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    if !status.success() {
        return Err(format!("app exited with {status}"));
    }
    let text = std::fs::read_to_string(&out).map_err(|e| format!("no results written: {e}"))?;
    let _ = std::fs::remove_file(&out);
    let samples = latency::from_csv(&text).map_err(|e| e.to_string())?;
    if samples.len() < 2 {
        return Err(format!("only {} samples collected", samples.len()));
    }

    // The first cycle is cold (first paint after webview load); report it
    // separately and judge the budget on warm cycles only.
    let cold = samples[0];
    let warm = &samples[1..];
    let native: Vec<f64> = warm.iter().map(|s| s.native_ms).collect();
    let painted: Vec<f64> = warm.iter().map(|s| s.painted_ms).collect();
    let (Some(native), Some(painted)) = (latency::summarize(&native), latency::summarize(&painted))
    else {
        return Err("no warm samples".into());
    };

    println!(
        "hotkey-to-visible latency, {} warm cycles (ms)",
        painted.count
    );
    println!(
        "{:<28}{:>8}{:>8}{:>8}{:>8}",
        "metric", "min", "p50", "p95", "max"
    );
    print_row("native show+focus", &native);
    print_row("visible (2 frames painted)", &painted);
    println!(
        "cold first cycle: native {:.1} ms, visible {:.1} ms",
        cold.native_ms, cold.painted_ms
    );

    let ok = painted.p95 < HOTKEY_BUDGET_MS;
    println!(
        "budget: visible p95 < {HOTKEY_BUDGET_MS} ms -> {} ({:.1} ms)",
        if ok { "PASS" } else { "FAIL" },
        painted.p95
    );
    Ok(ok)
}

fn print_row(name: &str, s: &Stats) {
    println!(
        "{:<28}{:>8.1}{:>8.1}{:>8.1}{:>8.1}",
        name, s.min, s.p50, s.p95, s.max
    );
}
