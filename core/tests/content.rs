//! End-to-end content search over generated fixture files in temp folders.
//! Nothing here reads real user files.

#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lookout_core::actions::open::NoopLauncher;
use lookout_core::actions::GateConfig;
use lookout_core::content::idle::{AlwaysIdle, IdleSource, ManualIdle};
use lookout_core::content::ocr::{NoOcr, Ocr};
use lookout_core::crypto::Key;
use lookout_core::engine::{Sink, Update};
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::settings::Settings;
use lookout_core::storage::{DataPaths, Storage};

struct App {
    lookout: Lookout,
    updates: Arc<Mutex<Vec<Update>>>,
    next: u64,
    paths: DataPaths,
}

fn settings() -> Settings {
    Settings {
        idle_secs: 0,
        crawl_duty: 1.0,
        first_run_done: true,
        ..Settings::default()
    }
}

struct Opts {
    settings: Settings,
    idle: Arc<dyn IdleSource>,
    ocr: Arc<dyn Ocr>,
    pause: PauseState,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            settings: settings(),
            idle: Arc::new(AlwaysIdle),
            ocr: Arc::new(NoOcr),
            pause: PauseState::new(),
        }
    }
}

fn start(data: &Path, root: &Path, o: Opts) -> App {
    let updates: Arc<Mutex<Vec<Update>>> = Arc::default();
    let u = updates.clone();
    let sink: Sink = Arc::new(move |update| u.lock().unwrap().push(update));
    let paths = DataPaths::new(data);
    let lookout = Lookout::start(
        StartOptions {
            settings: o.settings,
            roots_override: vec![root.to_path_buf()],
            synthetic_files: None,
            synthetic_uses_folders: false,
            start_menu_dirs: vec![],
            storage: Storage::with_key(paths.clone(), Key::from_bytes(&[5u8; 32]).unwrap())
                .unwrap(),
            idle: o.idle,
            ocr: o.ocr,
            launcher: Arc::new(NoopLauncher),
            system: None,
            gate: GateConfig::default(),
            plugins: None,
        },
        sink,
        o.pause,
    )
    .unwrap();
    App {
        lookout,
        updates,
        next: 0,
        paths,
    }
}

impl App {
    fn ask(&mut self, text: &str) -> Update {
        self.next += 1;
        let generation = self.next;
        assert!(self.lookout.query(generation, text));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(u) = self
                .updates
                .lock()
                .unwrap()
                .iter()
                .find(|u| u.generation == generation && u.done)
            {
                return u.clone();
            }
            assert!(Instant::now() < deadline, "query never completed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Paths of content hits for `text`.
    fn content_hits(&mut self, text: &str) -> Vec<String> {
        self.ask(text)
            .items
            .into_iter()
            .filter(|i| i.source == "content")
            .filter_map(|i| i.uri)
            .collect()
    }

    fn wait_content_idle(&self) {
        let stats = self.lookout.content_stats().unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        // Give the first scan a moment to start, then wait for it to finish.
        std::thread::sleep(Duration::from_millis(300));
        while stats.busy.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "content indexing never finished");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn eventually(what: &str, cond: impl FnMut() -> bool) {
    eventually_for(what, Duration::from_secs(60), String::new, cond);
}

/// Waits up to `limit`; on timeout the failure carries `state()`, and the
/// elapsed time is printed on success.
fn eventually_for(
    what: &str,
    limit: Duration,
    state: impl Fn() -> String,
    mut cond: impl FnMut() -> bool,
) {
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < limit,
            "timed out after {:?} waiting for: {what}; {}",
            start.elapsed(),
            state()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("waited {:?} for: {what}", start.elapsed());
}

/// Waits for `cond` as long as `progress()` keeps changing: fails only when it
/// has stood still for `stall`. Each file costs a commit (and a merge wait), so
/// the total depends on machine load; being stuck does not.
fn eventually_progress(
    what: &str,
    stall: Duration,
    progress: impl Fn() -> u64,
    state: impl Fn() -> String,
    mut cond: impl FnMut() -> bool,
) {
    let start = Instant::now();
    let (mut last, mut moved) = (progress(), Instant::now());
    while !cond() {
        let now = progress();
        if now != last {
            (last, moved) = (now, Instant::now());
        }
        assert!(
            moved.elapsed() < stall,
            "no progress for {:?} (after {:?}) waiting for: {what}; {}",
            moved.elapsed(),
            start.elapsed(),
            state()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("waited {:?} for: {what}", start.elapsed());
}

fn fixture(dir: &Path) -> PathBuf {
    let root = dir.join("docs");
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(
        root.join("notes").join("meeting.txt"),
        "We discussed the zanzibar migration plan and the budget.",
    )
    .unwrap();
    fs::write(root.join("other.md"), "Grocery list: apples, bread, milk").unwrap();
    root
}

#[test]
fn finds_a_file_by_what_is_inside_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let mut app = start(&dir.path().join("data"), &root, Opts::default());
    app.wait_content_idle();
    let hits = app.content_hits("zanzibar migration");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].ends_with("meeting.txt"));
    assert_eq!(app.content_hits("milk").len(), 1);
    assert!(app.content_hits("nonexistentword").is_empty());
    assert!(app.lookout.status().content_docs >= 2);
    // The hit carries a readable preview.
    let item = app
        .ask("zanzibar")
        .items
        .into_iter()
        .find(|i| i.source == "content")
        .unwrap();
    assert!(item.subtitle.unwrap().contains("migration plan"));
}

#[test]
fn the_index_on_disk_is_encrypted_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let data = dir.path().join("data");
    {
        let app = start(&data, &root, Opts::default());
        app.wait_content_idle();
        app.lookout.shutdown();
    }
    for e in fs::read_dir(DataPaths::new(&data).content_dir())
        .unwrap()
        .flatten()
    {
        let bytes = fs::read(e.path()).unwrap();
        assert!(
            !bytes.windows(8).any(|w| w == b"zanzibar"),
            "plaintext in {:?}",
            e.path()
        );
    }
    // Paused restart: whatever is found comes from the stored index.
    let pause = PauseState::new();
    pause.set(true);
    let mut app = start(
        &data,
        &root,
        Opts {
            pause,
            ..Opts::default()
        },
    );
    assert_eq!(app.content_hits("zanzibar").len(), 1);
}

#[test]
fn masked_secrets_excluded_folders_and_sensitive_files_are_never_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let card = "4111 1111 1111 1111";
    fs::write(
        root.join("deploy.txt"),
        format!("platypus deploy notes\nkey {aws}\ncard {card}\npassword = Tr0ub4dor3xyz"),
    )
    .unwrap();
    fs::create_dir_all(root.join("private")).unwrap();
    fs::write(root.join("private").join("diary.txt"), "wombat in private").unwrap();
    fs::create_dir_all(root.join("node_modules")).unwrap();
    fs::write(
        root.join("node_modules").join("lib.txt"),
        "wombat in modules",
    )
    .unwrap();
    fs::write(root.join(".env"), "wombat=1").unwrap();
    fs::write(root.join("server.pem"), "wombat key").unwrap();
    let s = Settings {
        excluded_folders: vec![root.join("private")],
        ..settings()
    };
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            settings: s,
            ..Opts::default()
        },
    );
    app.wait_content_idle();

    // The file itself is indexed, but its secrets are not.
    assert_eq!(app.content_hits("platypus").len(), 1);
    assert!(app.content_hits("AKIAIOSFODNN7EXAMPLE").is_empty());
    assert!(app.content_hits("4111").is_empty());
    assert!(app.content_hits("Tr0ub4dor3xyz").is_empty());
    let preview = app
        .ask("platypus")
        .items
        .into_iter()
        .find(|i| i.source == "content")
        .and_then(|i| i.subtitle)
        .unwrap();
    assert!(!preview.contains("AKIA") && !preview.contains("4111") && preview.contains("[masked]"));
    // Nothing from excluded or sensitive places.
    assert!(app.content_hits("wombat").is_empty());
    // And no stored byte of the index contains the secrets either.
    app.lookout.shutdown();
    for e in fs::read_dir(app.paths.content_dir()).unwrap().flatten() {
        let b = fs::read(e.path()).unwrap();
        assert!(!b.windows(4).any(|w| w == b"AKIA"));
    }
}

