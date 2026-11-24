//! Crash and failure cases that need a real process or a real full disk.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lookout_core::content::idle::AlwaysIdle;
use lookout_core::content::service::{ContentConfig, ContentService};
use lookout_core::content::store::ContentStore;
use lookout_core::crypto::Key;
use lookout_core::exclude::Excludes;
use lookout_core::pause::PauseState;

fn example_exe(name: &str) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["build", "-p", "lookout-core", "--example", name])
        .arg("--message-format=json")
        .stderr(Stdio::inherit())
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == name)
        .find_map(|v| v["executable"].as_str().map(PathBuf::from))
        .expect("example not built")
}

#[test]
fn killing_the_process_while_indexing_leaves_a_working_index_that_resumes() {
    const FILES: usize = 600;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    fs::create_dir_all(&root).unwrap();
    for i in 0..FILES {
        fs::write(
            root.join(format!("f{i}.txt")),
            format!("crashword{i}z some text"),
        )
        .unwrap();
    }
    let index = dir.path().join("index");
    let mut child = Command::new(example_exe("crash_index"))
        .arg(&root)
        .arg(&index)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let started = Instant::now();
    loop {
        let l = lines.next().expect("child ended early").unwrap();
        let n: usize = l.trim_start_matches("docs=").parse().unwrap();
        if n >= 200 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "too slow");
    }
    child.kill().unwrap(); // no chance to save anything
    child.wait().unwrap();

    let key = Key::from_bytes(&[7u8; 32]).unwrap();
    let store = Arc::new(ContentStore::open(&index, &key).unwrap());
    let kept = usize::try_from(store.doc_count()).unwrap();
    assert!(
        kept > 0,
        "the index was thrown away: nothing survived the kill"
    );
    assert!(
        kept < FILES,
        "the kill came too late to test anything: {kept}"
    );
    assert_eq!(store.all_paths().len(), kept, "counts disagree");

    let svc = ContentService::start(ContentConfig {
        roots: vec![root],
        excludes: Excludes::default(),
        store: store.clone(),
        ocr: None,
        office: None,
        idle: Arc::new(AlwaysIdle),
        idle_secs: 0,
        duty: 1.0,
        commit_every: 20,
        pause: PauseState::new(),
        max_bytes: 512 * 1024 * 1024,
        fast: false,
    })
    .unwrap();
    let stats = svc.stats();
    let t = Instant::now();
    while !(stats.scans_done.load(Ordering::Acquire) >= 1 && !stats.busy.load(Ordering::Acquire)) {
        assert!(t.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(store.doc_count(), FILES as u64);
    let again = usize::try_from(stats.extractions.load(Ordering::Relaxed)).unwrap();
    assert!(
        again <= FILES - kept + 20,
        "re-read {again} files although {kept} were already indexed"
    );
}

fn many_folders(root: &std::path::Path, folders: usize, files: usize) {
    for d in 0..folders {
        let dir = root.join(format!("d{d}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..files {
            fs::write(dir.join(format!("f{f}.txt")), format!("movingword{d}x{f}")).unwrap();
        }
    }
}

#[test]
fn folders_deleted_or_renamed_during_the_crawls_do_not_break_them() {
    use lookout_core::index_service::{new_shared_index, read, IndexConfig, IndexService};
    const FOLDERS: usize = 200;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    many_folders(&root, FOLDERS, 5);

    let names = new_shared_index();
    let mut cfg = IndexConfig::new(vec![root.clone()], Excludes::default());
    cfg.duty = 0.15; // slow enough that the crawl is still going
    cfg.watch = false;
    let svc = IndexService::start(names.clone(), cfg, PauseState::new()).unwrap();

    let store = Arc::new(ContentStore::open_in_ram().unwrap());
    let content = ContentService::start(ContentConfig {
        roots: vec![root.clone()],
        excludes: Excludes::default(),
        store: store.clone(),
        ocr: None,
        office: None,
        idle: Arc::new(AlwaysIdle),
        idle_secs: 0,
        duty: 0.3,
        commit_every: 10,
        pause: PauseState::new(),
        max_bytes: 512 * 1024 * 1024,
        fast: false,
    })
    .unwrap();

    // While both are working: delete some folders, rename others, delete the
    // folder the crawl may be inside.
    for d in 0..40 {
        fs::remove_dir_all(root.join(format!("d{d}"))).unwrap();
    }
    for d in 40..80 {
        fs::rename(root.join(format!("d{d}")), root.join(format!("renamed{d}"))).unwrap();
    }
    assert!(
        svc.wait_for_crawl(Duration::from_secs(120)),
        "name crawl never ended"
    );

    // After a rescan both indexes agree with the disk.
    let before = svc.rescan_count();
    svc.request_rescan();
    let t = Instant::now();
    while svc.rescan_count() == before || svc.status().crawling {
        assert!(t.elapsed() < Duration::from_secs(120), "rescan never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    let want_files = (FOLDERS - 40) * 5;
    assert_eq!(read(&names).counts().files, want_files);

    let stats = content.stats();
    let mut tries = 0;
    loop {
        let before = stats.scans_done.load(Ordering::Acquire);
        content.request_rescan();
        while stats.scans_done.load(Ordering::Acquire) <= before
            || stats.busy.load(Ordering::Acquire)
        {
            assert!(
                t.elapsed() < Duration::from_secs(240),
                "content scan never ended"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        if store.all_paths().len() == want_files {
            break;
        }
        tries += 1;
        assert!(
            tries < 3,
            "content index holds {} files, disk has {want_files}",
            store.all_paths().len()
        );
    }
}

#[test]
fn a_second_process_cannot_open_an_index_that_a_running_one_holds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    many_folders(&root, 20, 20);
    let index = dir.path().join("index");
    let mut child = Command::new(example_exe("crash_index"))
        .arg(&root)
        .arg(&index)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    lines.next().expect("child ended early").unwrap();

    let key = Key::from_bytes(&[7u8; 32]).unwrap();
    let second = ContentStore::open(&index, &key);
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(still_running, "first process died");
    assert!(
        second.is_err(),
        "a second process opened the index while the first was running"
    );
    // After the first is gone the index opens again.
    assert!(ContentStore::open(&index, &key).is_ok());
}
