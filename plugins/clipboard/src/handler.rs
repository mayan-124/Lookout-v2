//! The JSON-RPC side of the clipboard plugin.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use lookout_core::crypto::Key;
use lookout_core::plugins::sdk::{items_changed, Handler};
use serde_json::{json, Value};

use crate::monitor::{app_key, lock, now_secs, run, ClipboardIo, Read, Shared};
use crate::smart::{self, Kind};
use crate::store::{Clip, Limits, Store};
use crate::text_actions;

const DEFAULT_MAX_CLIPS: u64 = 100;
const DEFAULT_RETENTION_HOURS: u64 = 168;

pub struct Clipboard {
    shared: Arc<Shared>,
    io: Arc<dyn ClipboardIo>,
    /// Start the background watcher on `initialize` (tests turn it off).
    watch: bool,
    /// Enter pastes (true) or only copies (false).
    paste_default: bool,
}

/// `default_action` from the config: "copy" or (anything else) "paste".
fn paste_default_from(config: &Value) -> bool {
    !config["default_action"]
        .as_str()
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("copy"))
}

fn limits_from(config: &Value) -> Limits {
    let num = |k: &str, default: u64, max: u64| {
        config
            .get(k)
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .unwrap_or(default)
            .clamp(1, max)
    };
    Limits {
        max_clips: usize::try_from(num("max_clips", DEFAULT_MAX_CLIPS, 5000)).unwrap_or(100),
        retention_secs: i64::try_from(num("retention_hours", DEFAULT_RETENTION_HOURS, 8760) * 3600)
            .unwrap_or(i64::MAX),
    }
}

