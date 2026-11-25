//! Quick actions: open a web address, open a fixed Settings page, switch
//! dark / light mode. Every one goes
//! through the action engine, so each shows a preview first. Lookout makes no
//! network connection itself: a web address is handed to the default browser.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::open::Launcher;
use super::{Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target, UndoPlan};
use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::quick::{self, What};
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};

pub const OPEN_URL: &str = "open_url";
pub const SYSTEM_PAGE: &str = "system_page";
pub const SYSTEM_THEME: &str = "system_theme";

/// `Kind::Custom` names used by the results of this module.
pub const KIND_URL: &str = "url";
pub const KIND_SYSTEM: &str = "system";

fn is_custom(kind: &Kind, name: &str) -> bool {
    matches!(kind, Kind::Custom(n) if n == name)
}

fn simple_plan(
    risk: Risk,
    title: String,
    lines: Vec<String>,
    warnings: Vec<String>,
    undoable: bool,
    snapshot: serde_json::Value,
    fingerprint: String,
) -> Plan {
    Plan {
        risk,
        preview: Preview {
            from: None,
            title,
            lines,
            warnings,
        },
        undoable,
        snapshot,
        fingerprint,
    }
}

// ---------------------------------------------------------------- theme

/// The two Personalize values, `None` when the value is not set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeValues {
    pub apps: Option<u32>,
    pub system: Option<u32>,
}

impl ThemeValues {
    /// Apps are dark when `AppsUseLightTheme` is 0; an unset value means light.
    pub fn is_dark(&self) -> bool {
        self.apps == Some(0)
    }

    fn both(light: bool) -> Self {
        let v = Some(u32::from(light));
        Self { apps: v, system: v }
    }
}

pub trait ThemeBackend: Send + Sync {
    fn read(&self) -> ThemeValues;
    /// Writes (or deletes, for `None`) the values and tells running programs.
    fn write(&self, v: &ThemeValues) -> Result<(), String>;
}

pub struct ThemeAction {
    backend: Arc<dyn ThemeBackend>,
}

impl ThemeAction {
    pub fn new(backend: Arc<dyn ThemeBackend>) -> Self {
        Self { backend }
    }
}

fn mode_name(dark: bool) -> &'static str {
    if dark {
        "dark"
    } else {
        "light"
    }
}

impl Action for ThemeAction {
    fn id(&self) -> &str {
        SYSTEM_THEME
    }

    fn label(&self) -> &str {
        "Switch dark / light mode"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        is_custom(kind, KIND_SYSTEM)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        if target.uri.as_deref() != Some("dark_mode") {
            return Err(ActionError::NotApplicable {
                action: SYSTEM_THEME.into(),
            });
        }
        let from = self.backend.read();
        let to = ThemeValues::both(from.is_dark());
        let name = mode_name(to.apps == Some(0));
        Ok(simple_plan(
            Risk::Safe,
            format!("Switch to {name} mode"),
            vec![format!(
                "Windows and apps change from {} to {name} at once.",
                mode_name(from.is_dark())
            )],
            Vec::new(),
            true,
            serde_json::json!({ "from": from, "to": to }),
            format!("theme|{from:?}|{to:?}"),
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let get = |k: &str| -> Result<ThemeValues, ActionError> {
            plan.snapshot
                .get(k)
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .ok_or_else(|| ActionError::Failed("bad plan".into()))
        };
        let (from, to) = (get("from")?, get("to")?);
        self.backend.write(&to).map_err(ActionError::Failed)?;
        let token = serde_json::to_string(&from).map_err(|e| ActionError::Failed(e.to_string()))?;
        Ok(Executed {
            message: format!("Switched to {} mode", mode_name(to.apps == Some(0))),
            undo_token: Some(token),
        })
    }

    fn plan_undo(&self, token: &str) -> Result<UndoPlan, ActionError> {
        let prev: ThemeValues = serde_json::from_str(token)
            .map_err(|_| ActionError::Failed("the undo information is damaged".into()))?;
        let now = self.backend.read();
        Ok(UndoPlan {
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: format!("Go back to {} mode", mode_name(prev.is_dark())),
                lines: vec!["The colors Windows had before are restored.".into()],
                warnings: Vec::new(),
            },
            fingerprint: format!("theme-undo|{token}|{now:?}"),
        })
    }

    fn undo(
        &self,
        token: &str,
        _plan: &UndoPlan,
        _proof: &Authorized,
    ) -> Result<String, ActionError> {
        let prev: ThemeValues = serde_json::from_str(token)
            .map_err(|_| ActionError::Failed("the undo information is damaged".into()))?;
        self.backend.write(&prev).map_err(ActionError::Failed)?;
        Ok(format!("Back to {} mode", mode_name(prev.is_dark())))
    }
}

// ---------------------------------------------------------- open things

/// Opens one of the fixed Settings pages. The address comes from the fixed
/// table in [`quick::ENTRIES`], never from the result's text.
pub struct PageAction {
    launcher: Arc<dyn Launcher>,
}

impl PageAction {
    pub fn new(launcher: Arc<dyn Launcher>) -> Self {
        Self { launcher }
    }
}

impl Action for PageAction {
    fn id(&self) -> &str {
        SYSTEM_PAGE
    }

