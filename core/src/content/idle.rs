//! "The user is idle": no keyboard or mouse input for a while. Content
//! extraction and OCR start only then (brief section 8).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub trait IdleSource: Send + Sync {
    /// How long since the user last touched the keyboard or mouse.
    fn idle_for(&self) -> Duration;
}

/// Windows' own input clock (`GetLastInputInfo`).
pub struct SystemIdle;

#[cfg(windows)]
impl IdleSource for SystemIdle {
    fn idle_for(&self) -> Duration {
        use windows_sys::Win32::System::SystemInformation::GetTickCount;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a valid, correctly sized LASTINPUTINFO for the call.
        let ok = unsafe { GetLastInputInfo(&mut info) };
        if ok == 0 {
            return Duration::ZERO; // cannot tell: assume busy
        }
        // SAFETY: no arguments, no preconditions.
        let now = unsafe { GetTickCount() };
        Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime)))
    }
}

#[cfg(not(windows))]
impl IdleSource for SystemIdle {
    fn idle_for(&self) -> Duration {
        Duration::ZERO
    }
}

/// Always idle (tests, benchmarks, `idle_secs = 0`).
pub struct AlwaysIdle;

impl IdleSource for AlwaysIdle {
    fn idle_for(&self) -> Duration {
        Duration::from_secs(u64::from(u32::MAX))
    }
}

/// A settable idle time, for tests.
#[derive(Default)]
pub struct ManualIdle(AtomicU64);

impl ManualIdle {
    pub fn set(&self, d: Duration) {
        self.0.store(d.as_millis() as u64, Ordering::Release);
    }
}

impl IdleSource for ManualIdle {
    fn idle_for(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_idle_reports_a_sane_value() {
        // Only checks that the Windows calls work; the value depends on the user.
        assert!(SystemIdle.idle_for() < Duration::from_secs(60 * 60 * 24 * 30));
    }
}
