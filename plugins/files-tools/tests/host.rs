//! The file tools as a separate process behind the real host and gate, on
//! generated files in temp folders. Also: with both bundled plugins turned off
//! the core (search, actions, undo) works and no plugin process exists.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lookout_core::actions::open::NoopLauncher;
use lookout_core::actions::system::{ProcessBackend, WindowsSystem};
use lookout_core::actions::{ActionError, GateConfig, Risk};
use lookout_core::engine::{Sink, Update};
use lookout_core::lookout::{Lookout, StartOptions};
use lookout_core::pause::PauseState;
use lookout_core::plugins::HostConfig;
use lookout_core::settings::Settings;
use lookout_core::storage::Storage;

const ID: &str = "com.lookout.files-tools";
const CLIP: &str = "com.lookout.clipboard";

fn built(package: &str, target: &str, example: bool) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["build", "-p", package, "--message-format=json"]);
    if example {
        cmd.args(["--example", target]);
    }
    let out = cmd.stderr(Stdio::inherit()).output().unwrap();
    assert!(out.status.success(), "building {package} failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == target)
        .find_map(|v| v["executable"].as_str().map(PathBuf::from))
        .unwrap_or_else(|| panic!("no executable for {target}"))
}

fn install(plugins: &Path, folder: &str, manifest_dir: &Path, exe: &Path, name: &str) {
    let dir = plugins.join(folder);
    fs::create_dir_all(&dir).unwrap();
    if fs::hard_link(exe, dir.join(name)).is_err() {
        fs::copy(exe, dir.join(name)).unwrap();
    }
    fs::copy(manifest_dir.join("plugin.json"), dir.join("plugin.json")).unwrap();
}

struct App {
    lookout: Lookout,
    updates: Arc<Mutex<Vec<Update>>>,
    next: u64,
    root: tempfile::TempDir,
}

impl App {
    fn files(&self) -> PathBuf {
        self.root.path().join("files")
    }
}

fn start(with_clipboard: bool) -> App {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("files")).unwrap();
    let plugins = root.path().join("plugins");
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    install(
        &plugins,
        "files-tools",
        here,
        Path::new(env!("CARGO_BIN_EXE_lookout-files-tools")),
        "lookout-files-tools.exe",
    );
    if with_clipboard {
        let exe = built("lookout-clipboard", "lookout-clipboard", false);
        install(
            &plugins,
            "clipboard",
            &here.join("../clipboard"),
            &exe,
            "lookout-clipboard.exe",
        );
    }
    let updates: Arc<Mutex<Vec<Update>>> = Arc::default();
    let u = updates.clone();
    let sink: Sink = Arc::new(move |update| u.lock().unwrap().push(update));
    let lookout = Lookout::start(
        StartOptions {
            settings: Settings {
                index_contents: false,
                ocr: false,
                first_run_done: true,
                crawl_duty: 1.0,
                ..Settings::default()
            },
            roots_override: vec![root.path().join("files")],
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
            plugins: Some(HostConfig::new(vec![plugins], root.path().join("data"))),
        },
        sink,
        PauseState::new(),
    )
    .unwrap();
    App {
        lookout,
        updates,
        next: 0,
        root,
    }
}

