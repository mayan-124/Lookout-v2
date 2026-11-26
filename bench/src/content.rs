//! Content-indexing speed benchmark (in this process, on a generated folder).
//!
//! - `bench content --mode now|idle [--files N]`: indexes a mixed folder
//!   (text, markdown, json, screenshots) and reports files per second, time per
//!   stage, CPU, threads and memory. `now` is "Index now"; `idle` is the normal
//!   background mode (idle gate open, duty cap at the default).
//! - `bench content --mode segments [--commits N]`: N single-file instant
//!   commits, then the segment count of the index.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lookout_core::actions::open::NoopLauncher;
use lookout_core::actions::GateConfig;
use lookout_core::content::idle::ManualIdle;
use lookout_core::content::ocr::{Ocr, WindowsOcr};
use lookout_core::crypto::Key;
use lookout_core::engine::Sink;
use lookout_core::fixture_image;
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::settings::Settings;
use lookout_core::storage::{DataPaths, Storage};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

use crate::memory::{process_cpu_seconds, self_usage};

const WORDS: &[&str] = &[
    "budget",
    "invoice",
    "meeting",
    "project",
    "quarterly",
    "report",
    "schedule",
    "travel",
    "contract",
    "notes",
    "design",
    "review",
    "customer",
    "revenue",
    "plan",
    "draft",
    "summary",
    "agenda",
    "policy",
    "release",
    "backlog",
    "sprint",
    "ticket",
    "deploy",
    "archive",
];

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn thread_count() -> u32 {
    // SAFETY: a plain toolhelp snapshot walk; the handle is closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut n = 0;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            if e.th32ProcessID == std::process::id() {
                n = e.cntThreads;
                break;
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
        n
    }
}

fn words(seed: &mut u64, bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 16);
    while s.len() < bytes {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        s.push_str(WORDS[(*seed >> 33) as usize % WORDS.len()]);
        s.push(' ');
    }
    s
}

/// ~44% text, 26% markdown, 25% json, 5% screenshots; every 40th text file
/// holds a fake secret so masking has work to do.
fn make_corpus(root: &Path, files: usize) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let images = files / 20;
    for i in 0..files {
        let dir = root.join(format!("folder{:02}", i % 40));
        fs::create_dir_all(&dir).map_err(io)?;
        let mut seed = (i as u64 + 1) * 2_654_435_761;
        let size = 1000 + (i * 7919) % 14_000;
        if i < images {
            let word = WORDS[i % WORDS.len()].to_uppercase();
            let data = fixture_image::bmp(&format!("{word} {}", 1000 + i), 6);
            fs::write(dir.join(format!("shot{i:05}.bmp")), data).map_err(io)?;
            continue;
        }
        let (name, body) = match i % 4 {
            0 | 1 => {
                let mut b = words(&mut seed, size);
                if i % 40 == 0 {
                    b.push_str("\npassword = hunter2hunter2hunter2\n");
                }
                (format!("note{i:05}.txt"), b)
            }
            2 => (
                format!("doc{i:05}.md"),
                format!("# Title {i}\n\n{}\n\n- item\n- item\n", words(&mut seed, size)),
            ),
            _ => (
                format!("data{i:05}.json"),
                format!(
                    "{{\"id\": {i}, \"name\": \"{}\", \"tags\": [\"{}\", \"{}\"], \"text\": \"{}\"}}",
                    WORDS[i % WORDS.len()],
                    WORDS[(i + 3) % WORDS.len()],
                    WORDS[(i + 7) % WORDS.len()],
                    words(&mut seed, size)
                ),
            ),
        };
        fs::write(dir.join(name), body).map_err(io)?;
    }
    Ok(())
}

fn start(
    base: &Path,
    root: &Path,
    idle: Arc<ManualIdle>,
    idle_secs: u32,
    speed: lookout_core::settings::IndexSpeed,
    with_office: bool,
) -> Result<Lookout, String> {
    let paths = DataPaths::new(base.join("data"));
    let plugins = if with_office {
        Some(install_office_helper(base)?)
    } else {
        None
    };
    let sink: Sink = Arc::new(|_| {});
    Lookout::start(
        StartOptions {
            settings: Settings {
                idle_secs,
                index_speed: speed,
                first_run_done: true,
                ..Settings::default() // duty cap stays at the default
            },
            roots_override: vec![root.to_path_buf()],
            synthetic_files: None,
            synthetic_uses_folders: false,
            start_menu_dirs: vec![],
            store_apps: false,
            storage: Storage::with_key(paths, Key::from_bytes(&[5u8; 32]).ok_or("bad key")?)
                .map_err(|e| e.to_string())?,
            idle,
            ocr: Arc::new(WindowsOcr),
            launcher: Arc::new(NoopLauncher),
            system: None,
            gate: GateConfig::default(),
            plugins,
        },
        sink,
        PauseState::new(),
    )
    .map_err(|e| e.to_string())
}