    fn label(&self) -> &str {
        "Open Settings"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        is_custom(kind, KIND_SYSTEM)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let entry = target
            .uri
            .as_deref()
            .and_then(quick::entry)
            .and_then(|e| match e.what {
                What::Page(p) => Some((e, p)),
                _ => None,
            })
            .ok_or_else(|| ActionError::NotApplicable {
                action: SYSTEM_PAGE.into(),
            })?;
        Ok(simple_plan(
            Risk::Safe,
            entry.0.title.to_owned(),
            vec![entry.0.subtitle.to_owned()],
            Vec::new(),
            false,
            serde_json::json!({ "page": entry.1 }),
            format!("page|{}", entry.1),
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let page = plan
            .snapshot
            .get("page")
            .and_then(|p| p.as_str())
            // Only a page from the fixed table is ever launched.
            .filter(|p| {
                quick::ENTRIES
                    .iter()
                    .any(|e| matches!(e.what, What::Page(q) if q == *p))
            })
            .ok_or_else(|| ActionError::Failed("bad plan".into()))?;
        self.launcher
            .open(Path::new(page))
            .map_err(|e| ActionError::Failed(format!("could not open Settings: {e}")))?;
        Ok(Executed {
            message: "Opened Settings".into(),
            undo_token: None,
        })
    }
}

/// Opens an `http` or `https` address in the default browser. The address is
/// checked again here; the action log gets only `scheme://host`.
pub struct OpenUrlAction {
    launcher: Arc<dyn Launcher>,
    /// Full addresses planned recently, by hash: the plan (and so the log)
    /// carries only the redacted form.
    stash: Mutex<Vec<(u64, String)>>,
}

impl OpenUrlAction {
    pub fn new(launcher: Arc<dyn Launcher>) -> Self {
        Self {
            launcher,
            stash: Mutex::new(Vec::new()),
        }
    }
}

fn hash_of(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

impl Action for OpenUrlAction {
    fn id(&self) -> &str {
        OPEN_URL
    }

    fn label(&self) -> &str {
        "Open in browser"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        is_custom(kind, KIND_URL)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let typed = target
            .uri
            .as_deref()
            .ok_or_else(|| ActionError::Refused("that result has nothing to open".into()))?;
        let url = quick::parse_web_url(typed).ok_or_else(|| {
            ActionError::Refused("only http and https addresses can be opened".into())
        })?;
        let (redacted, key) = (url.redacted(), hash_of(&url.url));
        {
            let mut s = self.stash.lock().unwrap_or_else(PoisonError::into_inner);
            s.retain(|(k, _)| *k != key);
            s.push((key, url.url.clone()));
            if s.len() > 8 {
                s.remove(0);
            }
        }
        Ok(simple_plan(
            Risk::Safe,
            format!("Open {}", url.url),
            vec!["Opens in your default browser.".into()],
            Vec::new(),
            false,
            serde_json::json!({ "url": redacted, "key": key }),
            format!("url|{redacted}|{key}"),
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let key = plan
            .snapshot
            .get("key")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ActionError::Failed("bad plan".into()))?;
        let url = self
            .stash
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, u)| u.clone())
            .ok_or_else(|| ActionError::Stale("that address expired; search again".into()))?;
        // The last check before anything leaves Lookout.
        if quick::parse_web_url(&url).is_none() {
            return Err(ActionError::Refused(
                "only http and https addresses can be opened".into(),
            ));
        }
        self.launcher
            .open(Path::new(&url))
            .map_err(|e| ActionError::Failed(format!("could not open the browser: {e}")))?;
        let shown = quick::redact(&url);
        Ok(Executed {
            message: format!("Opened {shown}"),
            undo_token: None,
        })
    }
}

// --------------------------------------------------------------- source

/// Offers web addresses and system entries for what was typed. Reads the
/// registry (one value) only for the dark-mode entry; nothing else is touched.
pub struct QuickSource {
    theme: Arc<dyn ThemeBackend>,
}

impl QuickSource {
    pub fn new(theme: Arc<dyn ThemeBackend>) -> Self {
        Self { theme }
    }
}

impl Source for QuickSource {
    fn id(&self) -> &'static str {
        "quick"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.kind.is_some() {
            return;
        }
        let mut items = Vec::new();
        if let Some(url) = quick::parse_web_url(&q.raw) {
            // The id never holds the path or query: ids reach the action log.
            let mut item = Item::new(
                format!("url:{}", url.redacted()),
                Kind::Custom(KIND_URL.into()),
                format!("Open {}", url.url),
                "link",
            );
            item.subtitle = Some("Opens in your default browser".into());
            item.uri = Some(url.url.clone());
            item.score = ANSWER_SCORE;
            item.score_features.match_quality = 1.0;
            item.metadata
                .insert("default_action".into(), OPEN_URL.into());
            items.push(item);
        }
        for (entry, score) in quick::match_entries(&q.raw) {
            let (title, action) = match entry.what {
                What::Theme => (
                    if self.theme.read().is_dark() {
                        "Switch to light mode".to_owned()
                    } else {
                        "Switch to dark mode".to_owned()
                    },
                    SYSTEM_THEME,
                ),
                What::Page(_) => (entry.title.to_owned(), SYSTEM_PAGE),
            };
            let mut item = Item::new(
                format!("system:{}", entry.id),
                Kind::Custom(KIND_SYSTEM.into()),
                title,
                "system",
            );
            item.subtitle = Some(entry.subtitle.to_owned());
            item.uri = Some(entry.id.to_owned());
            item.score = score;
            item.score_features.match_quality = score.min(1.0);
            item.metadata.insert("default_action".into(), action.into());
            items.push(item);
        }
        if !items.is_empty() {
            emit(items);
        }
    }
}

// ------------------------------------------------------ real Windows parts

#[cfg(windows)]
pub use real::WindowsTheme;

#[cfg(windows)]
mod real {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    use super::{ThemeBackend, ThemeValues};

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    const APPS: &str = "AppsUseLightTheme";
    const SYSTEM: &str = "SystemUsesLightTheme";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub struct WindowsTheme;