#[test]
fn pause_really_stops_content_extraction_and_resume_continues() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let pause = PauseState::new();
    pause.set(true);
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            pause: pause.clone(),
            ..Opts::default()
        },
    );
    std::thread::sleep(Duration::from_millis(800));
    let stats = app.lookout.content_stats().unwrap();
    assert_eq!(
        stats.extractions.load(Ordering::Relaxed),
        0,
        "read a file while paused"
    );
    assert_eq!(stats.docs.load(Ordering::Relaxed), 0);
    assert!(app.content_hits("zanzibar").is_empty());
    // New files made while paused are not read either.
    fs::write(root.join("late.txt"), "late arrival quokka").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(stats.extractions.load(Ordering::Relaxed), 0);

    pause.set(false);
    eventually("indexing after resume", || {
        !app.content_hits("quokka").is_empty() && !app.content_hits("zanzibar").is_empty()
    });
}

/// Pauses the moment the Nth unit of work asks to start.
struct PauseAfter {
    calls: AtomicUsize,
    after: usize,
    pause: PauseState,
}

impl IdleSource for PauseAfter {
    fn idle_for(&self) -> Duration {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 >= self.after {
            self.pause.set(true);
        }
        Duration::from_secs(3600)
    }
}

#[test]
fn pausing_in_the_middle_of_a_batch_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("many");
    fs::create_dir_all(&root).unwrap();
    for i in 0..120 {
        fs::write(
            root.join(format!("f{i:03}.txt")),
            format!("content number {i}"),
        )
        .unwrap();
    }
    let pause = PauseState::new();
    let idle = Arc::new(PauseAfter {
        calls: AtomicUsize::new(0),
        after: 30,
        pause: pause.clone(),
    });
    let app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            idle,
            pause: pause.clone(),
            ..Opts::default()
        },
    );
    eventually("pause to be triggered", || pause.is_paused());
    std::thread::sleep(Duration::from_millis(500));
    let stats = app.lookout.content_stats().unwrap();
    let frozen = stats.extractions.load(Ordering::Relaxed);
    assert!(frozen < 120, "the whole batch ran: {frozen}");
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(
        stats.extractions.load(Ordering::Relaxed),
        frozen,
        "extraction continued while paused"
    );
}

#[test]
fn content_work_waits_for_the_user_to_be_idle() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let idle = Arc::new(ManualIdle::default()); // user is active
    let s = Settings {
        idle_secs: 5,
        ..settings()
    };
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            settings: s,
            idle: idle.clone(),
            ..Opts::default()
        },
    );
    std::thread::sleep(Duration::from_millis(800));
    let stats = app.lookout.content_stats().unwrap();
    assert_eq!(
        stats.extractions.load(Ordering::Relaxed),
        0,
        "worked while the user was active"
    );
    idle.set(Duration::from_secs(6));
    eventually("indexing once idle", || {
        !app.content_hits("zanzibar").is_empty()
    });
}

