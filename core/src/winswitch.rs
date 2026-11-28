//! The window switcher. A search whose words are in an open window's title
//! offers up to two "switch to" results; `win ` and some words list every
//! match, and Home lists the front ones. Enter brings a window to the front;
//! it can also be minimized or closed. Titles live in the results on screen
//! only: they are never stored, never written to a log and never given to the
//! command line. The actions know a window by handle and process alone.

use std::sync::Arc;

use crate::actions::{Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target};
use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};

pub const SOURCE: &str = "windows";
pub const KIND: &str = "window";
pub const FOCUS: &str = "window_focus";
pub const MINIMIZE: &str = "window_minimize";
pub const CLOSE: &str = "window_close";
const PREFIX: &str = "win ";

/// One plain line for a window that will not come forward.
pub const REFUSED: &str = "Windows did not switch to that window. If it runs as administrator, \
                           click it on the taskbar.";
const CLOSED: &str = "That window has closed. Search again.";
const NOT_MINIMIZED: &str = "Windows did not minimize that window. If it runs as \
                             administrator, use its own button.";
const STILL_OPEN: &str = "That window is still open. It may be asking you something: \
                          switch to it.";
/// Most "switch to" results an ordinary search offers.
pub const MAX_MATCHES: usize = 2;
/// Above files and apps whose names only contain the words, and below an exact
/// name (`ranking::TIER_STEP` and up) or a Settings page's own name.
pub const MATCH_SCORE: f32 = 1.15;
/// Searches that belong to a command, not to window titles.
const COMMAND_WORDS: &[&str] = &[
    "clip",
    "clips",
    "clipboard",
    "shot",
    "screenshot",
    "scrolling",
    "scroll",
    "record",
    "extract",
    "timer",
    "kill",
    "fav",
];

/// The words to filter by, if the text begins `win ` (the space is part of it,
/// so a plain search for "win" stays a search).
pub fn command(typed: &str) -> Option<&str> {
    let t = typed.trim_start();
    let head = t.get(..PREFIX.len())?;
    head.eq_ignore_ascii_case(PREFIX)
        .then(|| t[PREFIX.len()..].trim())
}

/// Which window: the handle and the process that owned it when it was listed,
/// with the time that process started, so a reused number is not trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowId {
    pub handle: isize,
    pub pid: u32,
    /// 0 if Windows would not say.
    pub started: u64,
}

impl WindowId {
    fn encode(self) -> String {
        format!("{}:{}:{}", self.handle, self.pid, self.started)
    }

    fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let id = Self {
            handle: parts.next()?.parse().ok()?,
            pid: parts.next()?.parse().ok()?,
            started: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(id)
    }
}

pub struct OpenWindow {
    pub id: WindowId,
    pub title: String,
    pub minimized: bool,
}

pub trait Windows: Send + Sync {
    /// Visible windows with a title that are not Lookout's, front first.
    fn list(&self) -> Vec<OpenWindow>;
    /// Whether the handle still names a visible window of that same process.
    fn is_same(&self, id: WindowId) -> bool;
    /// Restores the window and gives it the focus; false if it did not get it.
    fn focus(&self, id: WindowId) -> bool;
    /// Minimizes without taking the focus; false if it stayed.
    fn minimize(&self, id: WindowId) -> bool;
    /// Asks the window to close, as its own close button does; false if it is
    /// still there a moment later.
    fn close(&self, id: WindowId) -> bool;
}

/// One window as a result. `how` is its second line.
pub fn item(w: OpenWindow, how: &str, score: f32) -> Item {
    // The id reaches the action log, so it holds no part of the title.
    let mut item = Item::new(
        format!("window:{}:{}", w.id.handle, w.id.pid),
        Kind::Custom(KIND.into()),
        w.title,
        SOURCE,
    );
    item.subtitle = Some(how.into());
    item.uri = Some(w.id.encode());
    item.score = score;
    item.score_features.match_quality = 1.0;
    item.metadata.insert("default_action".into(), FOCUS.into());
    item
}

/// Whether an ordinary search offers this window: every typed word is a whole
/// word of its title, or the title begins with the typed text.
fn title_matches(title: &str, typed: &str) -> bool {
    let (title, typed) = (title.to_lowercase(), typed.to_lowercase());
    let words: Vec<&str> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    title.starts_with(&typed) || typed.split_whitespace().all(|t| words.contains(&t))
}