    struct Key(HKEY);

    impl Key {
        fn open(access: u32) -> Option<Key> {
            let mut key: HKEY = std::ptr::null_mut();
            // SAFETY: `key` is written by the call; the path is NUL-terminated.
            let rc = unsafe {
                RegOpenKeyExW(HKEY_CURRENT_USER, wide(KEY).as_ptr(), 0, access, &mut key)
            };
            (rc == 0).then_some(Key(key))
        }

        fn get(&self, name: &str) -> Option<u32> {
            let (mut ty, mut data, mut len) = (0u32, 0u32, 4u32);
            // SAFETY: `data` has the 4 bytes `len` says; the name is NUL-terminated.
            let rc = unsafe {
                RegQueryValueExW(
                    self.0,
                    wide(name).as_ptr(),
                    std::ptr::null(),
                    &mut ty,
                    (&mut data as *mut u32).cast::<u8>(),
                    &mut len,
                )
            };
            (rc == 0 && ty == REG_DWORD).then_some(data)
        }

        fn put(&self, name: &str, value: Option<u32>) -> Result<(), String> {
            let n = wide(name);
            // SAFETY: the name is NUL-terminated and `v` is the 4 bytes passed.
            let rc = unsafe {
                match value {
                    Some(v) => RegSetValueExW(
                        self.0,
                        n.as_ptr(),
                        0,
                        REG_DWORD,
                        (&v as *const u32).cast::<u8>(),
                        4,
                    ),
                    // A value that was not there is removed again; "not found" is fine.
                    None => RegDeleteValueW(self.0, n.as_ptr()),
                }
            };
            if rc == 0 || (value.is_none() && rc == 2) {
                Ok(())
            } else {
                Err(format!(
                    "could not write the color setting (Windows error {rc})"
                ))
            }
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from RegOpenKeyExW and is closed once.
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    impl ThemeBackend for WindowsTheme {
        fn read(&self) -> ThemeValues {
            match Key::open(KEY_QUERY_VALUE) {
                Some(k) => ThemeValues {
                    apps: k.get(APPS),
                    system: k.get(SYSTEM),
                },
                None => ThemeValues {
                    apps: None,
                    system: None,
                },
            }
        }

        fn write(&self, v: &ThemeValues) -> Result<(), String> {
            let key = Key::open(KEY_SET_VALUE)
                .ok_or_else(|| "could not open the color settings".to_owned())?;
            key.put(APPS, v.apps)?;
            key.put(SYSTEM, v.system)?;
            drop(key);
            // Tell running programs so the change shows at once.
            let area = wide("ImmersiveColorSet");
            let mut result = 0usize;
            // SAFETY: `area` outlives the call; the timeout keeps a hung window from blocking.
            unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    WM_SETTINGCHANGE,
                    0,
                    area.as_ptr() as isize,
                    SMTO_ABORTIFHUNG,
                    1500,
                    &mut result,
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeTheme(Mutex<ThemeValues>);

    impl FakeTheme {
        fn new(apps: Option<u32>, system: Option<u32>) -> Arc<Self> {
            Arc::new(Self(Mutex::new(ThemeValues { apps, system })))
        }
    }

    impl ThemeBackend for FakeTheme {
        fn read(&self) -> ThemeValues {
            *self.0.lock().unwrap()
        }

        fn write(&self, v: &ThemeValues) -> Result<(), String> {
            *self.0.lock().unwrap() = *v;
            Ok(())
        }
    }

    struct Recorder(Mutex<Vec<String>>);

    impl Launcher for Recorder {
        fn open(&self, path: &Path) -> std::io::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(path.to_string_lossy().into_owned());
            Ok(())
        }
    }

    fn target(kind: &str, uri: &str) -> Target {
        Target {
            item_id: format!("{kind}:{uri}"),
            kind: Kind::Custom(kind.into()),
            uri: Some(uri.into()),
            port: None,
            origin: None,
        }
    }

    #[test]
    fn dark_mode_switches_both_values_and_undo_restores_the_previous_ones() {
        // Mixed state with one value unset: undo must put back exactly this.
        let theme = FakeTheme::new(Some(1), None);
        let act = ThemeAction::new(theme.clone());
        let t = target(KIND_SYSTEM, "dark_mode");
        let plan = act.plan(&t).unwrap();
        assert_eq!(plan.preview.title, "Switch to dark mode");
        assert!(plan.undoable);
        let done = act.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(
            theme.read(),
            ThemeValues {
                apps: Some(0),
                system: Some(0)
            }
        );
        let token = done.undo_token.unwrap();
        let up = act.plan_undo(&token).unwrap();
        assert_eq!(up.preview.title, "Go back to light mode");
        act.undo(&token, &up, &Authorized::for_tests()).unwrap();
        assert_eq!(
            theme.read(),
            ThemeValues {
                apps: Some(1),
                system: None
            }
        );
    }

    #[test]
    fn dark_mode_from_dark_offers_light_and_undo_returns_to_dark() {
        let theme = FakeTheme::new(Some(0), Some(0));
        let act = ThemeAction::new(theme.clone());
        let plan = act.plan(&target(KIND_SYSTEM, "dark_mode")).unwrap();
        assert_eq!(plan.preview.title, "Switch to light mode");
        let done = act.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(theme.read().apps, Some(1));
        let token = done.undo_token.unwrap();
        let up = act.plan_undo(&token).unwrap();
        act.undo(&token, &up, &Authorized::for_tests()).unwrap();
        assert_eq!(theme.read().apps, Some(0));
        assert_eq!(theme.read().system, Some(0));
        assert!(act.plan_undo("not json").is_err());
    }

    #[test]
    fn pages_open_only_from_the_fixed_table() {
        let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
        let act = PageAction::new(rec.clone());
        let plan = act.plan(&target(KIND_SYSTEM, "display")).unwrap();
        act.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(rec.0.lock().unwrap().as_slice(), ["ms-settings:display"]);
        assert!(act
            .plan(&target(KIND_SYSTEM, "ms-settings:display"))
            .is_err());
        assert!(act.plan(&target(KIND_SYSTEM, "dark_mode")).is_err());
        let mut forged = plan.clone();
        forged.snapshot = serde_json::json!({ "page": "calc.exe" });
        assert!(act.execute(&forged, &Authorized::for_tests()).is_err());
        assert_eq!(rec.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn urls_open_and_the_plan_never_carries_the_query() {
        let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
        let act = OpenUrlAction::new(rec.clone());
        let t = target(KIND_URL, "https://x.com/a?token=secret#f");
        let plan = act.plan(&t).unwrap();
        let logged = format!("{} {}", plan.snapshot, plan.fingerprint);
        assert!(!logged.contains("secret") && !logged.contains("/a"));
        act.execute(&plan, &Authorized::for_tests()).unwrap();
        assert_eq!(
            rec.0.lock().unwrap().as_slice(),
            ["https://x.com/a?token=secret#f"]
        );
    }

    #[test]
    fn dangerous_schemes_are_refused_at_plan_and_at_execute() {
        let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
        let act = OpenUrlAction::new(rec.clone());
        for bad in [
            "javascript:alert(1)",
            "file:///C:/Windows/notepad.exe",
            "data:text/html,x",
            "ms-settings:display",
            "calc.exe",
        ] {
            assert!(act.plan(&target(KIND_URL, bad)).is_err(), "{bad}");
        }
        let forged = simple_plan(
            Risk::Safe,
            String::new(),
            vec![],
            vec![],
            false,
            serde_json::json!({"url": "x", "key": 1}),
            String::new(),
        );
        assert!(act.execute(&forged, &Authorized::for_tests()).is_err());
        assert!(rec.0.lock().unwrap().is_empty());
    }

    fn query(src: &QuickSource, text: &str) -> Vec<Item> {
        let ctx = QueryContext::new(1, Arc::new(std::sync::atomic::AtomicU64::new(1)));
        let q = crate::query::parse(text);
        let mut out = Vec::new();
        src.query(&q, &ctx, &mut |items| out = items);
        out
    }

    #[test]
    fn the_source_labels_system_results_and_adds_a_link_result() {
        let src = QuickSource::new(FakeTheme::new(Some(1), Some(1)));
        let items = query(&src, "dark mode");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Switch to dark mode");
        assert_eq!(items[0].source, "system");
        assert_eq!(items[0].metadata["default_action"], SYSTEM_THEME);
        let dark = QuickSource::new(FakeTheme::new(Some(0), Some(0)));
        assert_eq!(query(&dark, "dark mode")[0].title, "Switch to light mode");

        let wifi = query(&src, "wifi");
        assert_eq!(wifi[0].metadata["default_action"], SYSTEM_PAGE);

        let link = query(&src, "apple.com");
        assert_eq!(link[0].title, "Open https://apple.com");
        assert_eq!(link[0].id, "url:https://apple.com");
        assert_eq!(link[0].metadata["default_action"], OPEN_URL);
        assert!(query(&src, "javascript:alert(1)").is_empty());
        assert!(query(&src, "quarterly report").is_empty());
        assert!(query(&src, "kind:file dark mode").is_empty());
    }

    #[test]
    fn system_results_rank_below_an_answer_and_a_link() {
        let src = QuickSource::new(FakeTheme::new(Some(1), Some(1)));
        let link = query(&src, "apple.com");
        let sys = query(&src, "wifi");
        assert!(link[0].score > sys[0].score);
        assert!(sys[0].score < ANSWER_SCORE);
    }
}