#[test]
fn edits_and_deletions_are_picked_up_by_the_watcher() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let mut app = start(&dir.path().join("data"), &root, Opts::default());
    app.wait_content_idle();
    let note = root.join("notes").join("meeting.txt");
    std::thread::sleep(Duration::from_millis(1100)); // distinct modified time
    fs::write(&note, "Now it is all about the narwhal budget").unwrap();
    eventually("edit", || {
        !app.content_hits("narwhal").is_empty() && app.content_hits("zanzibar").is_empty()
    });
    fs::write(root.join("fresh.txt"), "brand new axolotl file").unwrap();
    eventually("new file", || !app.content_hits("axolotl").is_empty());
    fs::remove_file(&note).unwrap();
    eventually("deletion", || app.content_hits("narwhal").is_empty());
    fs::remove_dir_all(root.join("notes")).ok();
}

#[test]
fn delete_my_index_also_removes_the_content_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let app = start(&dir.path().join("data"), &root, Opts::default());
    app.wait_content_idle();
    assert!(app.paths.content_dir().exists());
    app.lookout.wipe_index().unwrap();
    assert!(
        app.paths.leftovers().is_empty(),
        "{:?}",
        app.paths.leftovers()
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        app.paths.leftovers().is_empty(),
        "re-created: {:?}",
        app.paths.leftovers()
    );
}

// ------------------------------------------------------------------- OCR

use lookout_core::content::ocr::{OcrError, WindowsOcr};
use lookout_core::fixture_image;

/// Returns fixed text for every image and counts calls.
struct FakeOcr {
    text: String,
    calls: AtomicUsize,
}

impl Ocr for FakeOcr {
    fn recognize(&self, _: &Path) -> Result<String, OcrError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.text.clone())
    }
}

fn image_fixture(dir: &Path) -> PathBuf {
    let root = dir.join("shots");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.bmp"), fixture_image::bmp("INVOICE 4821", 8)).unwrap();
    fs::write(root.join("b.bmp"), fixture_image::bmp("MEETING AT NOON", 8)).unwrap();
    fs::write(root.join("notes.txt"), "the invoice folder is full").unwrap();
    root
}

#[test]
fn screenshot_with_text_finds_the_image_using_windows_ocr() {
    if WindowsOcr.recognize(Path::new("missing.png")).is_err() {
        // Distinguish "no engine" from "missing file" by trying a real image.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.bmp");
        fs::write(&p, fixture_image::bmp("TEST", 8)).unwrap();
        if matches!(WindowsOcr.recognize(&p), Err(OcrError::Unavailable)) {
            eprintln!("SKIPPED: Windows OCR is not available on this machine");
            return;
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let root = image_fixture(dir.path());
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            ocr: Arc::new(WindowsOcr),
            ..Opts::default()
        },
    );
    eventually("OCR of the screenshots", || {
        !app.content_hits("invoice 4821").is_empty()
    });
    app.wait_content_idle();
    let r = app.ask("screenshot with invoice 4821");
    let hits: Vec<_> = r.items.iter().filter(|i| i.source == "content").collect();
    assert_eq!(
        hits.len(),
        1,
        "{:?}",
        r.items.iter().map(|i| &i.title).collect::<Vec<_>>()
    );
    assert_eq!(hits[0].title, "a.bmp");
    assert_eq!(
        hits[0].metadata.get("ocr"),
        Some(&serde_json::Value::Bool(true))
    );
    eprintln!("OCR RAN: end-to-end screenshot search found a.bmp");
    // The text file that merely mentions "invoice" is not an image result.
    assert!(r.items.iter().all(|i| i.title != "notes.txt"));
    // Without the screenshot phrase both the image and the text file match "invoice".
    let all = app.content_hits("invoice");
    assert_eq!(all.len(), 2, "{all:?}");
    // Both indexes know the image by name too.
    assert!(
        app.lookout
            .content_stats()
            .unwrap()
            .ocr_runs
            .load(Ordering::Relaxed)
            >= 2
    );
}

/// An engine that finds every picture too large.
struct TooBigOcr;

impl Ocr for TooBigOcr {
    fn recognize(&self, _: &Path) -> Result<String, OcrError> {
        Err(OcrError::TooLarge(20_000))
    }
}

#[test]
fn a_picture_over_the_size_limit_is_shown_in_status_as_skipped_too_large() {
    let dir = tempfile::tempdir().unwrap();
    let root = image_fixture(dir.path());
    let app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            ocr: Arc::new(TooBigOcr),
            ..Opts::default()
        },
    );
    let stats = app.lookout.content_stats().unwrap();
    eventually("skipped: too large in Status", || {
        stats
            .last_error()
            .is_some_and(|e| e.starts_with("skipped: too large: ") && e.ends_with(".bmp"))
    });
    // Not "could not read": the picture is recorded (empty) so it is not retried.
    assert!(!stats.last_error().unwrap().contains("could not read"));
}

