//! Typing Ctrl+V into the program that was in front when Lookout opened. The one
//! paste path, run by the app itself (Windows lets no background process take the
//! front): the bar is hidden, the window is brought back and watched until it
//! really is in front, held modifier keys are released, then the keys are sent. A
//! program running as administrator is left alone: Windows drops keys sent to it
//! without saying so.

use std::ptr::null_mut;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
    OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindow, IsWindowVisible, PeekMessageW,
    SetForegroundWindow, ShowWindowAsync, EVENT_SYSTEM_FOREGROUND, GUITHREADINFO, MSG, PM_NOREMOVE,
    SW_RESTORE, WINEVENT_OUTOFCONTEXT,
};

use crate::toast::PASTE_FAILED;

/// How long the window may take to come back to the front.
const RESTORE_LIMIT: Duration = Duration::from_millis(600);
const POLL: Duration = Duration::from_millis(10);
/// Windows is asked again this often while the window is not yet in front.
const ASK_AGAIN: Duration = Duration::from_millis(50);
/// Polls in a row that must find the window ready.
const STEADY: u32 = 3;
/// An unassigned key, tapped so that releasing Alt or Win opens no menu.
const MASK_KEY: u16 = 0xE8;

const MODIFIERS: [u16; 8] = [
    VK_LSHIFT,
    VK_RSHIFT,
    VK_LCONTROL,
    VK_RCONTROL,
    VK_LMENU,
    VK_RMENU,
    VK_LWIN,
    VK_RWIN,
];

/// Whether this finished action asks for a paste: a note, or a clip of the
/// built-in clipboard add-on. No other add-on can have keys typed for it.
pub fn is_paste(action: &str) -> bool {
    matches!(
        action,
        crate::notes::PASTE
            | "plugin/com.lookout.clipboard/paste_clip"
            | "plugin/com.lookout.clipboard/paste_plain"
    )
}

/// Where a paste goes: a window and, if known, the control that had the keyboard
/// (raw `HWND`s, 0 = unknown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    window: usize,
    control: usize,
}

static TARGET: Mutex<Option<Target>> = Mutex::new(None);

/// The taskbar, the desktop and the tray overflow are never pasted into.
const SHELL: [&str; 5] = [
    "Shell_TrayWnd",
    "Progman",
    "WorkerW",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
];

/// Whether a window of process `pid` and class `class` can be pasted into.
fn eligible(pid: u32, own: u32, class: &str) -> bool {
    pid != 0 && pid != own && !SHELL.contains(&class)
}

/// The target after another program's `window` was seen in front with the
/// keyboard in `control` (`None`: not looked at, so what is known stays).
fn seen(current: Option<Target>, window: usize, control: Option<usize>) -> Target {
    let known = current
        .filter(|t| t.window == window)
        .map_or(0, |t| t.control);
    Target {
        window,
        control: control.unwrap_or(known),
    }
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0;
    // SAFETY: `pid` is a valid out-pointer; any handle value is accepted.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

fn thread_of(hwnd: HWND) -> u32 {
    // SAFETY: any handle value is accepted; the process id is not wanted.
    unsafe { GetWindowThreadProcessId(hwnd, null_mut()) }
}

/// The control that has the keyboard in the thread owning `hwnd` (0 = none).
fn focus_of(hwnd: HWND) -> usize {
    // SAFETY: a zeroed struct is valid once its size is set.
    let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
    let thread = thread_of(hwnd);
    // SAFETY: `info` is a valid out-pointer of the size named.
    if thread == 0 || unsafe { GetGUIThreadInfo(thread, &mut info) } == 0 {
        return 0;
    }
    info.hwndFocus as usize
}

fn note(hwnd: HWND, with_control: bool) {
    let mut class = [0u16; 64];
    // SAFETY: the buffer holds the 64 units named.
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 64) };
    let class = String::from_utf16_lossy(&class[..usize::try_from(len).unwrap_or(0)]);
    // SAFETY: no arguments.
    if eligible(pid_of(hwnd), unsafe { GetCurrentProcessId() }, &class) {
        let control = with_control.then(|| focus_of(hwnd));
        let mut target = TARGET.lock().unwrap_or_else(PoisonError::into_inner);
        *target = Some(seen(*target, hwnd as usize, control));
    }
}

/// Notes the window in front and its focused control. Call it when the shortcut
/// is pressed, before the bar is shown. Lookout's own windows and the taskbar
/// change nothing.
pub fn remember_front() {
    // SAFETY: no arguments.
    note(unsafe { GetForegroundWindow() }, true);
}

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    // A pinned bar can be left for another window and clicked again.
    note(hwnd, false);
}

