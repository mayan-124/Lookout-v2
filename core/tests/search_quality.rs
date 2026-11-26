//! Search quality: builds a synthetic corpus of real files in a temp folder,
//! starts the whole pipeline on it, runs the queries in
//! `search_quality/queries.txt` and scores where the expected result lands.
//! The scores must not fall below `search_quality/baseline.txt`.
//!
//! Set `LOOKOUT_QUALITY_LATENCY=1` to also print p50/p95 query times.

#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::fs::{self, File};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use lookout_core::actions::open::NoopLauncher;
use lookout_core::actions::GateConfig;
use lookout_core::content::idle::AlwaysIdle;
use lookout_core::content::ocr::NoOcr;
use lookout_core::crypto::Key;
use lookout_core::engine::{Sink, Update};
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::settings::Settings;
use lookout_core::storage::{DataPaths, Storage};

const QUERIES: &str = include_str!("search_quality/queries.txt");
const BASELINE: &str = include_str!("search_quality/baseline.txt");

/// (path below the corpus root, age in days, text content).
type Named = (&'static str, u64, Option<&'static str>);

const NAMED: &[Named] = &[
    ("Finance/budget-2026.xlsx", 10, None),
    ("Finance/Archive/budget-2025.xlsx", 400, None),
    ("Downloads/budget-2026-draft.xlsx", 3, None),
    (
        "Finance/Budget Reviews/summary-q1.txt",
        100,
        Some("Quarterly summary of department spending and forecasts."),
    ),
    ("Finance/taxes-2026.pdf", 200, None),
    ("Finance/annual-report-2024.pdf", 200, None),
    ("Finance/Billing/invoice-0042.pdf", 40, None),
    ("Finance/Billing/invoice-0043.pdf", 5, None),
    ("Finance/Billing/invoice_march.pdf", 70, None),
    ("Finance/Billing/invoice-archive-2025.zip", 300, None),
    ("Finance/receipt-grocery.pdf", 15, None),
    ("Finance/receipt-hardware.pdf", 90, None),
    ("Documents/Work/ProjectPlan.docx", 20, None),
    ("Archive/2019/old/ProjectPlan.docx", 2000, None),
    ("Documents/Work/MeetingMinutes-2025.docx", 60, None),
    ("Documents/Work/Q3_report_final.docx", 45, None),
    ("Documents/Work/Q3_report_final_v2.docx", 12, None),
    ("Documents/Work/Q3-report-draft.docx", 60, None),
    ("Documents/Work/final-report-v3.docx", 8, None),
    (
        "Documents/Work/meeting notes.txt",
        2,
        Some("Discussed the vendor contract and next steps for the onboarding checklist."),
    ),
    (
        "Archive/2019/meeting notes.txt",
        1500,
        Some("Old agenda: office move and printer replacement."),
    ),
    (
        "Documents/Work/meeting-2026-01-15.txt",
        100,
        Some("Agenda: migrate the payroll database to the new cluster before March."),
    ),
    (
        "Documents/Work/todo.md",
        4,
        Some("- renew the passport\n- book dentist appointment\n"),
    ),
    ("Documents/Work/calculator.xlsx", 100, None),
    (
        "Documents/Work/high-priority.txt",
        70,
        Some("Items that cannot wait until next sprint."),
    ),
    ("Documents/Work/Müller-Rechnung.pdf", 80, None),
    ("Documents/Personal/MyResume.pdf", 60, None),
    ("Archive/my_resume_old.docx", 900, None),
    ("Downloads/resume-template.docx", 30, None),
    ("Documents/Personal/résumé.pdf", 500, None),
    (
        "Documents/Personal/café-menu.txt",
        35,
        Some("Espresso, flat white and almond croissant on the weekend menu."),
    ),
    (
        "Documents/Personal/home-notes.txt",
        25,
        Some("Quote for the kitchen renovation invoice from Harbor Builders, total 18400 dollars."),
    ),
    (
        "Documents/Personal/recipe.txt",
        90,
        Some("Grandma's lemon cake needs three eggs and a pinch of saffron."),
    ),
    ("Documents/Personal/holiday-itinerary.pdf", 50, None),
    ("Documents/Personal/airplane tickets.pdf", 50, None),
    (
        "Documents/Personal/weight-log.csv",
        30,
        Some("date,kg\n2026-01-01,70\n"),
    ),
    (
        "Documents/Study/日本語ノート.txt",
        120,
        Some("Hiragana practice sheet."),
    ),
    ("Documents/Study/naïve-bayes.ipynb", 150, None),
    ("Pictures/x.png", 200, None),
    ("Pictures/Holiday Photos/IMG_2001.jpg", 160, None),
    ("Pictures/Holiday Photos/IMG_2002.jpg", 160, None),
    ("Pictures/Holiday Photos/IMG_2003.jpg", 160, None),
    ("Pictures/Holiday Photos/IMG_2004.jpg", 159, None),
    ("Pictures/Holiday Photos/IMG_2005.jpg", 159, None),
    ("Pictures/Holiday Photos/IMG_2006.jpg", 158, None),
    (
        "Pictures/Screenshots/Screenshot 2026-03-01 101500.png",
        8,
        None,
    ),
    (
        "Pictures/Screenshots/Screenshot 2026-03-04 163210.png",
        5,
        None,
    ),
    ("Downloads/chrome-bookmarks.html", 5, None),
    (
        "Downloads/notepad-tips.md",
        40,
        Some("Keyboard shortcuts for quick text edits."),
    ),
    (
        "Projects/lookout/readme.md",
        3,
        Some("A local search launcher for files, apps and notes."),
    ),
    (
        "Projects/lookout/src/readme.md",
        30,
        Some("Source layout of the launcher."),
    ),
    (
        "Projects/other/readme.md",
        200,
        Some("Scratch experiments."),
    ),
    (
        "Projects/lookout/config.json",
        10,
        Some("{\"theme\": \"dark\"}"),
    ),
    (
        "Projects/lookout/src/a/b/c/d/e/f/deep-settings-config.json",
        50,
        Some("{\"depth\": 7}"),
    ),
    (
        "Projects/lookout/gh-cli-cheatsheet.md",
        20,
        Some("gh pr create --fill"),
    ),
];