#[test]
fn text_read_from_images_is_masked_before_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let root = image_fixture(dir.path());
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let ocr = Arc::new(FakeOcr {
        text: format!("receipt kumquat {aws} card 4111 1111 1111 1111"),
        calls: AtomicUsize::new(0),
    });
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            ocr,
            ..Opts::default()
        },
    );
    eventually("OCR text indexed", || {
        !app.content_hits("kumquat").is_empty()
    });
    app.wait_content_idle();
    assert!(app.content_hits("AKIAIOSFODNN7EXAMPLE").is_empty());
    assert!(app.content_hits("4111").is_empty());
}

#[test]
fn pause_stops_ocr_and_ocr_can_be_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let root = image_fixture(dir.path());
    let ocr = Arc::new(FakeOcr {
        text: "walrus".into(),
        calls: AtomicUsize::new(0),
    });
    let pause = PauseState::new();
    pause.set(true);
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            ocr: ocr.clone(),
            pause: pause.clone(),
            ..Opts::default()
        },
    );
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(ocr.calls.load(Ordering::SeqCst), 0, "OCR ran while paused");
    pause.set(false);
    eventually("OCR after resume", || {
        !app.content_hits("walrus").is_empty()
    });
    app.lookout.shutdown();

    // Settings.ocr = false: images are never read.
    let ocr2 = Arc::new(FakeOcr {
        text: "walrus".into(),
        calls: AtomicUsize::new(0),
    });
    let off = Settings {
        ocr: false,
        ..settings()
    };
    let app2 = start(
        &dir.path().join("data2"),
        &root,
        Opts {
            settings: off,
            ocr: ocr2.clone(),
            ..Opts::default()
        },
    );
    app2.wait_content_idle();
    assert_eq!(ocr2.calls.load(Ordering::SeqCst), 0);
}

// ------------------------------------------------------------ time filters

fn set_age(path: &Path, days: u64, hours_extra: u64) {
    let when =
        std::time::SystemTime::now() - Duration::from_secs(days * 86_400 + hours_extra * 3600);
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(when).unwrap();
}

#[test]
fn time_phrases_narrow_name_and_content_results() {
    use chrono::{Datelike, Local};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("t");
    fs::create_dir_all(&root).unwrap();
    for name in ["recent", "weekold", "ancient"] {
        fs::write(
            root.join(format!("report {name}.txt")),
            format!("quarterly numbers {name}"),
        )
        .unwrap();
    }
    // 30 hours ago is always "yesterday" or "today"... pick 36 h: yesterday or the day before.
    // Use whole days at midday to be unambiguous: 1 day back from now is yesterday
    // unless now is before 01:00, so back off to the middle of yesterday.
    let now = Local::now();
    let into_today = u64::from(chrono::Timelike::hour(&now)) * 3600
        + u64::from(chrono::Timelike::minute(&now)) * 60;
    let midday_yesterday = into_today + 12 * 3600; // seconds before now
    let set_secs = |p: &Path, secs_ago: u64| {
        let f = fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    };
    set_secs(&root.join("report recent.txt"), midday_yesterday);
    set_age(&root.join("report weekold.txt"), 9, 0);
    set_age(&root.join("report ancient.txt"), 40, 0);
    let ancient_month = (now - chrono::Duration::days(40))
        .format("%B")
        .to_string()
        .to_lowercase();

    let mut app = start(&dir.path().join("data"), &root, Opts::default());
    app.wait_content_idle();
    let names = |app: &mut App, q: &str| -> Vec<String> {
        let mut v: Vec<String> = app.ask(q).items.into_iter().map(|i| i.title).collect();
        v.sort();
        v.dedup();
        v
    };
    // File names: only the file modified yesterday matches.
    assert_eq!(names(&mut app, "report yesterday"), ["report recent.txt"]);
    // Contents too.
    assert_eq!(
        names(&mut app, "quarterly yesterday"),
        ["report recent.txt"]
    );
    assert_eq!(app.content_hits("quarterly last 3 days").len(), 1);
    assert_eq!(app.content_hits("quarterly").len(), 3);
    // A time phrase alone lists recent files.
    assert_eq!(names(&mut app, "yesterday"), ["report recent.txt"]);
    // A month name reaches back to the 40-day-old file.
    let in_month = names(&mut app, &format!("quarterly in {ancient_month}"));
    assert!(
        in_month.contains(&"report ancient.txt".to_owned()),
        "{in_month:?}"
    );
    assert!(
        !in_month.contains(&"report recent.txt".to_owned()),
        "{in_month:?}"
    );
    // Nothing matches "last year" in this fixture.
    if now.month() > 2 {
        // (In January and February the 40-day-old file is itself from last year.)
        assert!(names(&mut app, "quarterly last year").is_empty());
    }
}

/// Restores a file's modified time (whole seconds are all the index compares).
fn set_mtime(path: &Path, t: std::time::SystemTime) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(t).unwrap();
}

#[test]
fn an_edit_that_keeps_the_same_modified_second_is_still_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let mut app = start(&dir.path().join("data"), &root, Opts::default());
    app.wait_content_idle();
    let note = root.join("notes").join("meeting.txt");
    assert!(!app.content_hits("zanzibar").is_empty());
    let before = fs::metadata(&note).unwrap().modified().unwrap();
    fs::write(&note, "We discussed the quokka migration plan").unwrap();
    set_mtime(&note, before);
    eventually("edit with unchanged mtime", || {
        !app.content_hits("quokka").is_empty() && app.content_hits("zanzibar").is_empty()
    });
}

