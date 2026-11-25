//! Keystroke-to-first-result benchmark over a synthetic name index.
//!
//! Two layers are timed for each query: the raw index search, and the whole
//! engine round trip (submit, source threads, merger, sink). The UI/IPC hop
//! to the webview is not included; the app logs that separately.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lookout_core::apps::AppEntry;
use lookout_core::engine::{Engine, EngineConfig, Sink, Update};
use lookout_core::frecency::FrecencyStore;
use lookout_core::index_service::{new_shared_index, read, unix_now, write};
use lookout_core::latency::{summarize, Stats};
use lookout_core::name_index::SearchParams;
use lookout_core::query;
use lookout_core::ranking::RankingWeights;
use lookout_core::sources::{AppsSource, CalculatorSource, FilesSource, Source, UnitsSource};
use lookout_core::synthetic;

use crate::memory;

/// Brief section 8: keystroke to first results, instant tier, 1M names.
const FIRST_RESULT_BUDGET_MS: f64 = 50.0;

const QUERIES: &[(&str, &str)] = &[
    ("one letter", "a"),
    ("two letters", "re"),
    ("common word", "report"),
    ("word + extension", "report pdf"),
    ("three tokens", "annual budget xlsx"),
    ("number", "2019"),
    ("camera name", "img 0042"),
    ("rare word", "wireframe v7"),
    ("no match", "zzzqq"),
    ("folders only", "kind:folder design"),
];