/// The typed text to match titles with, if this is an ordinary search.
fn ordinary(q: &ParsedQuery) -> Option<&str> {
    let first = q.raw.split_whitespace().next()?;
    let command = (q.trailing_space || q.raw.split_whitespace().nth(1).is_some())
        && COMMAND_WORDS.iter().any(|w| first.eq_ignore_ascii_case(w));
    (q.raw.chars().count() >= 2
        && q.kind.is_none()
        && q.time.is_none()
        && q.ops.is_empty()
        && !command)
        .then_some(q.raw.as_str())
}

/// A PC with nothing open.
pub struct NoWindows;

impl Windows for NoWindows {
    fn list(&self) -> Vec<OpenWindow> {
        Vec::new()
    }
    fn is_same(&self, _: WindowId) -> bool {
        false
    }
    fn focus(&self, _: WindowId) -> bool {
        false
    }
    fn minimize(&self, _: WindowId) -> bool {
        false
    }
    fn close(&self, _: WindowId) -> bool {
        false
    }
}

pub struct WindowSource(pub Arc<dyn Windows>);

impl Source for WindowSource {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        // The parser trims the text; the space after `win` is what counts.
        let typed = if q.trailing_space {
            format!("{} ", q.raw)
        } else {
            q.raw.clone()
        };
        let items: Vec<Item> = if let Some(filter) = command(&typed) {
            let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
            self.0
                .list()
                .into_iter()
                .filter(|w| {
                    let title = w.title.to_lowercase();
                    words.iter().all(|word| title.contains(word))
                })
                .enumerate()
                .map(|(n, w)| {
                    let how = if w.minimized {
                        "Minimized window"
                    } else {
                        "Open window"
                    };
                    item(w, how, ANSWER_SCORE - n as f32 * 0.001)
                })
                .collect()
        } else if let Some(typed) = ordinary(q) {
            self.0
                .list()
                .into_iter()
                .filter(|w| title_matches(&w.title, typed))
                .take(MAX_MATCHES)
                .enumerate()
                .map(|(n, w)| {
                    let how = if w.minimized {
                        "Switch to minimized window"
                    } else {
                        "Switch to window"
                    };
                    item(w, how, MATCH_SCORE - n as f32 * 0.001)
                })
                .collect()
        } else {
            return;
        };
        if !items.is_empty() {
            emit(items);
        }
    }
}

pub struct FocusAction(pub Arc<dyn Windows>);

impl FocusAction {
    fn check(&self, id: Option<WindowId>) -> Result<WindowId, ActionError> {
        id.filter(|&id| self.0.is_same(id))
            .ok_or_else(|| ActionError::Stale(CLOSED.into()))
    }
}

impl Action for FocusAction {
    fn id(&self) -> &str {
        FOCUS
    }

    fn label(&self) -> &str {
        "Switch to window"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::Custom(n) if n == KIND)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        if target.origin.is_some() {
            return Err(ActionError::NotApplicable {
                action: FOCUS.into(),
            });
        }
        let id = self.check(target.uri.as_deref().and_then(WindowId::decode))?;
        let uri = id.encode();
        Ok(Plan {
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: "Switch to the chosen window".into(),
                lines: Vec::new(),
                warnings: Vec::new(),
            },
            undoable: false,
            snapshot: serde_json::json!({ "window": uri }),
            fingerprint: uri,
        })
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let id = self.check(plan.snapshot["window"].as_str().and_then(WindowId::decode))?;
        if !self.0.focus(id) {
            return Err(ActionError::Refused(REFUSED.into()));
        }
        Ok(Executed {
            message: "Switched window".into(),
            undo_token: None,
        })
    }
}

/// "Minimize" and "Close" on a window result.
pub struct ShowAction {
    pub windows: Arc<dyn Windows>,
    pub close: bool,
}

impl Action for ShowAction {
    fn id(&self) -> &str {
        if self.close {
            CLOSE
        } else {
            MINIMIZE
        }
    }