#[test]
fn a_renamed_and_a_moved_file_are_found_at_the_new_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let mut app = start(&dir.path().join("data"), &root, Opts::default());
    app.wait_content_idle();
    let old = root.join("notes").join("meeting.txt");
    let renamed = root.join("notes").join("agenda.txt");
    fs::rename(&old, &renamed).unwrap();
    eventually("rename", || {
        let hits = app.content_hits("zanzibar");
        hits.len() == 1 && hits[0].ends_with("agenda.txt")
    });
    fs::create_dir_all(root.join("archive")).unwrap();
    let moved = root.join("archive").join("agenda.txt");
    fs::rename(&renamed, &moved).unwrap();
    eventually("move", || {
        let hits = app.content_hits("zanzibar");
        hits.len() == 1 && hits[0].contains("archive")
    });
}

#[test]
fn a_new_small_file_is_searchable_within_seconds_even_when_the_user_is_active() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let idle = Arc::new(ManualIdle::default()); // user is typing
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            settings: Settings {
                idle_secs: 600,
                ..settings()
            },
            idle,
            ..Opts::default()
        },
    );
    std::thread::sleep(Duration::from_millis(600)); // watcher is up
    let t0 = Instant::now();
    fs::write(root.join("instant.txt"), "the platypus appears").unwrap();
    // Normally well under a second; a loaded CI machine gets 10 s before this
    // counts as a stall. The measured time is always logged.
    let deadline = t0 + Duration::from_secs(10);
    while app.content_hits("platypus").is_empty() {
        assert!(
            Instant::now() < deadline,
            "not searchable within 10 s (the indexer is stalled, not just slow)"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!("time to searchable: {} ms", t0.elapsed().as_millis());
    // The same applies to an edit, but a big file still waits for idle.
    fs::write(
        root.join("big.txt"),
        format!("wombat {}", "x ".repeat(600_000)),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        app.content_hits("wombat").is_empty(),
        "big file ignored the idle gate"
    );
}

#[test]
fn the_instant_path_stops_while_paused_and_continues_after() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let pause = PauseState::new();
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            pause: pause.clone(),
            ..Opts::default()
        },
    );
    app.wait_content_idle();
    pause.set(true);
    fs::write(root.join("held.txt"), "held numbat").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        app.content_hits("numbat").is_empty(),
        "indexed while paused"
    );
    pause.set(false);
    eventually("after resume", || !app.content_hits("numbat").is_empty());
}

#[test]
fn index_now_ignores_idle_and_health_tells_the_truth() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    fs::write(
        root.join("big.txt"),
        format!("wombat {}", "x ".repeat(600_000)),
    )
    .unwrap();
    let idle = Arc::new(ManualIdle::default());
    let mut app = start(
        &dir.path().join("data"),
        &root,
        Opts {
            settings: Settings {
                idle_secs: 600,
                ..settings()
            },
            idle,
            ..Opts::default()
        },
    );
    eventually("waiting for idle is reported", || {
        app.lookout.health().content.state == "waiting for idle"
    });
    assert!(app.content_hits("wombat").is_empty());
    assert_eq!(app.lookout.health().screenshots.state, "unavailable"); // NoOcr
    assert!(app.lookout.index_now(true));
    eventually("index now reads everything", || {
        !app.content_hits("wombat").is_empty() && !app.content_hits("zanzibar").is_empty()
    });
    eventually("index now ends by itself", || {
        let h = app.lookout.health().content;
        !h.index_now_running && h.files_queued == 0
    });
    app.lookout.index_now(false);
}

// ------------------------------------------------------------ size limit

mod cap {
    use super::*;
    use lookout_core::content::service::{ContentConfig, ContentService};
    use lookout_core::content::store::{ContentQuery, ContentStore};
    use lookout_core::exclude::Excludes;

    const FILES: usize = 40;

    /// `FILES` text files of unique words, so every one adds to the index.
    fn many_files(dir: &Path) -> PathBuf {
        let root = dir.join("many");
        fs::create_dir_all(&root).unwrap();
        for i in 0..FILES {
            let text: String = (0..8000).map(|j| format!("t{i}q{j} ")).collect();
            fs::write(root.join(format!("f{i}.txt")), text).unwrap();
        }
        root
    }

    /// A helper that counts how often it is asked.
    struct Counting(Arc<std::sync::atomic::AtomicUsize>);