/// Starts noting which window was in front. Call it once, on a thread that runs
/// a message loop.
pub fn watch_foreground() {
    remember_front();
    // SAFETY: an out-of-context hook with a function that lives for the whole
    // process.
    unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            null_mut(),
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
    }
}

/// The window a paste would go to now. Read it before hiding the bar: hiding can
/// bring some other window forward.
pub fn target() -> Option<Target> {
    *TARGET.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why nothing was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    NotRestored,
    Administrator,
    Refused,
}

impl Failure {
    /// What the user is told.
    fn message(self) -> String {
        let why = match self {
            Self::NotRestored => "that window could not be brought back",
            Self::Administrator => "that app runs as administrator",
            Self::Refused => "Windows refused the keys",
        };
        format!("{PASTE_FAILED} ({why})")
    }

    /// The one line written to the log.
    fn log_line(self) -> &'static str {
        match self {
            Self::NotRestored => "paste: the earlier window could not be brought to the front",
            Self::Administrator => "paste: the earlier window runs as administrator",
            Self::Refused => "paste: Windows did not accept the keystrokes",
        }
    }
}

/// What delivering a paste needs from Windows.
trait Desk {
    fn usable(&self, window: usize) -> bool;
    fn administrator(&self, window: usize) -> bool;
    fn foreground(&self) -> usize;
    fn activate(&mut self, window: usize);
    /// Whether `control` still exists in `window`.
    fn holds(&self, window: usize, control: usize) -> bool;
    fn focus(&self, window: usize) -> usize;
    fn set_focus(&mut self, window: usize, control: usize);
    /// Releases held modifiers and types Ctrl+V; false if Windows refused.
    fn type_paste(&mut self) -> bool;
    fn elapsed(&self) -> Duration;
    fn wait(&mut self, time: Duration);
}

/// Brings the target back, polling until it is in front with the keyboard where
/// it was, then types. A control that will not take the focus back does not stop
/// the paste: the program has put the focus where it wants it.
fn deliver(desk: &mut impl Desk, target: Option<Target>) -> Result<(), Failure> {
    let Some(Target { window, control }) = target.filter(|t| desk.usable(t.window)) else {
        return Err(Failure::NotRestored);
    };
    if desk.administrator(window) {
        return Err(Failure::Administrator);
    }
    let start = desk.elapsed();
    let mut asked: Option<Duration> = None;
    let mut steady = 0;
    loop {
        let front = desk.foreground() == window;
        let now = desk.elapsed();
        if front {
            let wanted = control != 0 && desk.holds(window, control);
            if wanted && desk.focus(window) != control {
                desk.set_focus(window, control);
                steady = 0;
            } else {
                steady += 1;
            }
        } else {
            steady = 0;
            if asked.is_none_or(|at| now - at >= ASK_AGAIN) {
                desk.activate(window);
                asked = Some(now);
            }
        }
        if steady >= STEADY {
            break;
        }
        if now - start >= RESTORE_LIMIT {
            if front {
                break;
            }
            return Err(Failure::NotRestored);
        }
        desk.wait(POLL);
    }
    if desk.type_paste() {
        Ok(())
    } else {
        Err(Failure::Refused)
    }
}

/// The keys to send, as (key, released): what is still held is let go first.
fn keystrokes(held: &[u16]) -> Vec<(u16, bool)> {
    let mut keys = Vec::new();
    if held
        .iter()
        .any(|k| [VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN].contains(k))
    {
        keys.extend([(MASK_KEY, false), (MASK_KEY, true)]);
    }
    keys.extend(held.iter().map(|&k| (k, true)));
    let v = u16::from(b'V');
    keys.extend([
        (VK_CONTROL, false),
        (v, false),
        (v, true),
        (VK_CONTROL, true),
    ]);
    keys
}

/// Whether `process` runs elevated; `None` if Windows will not say.
fn elevated(process: HANDLE) -> Option<bool> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is a valid out-pointer; it is closed below.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut value = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
    let mut got = 0;
    // SAFETY: `value` is exactly `size` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            std::ptr::from_mut(&mut value).cast(),
            size,
            &mut got,
        )
    } != 0;
    // SAFETY: opened above.
    unsafe { CloseHandle(token) };
    ok.then_some(value.TokenIsElevated != 0)
}

/// True if keys sent to this window would be dropped: it runs as administrator
/// and this program does not.
fn out_of_reach(hwnd: HWND) -> bool {
    // SAFETY: a pseudo handle that needs no closing.
    if elevated(unsafe { GetCurrentProcess() }) == Some(true) {
        return false;
    }
    // SAFETY: query-only rights; closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid_of(hwnd)) };
    if process.is_null() {
        return true;
    }
    let theirs = elevated(process);
    // SAFETY: opened above.
    unsafe { CloseHandle(process) };
    theirs.unwrap_or(true)
}

