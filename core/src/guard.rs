//! Keeping Lookout running: contained panics, the safe-mode start count and
//! the single running copy.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// How often a background worker that faulted is started over before it is left alone.
pub const RESTARTS: usize = 3;

/// Runs `f`; a panic ends `f` only. The panic hook has already logged where.
pub fn contain<T>(what: &'static str, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(_) => {
            crate::log::error(what);
            None
        }
    }
}

/// This many starts in a row that never got going lead to safe mode.
pub const CRASHES_FOR_SAFE_MODE: u32 = 3;

/// Counts starts that have not (yet) proved healthy, in `starts` in the data folder.
pub struct Starts {
    file: PathBuf,
    safe: bool,
}

impl Starts {
    /// Records one more start.
    pub fn begin(data: &Path) -> Self {
        let file = data.join("starts");
        let before: u32 = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let _ = std::fs::create_dir_all(data);
        if std::fs::write(&file, before.saturating_add(1).to_string()).is_err() {
            crate::log::warn("could not record the start");
        }
        Self {
            file,
            safe: before >= CRASHES_FOR_SAFE_MODE,
        }
    }

    /// Whether the starts before this one all ended early.
    pub fn safe_mode(&self) -> bool {
        self.safe
    }

    /// This start got going, or ended on purpose: the count starts over.
    pub fn healthy(&self) {
        if std::fs::write(&self.file, "0").is_err() {
            crate::log::warn("could not record a good start");
        }
    }
}

#[cfg(windows)]
pub mod single {
    //! One running copy per data folder: a named mutex marks it, a named event
    //! asks it to come forward.

    use std::path::Path;

    use sha2::{Digest, Sha256};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};

    /// Held by the first copy for as long as it runs.
    pub struct First {
        mutex: isize,
        event: isize,
    }

    impl Drop for First {
        fn drop(&mut self) {
            // SAFETY: both handles are owned by this value and closed once.
            unsafe {
                CloseHandle(self.event as HANDLE);
                CloseHandle(self.mutex as HANDLE);
            }
        }
    }

    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(Some(0)).collect()
    }

    /// Claims the data folder. `Ok(None)`: another copy has it and, when
    /// `show`, was asked to come forward, so this one should simply exit.
    /// `Err`: Windows refused the objects; the caller starts as usual.
    pub fn claim(data: &Path, show: bool) -> Result<Option<First>, std::io::Error> {
        let digest = Sha256::digest(data.to_string_lossy().to_lowercase().as_bytes());
        let id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        let mutex_name = wide(&format!("Local\\Lookout.single.{id}"));
        let event_name = wide(&format!("Local\\Lookout.show.{id}"));
        // SAFETY: NUL-terminated names that outlive the calls; default security.
        unsafe {
            // The event comes first, so a second copy can always signal it.
            let event = CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr());
            if event.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let mutex = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
            let existed = GetLastError() == ERROR_ALREADY_EXISTS;
            if mutex.is_null() {
                let e = std::io::Error::last_os_error();
                CloseHandle(event);
                return Err(e);
            }
            let first = First {
                mutex: mutex as isize,
                event: event as isize,
            };
            if !existed {
                return Ok(Some(first));
            }
            if show {
                AllowSetForegroundWindow(ASFW_ANY);
                SetEvent(event);
            }
            Ok(None)
        }
    }

    impl First {
        /// Calls `show` on a thread of its own each time another copy is started.
        pub fn listen(self, show: impl Fn() + Send + 'static) -> std::io::Result<()> {
            std::thread::Builder::new()
                .name("lookout-single".into())
                .spawn(move || {
                    let first = self;
                    // SAFETY: the handle lives as long as `first`.
                    while unsafe { WaitForSingleObject(first.event as HANDLE, INFINITE) }
                        == WAIT_OBJECT_0
                    {
                        super::contain("showing the bar failed", &show);
                    }
                })
                .map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_is_contained() {
        assert_eq!(contain("test fault", || 1), Some(1));
        assert_eq!(contain("test fault", || -> u8 { panic!("boom") }), None);
    }

    #[test]
    fn three_unfinished_starts_lead_to_safe_mode() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..CRASHES_FOR_SAFE_MODE {
            assert!(!Starts::begin(dir.path()).safe_mode());
        }
        let fourth = Starts::begin(dir.path());
        assert!(fourth.safe_mode());
        fourth.healthy();
        assert!(!Starts::begin(dir.path()).safe_mode());
    }

    #[cfg(windows)]
    #[test]
    fn a_second_copy_is_told_to_leave_and_the_first_is_asked_to_show() {
        let dir = tempfile::tempdir().unwrap();
        let first = single::claim(dir.path(), true).unwrap().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        first.listen(move || tx.send(()).unwrap()).unwrap();
        assert!(single::claim(dir.path(), true).unwrap().is_none());
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    }
}
