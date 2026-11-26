//! Timers: the typed forms (`timer 10 min`, `90s timer`, `timer 1h30m tea`),
//! the encrypted list of running timers, and the one background thread that
//! sleeps until the nearest end time and shows a notification. There is no
//! polling: with no timer the thread waits without a timeout, with timers it
//! waits for the nearest one (at most 30 s at a time, so a resume from sleep
//! is noticed; end times are wall-clock). Pausing indexing or hiding the window
//! does not touch it.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::{Local, TimeZone};

use crate::actions::{Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target};
use crate::item::{Item, Kind};
use crate::notify::Notifier;
use crate::query::ParsedQuery;
use crate::securedb::{DbError, SecureDb};
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};

pub const MIN_SECS: u64 = 1;
pub const MAX_SECS: u64 = 24 * 3600;
pub const MAX_ACTIVE: usize = 10;
const MAX_LABEL: usize = 60;

pub const START: &str = "timer_start";
pub const CANCEL: &str = "timer_cancel";
/// `Kind::Custom` names of timer results.
pub const KIND_NEW: &str = "timer_new";
pub const KIND_RUNNING: &str = "timer";
pub const KIND_INFO: &str = "timer_info";

// --------------------------------------------------------------- parser

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// The text is not a timer request; search as usual.
    NotTimer,
    Timer {
        secs: u64,
        label: String,
    },
    /// Looks like a timer request but cannot be started; the text says why.
    Invalid(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    H,
    M,
    S,
}

fn unit_of(word: &str) -> Option<Unit> {
    match word {
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(Unit::H),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(Unit::M),
        "s" | "sec" | "secs" | "second" | "seconds" => Some(Unit::S),
        _ => None,
    }
}

fn clean_label(words: &[&str]) -> String {
    let joined = crate::quick::strip_controls(&words.join(" "));
    joined.trim().chars().take(MAX_LABEL).collect()
}

/// Reads the duration at the start of `words`. Returns the seconds (before
/// limits are applied) and how many words were used. `None` if the words do
/// not start with a number or the duration is malformed.
fn read_duration(words: &[&str]) -> Option<(f64, usize)> {
    // Split "1h30m" into number and letter runs, remembering each run's word.
    let mut spans: Vec<(usize, String)> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let mut cur = String::new();
        let mut digit: Option<bool> = None;
        for c in w.to_lowercase().chars() {
            let is_d = c.is_ascii_digit() || c == '.';
            if digit.is_some_and(|d| d != is_d) {
                spans.push((i, std::mem::take(&mut cur)));
            }
            digit = Some(is_d);
            cur.push(c);
        }
        if !cur.is_empty() {
            spans.push((i, cur));
        }
        if spans.len() > 12 {
            return None;
        }
    }
    let (mut total, mut used, mut idx) = (0.0f64, 0, 0);
    let mut last: Option<Unit> = None;
    while let Some((word_i, tok)) = spans.get(idx) {
        if !tok.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
            break;
        }
        if tok.len() > 9 || tok.matches('.').count() > 1 || tok == "." {
            return None;
        }
        let n: f64 = tok.parse().ok()?;
        let named = spans
            .get(idx + 1)
            .and_then(|(w, u)| unit_of(u).map(|u| (*w, u)));
        let (unit, end_word, step) = match named {
            Some((w, u)) => (u, w + 1, 2),
            None => {
                // A bare number: minutes alone, or the next smaller unit after "1h" / "5m".
                let u = match last {
                    None | Some(Unit::H) => Unit::M,
                    Some(Unit::M) => Unit::S,
                    Some(Unit::S) => break,
                };
                (u, word_i + 1, 1)
            }
        };
        // Units go down: no "5m 1h", no repeats.
        if last.is_some_and(|l| rank(l) <= rank(unit)) {
            return None;
        }
        total += n * mult(unit);
        last = Some(unit);
        used = end_word;
        idx += step;
        if named.is_none() {
            // A bare number ends the duration ("25 tea").
            break;
        }
    }
    (used > 0).then_some((total, used))
}