    impl lookout_core::content::office::OfficeText for Counting {
        fn available(&self) -> bool {
            true
        }
        fn extract(
            &self,
            _: &str,
            _: &[u8],
        ) -> Result<lookout_core::content::office::OfficeOutcome, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(lookout_core::content::office::OfficeOutcome::Text(
                "pdfword".into(),
            ))
        }
    }

    fn start_service(root: &Path, store: &Arc<ContentStore>, max_bytes: u64) -> ContentService {
        start_service_with(root, store, max_bytes, None)
    }

    fn start_service_with(
        root: &Path,
        store: &Arc<ContentStore>,
        max_bytes: u64,
        office: Option<Arc<dyn lookout_core::content::office::OfficeText>>,
    ) -> ContentService {
        ContentService::start(ContentConfig {
            roots: vec![root.to_path_buf()],
            excludes: Excludes::default(),
            store: store.clone(),
            ocr: None,
            office,
            idle: Arc::new(AlwaysIdle),
            idle_secs: 0,
            duty: 1.0,
            commit_every: 1,
            pause: PauseState::new(),
            max_bytes,
            fast: false,
        })
        .unwrap()
    }

    /// Waits for the first scan round to have run and finished.
    fn wait_done(svc: &ContentService) {
        let stats = svc.stats();
        eventually("scan finishes", || {
            stats.scans_done.load(Ordering::Acquire) >= 1 && !stats.busy.load(Ordering::Acquire)
        });
    }

    /// Asks for a rescan and waits for a round that ended after the request.
    fn rescan_done(svc: &ContentService) {
        let stats = svc.stats();
        let before = stats.scans_done.load(Ordering::Acquire);
        svc.request_rescan();
        eventually("rescan finishes", || {
            stats.scans_done.load(Ordering::Acquire) > before && !stats.busy.load(Ordering::Acquire)
        });
    }

    fn found(store: &ContentStore, i: usize) -> bool {
        let text = format!("t{i}q5");
        !store
            .search(&ContentQuery {
                text: &text,
                limit: 5,
                ..ContentQuery::default()
            })
            .unwrap()
            .is_empty()
    }

    #[test]
    fn indexing_stops_at_the_cap_and_resumes_when_it_is_raised() {
        let dir = tempfile::tempdir().unwrap();
        let root = many_files(dir.path());
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        let cap = 150 * 1024;
        let svc = start_service(&root, &store, cap);
        wait_done(&svc);

        // Stopped: some files are in, not all, and the index is at the cap.
        let kept = store.all_paths();
        assert!(!kept.is_empty() && kept.len() < FILES, "{}", kept.len());
        assert!(svc.stats().full());
        assert!(store.size_bytes() >= cap);

        // Everything already indexed stays, and stays searchable, even after another scan.
        rescan_done(&svc);
        let mut after = store.all_paths();
        let mut before = kept.clone();
        before.sort();
        after.sort();
        assert_eq!(before, after, "a full index neither adds nor evicts");
        for p in &kept {
            let i: usize = p
                .rsplit('f')
                .next()
                .unwrap()
                .trim_end_matches(".txt")
                .parse()
                .unwrap();
            assert!(found(&store, i), "file {i} no longer searchable");
        }

        // Raising the cap resumes indexing until everything is in.
        svc.set_max_bytes(512 * 1024 * 1024);
        let stats = svc.stats();
        eventually_for(
            "indexing resumes",
            Duration::from_secs(60),
            || {
                format!(
                    "index size {} (tracked {}), cap {}, files indexed {}/{FILES}, busy {}",
                    store.size_bytes(),
                    stats.size_bytes.load(Ordering::Relaxed),
                    stats.max_bytes.load(Ordering::Relaxed),
                    store.doc_count(),
                    stats.busy.load(Ordering::Acquire)
                )
            },
            || store.doc_count() == FILES as u64,
        );
        assert!(!svc.stats().full());
        for i in 0..FILES {
            assert!(found(&store, i));
        }
    }

    /// A helper that dies on files containing "BOOM" and counts every call.
    struct Dying(Arc<std::sync::atomic::AtomicUsize>);

    impl lookout_core::content::office::OfficeText for Dying {
        fn available(&self) -> bool {
            true
        }
        fn extract(
            &self,
            _: &str,
            bytes: &[u8],
        ) -> Result<lookout_core::content::office::OfficeOutcome, String> {
            if bytes.starts_with(b"BOOM") {
                self.0.fetch_add(1, Ordering::SeqCst);
                return Err(format!(
                    "{}died",
                    lookout_core::content::office::CRASH_PREFIX
                ));
            }
            Ok(lookout_core::content::office::OfficeOutcome::Text(
                "okword".into(),
            ))
        }
    }

    #[test]
    fn a_file_that_kills_the_helper_is_tried_twice_per_version() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("crash");
        fs::create_dir_all(&root).unwrap();
        let bad = root.join("a_bad.pdf");
        fs::write(&bad, b"BOOM 1").unwrap();
        fs::write(root.join("z_good.pdf"), b"%PDF fine").unwrap();
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let svc = start_service_with(
            &root,
            &store,
            512 * 1024 * 1024,
            Some(Arc::new(Dying(calls.clone()))),
        );
        wait_done(&svc);
        // The file after the crashing one is indexed anyway.
        assert!(store.all_paths().iter().any(|p| p.ends_with("z_good.pdf")));
        assert!(!store.all_paths().iter().any(|p| p.ends_with("a_bad.pdf")));
        for _ in 0..4 {
            rescan_done(&svc);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one try and one retry");

        // A changed file (new size) gets a new attempt, again at most two.
        fs::write(&bad, b"BOOM 12345").unwrap();
        for _ in 0..4 {
            rescan_done(&svc);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    /// A helper whose calls always time out; counts calls.
    struct TimingOut(Arc<std::sync::atomic::AtomicUsize>);

    impl lookout_core::content::office::OfficeText for TimingOut {
        fn available(&self) -> bool {
            true
        }
        fn extract(
            &self,
            _: &str,
            _: &[u8],
        ) -> Result<lookout_core::content::office::OfficeOutcome, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(format!(
                "{}slow",
                lookout_core::content::office::TIMEOUT_PREFIX
            ))
        }
    }

    #[test]
    fn a_file_that_times_out_gets_three_attempts_with_delays_per_version() {
        use lookout_core::content::service::TIMEOUT_BACKOFF_SECS;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("slow");
        fs::create_dir_all(&root).unwrap();
        let slow = root.join("slow.pdf");
        fs::write(&slow, b"one").unwrap();
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let svc = start_service_with(
            &root,
            &store,
            512 * 1024 * 1024,
            Some(Arc::new(TimingOut(calls.clone()))),
        );
        wait_done(&svc);
        // The delay (60 s) has not passed: rescans do not ask again.
        for _ in 0..3 {
            rescan_done(&svc);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // With no delay, two more attempts, then no more.
        TIMEOUT_BACKOFF_SECS.store(0, Ordering::Relaxed);
        for _ in 0..5 {
            rescan_done(&svc);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3, "three attempts in all");
        // A changed file gets new attempts.
        fs::write(&slow, b"changed!").unwrap();
        for _ in 0..5 {
            rescan_done(&svc);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        TIMEOUT_BACKOFF_SECS.store(60, Ordering::Relaxed);
        assert!(svc
            .stats()
            .last_error
            .lock()
            .unwrap()
            .as_deref()
            .unwrap()
            .contains("timed out"));
    }

    #[test]
    fn a_full_index_skips_pdfs_without_reading_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = many_files(dir.path());
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let svc = start_service_with(
            &root,
            &store,
            150 * 1024,
            Some(Arc::new(Counting(calls.clone()))),
        );
        wait_done(&svc);
        assert!(svc.stats().full());
        let reads_before = svc.stats().extractions.load(Ordering::Relaxed);
        calls.store(0, Ordering::SeqCst);
        for i in 0..5 {
            fs::write(root.join(format!("late{i}.pdf")), b"%PDF-1.4 whatever").unwrap();
        }
        rescan_done(&svc);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "the helper was asked");
        assert_eq!(
            svc.stats().extractions.load(Ordering::Relaxed),
            reads_before,
            "a PDF was read while the index was full"
        );
        assert!(!store.all_paths().iter().any(|p| p.ends_with(".pdf")));
        // With room again they are read.
        svc.set_max_bytes(512 * 1024 * 1024);
        let pdfs = || {
            store
                .all_paths()
                .iter()
                .filter(|p| p.ends_with(".pdf"))
                .count()
        };
        // This test uses a stub helper: no process, so no start count, crash
        // records or backoff; the call count and the service's state say it all.
        eventually_progress(
            "pdfs indexed",
            Duration::from_secs(60),
            || calls.load(Ordering::SeqCst) as u64 + svc.stats().docs.load(Ordering::Relaxed),
            || {
                let s = svc.stats();
                format!(
                    "pdfs indexed {}, calls {}, errors {} (last {:?}), docs {}, size {} cap {}, \
                     state {}, busy {}, queued {}, pending {}",
                    pdfs(),
                    calls.load(Ordering::SeqCst),
                    s.errors.load(Ordering::Relaxed),
                    s.last_error(),
                    s.docs.load(Ordering::Relaxed),
                    s.size_bytes.load(Ordering::Relaxed),
                    s.max_bytes.load(Ordering::Relaxed),
                    s.state.load(Ordering::Relaxed),
                    s.busy.load(Ordering::Relaxed),
                    s.queued.load(Ordering::Relaxed),
                    s.pending.load(Ordering::Relaxed),
                )
            },
            || pdfs() == 5,
        );
    }

    #[test]
    fn a_full_index_still_finds_files_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = many_files(dir.path());
        let mut app = start(
            &dir.path().join("data"),
            &root,
            Opts {
                settings: Settings {
                    content_index_max_mb: 1,
                    ..settings()
                },
                ..Opts::default()
            },
        );
        // 40 files of ~70 KB of unique words: the 1 MB cap is reached.
        eventually_for(
            "index full",
            Duration::from_secs(60),
            || {
                let c = app.lookout.health().content;
                format!(
                    "index size {}, cap {}, state {:?}, files done {} queued {}, busy {}",
                    c.size_bytes,
                    c.max_bytes,
                    c.state,
                    c.files_done,
                    c.files_queued,
                    c.index_now_running
                )
            },
            || app.lookout.health().content.state == "full",
        );
        let h = app.lookout.health().content;
        assert!(h.full && h.size_bytes >= h.max_bytes && h.max_bytes == 1 << 20);
        assert!(h
            .reason
            .contains("Content index full: raise the limit or delete the index"));
        // Names are unaffected, including a file created after the index filled.
        fs::write(root.join("latecomer_aardvark.txt"), "quokkaword").unwrap();
        eventually("name found while full", || {
            app.ask("latecomer_aardvark")
                .items
                .iter()
                .any(|i| i.title == "latecomer_aardvark.txt")
        });
        app.lookout.index_now(true);
        app.wait_content_idle();
        assert!(
            app.content_hits("quokkaword").is_empty(),
            "nothing new while full"
        );
    }
    #[cfg(windows)]
    #[test]
    fn a_locked_office_file_is_retried_after_it_is_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("locked");
        fs::create_dir_all(&root).unwrap();
        let pdf = root.join("busy.pdf");
        fs::write(&pdf, b"%PDF-1.4 whatever").unwrap();
        // Another program has the file open and allows nobody else to read it.
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&pdf)
            .unwrap();
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let svc = start_service_with(
            &root,
            &store,
            512 * 1024 * 1024,
            Some(Arc::new(Counting(calls.clone()))),
        );
        wait_done(&svc);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(lock);
        rescan_done(&svc);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "never retried after the unlock"
        );
        assert!(store.all_paths().iter().any(|p| p.ends_with("busy.pdf")));
    }

    /// A fake OCR step: counts calls and returns fixed text.
    struct FakeOcr(Arc<std::sync::atomic::AtomicUsize>);

    impl lookout_core::content::ocr::Ocr for FakeOcr {
        fn recognize(&self, _: &Path) -> Result<String, lookout_core::content::ocr::OcrError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("ocrword".into())
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_image_is_retried_after_it_is_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("locked");
        fs::create_dir_all(&root).unwrap();
        let img = root.join("busy.png");
        fs::write(&img, b"not really a png").unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&img)
            .unwrap();
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let svc = ContentService::start(ContentConfig {
            roots: vec![root.clone()],
            excludes: Excludes::default(),
            store: store.clone(),
            ocr: Some(Arc::new(FakeOcr(calls.clone()))),
            office: None,
            idle: Arc::new(AlwaysIdle),
            idle_secs: 0,
            duty: 1.0,
            commit_every: 1,
            pause: PauseState::new(),
            max_bytes: 512 * 1024 * 1024,
            fast: false,
        })
        .unwrap();
        wait_done(&svc);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(store.all_paths().is_empty(), "recorded while locked");
        drop(lock);
        rescan_done(&svc);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "never retried after the unlock"
        );
        assert!(store.all_paths().iter().any(|p| p.ends_with("busy.png")));
    }
}