const APPS: &[&str] = &[
    "Visual Studio Code",
    "Visual Studio 2022",
    "Google Chrome",
    "Notepad",
    "Notepad++",
    "Calculator",
    "Microsoft Edge",
    "Microsoft Word",
    "Windows Terminal",
    "Steam",
];

const TOPICS: &[&str] = &[
    "lecture",
    "slides",
    "homework",
    "thesis",
    "chapter",
    "exam",
    "grades",
    "workout",
    "playlist",
    "podcast",
    "mockup",
    "logo",
    "banner",
    "icon",
    "backup",
    "dataset",
    "metrics",
    "dashboard",
    "template",
    "keynote",
    "manual",
    "tutorial",
    "course",
    "survey",
    "analysis",
    "training",
    "model",
    "results",
    "schedule",
    "calendar",
    "travel",
    "ticket",
    "booking",
    "insurance",
    "statement",
    "letter",
    "proposal",
    "roadmap",
    "license",
    "design",
    "family",
];
const EXTS: &[&str] = &[
    "pdf", "docx", "txt", "md", "jpg", "png", "mp3", "mp4", "zip", "csv", "pptx", "json",
];
const FILLER_DIRS: &[&str] = &[
    "Documents/Study",
    "Media/Music",
    "Media/Video",
    "Backups/2022",
    "Backups/2023",
    "Downloads",
    "Pictures/Camera",
    "Documents/Misc/Old/Stuff",
];
const FILLER_COUNT: usize = 240;
const TEXT_EXTS: &[&str] = &["txt", "md", "json", "csv"];

