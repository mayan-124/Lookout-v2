//! Probes of the whole search pipeline with odd input, an empty index and an
//! index that is still filling. Every query must finish and none may panic.

#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

struct App {
    lookout: Lookout,
    rx: mpsc::Receiver<Update>,
    next: u64,
}

impl App {
    fn ask(&mut self, text: &str) -> Update {
        self.next += 1;
        let generation = self.next;
        assert!(self.lookout.query(generation, text), "query refused");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let u = self
                .rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("query never finished: {:?}", text.chars().take(40)));
            if u.generation == generation && u.done {
                return u;
            }
        }
    }
}

fn titles(u: &Update) -> Vec<String> {
    u.items.iter().map(|i| i.title.clone()).collect()
}

fn start(dir: &Path, root: &Path, wait: bool) -> App {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |u| {
        let _ = tx.lock().unwrap().send(u);
    });
    let start_menu = dir.join("startmenu");
    fs::create_dir_all(&start_menu).unwrap();
    for app in ["Notepad", "Display Settings Tool"] {
        fs::write(start_menu.join(format!("{app}.lnk")), b"").unwrap();
    }
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
            storage: Storage::with_key(
                DataPaths::new(dir.join("data")),
                Key::from_bytes(&[5u8; 32]).unwrap(),
            )
            .unwrap(),
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
    if wait {
        let deadline = Instant::now() + Duration::from_secs(60);
        while lookout.status().indexing || lookout.status().apps < 2 {
            assert!(Instant::now() < deadline, "index never filled");
            std::thread::sleep(Duration::from_millis(30));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    App {
        lookout,
        rx,
        next: 0,
    }
}

fn corpus(root: &Path) {
    for f in [
        "update.txt",
        "display-driver.txt",
        "wifi-notes.txt",
        "notes.txt",
        "dark mode.txt",
        "😀 party.txt",
        "report [final] (1).txt",
    ] {
        fs::write(root.join(f), "x").unwrap();
    }
}

#[test]
fn odd_queries_finish_and_do_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    fs::create_dir_all(&root).unwrap();
    corpus(&root);
    let mut app = start(dir.path(), &root, true);
    let long = "a".repeat(5000);
    let many_words = vec!["word"; 400].join(" ");
    let odd = [
        "",
        "   ",
        "\t\n",
        &long,
        &many_words,
        "😀",
        "😀 party",
        "👨‍👩‍👧‍👦",
        "İstanbul ß ǅ",
        "((",
        "[a-z]+",
        ".*",
        "\\",
        "C:\\",
        "\\\\",
        "\\\\server\\share",
        "a\u{0}b",
        "kind:",
        "kind:file",
        "kind: file",
        "%",
        "_",
        "\"",
        "last",
        "last week",
        "yesterday pdf",
        "pdf last week",
        "kind:folder last week",
        "10 * (2 +",
        "1/0",
        "999999999999999999999999 * 999999999999999999999999",
        "5 kg to",
        "timer",
        "timer 0",
        "timer 99999999999999999999 minutes",
        "remind me in 5 minutes to",
    ];
    for q in odd {
        let _ = app.ask(q);
    }
    // Only spaces: nothing, and no filters.
    let u = app.ask("     ");
    assert!(u.items.is_empty(), "{:?}", titles(&u));
    // Case and trailing space do not change what is found.
    for q in ["notepad", "NoTePaD", "notepad ", "  NOTEPAD  "] {
        let t = titles(&app.ask(q));
        assert!(t.iter().any(|t| t == "Notepad"), "{q:?}: {t:?}");
    }
    // Emoji in a name is found by the emoji and by the word beside it.
    for q in ["😀", "party", "😀 party"] {
        let t = titles(&app.ask(q));
        assert!(t.iter().any(|t| t.contains("party")), "{q:?}: {t:?}");
    }
    // Regex characters are plain text.
    let t = titles(&app.ask("[final]"));
    assert!(t.iter().any(|t| t.contains("[final]")), "{t:?}");
    assert!(titles(&app.ask(".*"))
        .iter()
        .all(|t| t.contains(".*") || t.contains("Open ") || t.contains("Switch") || !t.is_empty()));
}

#[test]
fn system_rows_do_not_outrank_strong_file_and_app_matches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    fs::create_dir_all(&root).unwrap();
    corpus(&root);
    let mut app = start(dir.path(), &root, true);
    // An exact file name beats the Settings row for the same word.
    let t = titles(&app.ask("update"));
    let file = t.iter().position(|t| t == "update.txt").unwrap();
    let sys = t.iter().position(|t| t.contains("Windows Update"));
    assert!(sys.is_none_or(|s| file < s), "{t:?}");
    // "dark mode" as an exact file name also stays first.
    let t = titles(&app.ask("dark mode"));
    assert_eq!(
        t.first().map(String::as_str),
        Some("dark mode.txt"),
        "{t:?}"
    );
    // A whole Settings word ranks above names that only begin with it.
    let t = titles(&app.ask("display"));
    let file = t.iter().position(|t| t == "display-driver.txt").unwrap();
    let app_pos = t.iter().position(|t| t == "Display Settings Tool").unwrap();
    let sys = t.iter().position(|t| t == "Open Display settings").unwrap();
    assert!(sys < file && sys < app_pos, "{t:?}");
    let t = titles(&app.ask("wifi"));
    let file = t.iter().position(|t| t == "wifi-notes.txt").unwrap();
    let sys = t.iter().position(|t| t == "Open Wi-Fi settings").unwrap();
    assert!(sys < file, "{t:?}");
}

#[test]
fn an_empty_index_and_a_filling_index_both_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("empty");
    fs::create_dir_all(&root).unwrap();
    let mut app = start(dir.path(), &root, true);
    assert!(app.ask("anything").items.is_empty());
    assert!(app.ask("update").items.iter().all(|i| i.source != "files"));
    // A big folder, queried at once without waiting for the crawl.
    let dir2 = tempfile::tempdir().unwrap();
    let root2 = dir2.path().join("many");
    fs::create_dir_all(&root2).unwrap();
    for i in 0..3000 {
        fs::write(root2.join(format!("file-{i}.txt")), "x").unwrap();
    }
    let mut app = start(dir2.path(), &root2, false);
    for _ in 0..5 {
        let _ = app.ask("file-17");
        let _ = app.ask("notepad");
    }
}

#[test]
fn the_exact_words_row_and_chips_combine_with_words() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    fs::create_dir_all(&root).unwrap();
    corpus(&root);
    let mut app = start(dir.path(), &root, true);
    // "notes last week" is a date search; skipping the date chip treats the
    // words literally.
    let u = app.ask("notes last week");
    assert!(u.filters.date.is_some(), "{:?}", u.filters);
    app.next += 1;
    let generation = app.next;
    assert!(app.lookout.query_with(
        generation,
        "notes last week",
        lookout_core::query::Skip::ALL
    ));
    loop {
        let u = app.rx.recv_timeout(Duration::from_secs(10)).unwrap();
        if u.generation == generation && u.done {
            assert!(u.filters.is_empty(), "{:?}", u.filters);
            break;
        }
    }
    // A kind word without a date phrase is not a filter.
    let u = app.ask("pdf notes");
    assert!(u.filters.is_empty());
}
