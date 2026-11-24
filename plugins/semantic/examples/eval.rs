//! Evaluation of semantic search on generated fixtures (never real files).
//!
//!   cargo build --release -p lookout-semantic
//!   cargo run --release -p lookout-semantic --example eval -- run <assets-dir>
//!   cargo run --release -p lookout-semantic --example eval -- embed <model.onnx> <assets-dir> <out.json>
//!   cargo run --release -p lookout-semantic --example eval -- compare <a.json> <b.json>
//!
//! `run` builds a temporary folder from `fixtures/semantic/corpus.json`, starts
//! the real engine with content indexing, OCR and the real helper process, and
//! scores the 50 queries of `queries.json`: keyword-only (helper off) against
//! keyword plus semantic. The minimum similarity is chosen on the `tune`
//! queries only; `holdout` queries are scored once with that choice.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lookout_core::actions::open::NoopLauncher;
use lookout_core::actions::GateConfig;
use lookout_core::content::idle::AlwaysIdle;
use lookout_core::content::ocr::WindowsOcr;
use lookout_core::crypto::Key;
use lookout_core::engine::{Sink, Update};
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::plugins::HostConfig;
use lookout_core::semantic::{SemanticState, PLUGIN_ID};
use lookout_core::settings::Settings;
use lookout_core::storage::{DataPaths, Storage};
use serde_json::{json, Value};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn load(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(repo().join(rel)).unwrap()).unwrap()
}

#[cfg(windows)]
fn private_ws(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::K32QueryWorkingSet;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };
    // SAFETY: plain Win32 calls; the buffer is sized as the API documents
    // ([count, entries...] of usize) and the handle is closed.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut capacity = 1usize << 16;
        let mut out = None;
        for _ in 0..8 {
            let mut buf = vec![0usize; capacity + 1];
            let ok = K32QueryWorkingSet(
                h,
                buf.as_mut_ptr().cast(),
                (buf.len() * std::mem::size_of::<usize>()) as u32,
            );
            if ok != 0 {
                let n = buf[0].min(capacity);
                out = Some(buf[1..=n].iter().filter(|e| (**e >> 8) & 1 == 0).count() as u64 * 4096);
                break;
            }
            capacity = buf[0] + 4096;
        }
        CloseHandle(h);
        out
    }
}

#[cfg(not(windows))]
fn private_ws(_: u32) -> Option<u64> {
    None
}