fn rank(u: Unit) -> u8 {
    match u {
        Unit::H => 2,
        Unit::M => 1,
        Unit::S => 0,
    }
}

fn mult(u: Unit) -> f64 {
    match u {
        Unit::H => 3600.0,
        Unit::M => 60.0,
        Unit::S => 1.0,
    }
}

fn finish(total: f64, label: String) -> Parsed {
    if !total.is_finite() || total < 0.5 {
        return Parsed::Invalid("A timer must be at least 1 second.".into());
    }
    let secs = total.round() as u64;
    if secs > MAX_SECS {
        return Parsed::Invalid("A timer can be at most 24 hours.".into());
    }
    Parsed::Timer {
        secs: secs.max(MIN_SECS),
        label,
    }
}

/// Reads typed text as a timer request.
///
/// Forms: `timer 10 min`, `timer 90s`, `timer 1h30m`, `timer 25` (minutes),
/// `10 min timer`, each with an optional label (`timer 5 min tea`).
pub fn parse(text: &str) -> Parsed {
    let cleaned = crate::quick::strip_controls(text);
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.len() < 2 {
        return Parsed::NotTimer;
    }
    let lead = |w: &str| w.eq_ignore_ascii_case("timer");
    if lead(words[0]) {
        words.remove(0);
        if words.first().is_some_and(|w| w.eq_ignore_ascii_case("for")) {
            words.remove(0);
        }
        return match read_duration(&words) {
            Some((total, used)) => finish(total, clean_label(&words[used..])),
            None => Parsed::NotTimer,
        };
    }
    // "10 min timer [label]"
    match read_duration(&words) {
        Some((total, used)) if words.get(used).is_some_and(|w| lead(w)) => {
            finish(total, clean_label(&words[used + 1..]))
        }
        _ => Parsed::NotTimer,
    }
}

/// True for the text that lists running timers.
pub fn is_list_request(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("timers")
}

/// "10 minute", "90 second", "1h 30m": for "Start 10 minute timer".
pub fn adjective(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (h, m, s) {
        (h, 0, 0) if h > 0 => format!("{h} hour"),
        (0, m, 0) => format!("{m} minute"),
        (0, 0, s) => format!("{s} second"),
        _ => {
            let mut parts = Vec::new();
            if h > 0 {
                parts.push(format!("{h}h"));
            }
            if m > 0 {
                parts.push(format!("{m}m"));
            }
            if s > 0 {
                parts.push(format!("{s}s"));
            }
            parts.join(" ")
        }
    }
}

