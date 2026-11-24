//! The real helper process behind the real host and the real content indexer:
//! files are found by a word inside, secrets are masked, a bad file is skipped,
//! and a crashed helper costs one call. Temp folders only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{docx, pdf, pdf_encrypted, pdf_scan, pptx, xlsx, zip_of};
use lookout_core::actions::open::NoopLauncher;
use lookout_core::actions::GateConfig;
use lookout_core::content::idle::AlwaysIdle;
use lookout_core::content::ocr::NoOcr;
use lookout_core::content::office::{OfficeOutcome, OfficeText, PluginOffice, PLUGIN_ID};
use lookout_core::crypto::Key;
use lookout_core::engine::{Sink, Update};
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::plugins::HostConfig;
use lookout_core::settings::Settings;
use lookout_core::storage::{DataPaths, Storage};

fn install_helper(dir: &Path) -> HostConfig {
    let plugin = dir.join("plugins").join("office-text");
    fs::create_dir_all(&plugin).unwrap();
    let exe = plugin.join("lookout-office-text.exe");
    let built = env!("CARGO_BIN_EXE_lookout-office-text");
    if fs::hard_link(built, &exe).is_err() {
        fs::copy(built, &exe).unwrap();
    }
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json"),
        plugin.join("plugin.json"),
    )
    .unwrap();
    HostConfig::new(vec![dir.join("plugins")], dir.join("data"))
}

struct App {
    lookout: Lookout,
    updates: Arc<Mutex<Vec<Update>>>,
    next: u64,
}

fn start(dir: &Path, root: &Path, enable: bool) -> App {
    let updates: Arc<Mutex<Vec<Update>>> = Arc::default();
    let u = updates.clone();
    let sink: Sink = Arc::new(move |update| u.lock().unwrap().push(update));
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
            start_menu_dirs: vec![],
            storage: Storage::with_key(paths, Key::from_bytes(&[5u8; 32]).unwrap()).unwrap(),
            idle: Arc::new(AlwaysIdle),
            ocr: Arc::new(NoOcr),
            launcher: Arc::new(NoopLauncher),
            system: None,
            gate: GateConfig::default(),
            plugins: Some(install_helper(dir)),
        },
        sink,
        PauseState::new(),
    )
    .unwrap();
    if enable {
        lookout
            .plugins()
            .unwrap()
            .set_enabled(PLUGIN_ID, true, true)
            .unwrap();
    }
    App {
        lookout,
        updates,
        next: 0,
    }
}

impl App {
    fn hits(&mut self, text: &str) -> Vec<String> {
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
                return u
                    .items
                    .iter()
                    .filter(|i| i.source == "content")
                    .filter_map(|i| i.uri.clone())
                    .collect();
            }
            assert!(Instant::now() < deadline, "query never completed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Scans now and waits until the worker is idle again.
    fn index_and_wait(&self) {
        let stats = self.lookout.content_stats().unwrap();
        self.lookout.index_now(true);
        std::thread::sleep(Duration::from_millis(300));
        let deadline = Instant::now() + Duration::from_secs(60);
        while stats.busy.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "indexing never finished");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn dir_size(p: &Path) -> u64 {
    fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

#[test]
fn office_files_are_found_by_a_word_inside_and_secrets_are_masked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("docs");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.docx"), docx(&["The zanzibar migration plan"])).unwrap();
    fs::write(root.join("b.xlsx"), xlsx(&["quokka budget"])).unwrap();
    fs::write(root.join("c.pptx"), pptx(&["wombat roadmap"])).unwrap();
    fs::write(
        root.join("secret.docx"),
        docx(&["platypus notes", "password=hunter2hunter2"]),
    )
    .unwrap();
    fs::write(root.join("corrupt.docx"), b"PK\x03\x04 not really a zip").unwrap();
    let mut app = start(dir.path(), &root, true);
    app.index_and_wait();
    assert_eq!(app.hits("zanzibar").len(), 1);
    assert_eq!(app.hits("quokka").len(), 1);
    assert_eq!(app.hits("wombat").len(), 1);
    // The document is indexed, its secret is not.
    assert_eq!(app.hits("platypus").len(), 1);
    assert!(app.hits("hunter2hunter2").is_empty());
    assert!(app.hits("hunter2").is_empty());
    // The corrupt file did not stop the others and is not an error.
    assert_eq!(
        app.lookout
            .content_stats()
            .unwrap()
            .errors
            .load(Ordering::Relaxed),
        0
    );
}

#[test]
fn pdfs_are_found_masked_or_skipped_through_the_real_helper() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("docs");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.pdf"), pdf(&["The zanzibar migration plan"])).unwrap();
    fs::write(
        root.join("secret.pdf"),
        pdf(&["platypus notes", "password=hunter2hunter2"]),
    )
    .unwrap();
    fs::write(root.join("locked.pdf"), pdf_encrypted("narwhal locked")).unwrap();
    fs::write(root.join("scan.pdf"), pdf_scan()).unwrap();
    let good = pdf(&["pangolin truncated"]);
    fs::write(root.join("cut.pdf"), &good[..good.len() / 2]).unwrap();
    fs::write(root.join("z_last.pdf"), pdf(&["ocelot after the bad ones"])).unwrap();
    let mut app = start(dir.path(), &root, true);
    app.index_and_wait();
    assert_eq!(app.hits("zanzibar").len(), 1);
    assert_eq!(app.hits("platypus").len(), 1);
    assert!(app.hits("hunter2hunter2").is_empty());
    assert!(app.hits("hunter2").is_empty());
    assert!(app.hits("narwhal").is_empty());
    // The helper survived the bad files and went on to the last one.
    assert_eq!(app.hits("ocelot").len(), 1);
    // Skipped files are recorded (not retried) and are not errors.
    let stats = app.lookout.content_stats().unwrap();
    assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
    let reads = stats.extractions.load(Ordering::Relaxed);
    app.index_and_wait();
    assert_eq!(stats.extractions.load(Ordering::Relaxed), reads, "retried");
}

#[test]
fn files_are_left_alone_while_the_helper_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("docs");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.docx"), docx(&["The zanzibar migration plan"])).unwrap();
    let mut app = start(dir.path(), &root, false);
    app.index_and_wait();
    assert!(app.hits("zanzibar").is_empty());
    // Turned on later, the next scan picks the file up.
    app.lookout
        .plugins()
        .unwrap()
        .set_enabled(PLUGIN_ID, true, true)
        .unwrap();
    app.index_and_wait();
    assert_eq!(app.hits("zanzibar").len(), 1);
}

