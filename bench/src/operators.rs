//! `bench operators [--files N] [--reps N]`: the whole engine round trip for
//! operator queries, over a synthetic name index and a full-text index (the
//! encrypted on-disk one) with one short document per file.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lookout_core::content::source::ContentSource;
use lookout_core::content::store::{ContentDoc, ContentStore, DocKind};
use lookout_core::crypto::Key;
use lookout_core::engine::{Engine, EngineConfig, Sink, Update};
use lookout_core::index_service::{new_shared_index, read, unix_now, write};
use lookout_core::latency::summarize;
use lookout_core::ranking::RankingWeights;
use lookout_core::sources::{FilesSource, Source};
use lookout_core::synthetic;

/// p95 of the complete answer to an operator query.
const BUDGET_MS: f64 = 15.0;

const QUERIES: &[&str] = &[
    "ext:pdf budget",
    "in:downloads report",
    "size:>5mb",
    "name:invoice",
    "ext:pdf size:>1mb tax",
];

/// Body words; the queried ones are four of forty.
const VOCABULARY: &[&str] = &[
    "budget", "report", "invoice", "tax", "meeting", "project", "schedule", "travel", "contract",
    "notes", "design", "review", "customer", "revenue", "plan", "draft", "summary", "agenda",
    "policy", "release", "backlog", "sprint", "ticket", "deploy", "archive", "family", "holiday",
    "garden", "recipe", "lecture", "thesis", "chapter", "letter", "receipt", "booking", "banner",
    "export", "import", "logo", "roadmap",
];

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mix(n: u64) -> u64 {
    let z = n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^ (z >> 27)
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let (mut files, mut reps) = (500_000usize, 100usize);
    while let Some(a) = args.next() {
        let n = args
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("{a} needs a number"))?;
        match a.as_str() {
            "--files" => files = n,
            "--reps" => reps = n,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let now = unix_now();
    let index = new_shared_index();
    {
        let mut w = write(&index);
        // One file in twenty-five lies in a Downloads folder.
        let downloads = files / 25;
        synthetic::populate(&mut w, files - downloads, 42, now).map_err(|e| e.to_string())?;
        let dir = w.add_dir(0, "Downloads", now).map_err(|e| e.to_string())?;
        for i in 0..downloads {
            let r = mix(i as u64);
            let word = VOCABULARY[(r % VOCABULARY.len() as u64) as usize];
            let ext = ["pdf", "docx", "zip", "jpg"][(r >> 20) as usize % 4];
            w.add_sized_file(
                dir,
                &format!("{word} {i}.{ext}"),
                now - (r % 50_000_000) as u32,
                synthetic::file_bytes(r),
            )
            .map_err(|e| e.to_string())?;
        }
        w.shrink_to_fit();
    }

    let scratch = Scratch(
        std::env::temp_dir().join(format!("lookout-bench-operators-{}", std::process::id())),
    );
    let key = Key::generate().map_err(|e| e.to_string())?;
    let store =
        Arc::new(ContentStore::open(&scratch.0.join("content"), &key).map_err(|e| e.to_string())?);
    let built = Instant::now();
    let mut documents = 0u64;
    {
        let r = read(&index);
        let c = r.counts();
        for entry in 0..(c.files + c.folders) as u32 {
            let Some(info) = r.entry_info(entry).filter(|i| !i.is_dir) else {
                continue;
            };
            let seed = u64::from(entry);
            let text: Vec<&str> = (0..40)
                .map(|n| VOCABULARY[(mix(seed * 41 + n) % VOCABULARY.len() as u64) as usize])
                .collect();
            let doc = ContentDoc {
                path: info.path.to_string_lossy().into_owned(),
                mtime: u64::from(info.modified),
                kind: DocKind::Text,
                size: synthetic::file_bytes(seed),
                text: text.join(" "),
            };
            store.upsert(&doc).map_err(|e| e.to_string())?;
            documents += 1;
        }
    }
    store.commit().map_err(|e| e.to_string())?;
    println!(
        "{files} names; {documents} documents of 40 words indexed in {:.0} s, {} segments, {:.0} MB",
        built.elapsed().as_secs_f64(),
        store.segment_count(),
        store.size_bytes() as f64 / 1_048_576.0
    );

    let weights = RankingWeights::default();
    let sources: Vec<Arc<dyn Source>> = vec![
        Arc::new(FilesSource::new(index.clone(), weights)),
        Arc::new(ContentSource::new(store, weights)),
    ];
    let (tx, rx) = mpsc::channel::<(Update, Instant)>();
    let sink: Sink = Arc::new(move |u| {
        let _ = tx.send((u, Instant::now()));
    });
    let engine = Engine::new(sources, sink, EngineConfig::default()).map_err(|e| e.to_string())?;

    println!("times in ms, {reps} repetitions each");
    println!(
        "{:<26}{:>5} | {:^21} | {:^21}",
        "query", "rows", "first update", "complete"
    );
    println!(
        "{:<26}{:>5} | {:>6}{:>7}{:>7} | {:>6}{:>7}{:>7}",
        "", "", "p50", "p95", "max", "p50", "p95", "max"
    );
    let mut generation = 0u64;
    let mut worst = 0.0f64;
    for text in QUERIES {
        let (mut first_ms, mut done_ms, mut rows) = (Vec::new(), Vec::new(), 0);
        for _ in 0..reps.max(1) {
            generation += 1;
            let started = Instant::now();
            engine.submit(generation, text);
            let mut first = None;
            loop {
                let (u, at) = rx
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|_| format!("no update for {text:?}"))?;
                if u.generation != generation {
                    continue;
                }
                let ms = at.duration_since(started).as_secs_f64() * 1000.0;
                if first.is_none() && (!u.items.is_empty() || u.done) {
                    first = Some(ms);
                }
                if u.done {
                    rows = u.items.len();
                    first_ms.push(first.unwrap_or(ms));
                    done_ms.push(ms);
                    break;
                }
            }
        }
        let (Some(a), Some(b)) = (summarize(&first_ms), summarize(&done_ms)) else {
            return Err("no samples".into());
        };
        worst = worst.max(b.p95);
        println!(
            "{text:<26}{rows:>5} | {:>6.2}{:>7.2}{:>7.2} | {:>6.2}{:>7.2}{:>7.2}",
            a.p50, a.p95, a.max, b.p50, b.p95, b.max
        );
    }
    let ok = worst < BUDGET_MS;
    println!(
        "budget: complete p95 < {BUDGET_MS} ms for every query -> {} (worst {worst:.2} ms)",
        if ok { "PASS" } else { "FAIL" }
    );
    Ok(ok)
}