/// Runs `f` while this thread shares input with the thread owning `hwnd`, which
/// Windows asks of anyone moving the front or the focus there. A hung program is
/// never attached to: that would hang this one.
fn attached(hwnd: HWND, f: impl FnOnce()) {
    // SAFETY: these accept any handle value; `msg` is a valid out-pointer, and
    // peeking gives this thread the message queue that attaching needs.
    unsafe {
        let (me, other) = (GetCurrentThreadId(), thread_of(hwnd));
        if other == 0 || other == me || IsHungAppWindow(hwnd) != 0 {
            return;
        }
        let mut msg: MSG = std::mem::zeroed();
        PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
        if AttachThreadInput(me, other, 1) != 0 {
            f();
            AttachThreadInput(me, other, 0);
        }
    }
}

struct WindowsDesk(Instant);

impl Desk for WindowsDesk {
    fn usable(&self, window: usize) -> bool {
        let hwnd = window as HWND;
        // SAFETY: both calls accept any handle value.
        unsafe { IsWindow(hwnd) != 0 && IsWindowVisible(hwnd) != 0 }
    }
    fn administrator(&self, window: usize) -> bool {
        out_of_reach(window as HWND)
    }
    fn foreground(&self) -> usize {
        // SAFETY: no arguments.
        unsafe { GetForegroundWindow() as usize }
    }
    fn activate(&mut self, window: usize) {
        let hwnd = window as HWND;
        // SAFETY: these accept any handle value.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindowAsync(hwnd, SW_RESTORE);
            }
            if SetForegroundWindow(hwnd) == 0 {
                // Refused: ask again sharing input with whoever is in front.
                attached(GetForegroundWindow(), || {
                    BringWindowToTop(hwnd);
                    SetForegroundWindow(hwnd);
                });
            }
        }
    }
    fn holds(&self, window: usize, control: usize) -> bool {
        // SAFETY: accepts any handle value.
        let there = unsafe { IsWindow(control as HWND) } != 0;
        there && thread_of(control as HWND) == thread_of(window as HWND)
    }
    fn focus(&self, window: usize) -> usize {
        focus_of(window as HWND)
    }
    fn set_focus(&mut self, window: usize, control: usize) {
        attached(window as HWND, || {
            // SAFETY: a window of the attached thread, checked by `holds`.
            unsafe { SetFocus(control as HWND) };
        });
    }
    fn type_paste(&mut self) -> bool {
        let held: Vec<u16> = MODIFIERS
            .into_iter()
            // SAFETY: reads the state of one key.
            .filter(|&k| unsafe { GetAsyncKeyState(i32::from(k)) } < 0)
            .collect();
        let inputs: Vec<INPUT> = keystrokes(&held)
            .into_iter()
            .map(|(vk, up)| {
                let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
                if vk == VK_RCONTROL || vk == VK_RMENU {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: vk,
                            wScan: 0,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                }
            })
            .collect();
        let count = inputs.len() as u32;
        let size = std::mem::size_of::<INPUT>() as i32;
        // SAFETY: a valid array of `count` INPUT structs of the size named.
        unsafe { SendInput(count, inputs.as_ptr(), size) == count }
    }
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
    fn wait(&mut self, time: Duration) {
        std::thread::sleep(time);
    }
}