    fn label(&self) -> &str {
        if self.close {
            "Close window"
        } else {
            "Minimize window"
        }
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::Custom(n) if n == KIND)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        if target.origin.is_some() {
            return Err(ActionError::NotApplicable {
                action: self.id().into(),
            });
        }
        let id = FocusAction(self.windows.clone())
            .check(target.uri.as_deref().and_then(WindowId::decode))?;
        let uri = id.encode();
        Ok(Plan {
            // The program itself asks about unsaved work, as with its own button.
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: self.label().into(),
                lines: Vec::new(),
                warnings: Vec::new(),
            },
            undoable: false,
            snapshot: serde_json::json!({ "window": uri }),
            fingerprint: uri,
        })
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let id = FocusAction(self.windows.clone())
            .check(plan.snapshot["window"].as_str().and_then(WindowId::decode))?;
        let (worked, refused, message) = if self.close {
            (self.windows.close(id), STILL_OPEN, "Closed the window")
        } else {
            (
                self.windows.minimize(id),
                NOT_MINIMIZED,
                "Minimized the window",
            )
        };
        if !worked {
            return Err(ActionError::Refused(refused.into()));
        }
        Ok(Executed {
            message: message.into(),
            undo_token: None,
        })
    }
}

#[cfg(windows)]
pub use real::SystemWindows;