fn mb(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

struct Run {
    lookout: Lookout,
    updates: Arc<Mutex<HashMap<u64, Update>>>,
    next: u64,
}

impl Run {
    fn ask(&mut self, text: &str) -> (Vec<String>, Duration) {
        self.next += 1;
        let g = self.next;
        let t = Instant::now();
        assert!(self.lookout.query(g, text));
        loop {
            if let Some(u) = self.updates.lock().unwrap().get(&g).filter(|u| u.done) {
                let names = u
                    .items
                    .iter()
                    .map(|i| {
                        i.uri
                            .as_deref()
                            .and_then(|p| Path::new(p).file_name())
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    })
                    .collect();
                return (names, t.elapsed());
            }
            assert!(t.elapsed() < Duration::from_secs(30), "query timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn score(
    run: &mut Run,
    queries: &[&Value],
    names: &HashMap<String, String>,
) -> (usize, usize, usize) {
    let (mut top1, mut top5) = (0, 0);
    for q in queries {
        let want = &names[q["target"].as_str().unwrap()];
        let (got, _) = run.ask(q["q"].as_str().unwrap());
        if got.first() == Some(want) {
            top1 += 1;
        }
        if got.iter().take(5).any(|n| n == want) {
            top5 += 1;
        } else if std::env::var_os("EVAL_MISSES").is_some() {
            println!("    miss q{} {:?}", q["id"], q["q"]);
        }
    }
    (top1, top5, queries.len())
}

// ------------------------------------------------- distractors and keyword baseline

/// Topic word pools for the generated distractor documents.
const TOPICS: &[&str] = &[
    "flour butter oven recipe sugar dough simmer sauce garlic onion spice roast whisk bake",
    "airport ticket hotel passport luggage itinerary museum harbour tram booking visa suitcase sightseeing hostel",
    "invoice ledger audit receipts deduction filing refund payroll accountant quarterly expense balance deposit",
    "doctor symptoms prescription clinic dosage appointment allergy vaccine fever recovery nurse therapy checkup",
    "treadmill squat barbell stretching marathon protein cardio dumbbell hydration sprint stamina yoga warmup",
    "faucet wrench plumber drywall hammer ladder tiles paint bracket socket insulation sealant drill hinge",
    "seedlings compost watering pruning soil greenhouse harvest weeds mulch irrigation lawn shrubs trellis",
    "puppy leash vet kennel kitten grooming collar adoption feeding aquarium hamster parrot training treats",
    "python function compiler repository variable debugging module database query schema script library refactor",
    "server uptime firewall router bandwidth latency cluster backup outage deployment monitoring cache proxy",
    "password encryption phishing malware authentication patch breach firewall token credential audit vulnerability",
    "campaign audience newsletter branding budget slogan analytics launch influencer poster survey sponsorship",
    "interview resume promotion manager salary onboarding colleague deadline performance review mentoring hiring",
    "gravity molecule voltage quantum orbit particle momentum friction wavelength pressure velocity nucleus",
    "empire medieval revolution treaty dynasty printing crusade archive monarch colony manuscript conquest",
    "glacier emissions drought wetlands rainfall ecosystem renewable turbine pollution coastline biodiversity solar",
    "equation integral matrix theorem geometry probability fraction algebra proof triangle calculus vector",
    "novel chapter protagonist poetry author sequel plot publisher manuscript memoir narrator editor library",
    "cinema director soundtrack sequel actor screenplay festival trailer documentary animation premiere casting",
    "piano guitar chords rehearsal melody tempo concert orchestra lyrics violin metronome playlist choir",
    "balloons cake invitation guests catering speech toast venue decorations anniversary candles playlist reunion",
    "bride ceremony bouquet reception vows photographer florist rings seating honeymoon banquet bridesmaid dress",
    "engine brakes tyre mileage garage battery gearbox exhaust oil dashboard steering licence parking",
    "premium policy claim coverage deductible renewal liability broker warranty theft damages quote agent",
    "landlord tenant lease deposit mortgage rent apartment furniture neighbour utilities moving boxes keys",
    "savings budget pension dividend portfolio inflation index stocks bonds loan interest broker retirement",
    "startup founder prototype investor pitch customers market revenue funding roadmap subscription volunteers",
    "complaint refund shipment courier warranty replacement apology escalation helpdesk feedback ticket parcel",
    "insomnia pillow bedtime mattress dreams nap caffeine routine melatonin alarm snoring lullaby",
    "breathing mindfulness stress retreat journaling gratitude calm anxiety posture silence cushion chanting",
    "summit trail backpack compass ridge boots canyon waterfall sunrise campsite glacier binoculars trekking",
    "tent lantern sleeping stove firewood canoe fishing marshmallows hammock tarp flashlight campfire thermos",
    "lens aperture shutter portrait tripod exposure landscape editing filter flash gallery camera",
    "espresso beans grinder kettle latte roast barista teapot brewing filter cup mug",
    "chess opening checkmate puzzle tournament dice cards board strategy pawn trophy level",
    "telescope planet comet nebula galaxy eclipse satellite astronomer orbit meteor lunar rocket",
    "vocabulary grammar pronunciation fluency dialect phrasebook translation lesson tutor accent verbs idioms",
    "yarn needles stitches pattern scarf wool crochet fabric sewing thread seam embroidery",
    "sailboat anchor harbour tide mast navigation regatta lifejacket rope marina compass voyage",
    "contract clause lawyer lawsuit witness tribunal copyright patent signature compliance court settlement",
    "blueprint foundation concrete facade renovation architect roofing staircase permit scaffolding beams planning",
    "laptop battery screen keyboard charger firmware download storage update printer headphones tablet",
];

const FRAMES: &[&str] = &[
    "Notes on {x} and {y}. Need to sort out {z} before the end of the month.",
    "Reminder: {x} is the priority, then {y}; ask about {z} next week.",
    "Draft thoughts about {x}. The {y} part is unclear and {z} still needs a decision.",
    "Checklist for {x}: first {y}, then {z}, and keep a copy for later.",
    "Summary of the call about {x}. Agreed to revisit {y} and {z} on Monday.",
    "Ideas for {x}, maybe combine with {y}. The cost of {z} is the main worry.",
    "Something went wrong with {x} again. Check {y} and compare with {z} from last time.",
    "Comparing options: {x} against {y}, with {z} as the tie-breaker.",
];

/// `n` invented documents on 40 topics, the same every run.
fn distractors(n: usize) -> Vec<(String, String)> {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    (0..n)
        .map(|i| {
            let words: Vec<&str> = TOPICS[(next() % TOPICS.len() as u64) as usize]
                .split(' ')
                .collect();
            let mut text = String::new();
            for _ in 0..(2 + next() % 2) {
                let mut f = FRAMES[(next() % FRAMES.len() as u64) as usize].to_owned();
                for slot in ["{x}", "{y}", "{z}"] {
                    f = f.replace(slot, words[(next() % words.len() as u64) as usize]);
                }
                text.push_str(&f);
                text.push(' ');
            }
            (format!("doc-{i:05}.txt"), text.trim().to_owned())
        })
        .collect()
}

const STOP: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "in", "on", "for", "with", "at", "by", "from", "is",
    "are", "was", "were", "be", "it", "this", "that", "my", "me", "i", "we", "our", "how", "what",
    "when", "which", "do", "does", "not", "as", "so", "than", "then", "about", "into", "up", "out",
    "too", "very",
];

fn terms(text: &str, st: &rust_stemmers::Stemmer) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !STOP.contains(w))
        .map(|w| st.stem(w).into_owned())
        .collect()
}

/// The fair keyword baseline: any query word may match (OR), words are
/// stemmed with Snowball English and stop words dropped, documents ranked
/// by BM25 (k1 1.2, b 0.75). Returns file names, best first.
struct Bm25 {
    names: Vec<String>,
    tf: Vec<HashMap<String, f32>>,
    len: Vec<f32>,
    df: HashMap<String, f32>,
    avg: f32,
    stemmer: rust_stemmers::Stemmer,
}

impl Bm25 {
    fn new(docs: &[(String, String)]) -> Self {
        let stemmer = rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English);
        let mut df: HashMap<String, f32> = HashMap::new();
        let mut tf = Vec::new();
        let mut len = Vec::new();
        for (_, text) in docs {
            let mut m: HashMap<String, f32> = HashMap::new();
            let ts = terms(text, &stemmer);
            len.push(ts.len() as f32);
            for t in ts {
                *m.entry(t).or_default() += 1.0;
            }
            for t in m.keys() {
                *df.entry(t.clone()).or_default() += 1.0;
            }
            tf.push(m);
        }
        let avg = len.iter().sum::<f32>() / len.len().max(1) as f32;
        Self {
            names: docs.iter().map(|d| d.0.clone()).collect(),
            tf,
            len,
            df,
            avg,
            stemmer,
        }
    }

    fn rank(&self, q: &str) -> Vec<String> {
        let n = self.names.len() as f32;
        let qt = terms(q, &self.stemmer);
        let mut scored: Vec<(usize, f32)> = (0..self.names.len())
            .map(|i| {
                let s = qt
                    .iter()
                    .filter_map(|t| {
                        let f = *self.tf[i].get(t)?;
                        let d = self.df[t];
                        let idf = ((n - d + 0.5) / (d + 0.5) + 1.0).ln();
                        Some(idf * f * 2.2 / (f + 1.2 * (0.25 + 0.75 * self.len[i] / self.avg)))
                    })
                    .sum();
                (i, s)
            })
            .filter(|(_, s)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored
            .into_iter()
            .map(|(i, _)| self.names[i].clone())
            .collect()
    }
}

fn cmd_run(assets: &Path, extra: usize) {
    let corpus = load("fixtures/semantic/corpus.json");
    let queries_file = load("fixtures/semantic/queries.json");
    let queries: Vec<&Value> = queries_file["queries"].as_array().unwrap().iter().collect();
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let mut names: HashMap<String, String> = HashMap::new();
    for d in corpus["docs"].as_array().unwrap() {
        let name = d["name"].as_str().unwrap();
        std::fs::write(docs.join(name), d["text"].as_str().unwrap()).unwrap();
        names.insert(d["id"].as_str().unwrap().into(), name.into());
    }
    for s in corpus["screenshots"].as_array().unwrap() {
        let name = s["name"].as_str().unwrap();
        std::fs::write(
            docs.join(name),
            lookout_core::fixture_image::bmp(s["text"].as_str().unwrap(), 8),
        )
        .unwrap();
        names.insert(s["id"].as_str().unwrap().into(), name.into());
    }
    // Text of every file as the keyword baseline sees it (screenshots: the
    // text that was drawn, which is kinder to the baseline than OCR output).
    let mut texts: Vec<(String, String)> = Vec::new();
    for d in corpus["docs"].as_array().unwrap() {
        texts.push((
            d["name"].as_str().unwrap().into(),
            d["text"].as_str().unwrap().into(),
        ));
    }
    for s in corpus["screenshots"].as_array().unwrap() {
        texts.push((
            s["name"].as_str().unwrap().into(),
            s["text"].as_str().unwrap().into(),
        ));
    }
    for (name, text) in distractors(extra) {
        std::fs::write(docs.join(&name), &text).unwrap();
        texts.push((name, text));
    }

    // Plugin folder: the built helper and the manifest from the repository.
    let exe = std::env::current_exe().unwrap();
    let helper = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("lookout-semantic.exe");
    assert!(
        helper.exists(),
        "build the helper first: cargo build --release -p lookout-semantic"
    );
    let plugin = tmp.path().join("plugins").join("semantic");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::copy(&helper, plugin.join("lookout-semantic.exe")).unwrap();
    std::fs::copy(
        repo().join("plugins/semantic/plugin.json"),
        plugin.join("plugin.json"),
    )
    .unwrap();
    let paths = DataPaths::new(tmp.path().join("data"));
    let assets_dst = paths.root().join("plugins").join(PLUGIN_ID).join("assets");
    std::fs::create_dir_all(&assets_dst).unwrap();
    for f in ["model.onnx", "tokenizer.json", "onnxruntime.dll"] {
        std::fs::copy(assets.join(f), assets_dst.join(f)).unwrap();
    }

    let updates: Arc<Mutex<HashMap<u64, Update>>> = Arc::default();
    let u = updates.clone();
    let sink: Sink = Arc::new(move |up| {
        u.lock().unwrap().insert(up.generation, up);
    });
    let lookout = Lookout::start(
        StartOptions {
            settings: Settings {
                index_roots: vec![docs.clone()],
                index_contents: true,
                ocr: true,
                idle_secs: 0,
                crawl_duty: 1.0,
                first_run_done: true,
                ..Settings::default()
            },
            roots_override: vec![],
            synthetic_files: None,
            synthetic_uses_folders: false,
            start_menu_dirs: vec![],
            storage: Storage::with_key(paths.clone(), Key::generate().unwrap()).unwrap(),
            idle: Arc::new(AlwaysIdle),
            ocr: Arc::new(WindowsOcr),
            launcher: Arc::new(NoopLauncher),
            system: None,
            gate: GateConfig::default(),
            plugins: Some(HostConfig::new(
                vec![tmp.path().join("plugins")],
                paths.root().to_path_buf(),
            )),
        },
        sink,
        PauseState::new(),
    )
    .unwrap();
    let mut run = Run {
        lookout,
        updates,
        next: 0,
    };
    let total_files = (names.len() + extra) as u64;
    let t = Instant::now();
    while run.lookout.status().content_docs < total_files
        || run
            .lookout
            .content_stats()
            .is_some_and(|s| s.busy.load(std::sync::atomic::Ordering::Acquire))
    {
        assert!(
            t.elapsed() < Duration::from_secs(120),
            "content indexing did not finish"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "content index: {} documents of {total_files} files",
        run.lookout.status().content_docs
    );
    let me = std::process::id();
    let daemon_before = private_ws(me).unwrap_or(0);

    let split = |s: &str| -> Vec<&Value> {
        queries
            .iter()
            .copied()
            .filter(|q| q["split"] == s)
            .collect()
    };
    let (tune, holdout) = (split("tune"), split("holdout"));
    let overlap: Vec<&Value> = queries
        .iter()
        .copied()
        .filter(|q| q["overlap"] == true)
        .collect();

    // ---- keyword only (the plugin is off)
    let kw_tune = score(&mut run, &tune, &names);
    let kw_hold = score(&mut run, &holdout, &names);
    let kw_all = score(&mut run, &queries, &names);
    println!(
        "keyword only: tune top1 {}/{} top5 {}; holdout top1 {} top5 {}; all top1 {} top5 {}",
        kw_tune.0, kw_tune.2, kw_tune.1, kw_hold.0, kw_hold.1, kw_all.0, kw_all.1
    );

    // ---- fair keyword baseline: any word, stemmed, BM25 (same files)
    let bm = Bm25::new(&texts);
    let fair = |qs: &[&Value]| -> (usize, usize) {
        let (mut t1, mut t5) = (0, 0);
        for q in qs {
            let got = bm.rank(q["q"].as_str().unwrap());
            let want = &names[q["target"].as_str().unwrap()];
            t1 += usize::from(got.first() == Some(want));
            t5 += usize::from(got.iter().take(5).any(|n| n == want));
        }
        (t1, t5)
    };
    let (f_t, f_h, f_a) = (fair(&tune), fair(&holdout), fair(&queries));
    println!("fair keyword baseline (any word, stemmed, BM25) over {} files: tune top1 {} top5 {}; holdout top1 {} top5 {}; all top1 {} top5 {}",
        texts.len(), f_t.0, f_t.1, f_h.0, f_h.1, f_a.0, f_a.1);
    let (ov1, ov5) = fair(&overlap);
    println!("  fair baseline on the {} queries that share a word with their target: top1 {ov1} top5 {ov5}", overlap.len());

    // ---- turn the helper on and index, sampling its memory
    let host = run.lookout.plugins().unwrap().clone();
    host.set_config(PLUGIN_ID, json!({"min_similarity": 0.0}))
        .unwrap();
    host.set_enabled(PLUGIN_ID, true, true).unwrap();
    let t = Instant::now();
    let mut peak = 0u64;
    loop {
        if let Some(pid) = host.pid(PLUGIN_ID) {
            peak = peak.max(private_ws(pid).unwrap_or(0));
        }
        let st = run.lookout.semantic_status().unwrap();
        if st.state == SemanticState::Ready {
            break;
        }
        if st.state == SemanticState::Unavailable {
            println!("semantic unavailable: {:?}", st.reason);
            if t.elapsed() > Duration::from_secs(10) {
                std::process::exit(2);
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(300),
            "indexing did not finish: {st:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let st = run.lookout.semantic_status().unwrap();
    println!("semantic index: {}/{} files in {:.1}s; helper peak private working set while indexing {:.1} MB (cap 1024 MB)",
        st.done, st.total, t.elapsed().as_secs_f64(), mb(peak));

    // ---- helper idle: it must be gone after 60 s; daemon memory unchanged
    let t = Instant::now();
    while host.pid(PLUGIN_ID).is_some() {
        assert!(
            t.elapsed() < Duration::from_secs(120),
            "the helper was not stopped after being idle"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    println!(
        "helper process gone {:.0}s after the last call",
        t.elapsed().as_secs_f64()
    );
    let daemon_after = private_ws(me).unwrap_or(0);
    println!("daemon (this process) private working set: {:.1} MB before, {:.1} MB after semantic indexing finished and the helper exited", mb(daemon_before), mb(daemon_after));

    // ---- compare cut-off rules: all 50 queries, plus off-topic queries
    let negatives = [
        "quarterly earnings of a copper mining company in chile",
        "how to knit a wool scarf with cable stitches",
        "volcano eruption lava flow evacuation orders",
        "latin grammar declension tables",
        "purple elephant sandwich telescope",
        "xylophone quokka marmalade",
        "how tall is mount everest",
        "recipe for sourdough pizza dough",
        "football world cup final penalty shootout",
        "qwerty zxcvb plugh",
        "quantum chromodynamics lattice gauge",
        "medieval castle siege catapult",
        "tropical fish aquarium water chemistry",
        "violin concerto sheet music",
    ];
    // Floors only. A margin below the best hit was measured with a temporary patch:
    // it can never remove the best hit, so it cannot clear an off-topic query (docs/decisions.md).
    println!(
        "rule comparison (all {} queries; off-topic = queries with any result, of {}):",
        queries.len(),
        negatives.len()
    );
    for floor in [0.55f32, 0.60, 0.65, 0.70, 0.75] {
        host.set_config(PLUGIN_ID, json!({"min_similarity": floor}))
            .unwrap();
        let (t1, t5, n) = score(&mut run, &queries, &names);
        let false_hits = negatives
            .iter()
            .filter(|q| !run.ask(q).0.is_empty())
            .count();
        println!("  floor {floor:.2}: top1 {t1}/{n} top5 {t5}/{n}, off-topic with results {false_hits}/{}", negatives.len());
    }
    // The shipped default is scored below.
    let tau = 0.60f32;
    host.set_config(PLUGIN_ID, json!({"min_similarity": tau}))
        .unwrap();

    // ---- keyword plus semantic, final numbers
    let s_tune = score(&mut run, &tune, &names);
    let s_hold = score(&mut run, &holdout, &names);
    if std::env::var_os("EVAL_MISSES").is_some() {
        println!("  misses of the final system (not in top 5), and whether the fair baseline had them in its top 5:");
    }
    let s_all = score(&mut run, &queries, &names);
    if std::env::var_os("EVAL_MISSES").is_some() {
        for q in &queries {
            let want = &names[q["target"].as_str().unwrap()];
            if bm
                .rank(q["q"].as_str().unwrap())
                .iter()
                .take(5)
                .any(|n| n == want)
            {
                print!(" q{}", q["id"]);
            }
        }
        println!("  <- queries the fair baseline has in its top 5");
    }
    let k_ov = score_kw_only_overlap(&overlap);
    let s_ov = score(&mut run, &overlap, &names);
    println!("keyword + semantic: tune top1 {}/{} top5 {}; holdout top1 {}/{} top5 {}; all top1 {}/{} top5 {}",
        s_tune.0, s_tune.2, s_tune.1, s_hold.0, s_hold.2, s_hold.1, s_all.0, s_all.2, s_all.1);
    println!(
        "queries that share a word with their target ({}): semantic top1 {} top5 {} {k_ov}",
        overlap.len(),
        s_ov.0,
        s_ov.1
    );

    // ---- latency: cold = helper not running, warm = running
    host.set_config(PLUGIN_ID, json!({"min_similarity": tau}))
        .unwrap(); // stops the helper
    let (_, cold) = run.ask("where does the kitten sleep at night");
    let mut warm: Vec<Duration> = (0..10)
        .map(|i| {
            run.ask(&format!("where does the kitten sleep at night {i}"))
                .1
        })
        .collect();
    warm.sort();
    println!("first query end to end (includes the 150 ms typing pause, starting the helper, loading the model): cold {:.0} ms; warm median {:.0} ms, max {:.0} ms",
        cold.as_secs_f64() * 1e3, warm[5].as_secs_f64() * 1e3, warm[9].as_secs_f64() * 1e3);
    if let Some(pid) = host.pid(PLUGIN_ID) {
        println!(
            "helper private working set when warm and idle: {:.1} MB",
            mb(private_ws(pid).unwrap_or(0))
        );
    }
    run.lookout.shutdown();
}

fn score_kw_only_overlap(_: &[&Value]) -> &'static str {
    "(keyword-only numbers for these are in the all-queries line)"
}

// ------------------------------------------------------------ embedding checks

fn cmd_embed(model: &Path, assets: &Path, out: &Path) {
    use lookout_semantic::embed::{Embedder, OrtEmbedder};
    let corpus = load("fixtures/semantic/corpus.json");
    let queries_file = load("fixtures/semantic/queries.json");
    let mut e = OrtEmbedder::new(
        &assets.join("onnxruntime.dll"),
        &std::fs::read(model).unwrap(),
        &std::fs::read(assets.join("tokenizer.json")).unwrap(),
    )
    .unwrap();
    let mut ids = Vec::new();
    let mut vecs: Vec<Vec<f32>> = Vec::new();
    for d in corpus["docs"].as_array().unwrap() {
        let name = d["name"].as_str().unwrap();
        let stem = name
            .rsplit_once('.')
            .map_or(name, |x| x.0)
            .replace(['-', '_'], " ");
        ids.push(d["id"].as_str().unwrap().to_owned());
        vecs.push(
            e.embed(&format!("{stem}. {}", d["text"].as_str().unwrap()), false)
                .unwrap(),
        );
    }
    for s in corpus["screenshots"].as_array().unwrap() {
        let name = s["name"].as_str().unwrap();
        let stem = name
            .rsplit_once('.')
            .map_or(name, |x| x.0)
            .replace(['-', '_'], " ");
        ids.push(s["id"].as_str().unwrap().to_owned());
        vecs.push(
            e.embed(&format!("{stem}. {}", s["text"].as_str().unwrap()), false)
                .unwrap(),
        );
    }
    let (mut t1, mut t5, mut qv) = (0, 0, Vec::new());
    for q in queries_file["queries"].as_array().unwrap() {
        let v = e.embed(q["q"].as_str().unwrap(), true).unwrap();
        let mut ranked: Vec<(usize, f32)> = vecs
            .iter()
            .enumerate()
            .map(|(i, d)| (i, d.iter().zip(&v).map(|(a, b)| a * b).sum()))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let want = q["target"].as_str().unwrap();
        if ids[ranked[0].0] == want {
            t1 += 1;
        }
        if ranked.iter().take(5).any(|(i, _)| ids[*i] == want) {
            t5 += 1;
        }
        qv.push(v);
    }
    println!(
        "{}: embeddings alone, 50 queries over 60 texts (no keyword, no OCR): top1 {t1}, top5 {t5}",
        model.display()
    );
    std::fs::write(out, json!({"docs": vecs, "queries": qv}).to_string()).unwrap();
}

fn cmd_compare(a: &Path, b: &Path) {
    let rd =
        |p: &Path| -> Value { serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap() };
    let (a, b) = (rd(a), rd(b));
    for key in ["docs", "queries"] {
        let (x, y) = (a[key].as_array().unwrap(), b[key].as_array().unwrap());
        let cos: Vec<f32> = x
            .iter()
            .zip(y)
            .map(|(p, q)| {
                let f = |v: &Value| {
                    v.as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_f64().unwrap() as f32)
                        .collect::<Vec<_>>()
                };
                f(p).iter().zip(f(q)).map(|(m, n)| m * n).sum()
            })
            .collect();
        let min = cos.iter().copied().fold(1.0f32, f32::min);
        println!(
            "{key}: cosine between the two models' vectors: mean {:.4}, min {min:.4}",
            cos.iter().sum::<f32>() / cos.len() as f32
        );
    }
}

/// Builds a folder for the real memory bench: the fixture files, a data folder
/// with settings, the model files and the plugin already approved (on or off).
fn cmd_prepare(assets: &Path, out: &Path, extra: usize, enabled: bool) {
    let corpus = load("fixtures/semantic/corpus.json");
    let docs = out.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    for d in corpus["docs"].as_array().unwrap() {
        std::fs::write(
            docs.join(d["name"].as_str().unwrap()),
            d["text"].as_str().unwrap(),
        )
        .unwrap();
    }
    for s in corpus["screenshots"].as_array().unwrap() {
        std::fs::write(
            docs.join(s["name"].as_str().unwrap()),
            lookout_core::fixture_image::bmp(s["text"].as_str().unwrap(), 8),
        )
        .unwrap();
    }
    for (name, text) in distractors(extra) {
        std::fs::write(docs.join(name), text).unwrap();
    }
    let data = out.join("data");
    let dst = data.join("plugins").join(PLUGIN_ID).join("assets");
    std::fs::create_dir_all(&dst).unwrap();
    for f in ["model.onnx", "tokenizer.json", "onnxruntime.dll"] {
        std::fs::copy(assets.join(f), dst.join(f)).unwrap();
    }
    let settings = json!({"index_roots": [docs], "idle_secs": 0, "first_run_done": true,
                          "index_contents": true, "ocr": true});
    std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
    let manifest = lookout_core::plugins::Manifest::parse(
        &std::fs::read_to_string(repo().join("plugins/semantic/plugin.json")).unwrap(),
    )
    .unwrap();
    let state = json!({PLUGIN_ID: {"enabled": enabled, "approved": manifest.approval_key(), "config": null}});
    std::fs::write(data.join("plugins.json"), state.to_string()).unwrap();
    println!(
        "prepared {} (plugin {})",
        out.display(),
        if enabled { "on" } else { "off" }
    );
}

/// 100,000 generated vectors: load time, scan time, and the helper's cold and
/// warm first query and memory through the real host.
fn cmd_scale(assets: &Path, n: usize) {
    use lookout_core::crypto::{derive_plugin_key, KeyStore};
    use lookout_core::plugins::PluginHost;
    use lookout_core::semantic::SemanticClient;
    use lookout_semantic::embed::DIM;
    use lookout_semantic::store::Store;
    let tmp = tempfile::tempdir().unwrap();
    let master = KeyStore::new(tmp.path().join("lookout.key"))
        .load_or_create()
        .unwrap();
    let key = derive_plugin_key(&master, PLUGIN_ID);
    let data = tmp.path().join("data");
    let own = data.join("plugins").join(PLUGIN_ID);
    std::fs::create_dir_all(own.join("assets")).unwrap();
    for f in ["model.onnx", "tokenizer.json", "onnxruntime.dll"] {
        std::fs::copy(assets.join(f), own.join("assets").join(f)).unwrap();
    }
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f32 / (1u64 << 53) as f32 - 0.5
    };
    let t = Instant::now();
    let mut store = Store::open(Some(own.join("vectors.log")), Some(key.clone()));
    for chunk in (0..n).collect::<Vec<_>>().chunks(1000) {
        let docs = chunk
            .iter()
            .map(|i| {
                let mut v: Vec<f32> = (0..DIM).map(|_| rnd()).collect();
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                v.iter_mut().for_each(|x| *x /= norm);
                (
                    format!("C:/generated/folder{}/file{i}.txt", i % 500),
                    1u64,
                    Some(v),
                )
            })
            .collect();
        store.put_many(docs).unwrap();
    }
    drop(store);
    let size = std::fs::metadata(own.join("vectors.log")).unwrap().len();
    println!(
        "generated {n} vectors in {:.1}s; vectors.log {:.1} MB",
        t.elapsed().as_secs_f64(),
        size as f64 / 1_048_576.0
    );
    let t = Instant::now();
    let store = Store::open(Some(own.join("vectors.log")), Some(key));
    println!(
        "vector load (read, decrypt, parse {} entries): {:.0} ms",
        store.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    let q: Vec<f32> = {
        let mut v: Vec<f32> = (0..DIM).map(|_| rnd()).collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);
        v
    };
    let mut times: Vec<Duration> = (0..20)
        .map(|_| {
            let t = Instant::now();
            let _ = store.search(&q, 20, 0.0);
            t.elapsed()
        })
        .collect();
    times.sort();
    println!(
        "brute-force scan of {n} vectors (this process, 1 thread): median {:.1} ms, max {:.1} ms",
        times[10].as_secs_f64() * 1e3,
        times[19].as_secs_f64() * 1e3
    );
    drop(store);

    // The helper through the real host.
    let plugin = tmp.path().join("plugins").join("semantic");
    std::fs::create_dir_all(&plugin).unwrap();
    let exe = std::env::current_exe().unwrap();
    std::fs::copy(
        exe.parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("lookout-semantic.exe"),
        plugin.join("lookout-semantic.exe"),
    )
    .unwrap();
    std::fs::copy(
        repo().join("plugins/semantic/plugin.json"),
        plugin.join("plugin.json"),
    )
    .unwrap();
    let mut cfg = HostConfig::new(vec![tmp.path().join("plugins")], data.clone());
    cfg.master_key = Some(master);
    let host = lookout_core::plugins::PluginHost::start(cfg, PauseState::new());
    host.set_enabled(PLUGIN_ID, true, true).unwrap();
    let client = lookout_core::semantic::SemanticClient::new(host.clone(), PLUGIN_ID, &data);
    let _: Option<&PluginHost> = None;
    let _: Option<SemanticClient> = None;
    let never = || false;
    let t = Instant::now();
    let st = client.status(&never).unwrap();
    println!("helper status (starts the process, hashes the files, loads the model, loads the vectors): {:.0} ms, ready {} count {}", t.elapsed().as_secs_f64() * 1e3, st.ready, st.count);
    let pid = host.pid(PLUGIN_ID).unwrap();
    println!(
        "helper private working set with {n} vectors loaded, before any search: {:.1} MB",
        mb(private_ws(pid).unwrap_or(0))
    );
    host.set_enabled(PLUGIN_ID, false, false).unwrap();
    host.set_enabled(PLUGIN_ID, true, true).unwrap();
    let t = Instant::now();
    let hits = client
        .search("where does the kitten sleep at night", 10, &never)
        .unwrap();
    println!(
        "cold first query (process start to answer, {} hits): {:.0} ms",
        hits.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    let pid = host.pid(PLUGIN_ID).unwrap();
    println!(
        "helper private working set after the first search: {:.1} MB",
        mb(private_ws(pid).unwrap_or(0))
    );
    let mut warm: Vec<Duration> = (0..10)
        .map(|i| {
            let t = Instant::now();
            let _ = client
                .search(&format!("where does the kitten sleep {i}"), 10, &never)
                .unwrap();
            t.elapsed()
        })
        .collect();
    warm.sort();
    println!(
        "warm query (embed + scan, over the pipe): median {:.0} ms, max {:.0} ms",
        warm[5].as_secs_f64() * 1e3,
        warm[9].as_secs_f64() * 1e3
    );
    host.shutdown();
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("run") => cmd_run(
            Path::new(&a[2]),
            a.get(3).and_then(|n| n.parse().ok()).unwrap_or(0),
        ),
        Some("prepare") => cmd_prepare(
            Path::new(&a[2]),
            Path::new(&a[3]),
            a[4].parse().unwrap(),
            a[5] == "on",
        ),
        Some("scale") => cmd_scale(
            Path::new(&a[2]),
            a.get(3).and_then(|n| n.parse().ok()).unwrap_or(100_000),
        ),
        Some("embed") => cmd_embed(Path::new(&a[2]), Path::new(&a[3]), Path::new(&a[4])),
        Some("compare") => cmd_compare(Path::new(&a[2]), Path::new(&a[3])),
        _ => eprintln!("usage: eval run <assets> | embed <model> <assets> <out> | compare <a> <b>"),
    }
}
