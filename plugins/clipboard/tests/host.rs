//! The clipboard plugin as a separate process behind the real host and gate.
//! History files live in a temp folder; the system clipboard is not touched
//! (the watcher is switched off), except by the one `#[ignore]` test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lookout_clipboard::store::{Limits, Store};
use lookout_core::actions::open::NoopLauncher;
use lookout_core::actions::{GateConfig, Risk};
use lookout_core::crypto::KeyStore;
use lookout_core::engine::{Sink, Update};
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::plugins::HostConfig;
use lookout_core::settings::Settings;
use lookout_core::storage::Storage;

const ID: &str = "com.lookout.clipboard";
const SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

fn setup(dir: &Path) -> HostConfig {
    let plugin = dir.join("plugins").join("clipboard");
    fs::create_dir_all(&plugin).unwrap();
    let exe = plugin.join("lookout-clipboard.exe");
    let built = env!("CARGO_BIN_EXE_lookout-clipboard");
    if fs::hard_link(built, &exe).is_err() {
        fs::copy(built, &exe).unwrap();
    }
    // The manifest shipped in the repository, unchanged.
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json"),
        plugin.join("plugin.json"),
    )
    .unwrap();
    let keyfile = dir.join("lookout.key");
    let master = KeyStore::new(keyfile).load_or_create().unwrap();
    let key = lookout_core::crypto::derive_plugin_key(&master, ID);
    // Seed the history the way the watcher would have.
    let data = dir.join("data");
    let mut store = Store::open(&data.join("plugins").join(ID).join("clips.bin"), key);
    let limits = Limits {
        max_clips: 100,
        retention_secs: i64::MAX / 4,
    };
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    store.add("meeting notes for tuesday", now - 90, limits);
    store.add(&format!("deploy key {SECRET}"), now - 30, limits);
    store.save().unwrap();
    std::env::set_var("LOOKOUT_CLIPBOARD_NO_WATCH", "1");
    let mut cfg = HostConfig::new(vec![dir.join("plugins")], data);
    cfg.master_key = Some(master);
    cfg
}

struct App {
    lookout: Lookout,
    updates: Arc<Mutex<Vec<Update>>>,
    next: u64,
}

fn app(cfg: HostConfig) -> App {
    let updates: Arc<Mutex<Vec<Update>>> = Arc::default();
    let u = updates.clone();
    let sink: Sink = Arc::new(move |update| u.lock().unwrap().push(update));
    let lookout = Lookout::start(
        StartOptions {
            settings: Settings {
                index_contents: false,
                ocr: false,
                first_run_done: true,
                ..Settings::default()
            },
            roots_override: vec![],
            synthetic_files: None,
            synthetic_uses_folders: false,
            start_menu_dirs: vec![],
            store_apps: false,
            windows: None,
            storage: Storage::in_memory().unwrap(),
            idle: Arc::new(lookout_core::content::idle::AlwaysIdle),
            ocr: Arc::new(lookout_core::content::ocr::NoOcr),
            launcher: Arc::new(NoopLauncher),
            system: None,
            gate: GateConfig {
                min_dwell: Duration::from_millis(60),
                ..GateConfig::default()
            },
            plugins: Some(cfg),
        },
        sink,
        PauseState::new(),
    )
    .unwrap();
    App {
        lookout,
        updates,
        next: 0,
    }
}