#[test]
fn turning_the_helper_on_queues_the_files_it_could_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("docs");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.docx"), docx(&["alpha zanzibar"])).unwrap();
    fs::write(root.join("b.xlsx"), xlsx(&["bravo quokka"])).unwrap();
    fs::write(root.join("c.pptx"), pptx(&["charlie wombat"])).unwrap();
    fs::write(root.join("d.pdf"), pdf(&["delta ocelot"])).unwrap();
    let mut app = start(dir.path(), &root, false);
    app.index_and_wait();
    assert!(app.hits("zanzibar").is_empty());
    app.lookout
        .plugins()
        .unwrap()
        .set_enabled(PLUGIN_ID, true, true)
        .unwrap();
    app.lookout.rescan_contents(); // what the toggle does; no "Check files now"
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.hits("ocelot").is_empty() {
        assert!(Instant::now() < deadline, "the PDF was never read");
        std::thread::sleep(Duration::from_millis(100));
    }
    for w in ["zanzibar", "quokka", "wombat"] {
        assert_eq!(app.hits(w).len(), 1, "{w}");
    }
}

fn kill(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
}

#[test]
fn a_corrupt_file_is_skipped_and_a_crashed_helper_costs_one_call() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("docs");
    fs::create_dir_all(&root).unwrap();
    let app = start(dir.path(), &root, true);
    let host = app.lookout.plugins().unwrap().clone();
    let office = PluginOffice(host.clone());
    assert!(office.available());
    let good = docx(&["hello zanzibar"]);
    match office.extract("docx", &good).unwrap() {
        OfficeOutcome::Text(t) => assert!(t.contains("hello zanzibar")),
        other => panic!("{other:?}"),
    }
    // A corrupt file: a reason, and the helper is still the same process.
    let pid = host.pid(PLUGIN_ID).unwrap();
    assert!(matches!(
        office.extract("docx", b"not a zip").unwrap(),
        OfficeOutcome::Skipped(_)
    ));
    assert!(matches!(
        office
            .extract(
                "xlsx",
                &zip_of(&[("xl/sharedStrings.xml", b"<a><b></a>".to_vec())])
            )
            .unwrap(),
        OfficeOutcome::Skipped(_)
    ));
    assert_eq!(host.pid(PLUGIN_ID), Some(pid));
    // The helper dies: this process (the daemon's side) carries on, and a
    // later call is served by a new helper.
    kill(pid);
    std::thread::sleep(Duration::from_millis(300));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut failed_once = false;
    loop {
        match office.extract("docx", &good) {
            Ok(OfficeOutcome::Text(t)) => {
                assert!(t.contains("zanzibar"));
                break;
            }
            Ok(other) => panic!("{other:?}"),
            Err(_) => failed_once = true,
        }
        assert!(Instant::now() < deadline, "the helper never came back");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(failed_once || host.pid(PLUGIN_ID) != Some(pid));
}