struct Settings {
    files: usize,
    reps: usize,
    seed: u64,
    /// Cap on threads one index scan may use; `None` keeps the default (8).
    threads: Option<usize>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Settings, String> {
    let mut s = Settings {
        files: 1_000_000,
        reps: 100,
        seed: 42,
        threads: None,
    };
    while let Some(a) = args.next() {
        let mut num = |what: &str| -> Result<u64, String> {
            args.next()
                .and_then(|v| v.parse().ok())
                .ok_or(format!("{what} needs a number"))
        };
        match a.as_str() {
            "--files" => s.files = num("--files")? as usize,
            "--reps" => s.reps = num("--reps")? as usize,
            "--seed" => s.seed = num("--seed")?,
            "--threads" => s.threads = Some(num("--threads")? as usize),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(s)
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn row(name: &str, text: &str, hits: usize, search: &Stats, first: &Stats, done: &Stats) {
    println!(
        "{:<18}{:<22}{:>7} | {:>6.2}{:>7.2}{:>7.2} | {:>6.2}{:>7.2}{:>7.2} | {:>6.2}{:>7.2}",
        name,
        text,
        hits,
        search.p50,
        search.p95,
        search.max,
        first.p50,
        first.p95,
        first.max,
        done.p50,
        done.p95
    );
}

pub fn run(args: impl Iterator<Item = String>) -> Result<bool, String> {
    let s = parse(args)?;
    let hw = std::thread::available_parallelism().map_or(1, usize::from);
    println!(
        "synthetic corpus: {} files, seed {}; {} hardware threads available, index scans capped at {} (parallel above 200,000 entries)",
        s.files,
        s.seed,
        hw,
        s.threads.unwrap_or(8).min(hw)
    );

    let before = memory::self_usage()?;
    let index = new_shared_index();
    let built = Instant::now();
    {
        let mut w = write(&index);
        synthetic::populate(&mut w, s.files, s.seed, unix_now()).map_err(|e| e.to_string())?;
        w.shrink_to_fit();
        if let Some(t) = s.threads {
            w.set_max_threads(t);
        }
    }
    let build_time = built.elapsed();
    let after = memory::self_usage()?;
    {
        let r = read(&index);
        let c = r.counts();
        println!(
            "built in {:.2} s: {} files, {} folders; index buffers {:.1} MB",
            build_time.as_secs_f64(),
            c.files,
            c.folders,
            r.heap_bytes() as f64 / (1024.0 * 1024.0)
        );
        if let Some(n) = r.file_name_stats() {
            println!(
                "file name length in characters: min {}  p50 {}  mean {:.1}  p95 {}  max {}  ({} names)",
                n.min, n.p50, n.mean, n.p95, n.max, n.count
            );
        }
    }
    println!(
        "bench process memory (MB)   working set {:.1} -> {:.1}   private bytes {:.1} -> {:.1}   private working set {:.1} -> {:.1}",
        mb(before.working_set),
        mb(after.working_set),
        mb(before.private_bytes),
        mb(after.private_bytes),
        mb(before.private_ws),
        mb(after.private_ws)
    );
    println!(
        "index cost: {:.1} MB private working set\n",
        mb(after.private_ws.saturating_sub(before.private_ws))
    );

    // Engine with the same sources as the app (no real apps: the list is tiny).
    let weights = RankingWeights::default();
    let apps: Vec<AppEntry> = Vec::new();
    let frecency = Arc::new(FrecencyStore::open_in_memory().map_err(|e| e.to_string())?);
    let sources: Vec<Arc<dyn Source>> = vec![
        Arc::new(CalculatorSource),
        Arc::new(UnitsSource),
        Arc::new(AppsSource::new(apps, frecency, weights)),
        Arc::new(FilesSource::new(index.clone(), weights)),
    ];
    let (tx, rx) = mpsc::channel::<(Update, Instant)>();
    let sink: Sink = Arc::new(move |u| {
        let _ = tx.send((u, Instant::now()));
    });
    let engine = Engine::new(sources, sink, EngineConfig::default()).map_err(|e| e.to_string())?;

    println!("times in ms, {} repetitions each; limit 50 results", s.reps);
    println!(
        "{:<18}{:<22}{:>7} | {:^21} | {:^21} | {:^14}",
        "", "", "", "index search", "first update", "complete"
    );
    println!(
        "{:<18}{:<22}{:>7} | {:>6}{:>7}{:>7} | {:>6}{:>7}{:>7} | {:>6}{:>7}",
        "query", "text", "hits", "p50", "p95", "max", "p50", "p95", "max", "p50", "p95"
    );

    let mut generation = 0u64;
    let mut worst_first_p95 = 0.0f64;
    let now = unix_now();
    for (name, text) in QUERIES {
        let parsed = query::parse(text);
        let tokens: Vec<Vec<u8>> = parsed
            .tokens
            .iter()
            .map(|t| t.as_bytes().to_vec())
            .collect();
        let mut search_ms = Vec::with_capacity(s.reps);
        let mut hits = 0;
        for _ in 0..s.reps.max(1) {
            let t = Instant::now();
            let found = read(&index).search(
                &SearchParams {
                    tokens: &tokens,
                    kind: parsed.kind.as_ref(),
                    limit: 50,
                    now,
                    weights,
                    time: None,
                    exts: &[],
                },
                &|| false,
            );
            search_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            hits = found.len();
        }

        let mut first_ms = Vec::with_capacity(s.reps);
        let mut done_ms = Vec::with_capacity(s.reps);
        for _ in 0..s.reps.max(1) {
            generation += 1;
            let t0 = Instant::now();
            engine.submit(generation, text);
            let mut first = None;
            loop {
                let (u, at) = rx
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|_| format!("no update for {text:?}"))?;
                if u.generation != generation {
                    continue;
                }
                let ms = at.duration_since(t0).as_secs_f64() * 1000.0;
                // "First results": the first update that has something to show
                // (or the final one, for queries with no matches).
                if first.is_none() && (!u.items.is_empty() || u.done) {
                    first = Some(ms);
                }
                if u.done {
                    first_ms.push(first.unwrap_or(ms));
                    done_ms.push(ms);
                    break;
                }
            }
        }
        let (Some(a), Some(b), Some(c)) = (
            summarize(&search_ms),
            summarize(&first_ms),
            summarize(&done_ms),
        ) else {
            return Err("no samples".into());
        };
        worst_first_p95 = worst_first_p95.max(b.p95);
        row(name, text, hits, &a, &b, &c);
    }

    // Fast typing: 18 keystrokes with no pause; nothing stale may arrive after the last one.
    println!("\nfast typing of \"annual budget xlsx\" (18 keystrokes back to back)");
    let phrase = "annual budget xlsx";
    let mut stale_after_final = 0usize;
    let mut final_first_ms = Vec::new();
    for _ in 0..s.reps.max(1) {
        while rx.try_recv().is_ok() {}
        let mut last_gen = 0;
        let mut t_last = Instant::now();
        for end in 1..=phrase.len() {
            generation += 1;
            last_gen = generation;
            t_last = Instant::now();
            engine.submit(generation, &phrase[..end]);
        }
        let mut first = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (u, at) = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| "typing burst never completed".to_owned())?;
            if u.generation == last_gen {
                if first.is_none() && (!u.items.is_empty() || u.done) {
                    first = Some(at.duration_since(t_last).as_secs_f64() * 1000.0);
                }
                if u.done {
                    break;
                }
            } else if first.is_some() {
                // An older generation arriving after the final one has started
                // answering is what would visibly flash over newer results.
                stale_after_final += 1;
            }
        }
        final_first_ms.extend(first);
    }
    if let Some(st) = summarize(&final_first_ms) {
        println!(
            "last keystroke to first update: p50 {:.2}  p95 {:.2}  max {:.2} ms",
            st.p50, st.p95, st.max
        );
    }
    println!(
        "updates for older generations delivered after the final query began answering: {stale_after_final}"
    );

    let ok = worst_first_p95 < FIRST_RESULT_BUDGET_MS && stale_after_final == 0;
    println!(
        "\nbudget: first update p95 < {FIRST_RESULT_BUDGET_MS} ms for every query, and no stale updates -> {} (worst p95 {:.2} ms)",
        if ok { "PASS" } else { "FAIL" },
        worst_first_p95
    );
    Ok(ok)
}