#[cfg(windows)]
mod real {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HWND, LPARAM};
    use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetAncestor, GetForegroundWindow, GetLastActivePopup, GetShellWindow,
        GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        PostMessageW, SetForegroundWindow, ShowWindowAsync, GA_ROOTOWNER, GWL_EXSTYLE, SW_RESTORE,
        SW_SHOWMINNOACTIVE, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    use super::{OpenWindow, WindowId, Windows};

    /// Most characters read of one title.
    const MAX_TITLE: usize = 512;

    pub struct SystemWindows;

    unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> i32 {
        // SAFETY: `list` is the vector `handles` passes, alive for the whole call.
        unsafe { (*(list as *mut Vec<HWND>)).push(hwnd) };
        1
    }

    fn handles() -> Vec<HWND> {
        let mut list: Vec<HWND> = Vec::new();
        // SAFETY: the callback only pushes into `list`, which outlives the call.
        unsafe { EnumWindows(Some(collect), std::ptr::addr_of_mut!(list) as LPARAM) };
        list
    }

    fn owner(hwnd: HWND) -> u32 {
        let mut pid = 0;
        // SAFETY: reads window state; a dead handle leaves `pid` at 0.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        pid
    }

    fn started(pid: u32) -> u64 {
        // SAFETY: the handle is checked and closed once; the four times are
        // written by the call.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return 0;
            }
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
            let ok = GetProcessTimes(process, &mut created, &mut exit, &mut kernel, &mut user);
            CloseHandle(process);
            if ok == 0 {
                return 0;
            }
            u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime)
        }
    }

    /// On screen or on the taskbar: visible, not a tool window, not the desktop
    /// and not hidden by Windows (suspended Store apps, other desktops).
    fn is_listed(hwnd: HWND) -> bool {
        // SAFETY: each call only reads window state; `cloaked` has the size passed.
        unsafe {
            if IsWindowVisible(hwnd) == 0 || hwnd == GetShellWindow() {
                return false;
            }
            let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            if style & WS_EX_TOOLWINDOW != 0 && style & WS_EX_APPWINDOW == 0 {
                return false;
            }
            let mut cloaked = 0u32;
            let read = DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                std::ptr::addr_of_mut!(cloaked).cast(),
                4,
            );
            read != 0 || cloaked == 0
        }
    }

    /// The title as Windows holds it; no message is sent to the other program,
    /// so a hung one cannot stall the search.
    fn title(hwnd: HWND) -> String {
        let mut buf = [0u16; MAX_TITLE];
        // SAFETY: the length passed is the buffer's.
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), MAX_TITLE as i32) };
        let text = String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0)]);
        crate::quick::strip_controls(&text).trim().to_owned()
    }

    impl Windows for SystemWindows {
        fn list(&self) -> Vec<OpenWindow> {
            let ours = std::process::id();
            handles()
                .into_iter()
                .filter(|&h| is_listed(h))
                .filter_map(|h| {
                    let pid = owner(h);
                    if pid == 0 || pid == ours {
                        return None;
                    }
                    let title = title(h);
                    (!title.is_empty()).then(|| OpenWindow {
                        id: WindowId {
                            handle: h as isize,
                            pid,
                            started: started(pid),
                        },
                        title,
                        // SAFETY: reads window state.
                        minimized: unsafe { IsIconic(h) } != 0,
                    })
                })
                .collect()
        }

        fn is_same(&self, id: WindowId) -> bool {
            let hwnd = id.handle as HWND;
            id.pid != std::process::id()
                && owner(hwnd) == id.pid
                && started(id.pid) == id.started
                && is_listed(hwnd)
        }

        fn focus(&self, id: WindowId) -> bool {
            let hwnd = id.handle as HWND;
            // SAFETY: calls on a window handle; a window that closed meanwhile
            // makes them fail, which the loop reports.
            unsafe {
                if IsIconic(hwnd) != 0 {
                    ShowWindowAsync(hwnd, SW_RESTORE);
                }
                // A window with a dialog open hands the focus to the dialog.
                let popup = GetLastActivePopup(hwnd);
                let front = if IsWindowVisible(popup) != 0 {
                    popup
                } else {
                    hwnd
                };
                SetForegroundWindow(front);
                let wanted = GetAncestor(hwnd, GA_ROOTOWNER);
                for _ in 0..25 {
                    let now = GetForegroundWindow();
                    if !now.is_null()
                        && GetAncestor(now, GA_ROOTOWNER) == wanted
                        && IsIconic(hwnd) == 0
                    {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            false
        }

        fn minimize(&self, id: WindowId) -> bool {
            let hwnd = id.handle as HWND;
            // SAFETY: a call on a window handle; a closed one makes it fail.
            unsafe { ShowWindowAsync(hwnd, SW_SHOWMINNOACTIVE) };
            // SAFETY: reads window state.
            wait_for(|| unsafe { IsIconic(hwnd) } != 0)
        }

        fn close(&self, id: WindowId) -> bool {
            // SAFETY: posts the message the close button sends; it carries no data.
            unsafe { PostMessageW(id.handle as HWND, WM_CLOSE, 0, 0) };
            wait_for(|| !self.is_same(id))
        }
    }

    /// Whether `done` became true within a second.
    fn wait_for(done: impl Fn() -> bool) -> bool {
        (0..50).any(|_| {
            done() || {
                std::thread::sleep(Duration::from_millis(20));
                false
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Mutex;

    struct Fake {
        open: Mutex<Vec<(u32, &'static str)>>,
        focused: Mutex<Vec<u32>>,
        refuse: bool,
    }

    impl Fake {
        fn new(refuse: bool) -> Arc<Self> {
            Arc::new(Self {
                open: Mutex::new(vec![(1, "Budget.xlsx - Excel"), (2, "Inbox - Mail")]),
                focused: Mutex::default(),
                refuse,
            })
        }
    }

    fn id(pid: u32) -> WindowId {
        WindowId {
            handle: pid as isize * 10,
            pid,
            started: 7,
        }
    }

    impl Windows for Fake {
        fn list(&self) -> Vec<OpenWindow> {
            self.open
                .lock()
                .unwrap()
                .iter()
                .map(|&(pid, title)| OpenWindow {
                    id: id(pid),
                    title: title.into(),
                    minimized: pid == 2,
                })
                .collect()
        }
        fn is_same(&self, w: WindowId) -> bool {
            self.open.lock().unwrap().iter().any(|&(p, _)| id(p) == w)
        }
        fn focus(&self, w: WindowId) -> bool {
            self.focused.lock().unwrap().push(w.pid);
            !self.refuse
        }
        fn minimize(&self, w: WindowId) -> bool {
            self.focus(w)
        }
        fn close(&self, w: WindowId) -> bool {
            if !self.refuse {
                self.open.lock().unwrap().retain(|&(p, _)| id(p) != w);
            }
            !self.refuse
        }
    }

    fn ask(f: &Arc<Fake>, text: &str) -> Vec<Item> {
        let q = crate::query::parse(text);
        let ctx = QueryContext::new(1, Arc::new(AtomicU64::new(1)));
        let mut out = Vec::new();
        WindowSource(f.clone()).query(&q, &ctx, &mut |i| out.extend(i));
        out
    }

    fn target(item: &Item) -> Target {
        Target {
            item_id: item.id.clone(),
            kind: item.kind.clone(),
            uri: item.uri.clone(),
            port: None,
            origin: None,
        }
    }

    #[test]
    fn the_prefix_with_its_space_lists_windows() {
        let f = Fake::new(false);
        assert_eq!(ask(&f, "win ").len(), 2);
        assert_eq!(ask(&f, "WIN  excel budget")[0].title, "Budget.xlsx - Excel");
        assert_eq!(
            ask(&f, "win mail")[0].subtitle.as_deref(),
            Some("Minimized window")
        );
        // Words that mean a date elsewhere are only title words here.
        assert!(ask(&f, "win today").is_empty());
        for no in ["win", "window", "winmail"] {
            assert!(ask(&f, no).is_empty(), "{no}");
        }
    }

    #[test]
    fn an_ordinary_search_offers_titles_by_whole_word_or_start() {
        let f = Fake::new(false);
        for yes in ["excel", "Budget", "bud", "excel budget", "budget.xlsx - ex"] {
            let found = ask(&f, yes);
            assert_eq!(found.len(), 1, "{yes}");
            assert_eq!(found[0].subtitle.as_deref(), Some("Switch to window"));
            assert!(found[0].score < crate::ranking::TIER_STEP);
        }
        // Part of a word, one letter, filters and other commands offer nothing.
        for no in [
            "udget",
            "xce",
            "b",
            "clip excel",
            "shot excel",
            "excel ext:pdf",
        ] {
            assert!(ask(&f, no).is_empty(), "{no}");
        }
        // Never more than two, front first.
        f.open
            .lock()
            .unwrap()
            .extend([(3, "Excel two"), (4, "Excel three")]);
        let titles: Vec<String> = ask(&f, "excel").into_iter().map(|i| i.title).collect();
        assert_eq!(titles, ["Budget.xlsx - Excel", "Excel two"]);
    }

    #[test]
    fn a_window_can_be_minimized_or_closed_and_a_refusal_is_one_line() {
        let f = Fake::new(false);
        let item = ask(&f, "win budget").remove(0);
        for close in [false, true] {
            let act = ShowAction {
                windows: f.clone(),
                close,
            };
            let plan = act.plan(&target(&item)).unwrap();
            act.execute(&plan, &Authorized::for_tests()).unwrap();
        }
        assert!(ask(&f, "win budget").is_empty(), "closed");
        let f = Fake::new(true);
        let item = ask(&f, "win budget").remove(0);
        let act = ShowAction {
            windows: f.clone(),
            close: true,
        };
        let plan = act.plan(&target(&item)).unwrap();
        let err = act.execute(&plan, &Authorized::for_tests()).unwrap_err();
        assert_eq!(err.to_string(), STILL_OPEN);
        assert!(!STILL_OPEN.contains('\n') && !NOT_MINIMIZED.contains('\n'));
    }

    #[test]
    fn nothing_kept_by_the_action_holds_a_title() {
        let f = Fake::new(false);
        let item = ask(&f, "win budget").remove(0);
        let act = FocusAction(f.clone());
        let plan = act.plan(&target(&item)).unwrap();
        let done = act.execute(&plan, &Authorized::for_tests()).unwrap();
        for kept in [
            item.id.clone(),
            item.uri.clone().unwrap(),
            plan.snapshot.to_string(),
            format!("{:?}", plan.preview),
            done.message,
        ] {
            assert!(!kept.contains("Budget"), "{kept}");
        }
        assert_eq!(*f.focused.lock().unwrap(), [1]);
        assert!(
            !crate::quickkeys::eligible(&item),
            "never stored as a quick key"
        );
    }

    #[test]
    fn a_closed_or_replaced_window_is_not_focused_and_a_refusal_is_one_line() {
        let f = Fake::new(true);
        let items = ask(&f, "win ");
        let act = FocusAction(f.clone());
        let plan = act.plan(&target(&items[0])).unwrap();
        let err = act.execute(&plan, &Authorized::for_tests()).unwrap_err();
        assert_eq!(err.to_string(), REFUSED);
        assert!(!REFUSED.contains('\n'));
        let mut reused = target(&items[0]);
        // Same handle and process number, started at another time.
        reused.uri = Some("10:1:8".into());
        assert!(matches!(act.plan(&reused), Err(ActionError::Stale(_))));
        f.open.lock().unwrap().remove(0);
        assert!(matches!(
            act.execute(&plan, &Authorized::for_tests()),
            Err(ActionError::Stale(_))
        ));
        assert_eq!(f.focused.lock().unwrap().len(), 1);
    }
}