// ------------------------------------------------------- queue and OCR cost

mod queue {
    use super::*;
    use lookout_core::content::ocr::OcrError;
    use lookout_core::content::service::{ContentConfig, ContentService, ContentStats};
    use lookout_core::content::store::ContentStore;
    use lookout_core::exclude::Excludes;
    use lookout_core::fixture_image;
    use std::sync::OnceLock;

    /// Records how many documents were already written when each image is read.
    struct SeeingOcr {
        stats: Arc<OnceLock<Arc<ContentStats>>>,
        docs_seen: Arc<Mutex<Vec<u64>>>,
    }

    impl Ocr for SeeingOcr {
        fn recognize(&self, _: &Path) -> Result<String, OcrError> {
            let docs = self
                .stats
                .get()
                .map_or(0, |s| s.docs.load(Ordering::Relaxed));
            self.docs_seen.lock().unwrap().push(docs);
            Ok("picture words".into())
        }
    }

    fn service(
        root: &Path,
        store: &Arc<ContentStore>,
        ocr: Option<Arc<dyn Ocr>>,
        idle: Arc<dyn IdleSource>,
        fast: bool,
    ) -> ContentService {
        ContentService::start(ContentConfig {
            roots: vec![root.to_path_buf()],
            excludes: lookout_core::exclude::Excludes::default(),
            store: store.clone(),
            ocr,
            office: None,
            idle,
            idle_secs: 0,
            duty: 1.0,
            commit_every: 1,
            pause: PauseState::new(),
            max_bytes: 512 * 1024 * 1024,
            fast,
        })
        .unwrap()
    }

