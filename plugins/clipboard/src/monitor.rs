//! Watches the clipboard and records text clips (opt-in: this runs only while
//! the plugin is turned on). Nothing is read while Lookout is paused.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::store::{Added, Limits, Store};

/// What reading the clipboard gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Text(String),
    /// The program that set it asked not to be recorded (password managers).
    Excluded,
    /// Not text (an image, files) or empty.
    NoText,
    /// Locked by another program; try again.
    Busy,
}

pub trait ClipboardIo: Send + Sync {
    /// Changes whenever the clipboard content changes.
    fn sequence(&self) -> u32;
    fn read(&self) -> Read;
    fn write_text(&self, text: &str) -> Result<(), String>;
    /// Program that owns the clipboard now (e.g. `chrome`); `None` if it cannot
    /// be determined. Never guessed from the foreground window.
    fn source_app(&self) -> Option<String> {
        None
    }
    /// Blocks until the clipboard may have changed, or `timeout` passes. The real
    /// clipboard wakes on Windows' change notification; others just sleep.
    fn wait_change(&self, timeout: Duration) {
        std::thread::sleep(timeout);
    }
    /// Sends Ctrl+V to the program that was active before Lookout opened.
    fn paste(&self) -> Result<(), String> {
        Err("pasting is not supported here".to_owned())
    }
    /// Opens an http(s) link or mailto address in the default program.
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Err("opening links is not supported here".to_owned())
    }
}

/// Program names in the exclude list: `Chrome`, `chrome.exe` and `chrome` all match.
pub fn app_key(name: &str) -> String {
    let n = name.trim().to_lowercase();
    n.strip_suffix(".exe").unwrap_or(&n).to_owned()
}

pub fn is_excluded(list: &[String], app: &str) -> bool {
    let app = app_key(app);
    list.contains(&app)
}

