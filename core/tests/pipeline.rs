//! End to end: a real crawl of a temporary folder tree feeding the engine
//! with all four Phase 1 sources. Only temporary folders are touched.

// Test code: a failed setup step should fail the test loudly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lookout_core::apps;
use lookout_core::engine::{Engine, EngineConfig, Sink, Update};
use lookout_core::exclude::Excludes;
use lookout_core::frecency::FrecencyStore;
use lookout_core::index_service::{new_shared_index, IndexConfig, IndexService};
use lookout_core::item::Kind;
use lookout_core::pause::PauseState;
use lookout_core::ranking::RankingWeights;
use lookout_core::sources::{AppsSource, CalculatorSource, FilesSource, Source, UnitsSource};

struct Harness {
    engine: Engine,
    updates: Arc<Mutex<Vec<Update>>>,
    signal: Receiver<()>,
    service: IndexService,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        fs::create_dir_all(files.join("work")).unwrap();
        fs::create_dir_all(files.join("photos")).unwrap();
        fs::create_dir_all(files.join("node_modules")).unwrap();
        fs::write(files.join("work/quarterly budget.xlsx"), b"x").unwrap();
        fs::write(files.join("work/Quarterly Review.docx"), b"x").unwrap();
        fs::write(files.join("photos/IMG_0001.jpg"), b"x").unwrap();
        fs::write(files.join("node_modules/hidden quarterly.js"), b"x").unwrap();
        let start = dir.path().join("start");
        fs::create_dir_all(&start).unwrap();
        fs::write(start.join("Notepad.lnk"), b"").unwrap();
        fs::write(start.join("Calculator.lnk"), b"").unwrap();

        let index = new_shared_index();
        let service = IndexService::start(
            index.clone(),
            {
                let mut c = IndexConfig::new(vec![files], Excludes::default());
                c.duty = 1.0;
                c.watch = false;
                c
            },
            PauseState::new(),
        )
        .unwrap();
        assert!(service.wait_for_crawl(Duration::from_secs(10)));

        let weights = RankingWeights::default();
        let frecency = Arc::new(FrecencyStore::open_in_memory().unwrap());
        let sources: Vec<Arc<dyn Source>> = vec![
            Arc::new(CalculatorSource),
            Arc::new(UnitsSource),
            Arc::new(AppsSource::new(
                apps::scan_start_menu(&[start]),
                frecency,
                weights,
            )),
            Arc::new(FilesSource::new(index, weights)),
        ];
        let updates = Arc::new(Mutex::new(Vec::new()));
        let (tx, signal) = mpsc::channel();
        let u = updates.clone();
        let sink: Sink = Arc::new(move |update| {
            u.lock().unwrap().push(update);
            let _ = tx.send(());
        });
        let engine = Engine::new(sources, sink, EngineConfig::default()).unwrap();
        Self {
            engine,
            updates,
            signal,
            service,
            _dir: dir,
        }
    }

    /// Submits `text` as `generation` and returns the final update for it.
    fn ask(&self, generation: u64, text: &str) -> Update {
        assert!(self.engine.submit(generation, text));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = self
                .updates
                .lock()
                .unwrap()
                .iter()
                .find(|u| u.generation == generation && u.done)
                .cloned();
            if let Some(u) = found {
                return u;
            }
            assert!(Instant::now() < deadline, "query {text:?} never completed");
            let _ = self.signal.recv_timeout(Duration::from_millis(50));
        }
    }
}

#[test]
fn calculator_and_conversions_come_first() {
    let h = Harness::new();
    let calc = h.ask(1, "2+2*5");
    assert_eq!(calc.items[0].title, "12");
    assert_eq!(calc.items[0].kind, Kind::Custom("calculator".into()));

    let conv = h.ask(2, "10 km to miles");
    assert_eq!(conv.items[0].title, "6.21371 mi");
    assert_eq!(conv.items[0].kind, Kind::Custom("conversion".into()));
}

#[test]
fn files_are_found_and_excluded_folders_are_not() {
    let h = Harness::new();
    let r = h.ask(1, "quarterly");
    let titles: Vec<_> = r.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(r.items.len(), 2, "{titles:?}");
    assert!(titles.contains(&"quarterly budget.xlsx") && titles.contains(&"Quarterly Review.docx"));
    let first = &r.items[0];
    assert!(Path::new(first.uri.as_deref().unwrap()).is_absolute());
}

#[test]
fn apps_are_found_and_kind_filters_work() {
    let h = Harness::new();
    let r = h.ask(1, "notepad");
    assert_eq!(r.items[0].kind, Kind::App);
    assert_eq!(r.items[0].title, "Notepad");

    let folders = h.ask(2, "kind:folder photos");
    assert_eq!(folders.items.len(), 1);
    assert_eq!(folders.items[0].kind, Kind::Folder);

    let files_only = h.ask(3, "kind:file photos");
    assert!(files_only.items.is_empty());
}

#[test]
fn typing_a_word_letter_by_letter_ends_on_the_right_results_and_never_goes_backwards() {
    let h = Harness::new();
    let word = "quarterly";
    for (i, end) in (1..=word.len()).enumerate() {
        h.engine.submit(i as u64 + 1, &word[..end]);
    }
    let last = word.len() as u64;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !h
        .updates
        .lock()
        .unwrap()
        .iter()
        .any(|u| u.generation == last && u.done)
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let updates = h.updates.lock().unwrap().clone();
    for w in updates.windows(2) {
        assert!(
            w[0].generation <= w[1].generation,
            "stale update after a newer one"
        );
    }
    let final_update = updates.iter().rev().find(|u| u.done).unwrap();
    assert_eq!(final_update.generation, last);
    assert_eq!(final_update.items.len(), 2);
    assert!(final_update
        .items
        .iter()
        .all(|i| i.title.to_lowercase().contains(word)));
    h.service.status();
}