/// The Office helper built next to this exe, installed into `base` and turned on.
fn install_office_helper(base: &Path) -> Result<lookout_core::plugins::HostConfig, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let built = exe
        .parent()
        .ok_or("no exe folder")?
        .join("lookout-office-text.exe");
    let dir = base.join("plugins").join("office-text");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::copy(&built, dir.join("lookout-office-text.exe"))
        .map_err(|e| format!("build lookout-office-text next to the bench first: {e}"))?;
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/office-text/plugin.json");
    fs::copy(manifest, dir.join("plugin.json")).map_err(|e| e.to_string())?;
    Ok(lookout_core::plugins::HostConfig::new(
        vec![base.join("plugins")],
        base.join("data"),
    ))
}

fn flag<T: std::str::FromStr>(
    args: &mut impl Iterator<Item = String>,
    what: &str,
) -> Result<T, String> {
    args.next()
        .and_then(|v| v.parse().ok())
        .ok_or(format!("{what} needs a value"))
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut mode = "now".to_owned();
    let mut files = 5000usize;
    let mut commits = 500usize;
    let mut speed = lookout_core::settings::IndexSpeed::Balanced;
    let mut dir = String::new();
    let mut max_secs = 600u64;
    let mut deep_idle = false;
    let mut gated = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--deep-idle" => deep_idle = true,
            "--gated" => gated = true,
            "--max-secs" => max_secs = flag(&mut args, "--max-secs")?,
            "--mode" => mode = flag(&mut args, "--mode")?,
            "--files" => files = flag(&mut args, "--files")?,
            "--commits" => commits = flag(&mut args, "--commits")?,
            "--dir" => dir = flag(&mut args, "--dir")?,
            "--speed" => {
                speed = match flag::<String>(&mut args, "--speed")?.as_str() {
                    "gentle" => lookout_core::settings::IndexSpeed::Gentle,
                    "balanced" => lookout_core::settings::IndexSpeed::Balanced,
                    "fast" => lookout_core::settings::IndexSpeed::Fast,
                    _ => return Err("--speed is gentle, balanced or fast".into()),
                }
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if mode == "segsearch" {
        return seg_search(files);
    }
    if mode == "ocr" {
        return ocr_timing(Path::new(&dir));
    }
    let base = std::env::temp_dir().join(format!("lookout-bench-content-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let _guard = Scratch(base.clone());
    let real = mode == "real";
    let root = if real {
        PathBuf::from(&dir) // read-only: only this process's own temp folder is ever deleted
    } else {
        base.join("corpus")
    };
    if !real {
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    }
    if mode == "segments" {
        return segments(&base, &root, commits);
    }
    if !real {
        make_corpus(&root, files)?;
    }

    let idle = Arc::new(ManualIdle::default()); // the user is active: nothing runs yet
    let name_t = Instant::now();
    let lookout = start(&base, &root, idle.clone(), 10, speed, real)?;
    if real {
        let host = lookout.plugins().ok_or("no plugin host")?;
        host.set_enabled(lookout_core::content::office::PLUGIN_ID, true, true)
            .map_err(|e| e.to_string())?;
    }
    while lookout.status().indexing {
        std::thread::sleep(Duration::from_millis(100));
    }
    if real {
        let st = lookout.status();
        let secs = name_t.elapsed().as_secs_f64();
        let n = (st.files + st.folders) as f64;
        println!(
            "name walk (name index, incl. start-up): {secs:.1} s, {} entries = {:.0} entries/s",
            st.files + st.folders,
            n / secs
        );
    }
    std::thread::sleep(Duration::from_millis(500));
    let stats = lookout.content_stats().ok_or("no content worker")?;
    let ocr_ok = WindowsOcr.available();

    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    let before = self_usage()?;
    let t0 = Instant::now();
    let cpu0 = process_cpu_seconds();
    match mode.as_str() {
        "now" | "real" => {
            if gated {
                // Normal idle-gated reading: idle past the gate, no "Check files now".
                idle.set(Duration::from_secs(30));
            } else {
                if deep_idle {
                    idle.set(Duration::from_secs(3600)); // away for an hour: Fast may use more threads
                }
                lookout.index_now(true);
            }
        }
        "idle" => idle.set(Duration::from_secs(3600)),
        _ => return Err("--mode is now, idle or segments".into()),
    }
    let (mut last_cpu, mut last_t) = (cpu0, t0);
    let (mut peak_cpu, mut peak_threads, mut peak_ws) = (0.0f64, 0u32, before.private_ws);
    let mut samples = 0u32;
    std::thread::sleep(Duration::from_millis(400));
    loop {
        let now = Instant::now();
        let cpu = process_cpu_seconds();
        let dt = now.duration_since(last_t).as_secs_f64();
        if dt > 0.0 {
            peak_cpu = peak_cpu.max((cpu - last_cpu) / dt);
        }
        (last_cpu, last_t) = (cpu, now);
        peak_threads = peak_threads.max(thread_count());
        if samples.is_multiple_of(2) {
            if let Ok(u) = self_usage() {
                peak_ws = peak_ws.max(u.private_ws); // a failed sample (a fast-growing process) is skipped
            }
        }
        samples += 1;
        let h = lookout.health().content;
        let finished = !stats.busy.load(Ordering::Acquire)
            && h.files_queued == 0
            && !h.index_now_running
            && stats.docs.load(Ordering::Relaxed) > 0;
        if finished {
            break;
        }
        if real && t0.elapsed() > Duration::from_secs(max_secs) {
            println!("(stopped after the {max_secs} s limit; rates below are for that time)");
            break;
        }
        if t0.elapsed() > Duration::from_secs(3600) {
            return Err("timed out after 1 h".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let wall = t0.elapsed().as_secs_f64();
    let cpu = process_cpu_seconds() - cpu0;
    let docs = stats.docs.load(Ordering::Relaxed);
    let st = &stats.stages;
    let ms = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed) as f64 / 1e6;
    println!("mode={mode} speed={speed:?} files={files} OCR available={ocr_ok}");
    println!(
        "indexed {docs} docs ({} text reads, {} OCR runs, {} errors) in {wall:.1} s = {:.1} files/s",
        stats.extractions.load(Ordering::Relaxed),
        stats.ocr_runs.load(Ordering::Relaxed),
        stats.errors.load(Ordering::Relaxed),
        docs as f64 / wall
    );
    println!("last error: {:?}", stats.last_error());
    for (n, files, ns) in [
        (
            "text files",
            st.text_files.load(Ordering::Relaxed),
            st.text_ns.load(Ordering::Relaxed),
        ),
        (
            "docx/xlsx/pptx",
            st.office_files.load(Ordering::Relaxed),
            st.office_ns.load(Ordering::Relaxed),
        ),
        (
            "pdf",
            st.pdf_files.load(Ordering::Relaxed),
            st.pdf_ns.load(Ordering::Relaxed),
        ),
        (
            "OCR images",
            stats.ocr_runs.load(Ordering::Relaxed),
            st.ocr.load(Ordering::Relaxed),
        ),
    ] {
        let s = ns as f64 / 1e9;
        println!(
            "  {n:<16}{files:>8} files {s:>8.1} s  {:>8.1} files/s",
            if s > 0.0 { files as f64 / s } else { 0.0 }
        );
    }
    println!("stage ms (sum over the run, single worker):");
    for (n, v) in [
        ("directory walk", ms(&st.walk)),
        ("unchanged check", ms(&st.unchanged_check)),
        ("read bytes", ms(&st.read)),
        ("decode text", ms(&st.extract)),
        ("secret masking", ms(&st.mask)),
        ("OCR", ms(&st.ocr)),
        ("tantivy add/delete", ms(&st.write)),
        ("tantivy commit+reload", ms(&st.commit)),
        ("duty-cap sleep", ms(&st.throttle)),
    ] {
        println!("  {n:<22}{v:>10.0} ms {:>5.1}%", v / (wall * 10.0));
    }
    println!(
        "CPU: avg {:.1}% of one core = {:.2}% of machine ({cores} logical cores); peak {:.0}% of one core = {:.1}% of machine",
        cpu / wall * 100.0,
        cpu / wall * 100.0 / cores,
        peak_cpu * 100.0,
        peak_cpu * 100.0 / cores
    );
    println!(
        "threads: peak {peak_threads} (process total; content worker is 1); private working set: before {:.0} MB, peak {:.0} MB",
        before.private_ws as f64 / 1048576.0,
        peak_ws as f64 / 1048576.0
    );
    lookout.shutdown();
    Ok(true)
}

fn segments(base: &Path, root: &Path, commits: usize) -> Result<bool, String> {
    fs::write(root.join("seed.txt"), "seed words").map_err(|e| e.to_string())?;
    let idle = Arc::new(ManualIdle::default());
    idle.set(Duration::from_secs(3600));
    let lookout = start(
        base,
        root,
        idle,
        0,
        lookout_core::settings::IndexSpeed::Balanced,
        false,
    )?;
    let stats = lookout.content_stats().ok_or("no content worker")?;
    std::thread::sleep(Duration::from_secs(2));
    let t0 = Instant::now();
    println!(
        "before: docs counter {}, index docs {}",
        stats.docs.load(Ordering::Relaxed),
        lookout.status().content_docs
    );
    for i in 0..commits {
        let before = stats.docs.load(Ordering::Relaxed);
        fs::write(
            root.join(format!("n{i:04}.txt")),
            format!("single file number{i}"),
        )
        .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while stats.docs.load(Ordering::Relaxed) == before {
            if Instant::now() > deadline {
                return Err(format!("file {i} was not indexed in 10 s"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    std::thread::sleep(Duration::from_millis(500)); // the last commit lands after its upsert
    println!(
        "{commits} instant commits in {:.0} s; segments now: {}; docs: {}",
        t0.elapsed().as_secs_f64(),
        lookout.content_segments(),
        lookout.status().content_docs
    );
    lookout.shutdown();
    Ok(true)
}

/// OCR time per image for every PNG in `dir`.
fn ocr_timing(dir: &Path) -> Result<bool, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| ["png", "jpg", "jpeg"].contains(&x.to_ascii_lowercase().as_str()))
        })
        .collect();
    files.sort();
    let ocr = WindowsOcr;
    if !ocr.available() {
        return Err("no OCR language installed".into());
    }
    let _ = ocr.recognize(&files[0]); // warm-up: engine start is not per-image cost
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let peak = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sampler = {
        let (stop, peak) = (stop.clone(), peak.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(u) = self_usage() {
                    peak.fetch_max(u.private_ws, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let mut ms = Vec::new();
    let mut words = 0;
    for f in &files {
        let t = Instant::now();
        let text = ocr.recognize(f).map_err(|e| e.to_string())?;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        words += text.split_whitespace().count();
    }
    stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();
    println!(
        "peak private working set {} MB",
        peak.load(Ordering::Relaxed) >> 20
    );
    ms.sort_by(f64::total_cmp);
    println!(
        "{} images: OCR ms per image: min {:.0}, median {:.0}, max {:.0}, mean {:.0}; {} words read in total",
        ms.len(),
        ms[0],
        ms[ms.len() / 2],
        ms[ms.len() - 1],
        ms.iter().sum::<f64>() / ms.len() as f64,
        words
    );
    Ok(true)
}

/// Query latency on 5,000 documents split into 3 and into 160 segments.
fn seg_search(files: usize) -> Result<bool, String> {
    use lookout_core::content::store::{ContentDoc, ContentQuery, ContentStore, DocKind};
    let queries: Vec<String> = (0..20)
        .map(|i| match i % 3 {
            0 => WORDS[i % WORDS.len()].to_owned(),
            1 => format!(
                "{} {}",
                WORDS[i % WORDS.len()],
                WORDS[(i + 5) % WORDS.len()]
            ),
            _ => WORDS[i % WORDS.len()][..4].to_owned(),
        })
        .collect();
    for segments in [3usize, 160] {
        let store = ContentStore::open_in_ram().map_err(|e| e.to_string())?;
        let per = files.div_ceil(segments);
        for i in 0..files {
            let mut seed = (i as u64 + 1) * 2_654_435_761;
            store
                .upsert(&ContentDoc {
                    path: format!(r"C:\bench\f{i:05}.txt"),
                    mtime: 1_700_000_000,
                    kind: DocKind::Text,
                    size: 0,
                    text: words(&mut seed, 1000 + (i * 7919) % 14_000),
                })
                .map_err(|e| e.to_string())?;
            if (i + 1) % per == 0 || i + 1 == files {
                store.commit_unmerged().map_err(|e| e.to_string())?;
            }
        }
        let mut ms = Vec::new();
        for rep in 0..4 {
            for q in &queries {
                let t = Instant::now();
                let hits = store
                    .search(&ContentQuery {
                        text: q,
                        limit: 10,
                        ..ContentQuery::default()
                    })
                    .map_err(|e| e.to_string())?;
                let d = t.elapsed().as_secs_f64() * 1000.0;
                assert!(!hits.is_empty());
                if rep > 0 {
                    ms.push(d); // first round warms caches
                }
            }
        }
        ms.sort_by(f64::total_cmp);
        println!(
            "{} segments ({} docs): {} queries, median {:.2} ms, p95 {:.2} ms, max {:.2} ms",
            store.segment_count(),
            store.doc_count(),
            ms.len(),
            ms[ms.len() / 2],
            ms[ms.len() * 95 / 100],
            ms[ms.len() - 1]
        );
    }
    Ok(true)
}
