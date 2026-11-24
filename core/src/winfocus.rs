//! Which window has the focus: used so the bar does not hide while a file
//! dialog or another window of this program has it.

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