/// Words from a fixed vocabulary, so the text is less repetitive than a loop.
fn words(seed: u64, n: usize) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = String::new();
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let w = x % 6000;
        out.push_str(&format!(
            "w{:x}{} ",
            w,
            ["ar", "on", "ity", "ing"][(w % 4) as usize]
        ));
    }
    out
}

/// Run once by hand: `cargo test -p lookout-office-text --test host index_size -- --ignored --nocapture`
#[test]
#[ignore = "measurement"]
fn index_size_growth_for_20_documents() {
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().join("docs");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("base.txt"), "baseline").unwrap();
    let app = start(dir.path(), &root, true);
    app.index_and_wait();
    let data = dir.path().join("data");
    let before = dir_size(&data);
    let (mut disk, mut text) = (0u64, 0usize);
    for i in 0..20u64 {
        let body = words(i + 1, 3500);
        text += body.len();
        let paras: Vec<&str> = body.as_str().split_inclusive(' ').collect();
        let chunks: Vec<String> = paras.chunks(50).map(|c| c.concat()).collect();
        let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
        let (name, bytes) = match i % 20 {
            0..=7 => (format!("d{i}.docx"), docx(&refs)),
            8..=13 => (format!("d{i}.xlsx"), xlsx(&refs)),
            _ => (format!("d{i}.pptx"), pptx(&refs)),
        };
        disk += bytes.len() as u64;
        fs::write(root.join(name), bytes).unwrap();
    }
    app.index_and_wait();
    let after = dir_size(&data);
    println!(
        "MEASURE docs=20 on_disk={disk} extracted_text={text} index_before={before} index_after={after} growth={}",
        after.saturating_sub(before)
    );
    assert!(after > before);
}

/// Run once by hand: `cargo test -p lookout-office-text --release --test host pdf_measure -- --ignored --nocapture`
#[test]
#[ignore = "measurement"]
fn pdf_measure() {
    use lookout_office_text::extract::{extract_office, Limits};
    // Extraction time for one generated 50-page text PDF.
    let pages: Vec<String> = (0..50)
        .map(|p| {
            words(p + 1, 400)
                .split_inclusive(' ')
                .collect::<Vec<_>>()
                .chunks(10)
                .map(|c| c.concat())
                .collect::<Vec<_>>()
                .join(
                    "
",
                )
        })
        .collect();
    let refs: Vec<&str> = pages.iter().map(String::as_str).collect();
    let bytes = pdf(&refs);
    let mut ms = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let text = extract_office(&bytes, "pdf", &Limits::default()).unwrap();
        ms.push((t.elapsed().as_secs_f64() * 1000.0, text.len()));
    }
    println!(
        "MEASURE pdf_pages=50 file_bytes={} text_bytes={} runs_ms={:?} per_page_ms_median={:.2}",
        bytes.len(),
        ms[0].1,
        ms.iter().map(|m| m.0.round()).collect::<Vec<_>>(),
        {
            let mut v: Vec<f64> = ms.iter().map(|m| m.0).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            v[2] / 50.0
        }
    );
    // Index growth for 20 generated PDFs of 10 pages each.
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().join("docs");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("base.txt"), "baseline").unwrap();
    let app = start(dir.path(), &root, true);
    app.index_and_wait();
    let data = dir.path().join("data");
    let before = dir_size(&data);
    let (mut disk, mut text) = (0u64, 0usize);
    for i in 0..20u64 {
        let pages: Vec<String> = (0..10)
            .map(|p| {
                words(i * 100 + p + 1, 350)
                    .split_inclusive(' ')
                    .collect::<Vec<_>>()
                    .chunks(10)
                    .map(|c| c.concat())
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    )
            })
            .collect();
        text += pages.iter().map(String::len).sum::<usize>();
        let refs: Vec<&str> = pages.iter().map(String::as_str).collect();
        let bytes = pdf(&refs);
        disk += bytes.len() as u64;
        fs::write(root.join(format!("p{i}.pdf")), bytes).unwrap();
    }
    app.index_and_wait();
    let after = dir_size(&data);
    println!(
        "MEASURE pdfs=20 on_disk={disk} extracted_text={text} index_before={before} index_after={after} growth={}",
        after.saturating_sub(before)
    );
    assert!(after > before);
}
