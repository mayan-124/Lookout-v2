//! Test helper: indexes the files in a folder into an on-disk content index and
//! prints `docs=<n>` as it goes, so a test can kill it in the middle.
//! Usage: crash_index <root> <index dir>
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use lookout_core::content::idle::AlwaysIdle;
use lookout_core::content::service::{ContentConfig, ContentService};
use lookout_core::content::store::ContentStore;
use lookout_core::crypto::Key;
use lookout_core::exclude::Excludes;
use lookout_core::pause::PauseState;

fn main() {
    let mut a = std::env::args().skip(1);
    let root = PathBuf::from(a.next().unwrap());
    let dir = PathBuf::from(a.next().unwrap());
    let key = Key::from_bytes(&[7u8; 32]).unwrap();
    let store = Arc::new(ContentStore::open(&dir, &key).unwrap());
    let svc = ContentService::start(ContentConfig {
        roots: vec![root],
        excludes: Excludes::default(),
        store,
        ocr: None,
        office: None,
        idle: Arc::new(AlwaysIdle),
        idle_secs: 0,
        duty: 0.2,
        commit_every: 20,
        pause: PauseState::new(),
        max_bytes: 512 * 1024 * 1024,
        fast: false,
    })
    .unwrap();
    let stats = svc.stats();
    loop {
        println!("docs={}", stats.docs.load(Ordering::Relaxed));
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }
}