/// `exclude_apps` from the config: a list, or one text split at commas/semicolons/lines.
fn exclude_from(config: &Value) -> Vec<String> {
    let raw: Vec<String> = match &config["exclude_apps"] {
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Value::String(s) => s.split([',', ';', '\n']).map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    raw.iter()
        .map(|a| app_key(a))
        .filter(|a| !a.is_empty())
        .collect()
}

/// "chrome" -> "Chrome".
fn display_app(app: &str) -> String {
    let mut c = app.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Control characters and stray line-ending styles removed; ends trimmed.
fn plain(text: &str) -> String {
    let kept: String = text
        .replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    kept.trim().to_owned()
}

fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(max).collect()
}

fn ago(now: i64, ts: i64) -> String {
    let s = (now - ts).max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

fn item_for(clip: &Clip, now: i64, paste: bool) -> Value {
    let mut parts: Vec<String> = Vec::new();
    if clip.pinned {
        parts.push("Pinned".into());
    }
    if clip.masked {
        parts.push("contains masked secrets".into());
    }
    if let Some(l) = smart::detect(&clip.text).label() {
        parts.push(l.into());
    }
    if let Some(app) = clip.source.as_deref().filter(|a| !a.contains("lookout")) {
        parts.push(display_app(app));
    }
    parts.push(ago(now, clip.ts));
    json!({
        "id": clip.id.to_string(),
        "title": one_line(&clip.text, 90),
        "subtitle": parts.join(" · "),
        "kind": "clipboard",
        "action": if paste { "paste_clip" } else { "copy_clip" },
        "params": {"clip_id": clip.id},
        "score": if clip.pinned { 5.5 } else { 5.0 },
    })
}

fn action(id: &str, label: &str) -> Value {
    json!({"id": id, "label": label, "risk": "safe"})
}

/// What can be done with one stored clip: Enter is the first.
fn clip_actions(clip: &Clip, paste: bool) -> Vec<Value> {
    let mut a = if paste {
        vec![
            action("paste_clip", "Paste"),
            action("copy_clip", "Copy only"),
        ]
    } else {
        vec![action("copy_clip", "Copy"), action("paste_clip", "Paste")]
    };
    a.push(action("paste_plain", "Paste as plain text"));
    match smart::detect(&clip.text) {
        Kind::Url => a.push(action("open_link", "Open link")),
        Kind::Email => a.push(action("open_email", "Write an email")),
        Kind::Color => a.push(action("preview_color", "Preview color")),
        Kind::Json => a.push(action("copy_pretty", "Copy pretty-printed")),
        _ => {}
    }
    a.push(if clip.pinned {
        action("unpin_clip", "Unpin")
    } else {
        action("pin_clip", "Pin")
    });
    a
}

/// The clip a plugin item id such as `plugin:<id>:7` names.
fn clip_id_of(item_id: &str) -> Option<u64> {
    item_id.rsplit(':').next()?.parse().ok()
}

impl Clipboard {
    pub fn new(io: Arc<dyn ClipboardIo>) -> Self {
        Self {
            shared: Arc::new(Shared {
                store: Mutex::new(Store::in_memory()),
                limits: Mutex::new(limits_from(&Value::Null)),
                paused: std::sync::atomic::AtomicBool::new(false),
                stop: std::sync::atomic::AtomicBool::new(false),
                exclude: Mutex::new(Vec::new()),
                ignore_next: std::sync::atomic::AtomicBool::new(false),
                last_seq: Mutex::new(0),
            }),
            io,
            watch: std::env::var_os("LOOKOUT_CLIPBOARD_NO_WATCH").is_none(),
            paste_default: true,
        }
    }

    /// For tests: use `store` and do not start the watcher.
    pub fn with_store(io: Arc<dyn ClipboardIo>, store: Store) -> Self {
        let mut c = Self::new(io);
        *lock(&c.shared.store) = store;
        c.watch = false;
        c
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    fn initialize(&mut self, params: &Value) -> Result<Value, String> {
        *lock(&self.shared.limits) = limits_from(&params["config"]);
        *lock(&self.shared.exclude) = exclude_from(&params["config"]);
        self.paste_default = paste_default_from(&params["config"]);
        *lock(&self.shared.last_seq) = self.io.sequence();
        self.shared.paused.store(
            params["paused"].as_bool().unwrap_or(false),
            Ordering::Release,
        );
        let key = params["key"]
            .as_str()
            .and_then(Key::from_hex)
            .ok_or("Lookout gave no encryption key")?;
        let data_dir = params["data_dir"]
            .as_str()
            .ok_or("Lookout gave no data folder")?;
        *lock(&self.shared.store) = Store::open(&Path::new(data_dir).join("clips.bin"), key);
        {
            let limits = *lock(&self.shared.limits);
            let mut store = lock(&self.shared.store);
            if store.prune(now_secs(), limits) {
                let _ = store.save();
            }
        }
        if self.watch {
            let (io, shared) = (self.io.clone(), self.shared.clone());
            std::thread::Builder::new()
                .name("clipboard-watch".into())
                .spawn(move || run(io, shared, items_changed))
                .map_err(|e| e.to_string())?;
        }
        Ok(json!({"protocol": 1}))
    }

    fn query(&self, params: &Value) -> Value {
        let rest = params["rest"].as_str().unwrap_or("").trim().to_lowercase();
        let now = now_secs();
        if self.watch {
            crate::monitor::catch_up(self.io.as_ref(), &self.shared);
        }
        let store = lock(&self.shared.store);
        if rest == "clear" {
            return json!({"items": [{
                "id": "clear",
                "title": format!("Delete clipboard history ({} items)", store.len()),
                "subtitle": "Removes every stored clip",
                "kind": "clipboard_clear",
                "action": "clear_history",
                "score": 5.0,
            }]});
        }
        let mut items: Vec<Value> = Vec::new();
        if rest.len() >= 3 && "ignore next copy private".contains(&rest) {
            items.push(json!({
                "id": "ignore_next",
                "title": "Ignore next copy",
                "subtitle": "The next text you copy is not recorded",
                "kind": "clipboard_action",
                "action": "ignore_next",
                "score": 6.0,
            }));
        }
        items.extend(
            text_actions::matching(&rest)
                .into_iter()
                .map(|(id, label)| {
                    json!({
                        "id": id,
                        "title": label,
                        "subtitle": "Applies to the text on the clipboard now",
                        "kind": "clipboard_action",
                        "action": id,
                        "score": 6.0,
                    })
                }),
        );
        items.extend(
            store
                .listing()
                .filter(|c| {
                    rest.is_empty()
                        || c.text.to_lowercase().contains(&rest)
                        || c.source.as_deref().is_some_and(|a| a.contains(&rest))
                })
                .take(10)
                .map(|c| item_for(c, now, self.paste_default)),
        );
        json!({ "items": items })
    }

    /// Another program (clipboard history, sync) often has the clipboard open for
    /// a moment right after a copy: wait for it to be free, and for text that is
    /// not there yet, rather than failing on the first try.
    fn read_with_retry(&self) -> Read {
        let mut r = self.io.read();
        for i in 0..40 {
            let wait = match r {
                Read::Busy => true,
                Read::NoText => i < 10,
                _ => false,
            };
            if !wait {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
            r = self.io.read();
        }
        r
    }

    fn clip_of(&self, params: &Value) -> Result<Clip, String> {
        let id = params["params"]["clip_id"]
            .as_u64()
            .ok_or("no clip was named")?;
        lock(&self.shared.store)
            .get(id)
            .cloned()
            .ok_or_else(|| "that clip is no longer in the history".to_owned())
    }
}

impl Handler for Clipboard {
    fn notify(&mut self, method: &str, params: Value) {
        if method == "pause_changed" {
            self.shared.paused.store(
                params["paused"].as_bool().unwrap_or(false),
                Ordering::Release,
            );
        }
    }

    fn handle(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "initialize" => self.initialize(&params),
            "query" => Ok(self.query(&params)),
            "get_actions" => {
                let id = params["items"][0]["id"].as_str().unwrap_or("");
                if id.ends_with(":ignore_next") {
                    Ok(json!({"actions": [action("ignore_next", "Ignore next copy")]}))
                } else if let Some(a) = text_actions::ACTIONS
                    .iter()
                    .find(|a| id.ends_with(&format!(":{}", a.0)))
                {
                    Ok(json!({"actions": [action(a.0, a.1)]}))
                } else if id.ends_with(":clear") {
                    Ok(
                        json!({"actions": [{"id": "clear_history", "label": "Delete clipboard history", "risk": "confirm"}]}),
                    )
                } else {
                    let clip =
                        clip_id_of(id).and_then(|n| lock(&self.shared.store).get(n).cloned());
                    Ok(match clip {
                        Some(c) => json!({"actions": clip_actions(&c, self.paste_default)}),
                        None => json!({"actions": [action("copy_clip", "Copy only")]}),
                    })
                }
            }
            "preview" => match params["action_id"].as_str() {
                Some(
                    a @ ("copy_clip" | "paste_clip" | "paste_plain" | "open_link" | "open_email"
                    | "preview_color" | "copy_pretty" | "pin_clip" | "unpin_clip"),
                ) => {
                    let clip = self.clip_of(&params)?;
                    let title = match a {
                        "paste_clip" => "Paste this clip into the app you were using",
                        "paste_plain" => "Paste this clip as plain text",
                        "open_link" => "Open this link in your browser",
                        "open_email" => "Start an email to this address",
                        "preview_color" => "Show the values of this color",
                        "copy_pretty" => "Copy this clip as pretty-printed JSON",
                        "pin_clip" => "Pin this clip so it is never removed",
                        "unpin_clip" => "Unpin this clip",
                        _ => "Copy this clip to the clipboard",
                    };
                    let lines: Vec<String> = clip
                        .text
                        .lines()
                        .take(8)
                        .map(|l| one_line(l, 200))
                        .collect();
                    let warnings = if clip.masked {
                        vec!["Secrets in this clip were masked when it was stored. Only the masked text can be copied.".to_owned()]
                    } else {
                        vec![]
                    };
                    Ok(json!({"title": title, "lines": lines,
                              "warnings": warnings, "undoable": false, "risk": "safe"}))
                }
                Some(id) if text_actions::label(id).is_some() => {
                    Ok(json!({"title": text_actions::label(id).unwrap_or(""),
                           "lines": ["Changes only the text on the clipboard."],
                           "warnings": [], "undoable": false, "risk": "safe"}))
                }
                Some("ignore_next") => Ok(
                    json!({"title": "Ignore the next copy", "lines": ["The next text you copy is not recorded."],
                           "warnings": [], "undoable": false, "risk": "safe"}),
                ),
                Some("clear_history") => {
                    let n = lock(&self.shared.store).len();
                    Ok(
                        json!({"title": format!("Delete all {n} clipboard history items"),
                              "lines": [], "warnings": ["This cannot be undone."],
                              "undoable": false, "risk": "confirm"}),
                    )
                }
                _ => Err("unknown action".into()),
            },
            "execute" => match params["action_id"].as_str() {
                Some("copy_clip") => {
                    let clip = self.clip_of(&params)?;
                    self.io.write_text(&clip.text)?;
                    Ok(json!({"message": "Copied to the clipboard"}))
                }
                Some(a @ ("paste_clip" | "paste_plain")) => {
                    let clip = self.clip_of(&params)?;
                    let text = if a == "paste_plain" {
                        plain(&clip.text)
                    } else {
                        clip.text
                    };
                    self.io.write_text(&text)?;
                    Ok(match self.io.paste() {
                        Ok(()) => json!({"message": "Pasted"}),
                        Err(_) => json!({"message": "Copied (nothing to paste into)"}),
                    })
                }
                Some("open_link") => {
                    let url = self.clip_of(&params)?.text.trim().to_owned();
                    if !smart::is_url(&url) {
                        return Err("This clip is not a web link".into());
                    }
                    self.io.open_url(&url)?;
                    Ok(json!({"message": "Opened the link"}))
                }
                Some("open_email") => {
                    let addr = self.clip_of(&params)?.text.trim().to_owned();
                    if !smart::is_email(&addr) {
                        return Err("This clip is not an email address".into());
                    }
                    self.io.open_url(&format!("mailto:{addr}"))?;
                    Ok(json!({"message": "Started an email"}))
                }
                Some("preview_color") => {
                    let clip = self.clip_of(&params)?;
                    let d = smart::describe_color(&clip.text).ok_or("This clip is not a color")?;
                    Ok(json!({"message": d}))
                }
                Some("copy_pretty") => {
                    let clip = self.clip_of(&params)?;
                    self.io
                        .write_text(&text_actions::apply("txt_json", &clip.text)?)?;
                    Ok(json!({"message": "Copied pretty-printed JSON"}))
                }
                Some(a @ ("pin_clip" | "unpin_clip")) => {
                    let id = params["params"]["clip_id"]
                        .as_u64()
                        .ok_or("no clip was named")?;
                    let mut store = lock(&self.shared.store);
                    if !store.set_pinned(id, a == "pin_clip") {
                        return Err("that clip is no longer in the history".into());
                    }
                    store.save().map_err(|e| e.to_string())?;
                    Ok(json!({"message": if a == "pin_clip" { "Pinned" } else { "Unpinned" }}))
                }
                Some("ignore_next") => {
                    self.shared.ignore_next.store(true, Ordering::Release);
                    Ok(json!({"message": "The next copy will not be recorded"}))
                }
                Some(id) if text_actions::label(id).is_some() => {
                    let Read::Text(text) = self.read_with_retry() else {
                        return Err("There is no text on the clipboard".to_owned());
                    };
                    let result = text_actions::apply(id, &text)?;
                    self.io.write_text(&result)?;
                    Ok(
                        json!({"message": format!("{}: done", text_actions::label(id).unwrap_or(""))}),
                    )
                }
                Some("clear_history") => {
                    let n = lock(&self.shared.store)
                        .clear()
                        .map_err(|e| e.to_string())?;
                    Ok(json!({"message": format!("Deleted {n} clipboard history items")}))
                }
                _ => Err("unknown action".into()),
            },
            "undo" => Err("nothing to undo".into()),
            other => Err(format!("unknown method {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Mutex<Vec<String>>);
    impl ClipboardIo for Fake {
        fn sequence(&self) -> u32 {
            0
        }
        fn read(&self) -> Read {
            Read::NoText
        }
        fn write_text(&self, t: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(t.to_owned());
            Ok(())
        }
        fn paste(&self) -> Result<(), String> {
            self.0.lock().unwrap().push("<PASTE>".to_owned());
            Ok(())
        }
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("<OPEN {url}>"));
            Ok(())
        }
    }

    const L: Limits = Limits {
        max_clips: 10,
        retention_secs: 1_000_000_000_000,
    };

    fn setup() -> (Clipboard, Arc<Fake>) {
        let io = Arc::new(Fake(Mutex::new(vec![])));
        let mut store = Store::in_memory();
        store.add("first clip", now_secs() - 120, L);
        store.add(
            "token ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            now_secs(),
            L,
        );
        (Clipboard::with_store(io.clone(), store), io)
    }

    #[test]
    fn query_filters_and_never_shows_a_secret() {
        let (mut c, _) = setup();
        let all = c.handle("query", json!({"rest": ""})).unwrap();
        assert_eq!(all["items"].as_array().unwrap().len(), 2);
        let text = all.to_string();
        assert!(!text.contains("ghp_abc") && text.contains("[masked]"));
        let some = c.handle("query", json!({"rest": "FIRST"})).unwrap();
        assert_eq!(some["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn copy_puts_the_stored_text_back_and_a_masked_clip_only_in_masked_form() {
        let (mut c, io) = setup();
        let items = c.handle("query", json!({"rest": ""})).unwrap();
        let masked = &items["items"][0];
        let p = json!({"action_id": "copy_clip", "params": masked["params"]});
        let preview = c.handle("preview", p.clone()).unwrap();
        assert!(!preview["warnings"].as_array().unwrap().is_empty());
        c.handle("execute", p).unwrap();
        let written = io.0.lock().unwrap().clone();
        assert_eq!(written.len(), 1);
        assert!(written[0].contains("[masked]") && !written[0].contains("ghp_abc"));
    }

    #[test]
    fn clearing_removes_everything_and_asks_for_confirmation() {
        let (mut c, _) = setup();
        let q = c.handle("query", json!({"rest": "clear"})).unwrap();
        assert_eq!(q["items"][0]["action"], "clear_history");
        let p = json!({"action_id": "clear_history", "params": null});
        assert_eq!(c.handle("preview", p.clone()).unwrap()["risk"], "confirm");
        c.handle("execute", p).unwrap();
        assert!(lock(&c.shared.store).is_empty());
    }

    #[test]
    fn limits_come_from_the_config_and_are_clamped() {
        let l = limits_from(&json!({"max_clips": 5, "retention_hours": "2"}));
        assert_eq!((l.max_clips, l.retention_secs), (5, 7200));
        let l = limits_from(&json!({"max_clips": 0, "retention_hours": 99999999}));
        assert_eq!((l.max_clips, l.retention_secs), (1, 8760 * 3600));
    }

    #[test]
    fn a_gone_clip_is_an_error_not_a_crash() {
        let (mut c, _) = setup();
        let p = json!({"action_id": "copy_clip", "params": {"clip_id": 999}});
        assert!(c.handle("preview", p).is_err());
    }

    fn add(c: &Clipboard, text: &str, app: Option<&str>, age: i64) -> u64 {
        let mut store = lock(&c.shared.store);
        store.add_from(text, app, now_secs() - age, L);
        let id = store.listing().find(|x| x.text == text).unwrap().id;
        id
    }

    fn run(c: &mut Clipboard, method: &str, action: &str, id: u64) -> Result<Value, String> {
        c.handle(
            method,
            json!({"action_id": action, "params": {"clip_id": id}}),
        )
    }

    fn titles(v: &Value) -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_owned())
            .collect()
    }

    fn action_ids(c: &mut Clipboard, id: u64) -> Vec<String> {
        let r = c
            .handle(
                "get_actions",
                json!({"items": [{"id": format!("plugin:x:{id}")}]}),
            )
            .unwrap();
        r["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn pinned_clips_show_first_and_say_so() {
        let (mut c, _) = setup();
        let id = add(&c, "old but precious", None, 5000);
        run(&mut c, "execute", "pin_clip", id).unwrap();
        let q = c.handle("query", json!({"rest": ""})).unwrap();
        assert_eq!(titles(&q)[0], "old but precious");
        assert!(q["items"][0]["subtitle"]
            .as_str()
            .unwrap()
            .starts_with("Pinned"));
        assert!(action_ids(&mut c, id).contains(&"unpin_clip".to_owned()));
        run(&mut c, "execute", "unpin_clip", id).unwrap();
        assert!(action_ids(&mut c, id).contains(&"pin_clip".to_owned()));
        assert!(run(&mut c, "execute", "pin_clip", 999).is_err());
    }

    #[test]
    fn subtitle_shows_app_and_time_and_the_app_name_filters() {
        let (mut c, _) = setup();
        add(&c, "from the browser", Some("chrome"), 300);
        add(&c, "from the editor", Some("code"), 10);
        let q = c.handle("query", json!({"rest": ""})).unwrap();
        let sub = q["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["title"] == "from the browser")
            .unwrap()["subtitle"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(sub, "Chrome · 5 min ago");
        let q = c.handle("query", json!({"rest": "chrome"})).unwrap();
        assert_eq!(titles(&q), ["from the browser"]);
    }

    #[test]
    fn paste_pastes_and_copy_only_does_not() {
        let (mut c, io) = setup();
        let id = add(&c, "  text\u{7} with junk  ", None, 1);
        assert_eq!(
            &action_ids(&mut c, id)[..3],
            ["paste_clip", "copy_clip", "paste_plain"]
        );
        let q = c.handle("query", json!({"rest": "junk"})).unwrap();
        assert_eq!(q["items"][0]["action"], "paste_clip");
        run(&mut c, "execute", "copy_clip", id).unwrap();
        assert!(!io.0.lock().unwrap().iter().any(|e| e == "<PASTE>"));
        run(&mut c, "execute", "paste_clip", id).unwrap();
        run(&mut c, "execute", "paste_plain", id).unwrap();
        let log = io.0.lock().unwrap().clone();
        assert_eq!(log[0], log[1]);
        assert_eq!(log[2], "<PASTE>");
        assert_eq!(log[3], "text with junk");
        assert_eq!(log[4], "<PASTE>");
    }

    #[test]
    fn smart_types_get_a_label_and_one_extra_action() {
        let (mut c, io) = setup();
        let url = add(&c, "https://example.com/x", None, 1);
        let mail = add(&c, "me@example.com", None, 1);
        let color = add(&c, "#ff8800", None, 1);
        let json_clip = add(&c, r#"{"a":1}"#, None, 1);
        let plain_clip = add(&c, "just words", None, 1);
        assert!(action_ids(&mut c, url).contains(&"open_link".to_owned()));
        assert!(action_ids(&mut c, mail).contains(&"open_email".to_owned()));
        assert!(action_ids(&mut c, color).contains(&"preview_color".to_owned()));
        assert!(action_ids(&mut c, json_clip).contains(&"copy_pretty".to_owned()));
        assert_eq!(action_ids(&mut c, plain_clip).len(), 4);
        let q = c.handle("query", json!({"rest": "example.com/x"})).unwrap();
        assert!(q["items"][0]["subtitle"]
            .as_str()
            .unwrap()
            .starts_with("Link"));
        run(&mut c, "execute", "open_link", url).unwrap();
        run(&mut c, "execute", "open_email", mail).unwrap();
        let m = run(&mut c, "execute", "preview_color", color).unwrap();
        assert!(m["message"].as_str().unwrap().contains("rgb(255, 136, 0)"));
        run(&mut c, "execute", "copy_pretty", json_clip).unwrap();
        let log = io.0.lock().unwrap().clone();
        assert_eq!(log[0], "<OPEN https://example.com/x>");
        assert_eq!(log[1], "<OPEN mailto:me@example.com>");
        assert_eq!(log[2], "{\n  \"a\": 1\n}");
        // A clip of the wrong type is refused rather than opened.
        assert!(run(&mut c, "execute", "open_link", plain_clip).is_err());
    }

    #[test]
    fn new_text_actions_are_found_by_typing() {
        let (mut c, _) = setup();
        for (word, id) in [
            ("sort", "txt_sort"),
            ("base64", "txt_b64_enc"),
            ("camel", "txt_camel"),
        ] {
            let q = c.handle("query", json!({"rest": word})).unwrap();
            assert!(
                q["items"].as_array().unwrap().iter().any(|i| i["id"] == id),
                "{word}"
            );
        }
    }

    #[test]
    fn exclude_list_is_read_from_the_config_in_either_form() {
        let a = exclude_from(&json!({"exclude_apps": "KeePass.exe, bitwarden;\n 1Password"}));
        assert_eq!(a, ["keepass", "bitwarden", "1password"]);
        let b = exclude_from(&json!({"exclude_apps": ["Chrome.EXE"]}));
        assert_eq!(b, ["chrome"]);
        assert!(exclude_from(&json!({})).is_empty());
    }

    #[test]
    fn ignore_next_copy_is_offered_and_arms_the_monitor() {
        let (mut c, _) = setup();
        let q = c.handle("query", json!({"rest": "ignore"})).unwrap();
        assert_eq!(q["items"][0]["action"], "ignore_next");
        c.handle(
            "execute",
            json!({"action_id": "ignore_next", "params": null}),
        )
        .unwrap();
        assert!(c.shared.ignore_next.load(Ordering::Acquire));
        let r = c
            .handle(
                "get_actions",
                json!({"items": [{"id": "plugin:x:ignore_next"}]}),
            )
            .unwrap();
        assert_eq!(r["actions"][0]["id"], "ignore_next");
    }

    /// A clipboard where there is no window to paste into.
    struct NoTarget(Mutex<Vec<String>>);
    impl ClipboardIo for NoTarget {
        fn sequence(&self) -> u32 {
            0
        }
        fn read(&self) -> Read {
            Read::NoText
        }
        fn write_text(&self, t: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(t.to_owned());
            Ok(())
        }
    }

    #[test]
    fn with_no_window_to_paste_into_the_text_is_only_copied() {
        let io = Arc::new(NoTarget(Mutex::new(vec![])));
        let mut c = Clipboard::with_store(io.clone(), Store::in_memory());
        let id = add(&c, "hello", None, 1);
        let r = run(&mut c, "execute", "paste_clip", id).unwrap();
        assert_eq!(r["message"], "Copied (nothing to paste into)");
        assert_eq!(io.0.lock().unwrap().as_slice(), ["hello"]);
    }

    #[test]
    fn default_action_decides_what_enter_does() {
        assert!(paste_default_from(&json!({})));
        assert!(paste_default_from(&json!({"default_action": "paste"})));
        assert!(paste_default_from(&json!({"default_action": "nonsense"})));
        assert!(!paste_default_from(&json!({"default_action": " Copy "})));
        let (mut c, _) = setup();
        c.paste_default = false;
        let q = c.handle("query", json!({"rest": ""})).unwrap();
        assert_eq!(q["items"][0]["action"], "copy_clip");
        let id = add(&c, "x y", None, 1);
        assert_eq!(action_ids(&mut c, id)[..2], ["copy_clip", "paste_clip"]);
    }

    #[test]
    fn lookout_is_never_shown_as_the_source() {
        let (c, _) = setup();
        add(&c, "own write", Some("lookout-clipboard"), 1);
        add(&c, "other", Some("chrome"), 1);
        let q = c.query(&json!({"rest": ""}));
        let sub = |t: &str| {
            q["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["title"] == t)
                .unwrap()["subtitle"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert!(!sub("own write").to_lowercase().contains("lookout"));
        assert!(sub("other").starts_with("Chrome"));
    }
}
