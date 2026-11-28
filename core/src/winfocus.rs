//! What is in front: used so the bar does not hide while a file dialog or
//! another window of this program has the focus, and so the shortcut can stay
//! quiet over a fullscreen game or presentation.

/// True if the foreground window belongs to this process.
#[cfg(windows)]
pub fn foreground_is_ours() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: both calls only read window state; `pid` outlives the call.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == std::process::id()
    }
}

#[cfg(not(windows))]
pub fn foreground_is_ours() -> bool {
    false
}

/// Whether Windows reports a fullscreen game, a fullscreen app or a
/// presentation in front. False if it will not say.
#[cfg(windows)]
pub fn fullscreen_in_front() -> bool {
    use windows_sys::Win32::UI::Shell::SHQueryUserNotificationState;
    let mut state = 0;
    // SAFETY: `state` is written by the call.
    let result = unsafe { SHQueryUserNotificationState(&mut state) };
    result == 0 && is_fullscreen_state(state)
}

#[cfg(not(windows))]
pub fn fullscreen_in_front() -> bool {
    false
}

/// `QUNS_BUSY` (a fullscreen app, or presentation settings), a fullscreen
/// Direct3D program, or presentation mode. Quiet hours, a locked screen and
/// ordinary use do not count.
#[cfg(windows)]
fn is_fullscreen_state(state: i32) -> bool {
    use windows_sys::Win32::UI::Shell::{
        QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    [
        QUNS_BUSY,
        QUNS_RUNNING_D3D_FULL_SCREEN,
        QUNS_PRESENTATION_MODE,
    ]
    .contains(&state)
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn only_fullscreen_and_presentation_states_count() {
        // The values of QUERY_USER_NOTIFICATION_STATE, 1 to 7.
        let quiet: Vec<i32> = (1..=7).filter(|&s| super::is_fullscreen_state(s)).collect();
        assert_eq!(quiet, [2, 3, 4]);
    }
}