fn is_text(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .is_some_and(|e| TEXT_EXTS.contains(&e))
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// The filler files: realistic names in several styles, spread over folders and
/// dates, with no word that any query uses. Deterministic.
fn filler() -> Vec<(String, u64, Option<String>)> {
    (0..FILLER_COUNT)
        .map(|i| {
            let topic = TOPICS[i % TOPICS.len()];
            let ext = EXTS[(i * 7 + i / 5) % EXTS.len()];
            let dir = FILLER_DIRS[(i * 3 + i / 7) % FILLER_DIRS.len()];
            let name = match i % 4 {
                0 => format!("{topic}-{i}.{ext}"),
                1 => format!("{}_{i}.{ext}", capitalised(topic)),
                2 => format!("{}{i}.{ext}", capitalised(topic)),
                _ if ext == "jpg" => format!("DSC_{}.{ext}", 5000 + i),
                _ => format!("{topic} {i}.{ext}"),
            };
            let content = TEXT_EXTS.contains(&ext).then(|| match ext {
                "json" => format!("{{\"n\": {i}}}"),
                "csv" => format!("a,b\n{i},{}\n", i * 2),
                _ => format!(
                    "Session {i} on {topic}: review the material and practise the examples."
                ),
            });
            (format!("{dir}/{name}"), 1 + (i as u64 * 37) % 1400, content)
        })
        .collect()
}

/// Writes the corpus below `root`; returns (file count, text file count).
fn generate(root: &Path) -> (usize, usize) {
    let now = SystemTime::now();
    let mut all: Vec<(String, u64, Option<String>)> = NAMED
        .iter()
        .map(|(p, age, c)| ((*p).to_owned(), *age, c.map(str::to_owned)))
        .collect();
    all.extend(filler());
    let mut texts = 0;
    for (rel, age, content) in &all {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content.as_deref().unwrap_or("")).unwrap();
        if content.is_some() && is_text(rel) {
            texts += 1;
        }
        let when = now - Duration::from_secs(age * 86_400);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
    (all.len(), texts)
}

struct Case {
    category: String,
    query: String,
    top1: String,
    acceptable: Vec<String>,
    /// Results that tie with `top1`, so the first place may be any of them.
    ties: Vec<String>,
}

fn parse_cases() -> Vec<Case> {
    QUERIES
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let f: Vec<&str> = l.split(" | ").map(str::trim).collect();
            let others: Vec<&str> = f
                .get(3)
                .map(|a| a.split(", ").collect())
                .unwrap_or_default();
            Case {
                category: f[0].to_owned(),
                query: f[1].to_owned(),
                top1: f[2].to_owned(),
                acceptable: others
                    .iter()
                    .map(|o| o.trim_start_matches('=').to_owned())
                    .collect(),
                ties: others
                    .iter()
                    .filter_map(|o| o.strip_prefix('='))
                    .map(str::to_owned)
                    .collect(),
            }
        })
        .collect()
}

struct App {
    lookout: Lookout,
    rx: mpsc::Receiver<Update>,
    next: u64,
    root: String,
}

impl App {
    fn ask(&mut self, text: &str) -> (Vec<String>, Duration) {
        self.next += 1;
        let generation = self.next;
        let start = Instant::now();
        assert!(self.lookout.query(generation, text));
        loop {
            let u = self.rx.recv_timeout(Duration::from_secs(10)).unwrap();
            if u.generation == generation && u.done {
                let took = start.elapsed();
                return (u.items.iter().map(|i| self.label(i)).collect(), took);
            }
        }
    }

    fn label(&self, item: &lookout_core::item::Item) -> String {
        if item.source == "apps" {
            return format!("app:{}", item.title);
        }
        match &item.uri {
            Some(uri) => {
                let norm = uri.replace('\\', "/");
                norm.strip_prefix(&self.root)
                    .map_or(norm.clone(), |r| r.trim_start_matches('/').to_owned())
            }
            None => item.title.clone(),
        }
    }
}