impl App {
    fn ask(&mut self, text: &str) -> Update {
        self.next += 1;
        let generation = self.next;
        assert!(self.lookout.query(generation, text));
        let end = Instant::now() + Duration::from_secs(20);
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

    /// Asks until a plugin result appears (the name index fills in the background).
    fn plugin_result(&mut self, text: &str) -> lookout_core::item::Item {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(i) = self
                .ask(text)
                .items
                .into_iter()
                .find(|i| i.source.starts_with("plugin:"))
            {
                return i;
            }
            assert!(Instant::now() < end, "no plugin result for {text:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn enable(&self) {
        self.lookout
            .plugins()
            .unwrap()
            .set_enabled(ID, true, true)
            .unwrap();
    }

    /// prepare, present, wait out the dwell, confirm.
    fn confirm(&self, item: &lookout_core::item::Item) -> lookout_core::actions::Done {
        let action = item.metadata["default_action"].as_str().unwrap();
        let prepared = self.lookout.prepare_action(action, &item.id).unwrap();
        assert_eq!(
            prepared.risk,
            Risk::Confirm,
            "file changes always need confirmation"
        );
        assert!(matches!(
            self.lookout.run_action(&prepared.ticket),
            Err(ActionError::ConfirmationRequired)
        ));
        self.lookout.present_action(&prepared.ticket).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        self.lookout.confirm_action(&prepared.ticket).unwrap()
    }

    fn undo(&self) -> lookout_core::actions::Done {
        let undo = self.lookout.prepare_undo().unwrap();
        self.lookout.present_action(&undo.ticket).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        self.lookout.confirm_action(&undo.ticket).unwrap()
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// A picture of random pixels: big as a PNG, and hard to compress.
fn noisy_png(path: &Path, w: u32, h: u32) {
    let mut seed = 0x1234_5678_u32;
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..w * h * 3 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        data.push((seed >> 24) as u8);
    }
    let mut enc = png::Encoder::new(fs::File::create(path).unwrap(), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&data).unwrap();
}

#[test]
fn compress_reaches_the_size_leaves_the_original_alone_and_can_be_undone() {
    let mut app = start(false);
    let png = app.files().join("holiday photo.png");
    noisy_png(&png, 640, 480);
    let original = fs::read(&png).unwrap();
    assert!(original.len() > 500_000);

    // Not usable until the user accepts the permissions.
    assert!(app
        .ask("compress holiday under 60 kb")
        .items
        .iter()
        .all(|i| !i.source.starts_with("plugin:")));
    assert!(!app.lookout.plugins().unwrap().is_running(ID));
    app.enable();

    let item = app.plugin_result("compress holiday under 60 kb");
    assert!(item.title.contains("60 KB"), "{}", item.title);
    let done = app.confirm(&item);
    assert!(done.undoable, "{}", done.message);

    let out = app.files().join("holiday photo (compressed).jpg");
    let bytes = fs::read(&out).unwrap();
    assert!(bytes.len() <= 60 * 1024, "{} bytes", bytes.len());
    assert!(
        bytes.len() > 4_000,
        "suspiciously small: {} bytes",
        bytes.len()
    );
    assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "not a JPEG");
    assert_eq!(fs::read(&png).unwrap(), original, "the original changed");

    app.undo();
    assert!(!out.exists());
    assert_eq!(fs::read(&png).unwrap(), original);
}

#[test]
fn a_picture_already_small_enough_is_refused_at_the_preview() {
    let mut app = start(false);
    app.enable();
    noisy_png(&app.files().join("tiny.png"), 8, 8);
    let item = app.plugin_result("compress tiny under 200 kb");
    let action = item.metadata["default_action"].as_str().unwrap();
    let err = app.lookout.prepare_action(action, &item.id).unwrap_err();
    assert!(err.to_string().contains("already"), "{err}");
}

#[test]
fn batch_rename_shows_a_live_preview_and_undo_restores_the_names() {
    let mut app = start(false);
    app.enable();
    let folder = app.files().join("trip pics");
    fs::create_dir_all(&folder).unwrap();
    for n in ["IMG_3.jpg", "IMG_1.jpg", "IMG_2.jpg"] {
        fs::write(folder.join(n), n).unwrap();
    }

    // The preview follows what is typed.
    let a = app.plugin_result("rename trip pics to Holiday {n}");
    assert!(
        a.subtitle
            .as_deref()
            .unwrap()
            .contains("IMG_1.jpg → Holiday 01.jpg"),
        "{:?}",
        a.subtitle
    );
    let b = app.plugin_result("rename trip pics to Beach {n}");
    assert!(
        b.subtitle.as_deref().unwrap().contains("Beach 01.jpg"),
        "{:?}",
        b.subtitle
    );
    let bad = app.plugin_result("rename trip pics to same");
    assert!(
        bad.subtitle.as_deref().unwrap().contains("{n}"),
        "{:?}",
        bad.subtitle
    );
    assert_eq!(
        names(&folder),
        ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"],
        "typing must not rename"
    );

    // The card lists every rename.
    // Only results of the latest query can be acted on.
    let a = app.plugin_result("rename trip pics to Holiday {n}");
    let action = a.metadata["default_action"].as_str().unwrap();
    let prepared = app.lookout.prepare_action(action, &a.id).unwrap();
    assert_eq!(prepared.preview.lines.len(), 3);
    assert!(
        prepared.preview.lines[2].contains("IMG_3.jpg")
            && prepared.preview.lines[2].contains("Holiday 03.jpg")
    );
    app.lookout.cancel_action(&prepared.ticket);

    app.confirm(&a);
    assert_eq!(
        names(&folder),
        ["Holiday 01.jpg", "Holiday 02.jpg", "Holiday 03.jpg"]
    );
    assert_eq!(
        fs::read_to_string(folder.join("Holiday 03.jpg")).unwrap(),
        "IMG_3.jpg"
    );
    app.undo();
    assert_eq!(names(&folder), ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"]);
    assert_eq!(
        fs::read_to_string(folder.join("IMG_3.jpg")).unwrap(),
        "IMG_3.jpg"
    );
}

#[test]
fn an_undo_that_would_overwrite_something_new_is_refused() {
    let mut app = start(false);
    app.enable();
    let folder = app.files().join("docs");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("a.txt"), "a").unwrap();
    fs::write(folder.join("b.txt"), "b").unwrap();
    let item = app.plugin_result("rename docs to New {n}");
    app.confirm(&item);
    // The user makes a new a.txt in the meantime.
    fs::write(folder.join("a.txt"), "new").unwrap();
    let err = app.lookout.prepare_undo().unwrap_err();
    assert!(err.to_string().contains("exists again"), "{err}");
    assert_eq!(fs::read_to_string(folder.join("a.txt")).unwrap(), "new");
}

#[test]
fn strip_metadata_writes_a_clean_copy_and_undo_deletes_only_that_copy() {
    let mut app = start(false);
    app.enable();
    let jpg = app.files().join("selfie.jpg");
    // SOI, JFIF, an EXIF segment with GPS text, a tiny scan, EOI.
    let seg = |m: u8, body: &[u8]| {
        let mut v = vec![0xFF, m];
        v.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    };
    let mut data = vec![0xFF, 0xD8];
    data.extend(seg(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
    data.extend(seg(0xE1, b"Exif\0\0GPS 51.5N 0.1W Canon"));
    data.extend(seg(0xDA, &[1, 1, 0, 0, 0x3F, 0]));
    data.extend_from_slice(&[0x12, 0x34, 0xFF, 0xD9]);
    fs::write(&jpg, &data).unwrap();

    let item = app.plugin_result("strip metadata selfie");
    app.confirm(&item);
    let clean = fs::read(app.files().join("selfie (no metadata).jpg")).unwrap();
    assert!(!String::from_utf8_lossy(&clean).contains("GPS"));
    assert!(clean.ends_with(&[0x12, 0x34, 0xFF, 0xD9]));
    assert_eq!(fs::read(&jpg).unwrap(), data, "the original changed");
    app.undo();
    assert_eq!(names(&app.files()), ["selfie.jpg"]);
}

// ---------------------------------------------- the core without the plugins

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn listening(port: u16) -> bool {
    !WindowsSystem.listeners(port).unwrap().is_empty()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn with_both_bundled_plugins_off_search_actions_and_undo_still_work() {
    static LISTENER: OnceLock<PathBuf> = OnceLock::new();
    let listener = LISTENER.get_or_init(|| built("lookout-core", "test_listener", true));

    let mut app = start(true);
    let host = app.lookout.plugins().unwrap().clone();
    assert_eq!(host.list().len(), 2);
    // Both plugins are off (never accepted).
    assert!(host.list().iter().all(|p| !p.enabled));
    fs::write(app.files().join("budget alpha.xlsx"), "x").unwrap();

    // Search works and shows nothing from plugins, even for their own trigger words.
    let end = Instant::now() + Duration::from_secs(20);
    while !app
        .ask("budget alpha")
        .items
        .iter()
        .any(|i| i.title.starts_with("budget alpha"))
    {
        assert!(Instant::now() < end, "file search found nothing");
        std::thread::sleep(Duration::from_millis(100));
    }
    for q in [
        "clip",
        "clip clear",
        "compress budget under 50 kb",
        "rename budget to x {n}",
        "strip budget",
    ] {
        let r = app.ask(q);
        assert!(
            r.items.iter().all(|i| !i.source.starts_with("plugin:")),
            "{q}"
        );
    }
    assert_eq!(app.ask("2+2").items[0].title, "4");

    // An ordinary action runs, and so do kill-port and its undo.
    let item = app
        .ask("budget alpha")
        .items
        .into_iter()
        .find(|i| i.title.starts_with("budget alpha"))
        .unwrap();
    let open = app.lookout.prepare_action("open", &item.id).unwrap();
    app.lookout.run_action(&open.ticket).unwrap();

    let port = free_port();
    let workdir = tempfile::tempdir().unwrap();
    let mut child = Command::new(listener)
        .arg(port.to_string())
        .arg("plugins off")
        .current_dir(workdir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the listener", || listening(port));
    let update = app.ask(&format!("kill port {port}"));
    let port_item = update
        .items
        .iter()
        .find(|i| i.id == format!("port:{port}"))
        .unwrap();
    let prepared = app
        .lookout
        .prepare_action("kill_port", &port_item.id)
        .unwrap();
    app.lookout.present_action(&prepared.ticket).unwrap();
    std::thread::sleep(Duration::from_millis(120));
    app.lookout.confirm_action(&prepared.ticket).unwrap();
    wait_until("the listener to stop", || !listening(port));
    let _ = child.wait();
    app.undo();
    wait_until("the restart", || listening(port));
    for pid in WindowsSystem.listeners(port).unwrap() {
        if let Some(info) = WindowsSystem.inspect(pid) {
            if info.name.eq_ignore_ascii_case("test_listener.exe") {
                let _ = WindowsSystem.terminate(&info);
            }
        }
    }

    // Through all of that no plugin process was ever started.
    assert!(!host.is_running(ID) && !host.is_running(CLIP));
    assert!(host.pid(ID).is_none() && host.pid(CLIP).is_none());
}
