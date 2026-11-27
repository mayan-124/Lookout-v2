//! The thumbnail helper as a real process (`examples/thumb_helper`): whatever
//! it does, the caller gets the icon or nothing and carries on.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use lookout_core::share::Rules;
use lookout_core::thumbnail::{AppIcons, Shot, Thumbnails, APP_ICON_ITEMS, MAX_CRASHES, TIMEOUT};

/// Long enough for a loaded machine to start a process.
const PATIENT: Duration = Duration::from_secs(30);

fn helper_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args(["build", "-p", "lookout-core", "--example", "thumb_helper"])
            .arg("--message-format=json")
            .stderr(Stdio::inherit())
            .output()
            .unwrap();
        assert!(out.status.success(), "building thumb_helper failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "thumb_helper")
            .find_map(|v| v["executable"].as_str().map(PathBuf::from))
            .expect("cargo did not report the thumb_helper executable")
    })
}

struct Fixture {
    dir: tempfile::TempDir,
    thumbnails: Thumbnails,
    started: Arc<AtomicUsize>,
    next: AtomicUsize,
}

impl Fixture {
    fn new(timeout: Duration) -> Self {
        let exe = helper_exe();
        let started = Arc::new(AtomicUsize::new(0));
        let count = started.clone();
        let launch = move || {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Command::new(exe))
        };
        Self {
            dir: tempfile::tempdir().unwrap(),
            thumbnails: Thumbnails::new(Arc::new(launch), timeout),
            started,
            next: AtomicUsize::new(1),
        }
    }

    /// The temp folder lies inside AppData, so it counts as an added folder.
    fn rules(&self) -> Rules {
        Rules {
            roots: vec![self.dir.path().to_path_buf()],
            ..Rules::default()
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        // Written once: a newer time would be a different picture.
        if !path.exists() {
            std::fs::write(&path, "some text").unwrap();
        }
        path
    }

    fn get(&self, name: &str) -> Option<Shot> {
        let request = self.next.fetch_add(1, Ordering::SeqCst) as u64;
        self.thumbnails
            .get(&self.file(name), 256, request, &self.rules())
    }

    fn starts(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }
}

/// What the picture says, for the made-up helper.
fn shows(shot: Option<Shot>) -> &'static str {
    match shot {
        Some(s) if s.icon && s.image.ends_with("aWNvbg==") => "icon",
        Some(s) if !s.icon && s.image.ends_with("dGh1bWI=") => "thumbnail",
        None => "nothing",
        Some(other) => panic!("{other:?}"),
    }
}

#[test]
fn a_thumbnail_arrives_and_is_kept_until_cleared() {
    let f = Fixture::new(PATIENT);
    assert_eq!(f.starts(), 0, "no helper before one is needed");
    assert_eq!(shows(f.get("a.docx")), "thumbnail");
    assert_eq!(shows(f.get("a.docx")), "thumbnail");
    assert_eq!(f.starts(), 1, "the second came from memory");
    f.thumbnails.clear();
    f.get("a.docx");
    assert_eq!(f.starts(), 2);
}

#[test]
fn a_helper_that_hangs_is_killed_and_the_icon_remains() {
    let f = Fixture::new(TIMEOUT);
    for _ in 0..=MAX_CRASHES {
        let started = Instant::now();
        assert_eq!(shows(f.get("hang.docx")), "icon");
        // Far less than the minute the helper sleeps: it was killed.
        assert!(started.elapsed() < Duration::from_secs(8));
    }
    assert!(!f.thumbnails.icons_only(), "a hang is not a crash");
    assert_eq!(shows(Fixture::new(PATIENT).get("b.docx")), "thumbnail");
}

#[test]
fn three_crashes_in_a_row_leave_icons_only() {
    let f = Fixture::new(PATIENT);
    for _ in 1..MAX_CRASHES {
        assert_eq!(shows(f.get("crash.docx")), "icon");
    }
    // A good one in between starts the count again.
    assert_eq!(shows(f.get("a.docx")), "thumbnail");
    for n in 1..=MAX_CRASHES {
        assert!(!f.thumbnails.icons_only(), "before crash {n}");
        assert_eq!(shows(f.get("crash.docx")), "icon");
    }
    assert!(f.thumbnails.icons_only());
    for name in ["b.docx", "crash.docx", "c.docx"] {
        assert_eq!(shows(f.get(name)), "icon", "{name}");
    }
    assert!(f.thumbnails.icons_only(), "for the rest of the session");
}

#[test]
fn a_silent_or_missing_helper_gives_nothing() {
    let f = Fixture::new(PATIENT);
    assert_eq!(shows(f.get("silent.docx")), "nothing");
    assert!(!f.thumbnails.icons_only());
    let missing = f.dir.path().join("no-such-helper.exe");
    let launch = move || Ok(Command::new(&missing));
    let t = Thumbnails::new(Arc::new(launch), PATIENT);
    assert_eq!(t.get(&f.file("a.docx"), 256, 1, &f.rules()), None);
}