/// Pastes the clipboard into `target` and returns what to tell the user. Hide the
/// bar first.
pub fn paste_into(target: Option<Target>) -> String {
    match deliver(&mut WindowsDesk(Instant::now()), target) {
        Ok(()) => "Pasted".to_owned(),
        Err(failure) => {
            crate::log::warn(failure.log_line());
            failure.message()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: usize = 10;
    const BOX: usize = 11;
    const BAR: usize = 99;
    const T: Option<Target> = Some(Target {
        window: APP,
        control: BOX,
    });

    /// A desktop where the app comes forward after `front_after`.
    #[derive(Default)]
    struct Fake {
        now: Duration,
        gone: bool,
        admin: bool,
        front_after: Option<Duration>,
        focus: usize,
        focus_stuck: bool,
        refuse: bool,
        asked: u32,
        typed: Option<Duration>,
    }

    impl Fake {
        fn after(ms: u64) -> Self {
            Self {
                front_after: Some(Duration::from_millis(ms)),
                ..Self::default()
            }
        }
    }

    impl Desk for Fake {
        fn usable(&self, _: usize) -> bool {
            !self.gone
        }
        fn administrator(&self, _: usize) -> bool {
            self.admin
        }
        fn foreground(&self) -> usize {
            match self.front_after {
                Some(at) if self.now >= at => APP,
                _ => BAR,
            }
        }
        fn activate(&mut self, _: usize) {
            self.asked += 1;
        }
        fn holds(&self, _: usize, _: usize) -> bool {
            true
        }
        fn focus(&self, _: usize) -> usize {
            self.focus
        }
        fn set_focus(&mut self, _: usize, control: usize) {
            if !self.focus_stuck {
                self.focus = control;
            }
        }
        fn type_paste(&mut self) -> bool {
            self.typed = Some(self.now);
            !self.refuse
        }
        fn elapsed(&self) -> Duration {
            self.now
        }
        fn wait(&mut self, time: Duration) {
            self.now += time;
        }
    }

    #[test]
    fn types_as_soon_as_the_window_is_in_front() {
        let mut desk = Fake::after(120);
        assert_eq!(deliver(&mut desk, T), Ok(()));
        let at = desk.typed.unwrap();
        assert!(at >= Duration::from_millis(120) && at < Duration::from_millis(200));
        assert_eq!(desk.focus, BOX);
        assert!(desk.asked >= 2);
    }

    #[test]
    fn a_window_that_never_comes_forward_is_not_typed_into() {
        for mut desk in [Fake::default(), Fake::after(5_000)] {
            assert_eq!(deliver(&mut desk, T), Err(Failure::NotRestored));
            assert_eq!(desk.typed, None);
            assert_eq!(desk.now, RESTORE_LIMIT);
        }
    }

    #[test]
    fn a_missing_window_and_an_administrator_window_are_told_apart() {
        let mut desk = Fake::after(0);
        assert_eq!(deliver(&mut desk, None), Err(Failure::NotRestored));
        desk.gone = true;
        assert_eq!(deliver(&mut desk, T), Err(Failure::NotRestored));
        desk.gone = false;
        desk.admin = true;
        assert_eq!(deliver(&mut desk, T), Err(Failure::Administrator));
        assert_eq!((desk.typed, desk.asked), (None, 0));
    }

    #[test]
    fn a_control_that_keeps_the_focus_elsewhere_still_gets_the_paste() {
        let mut desk = Fake::after(0);
        desk.focus_stuck = true;
        assert_eq!(deliver(&mut desk, T), Ok(()));
        assert_eq!(desk.typed, Some(RESTORE_LIMIT));
    }

    #[test]
    fn refused_keys_are_not_called_a_paste() {
        let mut desk = Fake::after(0);
        desk.refuse = true;
        assert_eq!(deliver(&mut desk, T), Err(Failure::Refused));
    }

    #[test]
    fn every_failure_says_the_text_was_copied() {
        for f in [
            Failure::NotRestored,
            Failure::Administrator,
            Failure::Refused,
        ] {
            assert!(f.message().starts_with(PASTE_FAILED));
        }
    }

    #[test]
    fn the_target_is_another_programs_window() {
        assert!(eligible(5, 1, "Notepad"));
        assert!(!eligible(1, 1, "Notepad"));
        assert!(!eligible(0, 1, "Notepad"));
        assert!(!eligible(5, 1, "Shell_TrayWnd"));
    }

    #[test]
    fn the_control_noted_at_the_shortcut_stays_with_its_window() {
        let at_shortcut = seen(None, APP, Some(BOX));
        assert_eq!(Some(at_shortcut), T);
        assert_eq!(seen(T, APP, None), at_shortcut);
        assert_eq!(seen(T, 20, None).control, 0);
        assert_eq!(seen(T, APP, Some(12)).control, 12);
    }

    #[test]
    fn held_modifiers_are_released_before_ctrl_v() {
        let plain = keystrokes(&[]);
        assert_eq!(plain.len(), 4);
        assert_eq!(plain[0], (VK_CONTROL, false));
        let held = keystrokes(&[VK_LSHIFT, VK_LMENU]);
        assert_eq!(
            held[..4],
            [
                (MASK_KEY, false),
                (MASK_KEY, true),
                (VK_LSHIFT, true),
                (VK_LMENU, true)
            ]
        );
        assert_eq!(held[4..], plain[..]);
        assert_eq!(keystrokes(&[VK_LSHIFT])[0], (VK_LSHIFT, true));
    }

    #[test]
    fn only_notes_and_the_built_in_clipboard_paste() {
        assert!(is_paste("note_paste"));
        assert!(is_paste("plugin/com.lookout.clipboard/paste_plain"));
        assert!(!is_paste("plugin/com.lookout.clipboard/copy_clip"));
        assert!(!is_paste("plugin/someone.else/paste_clip"));
    }
}
