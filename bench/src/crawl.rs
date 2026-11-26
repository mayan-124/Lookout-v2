//! Crawl, locking and watcher benchmarks. These create files only inside a
//! fresh folder under the system temp directory, never anywhere else.
//!
//! - `bench crawl`: real crawl of a generated tree at several duty cycles,
//!   with query latency measured while the crawl runs.
//! - `bench locks`: how long index writers hold the lock, and how long a
//!   writer waits behind scanning readers, on a 1M-name index.
//! - `bench watch`: how many of a burst of new files the watcher reports.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lookout_core::exclude::Excludes;
use lookout_core::index_service::{
    new_shared_index, read, unix_now, write, IndexConfig, IndexService, SharedIndex,
};
use lookout_core::latency::{summarize, Stats};
use lookout_core::name_index::SearchParams;
use lookout_core::pause::PauseState;
use lookout_core::ranking::RankingWeights;
use lookout_core::synthetic;

use crate::memory::process_cpu_seconds;

const FILES_PER_DIR: usize = 50;

fn parse_flag<T: std::str::FromStr>(
    args: &mut impl Iterator<Item = String>,
    what: &str,
) -> Result<T, String> {
    args.next()
        .and_then(|v| v.parse().ok())
        .ok_or(format!("{what} needs a value"))
}