#[test]
fn cancelling_kills_the_helper_at_once() {
    let f = Arc::new(Fixture::new(PATIENT));
    let waiting = {
        let f = f.clone();
        std::thread::spawn(move || f.get("hang.docx"))
    };
    while f.starts() == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    let started = Instant::now();
    f.thumbnails.cancel(1);
    assert_eq!(waiting.join().unwrap(), None);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        f.thumbnails.get(&f.file("a.docx"), 256, 1, &f.rules()),
        None
    );
    assert_eq!(f.starts(), 1, "an old request starts nothing");
}

#[test]
fn the_helper_checks_the_skip_rules_itself() {
    let f = Fixture::new(PATIENT);
    let get = |name: &str, rules: &Rules| f.thumbnails.get(&f.file(name), 256, 100, rules);
    // Without the temp folder as an added one, it lies in AppData.
    assert_eq!(get("a.docx", &Rules::default()), None);
    // A folder the user excluded.
    let mut excluded = f.rules();
    excluded.excludes = excluded.excludes.with_folders([f.dir.path().to_path_buf()]);
    assert_eq!(get("b.docx", &excluded), None);
    // A sensitive name.
    assert_eq!(get("server.pem", &f.rules()), None);
    // Too large.
    let big = f.dir.path().join("big.docx");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(lookout_core::preview::MAX_FILE + 1).unwrap();
    assert_eq!(f.thumbnails.get(&big, 256, 101, &f.rules()), None);
    assert_eq!(f.starts(), 4, "the helper was asked, and said no");
}

/// App icons from the made-up helper, counting how many were started.
fn app_icons(timeout: Duration) -> (AppIcons, Arc<AtomicUsize>) {
    let exe = helper_exe();
    let started = Arc::new(AtomicUsize::new(0));
    let count = started.clone();
    let launch = move || {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Command::new(exe))
    };
    (AppIcons::new(Arc::new(launch), timeout), started)
}

#[cfg(windows)]
#[test]
fn an_app_icon_that_fails_or_times_out_gives_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "x").unwrap();
        path
    };
    let (icons, started) = app_icons(PATIENT);
    // The temp folder lies in AppData, as the user's Start Menu does.
    let good = icons.get(&file("a.lnk")).expect("an icon");
    assert!(good.ends_with("aWNvbg=="));
    assert_eq!(icons.get(&file("a.lnk")), Some(good));
    assert_eq!(started.load(Ordering::SeqCst), 1, "kept in memory");
    assert_eq!(icons.get(&file("silent.exe")), None);
    assert_eq!(icons.get(&file("silent.exe")), None);
    assert_eq!(started.load(Ordering::SeqCst), 2, "a failure is kept too");
    assert_eq!(icons.get(&dir.path().join("gone.exe")), None);
    // Not apps: no helper at all.
    assert_eq!(icons.get(&file("a.docx")), None);
    assert_eq!(icons.get(Path::new(r"\\server\share\a.exe")), None);
    assert_eq!(started.load(Ordering::SeqCst), 3);

    // A helper that never answers is given up on and killed.
    let (icons, _) = app_icons(Duration::ZERO);
    assert_eq!(icons.get(&file("b.lnk")), None);
    let missing = dir.path().join("no-such-helper.exe");
    let icons = AppIcons::new(Arc::new(move || Ok(Command::new(&missing))), PATIENT);
    assert_eq!(icons.get(&file("c.lnk")), None);
}

#[test]
fn only_the_last_app_icons_are_kept() {
    let (icons, started) = app_icons(PATIENT);
    // Store ids need no file; the made-up helper answers any name.
    let app = |n: usize| PathBuf::from(format!(r"shell:AppsFolder\Vendor.App{n}_abc!App"));
    for n in 0..=APP_ICON_ITEMS {
        assert!(icons.get(&app(n)).is_some(), "{n}");
    }
    let all = APP_ICON_ITEMS + 1;
    icons.get(&app(APP_ICON_ITEMS));
    assert_eq!(started.load(Ordering::SeqCst), all, "the newest is kept");
    icons.get(&app(0));
    assert_eq!(
        started.load(Ordering::SeqCst),
        all + 1,
        "the oldest was dropped"
    );
    icons.clear();
    icons.get(&app(APP_ICON_ITEMS));
    assert_eq!(started.load(Ordering::SeqCst), all + 2);
}

#[cfg(windows)]
#[test]
fn windows_gives_a_png_of_a_real_file() {
    let f = Fixture::new(PATIENT);
    let shot = f.get("real.docx").expect("at least the icon");
    // "iVBORw0KGgo" is the PNG signature in base64.
    assert!(
        shot.image.starts_with("data:image/png;base64,iVBORw0KGgo"),
        "{}",
        &shot.image[..40.min(shot.image.len())]
    );
}