fn start(dir: &Path, root: &Path, expected_files: usize, expected_texts: usize) -> App {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |u| {
        let _ = tx.lock().unwrap().send(u);
    });
    let start_menu = dir.join("startmenu");
    fs::create_dir_all(&start_menu).unwrap();
    for app in APPS {
        fs::write(start_menu.join(format!("{app}.lnk")), b"").unwrap();
    }
    let paths = DataPaths::new(dir.join("data"));
    let lookout = Lookout::start(
        StartOptions {
            settings: Settings {
                idle_secs: 0,
                crawl_duty: 1.0,
                first_run_done: true,
                ..Settings::default()
            },
            roots_override: vec![root.to_path_buf()],
            synthetic_files: None,
            synthetic_uses_folders: false,
            start_menu_dirs: vec![start_menu],
            store_apps: false,
            storage: Storage::with_key(paths, Key::from_bytes(&[5u8; 32]).unwrap()).unwrap(),
            idle: Arc::new(AlwaysIdle),
            ocr: Arc::new(NoOcr),
            launcher: Arc::new(NoopLauncher),
            system: None,
            gate: GateConfig::default(),
            plugins: None,
        },
        sink,
        PauseState::new(),
    )
    .unwrap();
    // Wait for the file names, then for the contents.
    let deadline = Instant::now() + Duration::from_secs(120);
    while {
        let s = lookout.status();
        s.files < expected_files || s.indexing || s.apps < APPS.len()
    } {
        assert!(Instant::now() < deadline, "name index never filled");
        std::thread::sleep(Duration::from_millis(50));
    }
    let stats = lookout.content_stats().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    while stats.busy.load(Ordering::Acquire)
        || lookout.status().content_docs < expected_texts as u64
    {
        assert!(Instant::now() < deadline, "content index never filled");
        std::thread::sleep(Duration::from_millis(50));
    }
    App {
        lookout,
        rx,
        next: 0,
        root: root.to_string_lossy().replace('\\', "/"),
    }
}

fn baseline(key: &str) -> usize {
    BASELINE
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

#[test]
fn search_quality_does_not_regress() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("corpus");
    let (files, texts) = generate(&root);
    let mut app = start(dir.path(), &root, files, texts);
    let cases = parse_cases();

    let (mut top1, mut top3) = (0, 0);
    let mut misses = Vec::new();
    for c in &cases {
        let (got, _) = app.ask(&c.query);
        let first3 = &got[..got.len().min(3)];
        let hit1 = got
            .first()
            .is_some_and(|g| *g == c.top1 || c.ties.contains(g));
        let hit3 = first3.contains(&c.top1) || c.acceptable.iter().any(|a| first3.contains(a));
        top1 += usize::from(hit1);
        top3 += usize::from(hit3);
        if !hit1 || !hit3 {
            let shown: Vec<_> = got.iter().take(3).collect();
            misses.push(format!(
                "{} [{}] {:?}: expected {:?}, got {:?}",
                if hit3 { "TOP1" } else { "TOP3" },
                c.category,
                c.query,
                c.top1,
                shown
            ));
        }
    }
    for m in &misses {
        println!("{m}");
    }
    println!(
        "search quality: top-1 {top1}/{n} top-3 {top3}/{n} ({files} files)",
        n = cases.len()
    );

    if std::env::var_os("LOOKOUT_QUALITY_LATENCY").is_some() {
        let (mut name, mut content) = (Vec::new(), Vec::new());
        for c in &cases {
            for _ in 0..20 {
                let (_, took) = app.ask(&c.query);
                if c.category == "content" {
                    content.push(took);
                } else {
                    name.push(took);
                }
            }
        }
        for (label, v) in [("name", &mut name), ("content", &mut content)] {
            v.sort();
            println!(
                "latency {label}: p50 {:?} p95 {:?} ({} runs)",
                percentile(v, 0.5),
                percentile(v, 0.95),
                v.len()
            );
        }
    }

    assert!(
        top1 >= baseline("top1"),
        "top-1 fell to {top1}, baseline {}",
        baseline("top1")
    );
    assert!(
        top3 >= baseline("top3"),
        "top-3 fell to {top3}, baseline {}",
        baseline("top3")
    );
    app.lookout.shutdown();
}