/// "4m 10s", "1h 5m", "45s".
pub fn time_left(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn clock_of(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map_or_else(String::new, |t| t.format("%H:%M").to_string())
}

// ---------------------------------------------------------------- store

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    pub id: i64,
    pub label: String,
    pub secs: u64,
    /// Wall-clock end, milliseconds since the Unix epoch.
    pub end_ms: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum TimerError {
    #[error("A timer must be at least 1 second.")]
    TooShort,
    #[error("A timer can be at most 24 hours.")]
    TooLong,
    #[error("You already have {MAX_ACTIVE} timers running. Cancel one first.")]
    TooMany,
    #[error("{0}")]
    Db(#[from] DbError),
}

/// Running timers, kept in the encrypted `meta.db` so they survive a restart.
pub struct TimerStore {
    db: Arc<SecureDb>,
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn row_to_timer(r: &rusqlite::Row<'_>) -> rusqlite::Result<Timer> {
    Ok(Timer {
        id: r.get(0)?,
        label: r.get(1)?,
        secs: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
        end_ms: r.get(3)?,
    })
}

impl TimerStore {
    pub fn new(db: Arc<SecureDb>) -> Result<Self, DbError> {
        db.write(|c| {
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS timers (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    label TEXT NOT NULL,
                    secs INTEGER NOT NULL,
                    end_ms INTEGER NOT NULL
                )",
            )
        })?;
        Ok(Self { db })
    }

    pub fn start(&self, secs: u64, label: &str, now_ms: i64) -> Result<Timer, TimerError> {
        if secs < MIN_SECS {
            return Err(TimerError::TooShort);
        }
        if secs > MAX_SECS {
            return Err(TimerError::TooLong);
        }
        let label: String = label.chars().take(MAX_LABEL).collect();
        let end_ms = now_ms.saturating_add(i64::try_from(secs).unwrap_or(0) * 1000);
        let id = self.db.write(|c| {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM timers", [], |r| r.get(0))?;
            if usize::try_from(n).unwrap_or(usize::MAX) >= MAX_ACTIVE {
                return Ok(None);
            }
            c.execute(
                "INSERT INTO timers (label, secs, end_ms) VALUES (?1, ?2, ?3)",
                rusqlite::params![label, secs as i64, end_ms],
            )?;
            Ok(Some(c.last_insert_rowid()))
        })?;
        let id = id.ok_or(TimerError::TooMany)?;
        Ok(Timer {
            id,
            label,
            secs,
            end_ms,
        })
    }

    /// Inserts a timer with an exact end time (tests, restoring).
    pub fn insert_raw(&self, label: &str, secs: u64, end_ms: i64) -> Result<i64, DbError> {
        self.db.write(|c| {
            c.execute(
                "INSERT INTO timers (label, secs, end_ms) VALUES (?1, ?2, ?3)",
                rusqlite::params![label, secs as i64, end_ms],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn active(&self) -> Vec<Timer> {
        self.db
            .read(|c| {
                let mut s =
                    c.prepare("SELECT id, label, secs, end_ms FROM timers ORDER BY end_ms, id")?;
                let rows = s.query_map([], row_to_timer)?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    pub fn get(&self, id: i64) -> Option<Timer> {
        self.active().into_iter().find(|t| t.id == id)
    }

    pub fn count(&self) -> usize {
        self.active().len()
    }

    pub fn cancel(&self, id: i64) -> bool {
        self.db
            .write(|c| c.execute("DELETE FROM timers WHERE id = ?1", [id]))
            .is_ok_and(|n| n > 0)
    }

    /// Removes and returns every timer that has ended. One database
    /// transaction under one lock, so two callers never both get the same one.
    pub fn take_due(&self, now_ms: i64) -> Vec<Timer> {
        // The common case (nothing due) must not rewrite the sealed database.
        if self.next_end().is_none_or(|end| end > now_ms) {
            return Vec::new();
        }
        self.db
            .write(|c| {
                let due: Vec<Timer> = {
                    let mut s = c.prepare(
                        "SELECT id, label, secs, end_ms FROM timers WHERE end_ms <= ?1 ORDER BY end_ms, id",
                    )?;
                    let rows = s.query_map([now_ms], row_to_timer)?;
                    rows.collect::<rusqlite::Result<_>>()?
                };
                if !due.is_empty() {
                    c.execute("DELETE FROM timers WHERE end_ms <= ?1", [now_ms])?;
                }
                Ok(due)
            })
            .unwrap_or_default()
    }

    pub fn next_end(&self) -> Option<i64> {
        self.db
            .read(|c| c.query_row("SELECT MIN(end_ms) FROM timers", [], |r| r.get(0)))
            .ok()
            .flatten()
    }
}

// -------------------------------------------------------------- service

struct State {
    wake: bool,
    stop: bool,
}

struct Shared {
    store: Arc<TimerStore>,
    notifier: Arc<dyn Notifier>,
    state: Mutex<State>,
    cv: Condvar,
}

/// The wake-up thread. It sleeps until the nearest end time; `poke` makes it
/// look again after a timer is added or cancelled.
pub struct TimerService {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn body_for(t: &Timer) -> String {
    if t.label.is_empty() {
        format!("Your {} timer is up", adjective(t.secs))
    } else {
        t.label.clone()
    }
}

/// What to show for timers that ended while Lookout was closed.
pub fn missed_message(missed: &[Timer]) -> (String, String) {
    match missed {
        [t] => (
            "Missed timer".to_owned(),
            format!(
                "{} ended at {} while Lookout was closed",
                if t.label.is_empty() {
                    format!("Your {} timer", adjective(t.secs))
                } else {
                    t.label.clone()
                },
                clock_of(t.end_ms)
            ),
        ),
        many => (
            format!("{} missed timers", many.len()),
            many.iter()
                .map(|t| {
                    if t.label.is_empty() {
                        adjective(t.secs)
                    } else {
                        t.label.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
                + " ended while Lookout was closed",
        ),
    }
}

impl TimerService {
    /// Starts the thread. Timers that ended while the app was closed produce
    /// one "missed" notification, shown from the thread so start-up is not delayed.
    pub fn start(store: Arc<TimerStore>, notifier: Arc<dyn Notifier>) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            store,
            notifier,
            state: Mutex::new(State {
                wake: false,
                stop: false,
            }),
            cv: Condvar::new(),
        });
        let s = shared.clone();
        let handle = std::thread::Builder::new()
            .name("lookout-timers".into())
            .spawn(move || run(&s))?;
        Ok(Self {
            shared,
            thread: Mutex::new(Some(handle)),
        })
    }

    pub fn store(&self) -> &Arc<TimerStore> {
        &self.shared.store
    }

    /// Tells the thread the list changed.
    pub fn poke(&self) {
        let mut st = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        st.wake = true;
        self.shared.cv.notify_all();
    }

    pub fn stop(&self) {
        {
            let mut st = self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            st.stop = true;
            self.shared.cv.notify_all();
        }
        let h = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(h) = h {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for TimerService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(sh: &Shared) {
    let missed = sh.store.take_due(now_ms());
    if !missed.is_empty() {
        let (title, body) = missed_message(&missed);
        if sh.notifier.notify(&title, &body).is_err() {
            crate::log::warn("a timer could not be saved or shown");
        }
    }
    loop {
        let now = now_ms();
        for t in sh.store.take_due(now) {
            if sh.notifier.notify("Timer finished", &body_for(&t)).is_err() {
                crate::log::warn("a timer could not be saved or shown");
            }
        }
        let next = sh.store.next_end();
        let mut st = sh.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.stop {
            return;
        }
        if st.wake {
            // A timer was added or cancelled since we looked: look again.
            st.wake = false;
            continue;
        }
        match next {
            None => {
                let _unused = sh.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
            Some(end) => {
                // The condvar clock stops while the PC sleeps, the end time is
                // wall-clock. Waking at most every 30 s while a timer is pending
                // bounds the lateness after a resume; with none pending we sleep
                // until poked.
                let wait = u64::try_from((end - now_ms()).max(1))
                    .unwrap_or(1)
                    .min(30_000);
                let _unused = sh
                    .cv
                    .wait_timeout(st, Duration::from_millis(wait))
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

// -------------------------------------------------------------- actions

fn is_kind(kind: &Kind, name: &str) -> bool {
    matches!(kind, Kind::Custom(n) if n == name)
}

fn plan_of(title: String, lines: Vec<String>, snapshot: serde_json::Value, fp: String) -> Plan {
    Plan {
        risk: Risk::Safe,
        preview: Preview {
            from: None,
            title,
            lines,
            warnings: Vec::new(),
        },
        undoable: false,
        snapshot,
        fingerprint: fp,
    }
}

/// Starts a timer. The target's address carries `seconds|label`, made by
/// [`TimerSource`] from what was typed.
pub struct StartAction {
    service: Arc<TimerService>,
}

impl StartAction {
    pub fn new(service: Arc<TimerService>) -> Self {
        Self { service }
    }
}

fn split_spec(uri: &str) -> Option<(u64, String)> {
    let (secs, label) = uri.split_once('|')?;
    Some((secs.parse().ok()?, label.to_owned()))
}

impl Action for StartAction {
    fn id(&self) -> &str {
        START
    }

    fn label(&self) -> &str {
        "Start timer"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        is_kind(kind, KIND_NEW)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let (secs, label) = target.uri.as_deref().and_then(split_spec).ok_or_else(|| {
            ActionError::NotApplicable {
                action: START.into(),
            }
        })?;
        if !(MIN_SECS..=MAX_SECS).contains(&secs) {
            return Err(ActionError::Refused(
                "A timer must be between 1 second and 24 hours.".into(),
            ));
        }
        if self.service.store().count() >= MAX_ACTIVE {
            return Err(ActionError::Refused(TimerError::TooMany.to_string()));
        }
        let end = now_ms() + i64::try_from(secs).unwrap_or(0) * 1000;
        let mut lines = vec![format!("Ends at {}", clock_of(end))];
        if !label.is_empty() {
            lines.push(label.clone());
        }
        Ok(plan_of(
            format!("Start {} timer", adjective(secs)),
            lines,
            serde_json::json!({ "secs": secs, "label": label }),
            format!("timer_start|{secs}|{label}"),
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let secs = plan
            .snapshot
            .get("secs")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ActionError::Failed("bad plan".into()))?;
        let label = plan
            .snapshot
            .get("label")
            .and_then(|l| l.as_str())
            .unwrap_or_default();
        let t = self
            .service
            .store()
            .start(secs, label, now_ms())
            .map_err(|e| ActionError::Refused(e.to_string()))?;
        self.service.poke();
        Ok(Executed {
            message: format!(
                "Started a {} timer, ends at {}",
                adjective(secs),
                clock_of(t.end_ms)
            ),
            undo_token: None,
        })
    }
}

/// Cancels a running timer. No confirmation: nothing is lost but a countdown.
pub struct CancelAction {
    service: Arc<TimerService>,
}

impl CancelAction {
    pub fn new(service: Arc<TimerService>) -> Self {
        Self { service }
    }
}

impl Action for CancelAction {
    fn id(&self) -> &str {
        CANCEL
    }

    fn label(&self) -> &str {
        "Cancel timer"
    }

    fn applies_to(&self, kind: &Kind) -> bool {
        is_kind(kind, KIND_RUNNING)
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let id: i64 = target
            .uri
            .as_deref()
            .and_then(|u| u.parse().ok())
            .ok_or_else(|| ActionError::NotApplicable {
                action: CANCEL.into(),
            })?;
        let t = self
            .service
            .store()
            .get(id)
            .ok_or_else(|| ActionError::Stale("That timer has already ended.".into()))?;
        Ok(plan_of(
            format!("Cancel {} timer", adjective(t.secs)),
            vec![if t.label.is_empty() {
                format!("Ends at {}", clock_of(t.end_ms))
            } else {
                t.label.clone()
            }],
            serde_json::json!({ "id": id }),
            format!("timer_cancel|{id}"),
        ))
    }

    fn execute(&self, plan: &Plan, _proof: &Authorized) -> Result<Executed, ActionError> {
        let id = plan
            .snapshot
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| ActionError::Failed("bad plan".into()))?;
        if !self.service.store().cancel(id) {
            return Err(ActionError::NothingToDo(
                "That timer has already ended.".into(),
            ));
        }
        self.service.poke();
        Ok(Executed {
            message: "Timer cancelled".into(),
            undo_token: None,
        })
    }
}

// --------------------------------------------------------------- source

pub struct TimerSource {
    store: Arc<TimerStore>,
}

impl TimerSource {
    pub fn new(store: Arc<TimerStore>) -> Self {
        Self { store }
    }
}

fn info(id: &str, title: &str) -> Item {
    let mut item = Item::new(id, Kind::Custom(KIND_INFO.into()), title, "timer");
    item.score = ANSWER_SCORE;
    item.score_features.match_quality = 1.0;
    item
}

impl Source for TimerSource {
    fn id(&self) -> &'static str {
        "timer"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.kind.is_some() {
            return;
        }
        if is_list_request(&q.raw) {
            let now = now_ms();
            let items: Vec<Item> = self
                .store
                .active()
                .into_iter()
                .map(|t| {
                    let left = u64::try_from((t.end_ms - now).max(0) / 1000).unwrap_or(0);
                    let title = if t.label.is_empty() {
                        format!("{} timer", adjective(t.secs))
                    } else {
                        t.label.clone()
                    };
                    let mut item = Item::new(
                        format!("timer:{}", t.id),
                        Kind::Custom(KIND_RUNNING.into()),
                        title,
                        "timer",
                    );
                    item.subtitle = Some(format!(
                        "{} left · ends at {}",
                        time_left(left),
                        clock_of(t.end_ms)
                    ));
                    item.uri = Some(t.id.to_string());
                    item.score = ANSWER_SCORE;
                    item.score_features.match_quality = 1.0;
                    item.metadata.insert("default_action".into(), CANCEL.into());
                    item.metadata.insert("end_ms".into(), t.end_ms.into());
                    item.metadata.insert("secs".into(), t.secs.into());
                    item
                })
                .collect();
            emit(if items.is_empty() {
                vec![info("timer:none", "No active timers")]
            } else {
                items
            });
            return;
        }
        match parse(&q.raw) {
            Parsed::NotTimer => {}
            Parsed::Invalid(why) => emit(vec![info("timer:invalid", &why)]),
            Parsed::Timer { secs, label } => {
                let end = now_ms() + i64::try_from(secs).unwrap_or(0) * 1000;
                let mut item = Item::new(
                    format!("timer:new:{secs}"),
                    Kind::Custom(KIND_NEW.into()),
                    format!("Start {} timer", adjective(secs)),
                    "timer",
                );
                item.subtitle = Some(if label.is_empty() {
                    format!("Ends at {}", clock_of(end))
                } else {
                    format!("Ends at {} · {label}", clock_of(end))
                });
                item.uri = Some(format!("{secs}|{label}"));
                item.score = ANSWER_SCORE;
                item.score_features.match_quality = 1.0;
                item.metadata.insert("default_action".into(), START.into());
                item.metadata.insert("end_ms".into(), end.into());
                item.metadata.insert("secs".into(), secs.into());
                emit(vec![item]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::RecordingNotifier;

    fn secs(s: &str) -> Option<(u64, String)> {
        match parse(s) {
            Parsed::Timer { secs, label } => Some((secs, label)),
            _ => None,
        }
    }

    #[test]
    fn many_typed_forms_are_understood() {
        let cases: &[(&str, u64, &str)] = &[
            ("timer 10 min", 600, ""),
            ("timer 10min", 600, ""),
            ("Timer 10 Minutes", 600, ""),
            ("timer 90s", 90, ""),
            ("timer 90 sec", 90, ""),
            ("timer 1h30m", 5400, ""),
            ("timer 1h 30m", 5400, ""),
            ("timer 1 hour 30 minutes", 5400, ""),
            ("timer 1h30", 5400, ""),
            ("timer 2m30", 150, ""),
            ("timer 25", 1500, ""),
            ("timer 1.5h", 5400, ""),
            ("timer 0.5 min", 30, ""),
            ("timer for 5 min", 300, ""),
            ("10 min timer", 600, ""),
            ("90s timer", 90, ""),
            ("1h timer", 3600, ""),
            ("timer 5 min tea", 300, "tea"),
            ("timer 25 tea time", 1500, "tea time"),
            ("timer 1h30m call Sam", 5400, "call Sam"),
            ("10 min timer tea", 600, "tea"),
            ("  timer   10   min  ", 600, ""),
            ("timer 24h", 86400, ""),
            ("timer 1s", 1, ""),
            ("5 timer", 300, ""),
        ];
        for (text, want, label) in cases {
            assert_eq!(secs(text), Some((*want, (*label).to_owned())), "{text}");
        }
    }

    #[test]
    fn other_text_is_not_a_timer() {
        for t in [
            "timer",
            "timers",
            "timer app",
            "timer settings",
            "10 min",
            "10 minutes to hours",
            "kill port 3000",
            "my timer 5",
            "",
            "timer 1..5 min",
            "timer 5m 1h",
            "timer 1h 1h",
        ] {
            assert_eq!(parse(t), Parsed::NotTimer, "{t}");
        }
    }

    #[test]
    fn limits_are_reported_not_started() {
        for t in ["timer 0", "timer 0 min", "timer 0.2s", "timer 0s"] {
            assert!(
                matches!(parse(t), Parsed::Invalid(m) if m.contains("at least")),
                "{t}"
            );
        }
        for t in [
            "timer 25h",
            "timer 1441",
            "timer 24h 1s",
            "timer 90000s",
            "999999999 h timer",
        ] {
            assert!(
                matches!(parse(t), Parsed::Invalid(m) if m.contains("at most")),
                "{t}"
            );
        }
        assert_eq!(secs("timer 1441 min"), None);
        assert!(secs("timer 1440 min").is_some());
        // Absurdly long numbers are not a timer at all.
        assert_eq!(parse("timer 12345678901234 min"), Parsed::NotTimer);
    }

    #[test]
    fn labels_are_cleaned_and_bounded() {
        let long = "x".repeat(200);
        let (_, l) = secs(&format!("timer 5 min {long}")).unwrap();
        assert_eq!(l.chars().count(), MAX_LABEL);
        let (_, l) = secs("timer 5 min te\u{7}a").unwrap();
        assert_eq!(l, "tea");
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(adjective(600), "10 minute");
        assert_eq!(adjective(90), "1m 30s");
        assert_eq!(adjective(45), "45 second");
        assert_eq!(adjective(3600), "1 hour");
        assert_eq!(adjective(5400), "1h 30m");
        assert_eq!(time_left(250), "4m 10s");
        assert_eq!(time_left(3900), "1h 5m");
        assert_eq!(time_left(9), "9s");
    }

    fn store() -> Arc<TimerStore> {
        Arc::new(TimerStore::new(Arc::new(SecureDb::in_memory().unwrap())).unwrap())
    }

    #[test]
    fn at_most_ten_timers_and_limits_hold_in_the_store() {
        let s = store();
        assert!(matches!(s.start(0, "", 0), Err(TimerError::TooShort)));
        assert!(matches!(
            s.start(MAX_SECS + 1, "", 0),
            Err(TimerError::TooLong)
        ));
        for _ in 0..MAX_ACTIVE {
            s.start(60, "t", 0).unwrap();
        }
        assert!(matches!(s.start(60, "t", 0), Err(TimerError::TooMany)));
        assert_eq!(s.count(), MAX_ACTIVE);
        let first = s.active()[0].id;
        assert!(s.cancel(first));
        assert!(!s.cancel(first));
        assert!(s.start(60, "t", 0).is_ok());
    }

    #[test]
    fn timers_survive_a_restart_and_due_ones_are_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let key = crate::crypto::Key::from_bytes(&[3u8; 32]).unwrap();
        {
            let s = TimerStore::new(Arc::new(SecureDb::open(&path, key.clone()).unwrap())).unwrap();
            s.insert_raw("later", 60, 10_000_000).unwrap();
            s.insert_raw("gone", 60, 1_000).unwrap();
        }
        let s = TimerStore::new(Arc::new(SecureDb::open(&path, key).unwrap())).unwrap();
        assert_eq!(s.active().len(), 2);
        assert_eq!(s.next_end(), Some(1_000));
        let due = s.take_due(5_000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].label, "gone");
        assert!(s.take_due(5_000).is_empty(), "a timer is taken only once");
        assert_eq!(s.active().len(), 1);
        // The file on disk is sealed: the label is not readable in it.
        let raw = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("later"));
    }

    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(std::time::Instant::now() < end, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_timer_that_ended_while_closed_gives_one_missed_notice() {
        let s = store();
        let past = now_ms() - 60_000;
        s.insert_raw("tea", 300, past).unwrap();
        s.insert_raw("", 600, past + 1).unwrap();
        s.insert_raw("far", 3600, now_ms() + 3_600_000).unwrap();
        let n = Arc::new(RecordingNotifier::default());
        let svc = TimerService::start(s.clone(), n.clone()).unwrap();
        wait_for("missed notice", || !n.all().is_empty());
        std::thread::sleep(Duration::from_millis(100));
        let shown = n.all();
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert!(shown[0].0.contains("missed"));
        assert!(shown[0].1.contains("tea"));
        assert_eq!(s.active().len(), 1, "the far timer is still running");
        svc.stop();
    }

    #[test]
    fn the_thread_wakes_for_the_nearest_timer_and_for_new_ones() {
        let s = store();
        let n = Arc::new(RecordingNotifier::default());
        let svc = TimerService::start(s.clone(), n.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        // A far timer first, then a nearer one: the thread must re-plan.
        s.insert_raw("far", 3600, now_ms() + 3_600_000).unwrap();
        svc.poke();
        s.insert_raw("soon", 1, now_ms() + 200).unwrap();
        svc.poke();
        wait_for("notice", || !n.all().is_empty());
        let shown = n.all();
        assert_eq!(shown[0].0, "Timer finished");
        assert_eq!(shown[0].1, "soon");
        assert_eq!(s.active().len(), 1);
        svc.stop();
        assert_eq!(n.all().len(), 1);
    }

    #[test]
    fn a_cancelled_timer_never_notifies() {
        let s = store();
        let n = Arc::new(RecordingNotifier::default());
        let svc = TimerService::start(s.clone(), n.clone()).unwrap();
        let id = s.insert_raw("x", 1, now_ms() + 300).unwrap();
        svc.poke();
        assert!(s.cancel(id));
        svc.poke();
        std::thread::sleep(Duration::from_millis(600));
        assert!(n.all().is_empty());
        svc.stop();
    }

    fn target(kind: &str, uri: &str) -> Target {
        Target {
            item_id: "x".into(),
            kind: Kind::Custom(kind.into()),
            uri: Some(uri.into()),
            port: None,
            origin: None,
        }
    }

    #[test]
    fn start_and_cancel_actions_are_safe_and_check_their_target() {
        let s = store();
        let svc = Arc::new(
            TimerService::start(s.clone(), Arc::new(RecordingNotifier::default())).unwrap(),
        );
        let start = StartAction::new(svc.clone());
        let plan = start.plan(&target(KIND_NEW, "600|tea")).unwrap();
        assert_eq!(plan.risk, Risk::Safe);
        assert_eq!(plan.preview.title, "Start 10 minute timer");
        assert!(start.plan(&target(KIND_NEW, "0|")).is_err());
        assert!(start.plan(&target(KIND_NEW, "999999|")).is_err());
        assert!(start.plan(&target(KIND_NEW, "junk")).is_err());
        for _ in 0..MAX_ACTIVE {
            s.start(60, "", now_ms()).unwrap();
        }
        assert!(start.plan(&target(KIND_NEW, "600|tea")).is_err());
        let id = s.active()[0].id;
        let cancel = CancelAction::new(svc.clone());
        let p = cancel.plan(&target(KIND_RUNNING, &id.to_string())).unwrap();
        assert_eq!(p.risk, Risk::Safe);
        assert!(cancel.plan(&target(KIND_RUNNING, "99999")).is_err());
        svc.stop();
    }

    #[test]
    fn the_source_offers_a_start_result_and_lists_timers() {
        let s = store();
        let src = TimerSource::new(s.clone());
        let ask = |text: &str| {
            let q = crate::query::parse(text);
            let ctx = QueryContext::new(1, Arc::new(std::sync::atomic::AtomicU64::new(1)));
            let mut out = Vec::new();
            src.query(&q, &ctx, &mut |i| out.extend(i));
            out
        };
        let r = ask("timer 10 min tea");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Start 10 minute timer");
        assert_eq!(r[0].uri.as_deref(), Some("600|tea"));
        assert!(ask("report.pdf").is_empty());
        assert_eq!(ask("timers")[0].title, "No active timers");
        s.start(300, "tea", now_ms()).unwrap();
        let l = ask("timers");
        assert_eq!(l[0].title, "tea");
        assert!(l[0].subtitle.as_deref().unwrap().contains("left"));
        assert_eq!(l[0].metadata["default_action"], CANCEL);
        assert!(ask("timer 0")[0].title.contains("at least"));
    }
}