    #[test]
    fn documents_come_first_then_pictures_and_icons_and_copies_cost_no_ocr() {
        let _ = Excludes::default();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mixed");
        fs::create_dir_all(&root).unwrap();
        // Pictures sort before documents by name, so only the queue order can put them last.
        let big = fixture_image::bmp("INVOICE 4821 AND MORE WORDS", 8);
        fs::write(root.join("a1.bmp"), &big).unwrap();
        fs::write(root.join("a2.bmp"), &big).unwrap(); // an exact copy
        fs::write(root.join("a3.bmp"), fixture_image::bmp("A", 1)).unwrap(); // an icon
        for i in 0..5 {
            fs::write(root.join(format!("z{i}.txt")), format!("document {i}")).unwrap();
        }
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        let cell: Arc<OnceLock<Arc<ContentStats>>> = Arc::default();
        let seen: Arc<Mutex<Vec<u64>>> = Arc::default();
        let ocr = SeeingOcr {
            stats: cell.clone(),
            docs_seen: seen.clone(),
        };
        // The user is away at first, so nothing starts before the stats are known.
        let idle = Arc::new(ManualIdle::default());
        let svc = service(&root, &store, Some(Arc::new(ocr)), idle.clone(), false);
        let stats = svc.stats();
        cell.set(stats.clone()).ok();
        idle.set(Duration::from_secs(3600));
        eventually("scan finishes", || {
            stats.scans_done.load(Ordering::Acquire) >= 1 && !stats.busy.load(Ordering::Acquire)
        });
        // One real OCR run: the copy and the icon were not read.
        assert_eq!(
            *seen.lock().unwrap(),
            vec![5],
            "all five documents were written first"
        );
        assert_eq!(stats.ocr_runs.load(Ordering::Relaxed), 1);
        assert_eq!(stats.ocr_skipped.load(Ordering::Relaxed), 2);
        // The copy is still findable by its text.
        assert_eq!(store.all_paths().len(), 8);
    }

    #[test]
    fn fast_speed_reads_in_parallel_only_after_a_minute_idle_with_the_same_result() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("many");
        fs::create_dir_all(&root).unwrap();
        for i in 0..300 {
            fs::write(root.join(format!("f{i}.txt")), format!("uniqueword{i}")).unwrap();
        }
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        let idle = Arc::new(ManualIdle::default());
        idle.set(Duration::from_secs(3600));
        let svc = service(&root, &store, None, idle.clone(), true);
        let stats = svc.stats();
        eventually("scan finishes", || {
            stats.scans_done.load(Ordering::Acquire) >= 1 && !stats.busy.load(Ordering::Acquire)
        });
        assert_eq!(store.all_paths().len(), 300);
        assert_eq!(stats.docs.load(Ordering::Relaxed), 300);
        assert_eq!(
            stats.read_done.load(Ordering::Relaxed),
            0,
            "progress is cleared when done"
        );
    }
}