pub struct Shared {
    pub store: Mutex<Store>,
    pub limits: Mutex<Limits>,
    pub paused: AtomicBool,
    pub stop: AtomicBool,
    /// Programs whose copies are never recorded (normalised with [`app_key`]).
    pub exclude: Mutex<Vec<String>>,
    /// Set by "Ignore next copy": the next text copied is not recorded.
    pub ignore_next: AtomicBool,
    /// Clipboard sequence number already looked at.
    pub last_seq: Mutex<u32>,
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// One look at the clipboard. Returns true if a new clip was stored.
pub fn tick(io: &dyn ClipboardIo, shared: &Shared, last_seq: &mut u32) -> bool {
    let seq = io.sequence();
    if seq == *last_seq {
        return false;
    }
    if shared.paused.load(Ordering::Acquire) {
        // Whatever is copied while paused is never recorded, not even after resuming.
        *last_seq = seq;
        return false;
    }
    match io.read() {
        Read::Busy => false,
        Read::Excluded | Read::NoText => {
            *last_seq = seq;
            false
        }
        Read::Text(text) => {
            *last_seq = seq;
            if shared.ignore_next.swap(false, Ordering::AcqRel) {
                return false;
            }
            let source = io.source_app();
            if source
                .as_deref()
                .is_some_and(|a| is_excluded(&lock(&shared.exclude), a))
            {
                return false;
            }
            let limits = *lock(&shared.limits);
            let mut store = lock(&shared.store);
            let added = store.add_from(&text, source.as_deref(), now_secs(), limits);
            if added == Added::Ignored {
                return false;
            }
            if let Err(e) = store.save() {
                eprintln!("clipboard: {e}");
            }
            added == Added::Stored
        }
    }
}

/// Looks at the clipboard now if it changed since the last look (shared by the
/// watcher and by searches, so a copy just made is always already recorded).
pub fn catch_up(io: &dyn ClipboardIo, shared: &Shared) -> bool {
    let mut seq = lock(&shared.last_seq);
    tick(io, shared, &mut seq)
}

/// Sleeps until the clipboard changes (the sequence number is the fallback) until
/// `shared.stop` is set. `on_new` is called after each stored clip.
pub fn run(io: Arc<dyn ClipboardIo>, shared: Arc<Shared>, on_new: impl Fn()) {
    let mut last_prune = now_secs();
    while !shared.stop.load(Ordering::Acquire) {
        if catch_up(io.as_ref(), &shared) {
            on_new();
        }
        // A change we could not read yet (clipboard locked) is retried soon.
        let pending = io.sequence() != *lock(&shared.last_seq);
        io.wait_change(Duration::from_millis(if pending { 150 } else { 1500 }));
        if now_secs() - last_prune >= 60 {
            last_prune = now_secs();
            let limits = *lock(&shared.limits);
            let mut store = lock(&shared.store);
            if store.prune(now_secs(), limits) {
                let _ = store.save();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    struct Fake {
        seq: AtomicU32,
        next: Mutex<Read>,
        app: Mutex<Option<String>>,
    }

    impl Fake {
        fn copy(&self, r: Read) {
            *lock(&self.next) = r;
            self.seq.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl ClipboardIo for Fake {
        fn sequence(&self) -> u32 {
            self.seq.load(Ordering::SeqCst)
        }
        fn read(&self) -> Read {
            lock(&self.next).clone()
        }
        fn write_text(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn source_app(&self) -> Option<String> {
            lock(&self.app).clone()
        }
    }

    fn setup() -> (Fake, Shared) {
        (
            Fake {
                seq: AtomicU32::new(1),
                next: Mutex::new(Read::NoText),
                app: Mutex::new(None),
            },
            Shared {
                store: Mutex::new(Store::in_memory()),
                limits: Mutex::new(Limits {
                    max_clips: 10,
                    retention_secs: 1000,
                }),
                paused: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                exclude: Mutex::new(Vec::new()),
                ignore_next: AtomicBool::new(false),
                last_seq: Mutex::new(1),
            },
        )
    }

    #[test]
    fn records_text_and_skips_excluded_and_non_text() {
        let (io, shared) = setup();
        let mut seq = io.sequence();
        io.copy(Read::Text("hello".into()));
        assert!(tick(&io, &shared, &mut seq));
        io.copy(Read::Excluded);
        assert!(!tick(&io, &shared, &mut seq));
        io.copy(Read::NoText);
        assert!(!tick(&io, &shared, &mut seq));
        assert_eq!(lock(&shared.store).len(), 1);
    }

    #[test]
    fn a_locked_clipboard_is_retried_not_skipped() {
        let (io, shared) = setup();
        let mut seq = io.sequence();
        io.copy(Read::Busy);
        assert!(!tick(&io, &shared, &mut seq));
        *lock(&io.next) = Read::Text("later".into());
        assert!(tick(&io, &shared, &mut seq));
    }

    #[test]
    fn nothing_is_recorded_while_paused_and_nothing_catches_up_after() {
        let (io, shared) = setup();
        let mut seq = io.sequence();
        shared.paused.store(true, Ordering::Release);
        io.copy(Read::Text("copied while paused".into()));
        assert!(!tick(&io, &shared, &mut seq));
        shared.paused.store(false, Ordering::Release);
        assert!(!tick(&io, &shared, &mut seq));
        assert!(lock(&shared.store).is_empty());
        io.copy(Read::Text("after".into()));
        assert!(tick(&io, &shared, &mut seq));
    }

    #[test]
    fn catch_up_records_a_copy_made_just_before() {
        let (io, shared) = setup();
        io.copy(Read::Text("copied a moment ago".into()));
        assert!(catch_up(&io, &shared));
        assert!(!catch_up(&io, &shared));
        assert_eq!(lock(&shared.store).len(), 1);
    }

    #[test]
    fn the_source_program_is_recorded() {
        let (io, shared) = setup();
        let mut seq = io.sequence();
        *lock(&io.app) = Some("chrome".into());
        io.copy(Read::Text("from a browser".into()));
        assert!(tick(&io, &shared, &mut seq));
        let store = lock(&shared.store);
        assert_eq!(
            store.recent().next().unwrap().source.as_deref(),
            Some("chrome")
        );
    }

    #[test]
    fn excluded_programs_are_never_recorded() {
        let (io, shared) = setup();
        let mut seq = io.sequence();
        *lock(&shared.exclude) = vec![app_key("KeePass.exe")];
        *lock(&io.app) = Some("keepass".into());
        io.copy(Read::Text("hunter two".into()));
        assert!(!tick(&io, &shared, &mut seq));
        assert!(lock(&shared.store).is_empty());
        *lock(&io.app) = Some("notepad".into());
        io.copy(Read::Text("fine".into()));
        assert!(tick(&io, &shared, &mut seq));
        assert!(is_excluded(&[app_key("Chrome.EXE")], "chrome.exe"));
    }

    #[test]
    fn ignore_next_skips_exactly_one_copy() {
        let (io, shared) = setup();
        let mut seq = io.sequence();
        shared.ignore_next.store(true, Ordering::Release);
        io.copy(Read::Text("secret-ish".into()));
        assert!(!tick(&io, &shared, &mut seq));
        assert!(lock(&shared.store).is_empty());
        io.copy(Read::Text("next one".into()));
        assert!(tick(&io, &shared, &mut seq));
        assert_eq!(lock(&shared.store).len(), 1);
    }
}