impl App {
    fn ask(&mut self, text: &str) -> Update {
        self.next += 1;
        let generation = self.next;
        assert!(self.lookout.query(generation, text));
        let end = Instant::now() + Duration::from_secs(15);
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
            assert!(Instant::now() < end, "query never completed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn history_is_off_until_the_user_accepts_the_permissions_then_it_is_searchable_and_masked() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(setup(dir.path()));
    let host = app.lookout.plugins().unwrap().clone();

    let info = host.list();
    assert_eq!(info.len(), 1);
    assert!(!info[0].enabled && !info[0].approved);
    assert!(info[0].permissions.iter().any(|p| p.contains("clipboard")));
    assert!(info[0]
        .permissions
        .contains(&"Runs in the background".to_owned()));
    assert!(info[0]
        .permissions
        .iter()
        .any(|p| p.contains("Secret detection is best effort")));
    // Not approved: nothing starts, nothing is shown.
    assert!(app
        .ask("clip")
        .items
        .iter()
        .all(|i| !i.source.starts_with("plugin:")));
    assert!(!host.is_running(ID));

    host.set_enabled(ID, true, true).unwrap();
    let r = app.ask("clip");
    let titles: Vec<_> = r
        .items
        .iter()
        .filter(|i| i.source.starts_with("plugin:"))
        .map(|i| i.title.clone())
        .collect();
    assert_eq!(titles.len(), 2, "{titles:?}");
    assert!(titles.iter().all(|t| !t.contains(SECRET)));
    assert!(titles.iter().any(|t| t.contains("[masked]")));
    // A background plugin keeps running while it is on.
    assert!(host.is_running(ID));

    let found = app.ask("clip tuesday");
    assert_eq!(
        found
            .items
            .iter()
            .filter(|i| i.source.starts_with("plugin:"))
            .count(),
        1
    );
    // The secret is not in the plugin's file on disk either.
    let bytes = fs::read(dir.path().join("data/plugins").join(ID).join("clips.bin")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(SECRET));
}

#[test]
fn clearing_the_history_goes_through_the_gate_and_deletes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(setup(dir.path()));
    app.lookout
        .plugins()
        .unwrap()
        .set_enabled(ID, true, true)
        .unwrap();
    let file = dir.path().join("data/plugins").join(ID).join("clips.bin");
    assert!(file.exists());

    let r = app.ask("clip clear");
    let item = r
        .items
        .iter()
        .find(|i| i.source.starts_with("plugin:"))
        .unwrap();
    let action = item.metadata["default_action"].as_str().unwrap().to_owned();
    let prepared = app.lookout.prepare_action(&action, &item.id).unwrap();
    assert_eq!(prepared.risk, Risk::Confirm);
    assert!(prepared
        .preview
        .warnings
        .iter()
        .any(|w| w.contains("cannot be undone")));
    assert!(!prepared.undoable);
    assert!(app.lookout.run_action(&prepared.ticket).is_err());
    assert!(file.exists(), "deleted without confirmation");

    app.lookout.present_action(&prepared.ticket).unwrap();
    std::thread::sleep(Duration::from_millis(120));
    app.lookout.confirm_action(&prepared.ticket).unwrap();
    assert!(!file.exists());
    assert!(app
        .ask("clip")
        .items
        .iter()
        .all(|i| !i.source.starts_with("plugin:")));
}

/// Every text action through the real plugin process and the real gate, on the
/// real system clipboard (so it is ignored by default and restores the text
/// that was there): `cargo test -p lookout-clipboard --test host -- --ignored`.
/// Both ignored tests overwrite the one system clipboard, so they take turns.
#[cfg(windows)]
static CLIPBOARD_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Writes text, retrying while another program has the clipboard open.
#[cfg(windows)]
fn write_settled(cb: &lookout_clipboard::win::WindowsClipboard, text: &str) {
    use lookout_clipboard::monitor::ClipboardIo;
    for _ in 0..150 {
        if cb.write_text(text).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    cb.write_text(text).unwrap();
}

/// Other programs (clipboard history, sync) open the clipboard right after every
/// change, so a read or write made at that instant can fail or see nothing for a
/// moment. Tests wait briefly for the expected value instead of reading once.
#[cfg(windows)]
fn settled(
    cb: &lookout_clipboard::win::WindowsClipboard,
    want: &lookout_clipboard::monitor::Read,
) -> lookout_clipboard::monitor::Read {
    use lookout_clipboard::monitor::ClipboardIo;
    let mut got = cb.read();
    for _ in 0..150 {
        if got == *want {
            return got;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        got = cb.read();
    }
    got
}

#[cfg(windows)]
#[test]
#[ignore = "overwrites the system clipboard (restores text afterwards)"]
fn text_actions_over_json_rpc_with_real_clipboard_and_all_input_sizes() {
    use lookout_clipboard::monitor::{ClipboardIo, Read};
    use lookout_clipboard::text_actions::{apply, ACTIONS};
    use lookout_clipboard::win::WindowsClipboard;

    let _turn = CLIPBOARD_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cb = WindowsClipboard::new();
    let original = cb.read();
    let inputs: Vec<(&str, String)> = vec![
        (
            "short",
            "  Hello   World https://a.com/?id=1&utm_source=x  ".to_owned(),
        ),
        ("empty", String::new()),
        ("1 MB", "lorem ipsum dolor\n".repeat(1_048_576 / 18 + 1)),
        (
            "unicode+emoji",
            "Ünïcödé ß 日本語 😀👍🏽 e\u{301} ﬁ 👨‍👩‍👧".to_owned(),
        ),
        ("json", r#"{"a":[1,2],"b":"x"}"#.to_owned()),
    ];

    let dir = tempfile::tempdir().unwrap();
    let mut app = app(setup(dir.path()));
    app.lookout
        .plugins()
        .unwrap()
        .set_enabled(ID, true, true)
        .unwrap();

    for (id, label, words) in ACTIONS {
        let keyword = words.split_whitespace().next().unwrap().to_owned();
        for (name, input) in &inputs {
            write_settled(&cb, input);
            let before = settled(&cb, &Read::Text(input.clone()));
            let r = app.ask(&format!("clip {keyword}"));
            let item = r
                .items
                .iter()
                .find(|i| i.source.starts_with("plugin:") && i.id.ends_with(&format!(":{id}")))
                .unwrap_or_else(|| panic!("{id} not offered for '{keyword}'"));
            let action = item.metadata["default_action"].as_str().unwrap().to_owned();
            let prepared = app.lookout.prepare_action(&action, &item.id).unwrap();
            assert_eq!(prepared.risk, Risk::Safe, "{id}");
            assert!(!prepared.undoable, "{id}");
            // Preparing and previewing never touch the clipboard.
            assert_eq!(
                settled(&cb, &before),
                before,
                "{id}/{name}: written before running"
            );

            let outcome = app.lookout.run_action(&prepared.ticket);
            let expected = match &before {
                Read::Text(t) => apply(id, t),
                _ => Err("no text".to_owned()),
            };
            match (outcome, expected) {
                (Ok(done), Ok(want)) => {
                    assert!(
                        done.message.contains(label),
                        "{id}/{name}: {}",
                        done.message
                    );
                    assert_eq!(
                        settled(&cb, &Read::Text(want.clone())),
                        Read::Text(want),
                        "{id}/{name}: wrong result"
                    );
                    eprintln!("ok    {id:<14} {name}");
                }
                (Err(e), Err(_)) => {
                    assert_eq!(
                        settled(&cb, &before),
                        before,
                        "{id}/{name}: changed on error"
                    );
                    eprintln!("error {id:<14} {name}: {e} (clipboard unchanged)");
                }
                (o, e) => panic!(
                    "{id}/{name}: plugin gave {:?} but expected ok={}",
                    o.as_ref()
                        .map(|d| d.message.clone())
                        .map_err(ToString::to_string),
                    e.is_ok()
                ),
            }
        }
    }
    // The plugin process survived all of it.
    assert!(app.lookout.plugins().unwrap().is_running(ID));
    if let Read::Text(t) = original {
        cb.write_text(&t).unwrap();
    }
}

#[cfg(windows)]
mod real_clipboard {
    use super::settled;
    use std::ptr::null_mut;

    use lookout_clipboard::monitor::{ClipboardIo, Read};
    use lookout_clipboard::win::WindowsClipboard;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    fn put(bytes: &[u8]) -> *mut core::ffi::c_void {
        // SAFETY: allocates a movable block and fills it while locked.
        unsafe {
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
            let p = GlobalLock(h).cast::<u8>();
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            GlobalUnlock(h);
            h
        }
    }

    /// Replaces the clipboard with text plus the given extra formats.
    fn set(text: &str, extra: &[(&str, Vec<u8>)]) {
        let wide: Vec<u8> = text
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: plain clipboard calls; the clipboard owns the blocks after SetClipboardData.
        unsafe {
            let mut opened = false;
            for _ in 0..200 {
                if OpenClipboard(null_mut()) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(opened, "the clipboard stayed locked by another program");
            EmptyClipboard();
            SetClipboardData(13, put(&wide));
            for (name, data) in extra {
                let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                SetClipboardData(RegisterClipboardFormatW(w.as_ptr()), put(data));
            }
            CloseClipboard();
        }
    }

    /// Overwrites the real clipboard, so it is not part of the normal run:
    /// `cargo test -p lookout-clipboard -- --ignored`.
    #[test]
    #[ignore = "overwrites the system clipboard"]
    fn formats_that_ask_not_to_be_recorded_are_honoured() {
        let _turn = super::CLIPBOARD_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cb = WindowsClipboard::new();
        let want_text = Read::Text("hello clip".into());
        let is_text = |r: Read| r == want_text;

        set("hello clip", &[]);
        assert!(is_text(settled(&cb, &want_text)));
        set(
            "hello clip",
            &[("ExcludeClipboardContentFromMonitorProcessing", vec![1])],
        );
        assert_eq!(settled(&cb, &Read::Excluded), Read::Excluded);
        set("hello clip", &[("Clipboard Viewer Ignore", vec![1])]);
        assert_eq!(settled(&cb, &Read::Excluded), Read::Excluded);
        set(
            "hello clip",
            &[("CanIncludeInClipboardHistory", 0u32.to_le_bytes().to_vec())],
        );
        assert_eq!(settled(&cb, &Read::Excluded), Read::Excluded);
        set(
            "hello clip",
            &[("CanIncludeInClipboardHistory", 1u32.to_le_bytes().to_vec())],
        );
        assert!(is_text(settled(&cb, &want_text)));
        set(
            "hello clip",
            &[("CanUploadToCloudClipboard", 0u32.to_le_bytes().to_vec())],
        );
        assert_eq!(settled(&cb, &Read::Excluded), Read::Excluded);

        // Writing and reading back through the same interface.
        cb.write_text("round trip").unwrap();
        assert_eq!(
            settled(&cb, &Read::Text("round trip".into())),
            Read::Text("round trip".into())
        );
        let seq = cb.sequence();
        cb.write_text("again").unwrap();
        assert_ne!(cb.sequence(), seq);
    }
}