/// A scratch folder that deletes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Result<Self, String> {
        let p = std::env::temp_dir().join(format!("lookout-bench-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        Ok(Self(p))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `files` empty files, 50 per directory, directories grouped 1000 per parent.
pub(crate) fn make_tree(root: &Path, files: usize) -> Result<(), String> {
    let dirs = files.div_ceil(FILES_PER_DIR);
    let mut made = 0usize;
    for d in 0..dirs {
        let dir = root.join(format!("g{:03}", d / 1000)).join(format!("d{d}"));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for _ in 0..FILES_PER_DIR.min(files - made) {
            fs::File::create(dir.join(format!("file {made}.txt"))).map_err(|e| e.to_string())?;
            made += 1;
        }
    }
    Ok(())
}

fn search_ms(index: &SharedIndex, tokens: &[Vec<u8>]) -> f64 {
    let t = Instant::now();
    let _ = read(index).search(
        &SearchParams {
            tokens,
            kind: None,
            limit: 50,
            now: unix_now(),
            weights: RankingWeights::default(),
            time: None,
            exts: &[],
            filter: None,
        },
        &|| false,
    );
    t.elapsed().as_secs_f64() * 1000.0
}

fn fmt_stats(s: &Option<Stats>) -> String {
    s.map_or_else(
        || "n/a".to_owned(),
        |s| {
            format!(
                "n {:>6}  p50 {:>6.2}  p95 {:>6.2}  max {:>7.2} ms",
                s.count, s.p50, s.p95, s.max
            )
        },
    )
}

pub fn run_crawl(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut files = 1_000_000usize;
    let mut duties: Vec<f32> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--files" => files = parse_flag(&mut args, "--files")?,
            "--duty" => duties.push(parse_flag(&mut args, "--duty")?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if duties.is_empty() {
        duties = vec![1.0, 0.1];
    }

    let scratch = Scratch::new("crawl")?;
    println!(
        "creating {files} empty files under {} ...",
        scratch.0.display()
    );
    let t = Instant::now();
    make_tree(&scratch.0, files)?;
    println!(
        "created in {:.1} s ({:.0} files/s)\n",
        t.elapsed().as_secs_f64(),
        files as f64 / t.elapsed().as_secs_f64()
    );

    // Each duty cycle is crawled twice: once alone (clean CPU and time), once
    // with a user searching every 20 ms (query latency while the index is written).
    let runs: Vec<(f32, bool)> = duties
        .iter()
        .flat_map(|d| [(*d, false), (*d, true)])
        .collect();
    for (duty, probe) in runs {
        let index = new_shared_index();
        let mut cfg = IndexConfig::new(vec![scratch.0.clone()], Excludes::default());
        cfg.duty = duty;
        cfg.watch = false;

        // A user typing during the crawl: one query every 20 ms for the whole crawl.
        let stop = Arc::new(AtomicBool::new(false));
        let (qi, qs) = (index.clone(), stop.clone());
        let started = Instant::now();
        let prober = std::thread::spawn(move || {
            // A selective query ("file 99999.txt" is unique), like the idle benchmark's rare words.
            let tokens = vec![b"99999".to_vec()];
            let mut times = Vec::new();
            let mut first_hit = None;
            if !probe {
                while !qs.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                return (times, first_hit);
            }
            while !qs.load(Ordering::Relaxed) {
                let ms = search_ms(&qi, &tokens);
                times.push(ms);
                if first_hit.is_none() && read(&qi).counts().files > 0 {
                    first_hit = Some(started.elapsed());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            (times, first_hit)
        });

        let cpu0 = process_cpu_seconds();
        let t0 = Instant::now();
        let svc = IndexService::start(index.clone(), cfg, PauseState::new())
            .map_err(|e| e.to_string())?;
        println!(
            "duty {duty}: crawling (what the footer would show: files indexed, 'indexing...')"
        );
        let mut next_print = Duration::from_secs(5);
        while !svc.wait_for_crawl(Duration::from_millis(250)) {
            if t0.elapsed() >= next_print {
                let s = svc.status();
                println!(
                    "  {:>5.0} s  {:>9} files  {:>7} folders  indexing={}",
                    t0.elapsed().as_secs_f64(),
                    s.files,
                    s.folders,
                    s.crawling
                );
                next_print += Duration::from_secs(if duty < 0.5 { 30 } else { 5 });
            }
        }
        let wall = t0.elapsed();
        let cpu = process_cpu_seconds() - cpu0;
        stop.store(true, Ordering::Relaxed);
        let (times, first_hit) = prober.join().map_err(|_| "prober panicked".to_owned())?;
        let counts = read(&index).counts();
        println!(
            "  done: {} files + {} folders in {:.1} s = {:.0} files/s; process CPU {:.1} s over {:.1} s wall = {:.0}% of one core",
            counts.files,
            counts.folders,
            wall.as_secs_f64(),
            counts.files as f64 / wall.as_secs_f64(),
            cpu,
            wall.as_secs_f64(),
            100.0 * cpu / wall.as_secs_f64()
        );
        // Only the run with a searching user can see when the first file became searchable.
        if let Some(first) = first_hit {
            println!(
                "  first indexed file became searchable after {:.2} s",
                first.as_secs_f64()
            );
        }
        if probe {
            println!(
                "  query latency while crawling (includes waiting for the write lock): {}\n",
                fmt_stats(&summarize(&times))
            );
        } else {
            println!();
        }
        svc.stop();
    }
    Ok(true)
}

fn pct(label: &str, ms: &[f64]) {
    println!("  {label:<46}{}", fmt_stats(&summarize(ms)));
}

pub fn run_locks(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut files = 1_000_000usize;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--files" => files = parse_flag(&mut args, "--files")?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let index = new_shared_index();
    write(&index)
        .add_root(Path::new(r"C:\"), 0)
        .map_err(|e| e.to_string())?;
    synthetic::populate(&mut write(&index), files, 42, unix_now()).map_err(|e| e.to_string())?;
    println!(
        "index: {files} synthetic names ({:.1} MB of buffers)\n",
        read(&index).heap_bytes() as f64 / 1_048_576.0
    );

    println!("time a writer holds the write lock (single operation, ms):");
    let held = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed().as_secs_f64() * 1000.0
    };
    {
        let mut w = write(&index);
        println!(
            "  shrink_to_fit (run once after each crawl)      {:>8.2}",
            held(&mut || w.shrink_to_fit())
        );
    }
    // One representative watcher step: dedupe check plus insert, at full index size.
    let probe_dir = {
        let r = read(&index);
        r.dir_id_for_path(Path::new(r"C:\synthetic"))
            .ok_or("no synthetic root")?
    };
    let mut contains = Vec::new();
    let mut add = Vec::new();
    for i in 0..200 {
        let path = Path::new(r"C:\synthetic").join(format!("new burst file {i}.txt"));
        contains.push(held(&mut || {
            let _ = write(&index).contains_file(&path);
        }));
        add.push(held(&mut || {
            let _ = write(&index).add_file(probe_dir, &format!("new burst file {i}.txt"), 0);
        }));
    }
    pct("watcher: contains_file (scans the name arena)", &contains);
    pct("watcher: add_file", &add);
    let mut removes = Vec::new();
    for i in 0..50 {
        let path = Path::new(r"C:\synthetic").join(format!("new burst file {i}.txt"));
        removes.push(held(&mut || {
            write(&index).remove_path(&path);
        }));
    }
    pct("watcher: remove_path of one file", &removes);

    // Remove directory subtrees until a quarter of the entries are dead, then compact.
    let mut subtree = Vec::new();
    let mut id = 1u32;
    while read(&index).dead_ratio() < 0.25 && id < 4000 {
        let path = read(&index).dir_path(id);
        if let Some(p) = path {
            // C:\synthetic\<folder>: skip the roots themselves, remove first-level folders.
            if p.components().count() == 4 {
                subtree.push(held(&mut || {
                    write(&index).remove_path(&p);
                }));
            }
        }
        id += 1;
    }
    pct("remove a directory subtree (scans all entries)", &subtree);
    let dead = read(&index).dead_ratio();
    println!("  dead entries before compact: {:.0}%", dead * 100.0);
    println!(
        "  compact() (rebuilds both arenas)               {:>8.2}",
        held(&mut || write(&index).compact())
    );

    println!(
        "\nwriter wait while readers scan the index back to back (ms to acquire the write lock):"
    );
    for readers in [1usize, 4] {
        let stop = Arc::new(AtomicBool::new(false));
        let handles: Vec<_> = (0..readers)
            .map(|_| {
                let (i, s) = (index.clone(), stop.clone());
                std::thread::spawn(move || {
                    let tokens = vec![b"report".to_vec()];
                    let mut t = Vec::new();
                    while !s.load(Ordering::Relaxed) {
                        t.push(search_ms(&i, &tokens));
                    }
                    t
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(200));
        let mut waits = Vec::new();
        for i in 0..400 {
            let t = Instant::now();
            let mut w = write(&index);
            waits.push(t.elapsed().as_secs_f64() * 1000.0);
            let _ = w.add_file(probe_dir, &format!("lock probe {readers}-{i}.txt"), 0);
            drop(w);
            std::thread::sleep(Duration::from_millis(3));
        }
        stop.store(true, Ordering::Relaxed);
        let mut reads = Vec::new();
        for h in handles {
            reads.extend(h.join().map_err(|_| "reader panicked".to_owned())?);
        }
        pct(&format!("{readers} reader thread(s): writer wait"), &waits);
        pct(
            &format!("{readers} reader thread(s): scan time (reader side)"),
            &reads,
        );
    }
    Ok(true)
}

/// Private working set of this process, sampled every 250 ms across a rescan
/// of a real 1M-file index (the rescan builds a second index, then swaps).
pub fn run_rescan_memory(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut files = 1_000_000usize;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--files" => files = parse_flag(&mut args, "--files")?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let scratch = Scratch::new("rescanmem")?;
    println!(
        "creating {files} empty files under {} ...",
        scratch.0.display()
    );
    make_tree(&scratch.0, files)?;

    let index = new_shared_index();
    let mut cfg = IndexConfig::new(vec![scratch.0.clone()], Excludes::default());
    cfg.watch = false; // default 10% duty cycle, as in the app
    let svc =
        IndexService::start(index.clone(), cfg, PauseState::new()).map_err(|e| e.to_string())?;
    if !svc.wait_for_crawl(Duration::from_secs(3600)) {
        return Err("initial crawl did not finish".into());
    }
    std::thread::sleep(Duration::from_secs(3));
    let base = crate::memory::self_usage()?;
    println!(
        "steady state after the first crawl ({} files): private working set {:.1} MB, private bytes {:.1} MB, working set {:.1} MB\n",
        svc.status().files,
        mb(base.private_ws),
        mb(base.private_bytes),
        mb(base.working_set)
    );

    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let origin = Instant::now();
    let sampler = std::thread::spawn(move || {
        let mut out = Vec::new();
        while !s.load(Ordering::Relaxed) {
            if let Ok(u) = crate::memory::self_usage() {
                out.push((origin.elapsed().as_secs_f64(), u));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        out
    });

    std::thread::sleep(Duration::from_secs(1));
    let started = origin.elapsed().as_secs_f64();
    svc.request_rescan();
    let t = Instant::now();
    while svc.rescan_count() == 0 && t.elapsed() < Duration::from_secs(1800) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let finished = origin.elapsed().as_secs_f64();
    std::thread::sleep(Duration::from_secs(10)); // watch the memory settle after the swap
    stop.store(true, Ordering::Relaxed);
    let samples = sampler.join().map_err(|_| "sampler panicked".to_owned())?;
    svc.stop();

    let peak = samples
        .iter()
        .max_by_key(|(_, u)| u.private_ws)
        .ok_or("no samples")?;
    let peak_bytes = samples
        .iter()
        .map(|(_, u)| u.private_bytes)
        .max()
        .unwrap_or(0);
    let settled = samples.last().map(|(_, u)| u.private_ws).unwrap_or(0);
    println!(
        "rescan ran from t={started:.1} s to t={finished:.1} s ({:.1} s); {} samples at 250 ms",
        finished - started,
        samples.len()
    );
    println!("  t (s)   private working set (MB)");
    for (i, (ts, u)) in samples.iter().enumerate() {
        if i % 20 == 0 || (ts - peak.0).abs() < 0.01 {
            println!(
                "  {ts:>6.1}   {:>8.1}{}",
                mb(u.private_ws),
                if (ts - peak.0).abs() < 0.01 {
                    "   <- peak"
                } else {
                    ""
                }
            );
        }
    }
    println!(
        "\nsteady {:.1} MB -> peak {:.1} MB at t={:.1} s (+{:.1} MB); settled {:.1} MB 10 s after the swap; peak private bytes {:.1} MB",
        mb(base.private_ws),
        mb(peak.1.private_ws),
        peak.0,
        mb(peak.1.private_ws) - mb(base.private_ws),
        mb(settled),
        mb(peak_bytes)
    );
    Ok(true)
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

pub fn run_watch(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut files = 100_000usize;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--files" => files = parse_flag(&mut args, "--files")?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let scratch = Scratch::new("watch")?;
    fs::create_dir_all(scratch.0.join("burst")).map_err(|e| e.to_string())?;
    fs::write(scratch.0.join("seed.txt"), b"x").map_err(|e| e.to_string())?;
    let index = new_shared_index();
    let mut cfg = IndexConfig::new(vec![scratch.0.clone()], Excludes::default());
    cfg.duty = 1.0;
    let svc =
        IndexService::start(index.clone(), cfg, PauseState::new()).map_err(|e| e.to_string())?;
    svc.wait_for_crawl(Duration::from_secs(30));
    let baseline = svc.status().files;

    println!("creating {files} files in one watched folder as fast as possible ...");
    let t = Instant::now();
    // Eight writers into eight folders, to push the event rate well past what one thread can.
    let writers = 8usize;
    let handles: Vec<_> = (0..writers)
        .map(|w| {
            let dir = scratch.0.join("burst").join(format!("w{w}"));
            let share = files / writers + usize::from(w < files % writers);
            std::thread::spawn(move || -> Result<(), String> {
                fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                for i in 0..share {
                    fs::File::create(dir.join(format!("burst file {i}.txt")))
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            })
        })
        .collect();
    for h in handles {
        h.join().map_err(|_| "writer panicked".to_owned())??;
    }
    println!(
        "created in {:.1} s ({:.0} files/s)",
        t.elapsed().as_secs_f64(),
        files as f64 / t.elapsed().as_secs_f64()
    );

    // Wait until the index count has been stable for 4 seconds.
    let mut last = 0usize;
    let mut stable_since = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(300);
    while Instant::now() < deadline {
        let now = svc.status().files;
        if now != last {
            last = now;
            stable_since = Instant::now();
        } else if stable_since.elapsed() > Duration::from_secs(4) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let seen = last - baseline;
    println!(
        "watcher alone: indexed {seen} of {files} new files; missed {} ({:.1}%)",
        files - seen.min(files),
        100.0 * (files - seen.min(files)) as f64 / files as f64
    );

    let t = Instant::now();
    svc.request_rescan();
    while svc.rescan_count() == 0 && t.elapsed() < Duration::from_secs(300) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let after = svc.status().files - baseline;
    println!(
        "after rescan ({:.1} s): indexed {after} of {files} new files; missed {}",
        t.elapsed().as_secs_f64(),
        files - after.min(files)
    );
    svc.stop();
    Ok(after == files)
}
