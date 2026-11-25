//! The confirmation gate: the only way to run an action.
//!
//! Flow: `prepare` (plans against the real state, returns a ticket and the
//! preview) -> `present` (the interface reports the preview is on screen) ->
//! `run` (risk `safe` only) or `confirm` (any risk, needs `present` first and a
//! short pause after it). A ticket is single use. Before anything executes the
//! gate plans again and refuses if the state differs from what was previewed.

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::undo_log::{Entry, Status, UndoLog};
use super::{Action, ActionError, ActionProvider, Plan, Preview, Risk, Target, UndoPlan};

/// Proof that the gate approved this execution. It cannot be built outside
/// this module, so an action's `execute` cannot be called from anywhere else.
pub struct Authorized {
    _seal: (),
}

impl Authorized {
    fn new() -> Self {
        Self { _seal: () }
    }

    /// For unit tests of an individual action only.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GateConfig {
    /// A ticket not confirmed within this time is discarded.
    pub ttl: Duration,
    /// Least time between `present` and `confirm`. A key held down (auto
    /// repeat) or a script firing both calls back to back cannot confirm.
    pub min_dwell: Duration,
    /// Most tickets alive at once; the oldest is dropped beyond this.
    pub max_pending: usize,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            min_dwell: Duration::from_millis(350),
            max_pending: 16,
        }
    }
}

/// What the interface needs to show the confirmation card.
#[derive(Debug, Clone, Serialize)]
pub struct Prepared {
    pub ticket: String,
    pub action_id: String,
    pub label: String,
    pub risk: Risk,
    pub preview: Preview,
    pub undoable: bool,
    pub is_undo: bool,
}

/// The outcome of a finished execution.
#[derive(Debug, Clone, Serialize)]
pub struct Done {
    pub action_id: String,
    pub message: String,
    pub items: Vec<String>,
    /// True when `Undo` is now possible for this execution.
    pub undoable: bool,
    pub was_undo: bool,
    pub log_id: i64,
}

enum Work {
    Act {
        action: Arc<dyn Action>,
        target: Target,
        plan: Plan,
    },
    Undo {
        action: Arc<dyn Action>,
        entry: Entry,
        plan: UndoPlan,
    },
}

impl Work {
    fn risk(&self) -> Risk {
        match self {
            Work::Act { plan, .. } => plan.risk,
            Work::Undo { plan, .. } => plan.risk,
        }
    }
}

struct Pending {
    work: Work,
    created: Instant,
    presented: Option<Instant>,
}

pub struct ActionEngine {
    actions: Vec<Arc<dyn Action>>,
    provider: Option<Arc<dyn ActionProvider>>,
    log: UndoLog,
    config: GateConfig,
    pending: Mutex<HashMap<String, Pending>>,
    /// Serialises executions, so two confirmations cannot interleave.
    exec: Mutex<()>,
    counter: AtomicU64,
    hasher: RandomState,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl ActionEngine {
    pub fn new(actions: Vec<Arc<dyn Action>>, log: UndoLog, config: GateConfig) -> Self {
        Self {
            actions,
            provider: None,
            log,
            config,
            pending: Mutex::new(HashMap::new()),
            exec: Mutex::new(()),
            counter: AtomicU64::new(1),
            hasher: RandomState::new(),
        }
    }

    /// Adds a source of actions that are looked up by id at run time (plugins).
    pub fn with_provider(mut self, provider: Arc<dyn ActionProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    fn find(&self, id: &str) -> Result<Arc<dyn Action>, ActionError> {
        self.actions
            .iter()
            .find(|a| a.id() == id)
            .cloned()
            .or_else(|| self.provider.as_ref().and_then(|p| p.find(id)))
            .ok_or_else(|| ActionError::UnknownAction(id.to_owned()))
    }

    fn new_ticket(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut h = self.hasher.build_hasher();
        h.write_u64(n);
        format!("t{n:x}-{:016x}", h.finish())
    }

    fn store(&self, work: Work) -> String {
        let ticket = self.new_ticket();
        let mut map = lock(&self.pending);
        let now = Instant::now();
        map.retain(|_, p| now.duration_since(p.created) < self.config.ttl);
        while map.len() >= self.config.max_pending {
            let oldest = map
                .iter()
                .min_by_key(|(_, p)| p.created)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => map.remove(&k),
                None => break,
            };
        }
        map.insert(
            ticket.clone(),
            Pending {
                work,
                created: now,
                presented: None,
            },
        );
        ticket
    }

    /// Looks at the real state and returns the preview and a ticket. Nothing
    /// is changed.
    pub fn prepare(&self, action_id: &str, target: Target) -> Result<Prepared, ActionError> {
        let action = self.find(action_id)?;
        if !action.applies_to(&target.kind) {
            return Err(ActionError::NotApplicable {
                action: action_id.to_owned(),
            });
        }
        let plan = action.plan(&target)?;
        let prepared = Prepared {
            ticket: String::new(),
            action_id: action.id().to_owned(),
            label: action.label().to_owned(),
            risk: plan.risk,
            preview: plan.preview.clone(),
            undoable: plan.undoable,
            is_undo: false,
        };
        let ticket = self.store(Work::Act {
            action,
            target,
            plan,
        });
        Ok(Prepared { ticket, ..prepared })
    }

    /// Prepares undoing the newest undoable log entry.
    pub fn prepare_undo(&self) -> Result<Prepared, ActionError> {
        let entry = self
            .log
            .latest_undoable()?
            .ok_or(ActionError::NothingToUndo)?;
        let action = self.find(&entry.action_id)?;
        let token = entry.undo_token.clone().ok_or(ActionError::NotUndoable)?;
        let plan = action.plan_undo(&token)?;
        let prepared = Prepared {
            ticket: String::new(),
            action_id: action.id().to_owned(),
            label: format!("Undo: {}", action.label()),
            risk: plan.risk,
            preview: plan.preview.clone(),
            undoable: false,
            is_undo: true,
        };
        let ticket = self.store(Work::Undo {
            action,
            entry,
            plan,
        });
        Ok(Prepared { ticket, ..prepared })
    }

    /// The interface calls this once the preview is actually on screen.
    pub fn present(&self, ticket: &str) -> Result<(), ActionError> {
        let mut map = lock(&self.pending);
        let now = Instant::now();
        let p = map.get_mut(ticket).ok_or(ActionError::UnknownTicket)?;
        if now.duration_since(p.created) >= self.config.ttl {
            map.remove(ticket);
            return Err(ActionError::UnknownTicket);
        }
        p.presented.get_or_insert(now);
        Ok(())
    }

    pub fn cancel(&self, ticket: &str) {
        lock(&self.pending).remove(ticket);
    }

    /// Runs a ticket whose risk is `safe`. A `confirm` or `dangerous` ticket
    /// is refused here and stays valid so it can still be confirmed.
    pub fn run(&self, ticket: &str) -> Result<Done, ActionError> {
        let pending = {
            let mut map = lock(&self.pending);
            let p = map.get(ticket).ok_or(ActionError::UnknownTicket)?;
            if p.work.risk().needs_confirm() {
                return Err(ActionError::ConfirmationRequired);
            }
            self.take(&mut map, ticket)?
        };
        self.execute(pending)
    }

    /// Runs a ticket the user confirmed. The preview must have been presented
    /// at least `min_dwell` ago.
    pub fn confirm(&self, ticket: &str) -> Result<Done, ActionError> {
        let pending = {
            let mut map = lock(&self.pending);
            let p = map.get(ticket).ok_or(ActionError::UnknownTicket)?;
            let shown = p.presented.ok_or(ActionError::NotPresented)?;
            if shown.elapsed() < self.config.min_dwell {
                return Err(ActionError::TooSoon);
            }
            self.take(&mut map, ticket)?
        };
        self.execute(pending)
    }

    /// Removes a ticket (single use) unless it has expired.
    fn take(
        &self,
        map: &mut HashMap<String, Pending>,
        ticket: &str,
    ) -> Result<Pending, ActionError> {
        let p = map.remove(ticket).ok_or(ActionError::UnknownTicket)?;
        if p.created.elapsed() >= self.config.ttl {
            return Err(ActionError::UnknownTicket);
        }
        Ok(p)
    }

    fn execute(&self, pending: Pending) -> Result<Done, ActionError> {
        let _one_at_a_time = lock(&self.exec);
        match pending.work {
            Work::Act {
                action,
                target,
                plan,
            } => self.execute_action(&action, &target, &plan),
            Work::Undo {
                action,
                entry,
                plan,
            } => self.execute_undo(&action, &entry, &plan),
        }
    }

    fn execute_action(
        &self,
        action: &Arc<dyn Action>,
        target: &Target,
        previewed: &Plan,
    ) -> Result<Done, ActionError> {
        // The world may have changed since the preview; what runs must be
        // exactly what the user saw.
        let fresh = action.plan(target)?;
        if fresh.fingerprint != previewed.fingerprint || fresh.risk != previewed.risk {
            return Err(ActionError::Stale(
                "things changed since the preview was made; nothing was done. Try again".into(),
            ));
        }
        let items = vec![target.item_id.clone()];
        let log_id = self
            .log
            .begin(action.id(), &items, &fresh.snapshot, unix_secs())?;
        let result = action.execute(&fresh, &Authorized::new());
        match result {
            Ok(done) => {
                self.log.finish(
                    log_id,
                    Status::Executed,
                    done.undo_token.as_deref(),
                    &done.message,
                    unix_secs(),
                )?;
                Ok(Done {
                    action_id: action.id().to_owned(),
                    message: done.message,
                    items,
                    undoable: done.undo_token.is_some(),
                    was_undo: false,
                    log_id,
                })
            }
            Err(e) => {
                self.log
                    .finish(log_id, Status::Failed, None, &e.to_string(), unix_secs())?;
                Err(e)
            }
        }
    }

    fn execute_undo(
        &self,
        action: &Arc<dyn Action>,
        previewed: &Entry,
        plan: &UndoPlan,
    ) -> Result<Done, ActionError> {
        let current = self
            .log
            .get(previewed.id)?
            .ok_or(ActionError::NothingToUndo)?;
        let token = match (&current.status, &current.undo_token) {
            (Status::Executed, Some(t)) if Some(t) == previewed.undo_token.as_ref() => t.clone(),
            _ => {
                return Err(ActionError::Stale(
                    "that action was already undone or changed; nothing was done".into(),
                ))
            }
        };
        let fresh = action.plan_undo(&token)?;
        if fresh.fingerprint != plan.fingerprint || fresh.risk != plan.risk {
            return Err(ActionError::Stale(
                "things changed since the preview was made; nothing was done. Try again".into(),
            ));
        }
        match action.undo(&token, &fresh, &Authorized::new()) {
            Ok(message) => {
                self.log
                    .finish_undo(previewed.id, true, &message, unix_secs())?;
                Ok(Done {
                    action_id: action.id().to_owned(),
                    message,
                    items: previewed.items.clone(),
                    undoable: false,
                    was_undo: true,
                    log_id: previewed.id,
                })
            }
            Err(e) => {
                self.log
                    .finish_undo(previewed.id, false, &e.to_string(), unix_secs())?;
                Err(e)
            }
        }
    }

    /// Logs a request that is not an action (a command-line search) in the same
    /// log, with `params` only; nothing here can be undone.
    pub fn record_external(&self, action_id: &str, params: &serde_json::Value, ok: bool) {
        let now = unix_secs();
        if let Ok(id) = self.log.begin(action_id, &[], params, now) {
            let status = if ok { Status::Executed } else { Status::Failed };
            let _ = self.log.finish(id, status, None, "", now);
        }
    }

    /// The newest `limit` log rows, newest first.
    pub fn history(&self, limit: usize) -> Result<Vec<Entry>, ActionError> {
        Ok(self.log.recent(limit)?)
    }

    pub fn can_undo(&self) -> bool {
        matches!(self.log.latest_undoable(), Ok(Some(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Kind;
    use std::sync::atomic::AtomicUsize;

    /// Counts how many times it really executed.
    struct Fake {
        risk: Risk,
        runs: Arc<AtomicUsize>,
        undos: Arc<AtomicUsize>,
        /// Changes the fingerprint on every plan when set.
        drift: bool,
        plans: AtomicUsize,
    }

    impl Fake {
        fn make(risk: Risk, drift: bool) -> (Arc<Self>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
            let runs = Arc::new(AtomicUsize::new(0));
            let undos = Arc::new(AtomicUsize::new(0));
            let fake = Arc::new(Self {
                risk,
                runs: runs.clone(),
                undos: undos.clone(),
                drift,
                plans: AtomicUsize::new(0),
            });
            (fake, runs, undos)
        }
    }

    impl Action for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn label(&self) -> &'static str {
            "Fake"
        }
        fn applies_to(&self, kind: &Kind) -> bool {
            *kind == Kind::File
        }
        fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
            let n = if self.drift {
                self.plans.fetch_add(1, Ordering::Relaxed)
            } else {
                0
            };
            Ok(Plan {
                risk: self.risk,
                preview: Preview {
                    from: None,
                    title: format!("fake {}", target.item_id),
                    lines: vec![],
                    warnings: vec![],
                },
                undoable: true,
                snapshot: serde_json::json!({"item": target.item_id}),
                fingerprint: format!("fp{n}"),
            })
        }
        fn execute(&self, _plan: &Plan, _p: &Authorized) -> Result<Executed, ActionError> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(Executed {
                message: "did it".into(),
                undo_token: Some("tok".into()),
            })
        }
        fn plan_undo(&self, _t: &str) -> Result<UndoPlan, ActionError> {
            Ok(UndoPlan {
                risk: Risk::Confirm,
                preview: Preview {
                    from: None,
                    title: "undo fake".into(),
                    lines: vec![],
                    warnings: vec![],
                },
                fingerprint: "u".into(),
            })
        }
        fn undo(&self, _t: &str, _p: &UndoPlan, _a: &Authorized) -> Result<String, ActionError> {
            self.undos.fetch_add(1, Ordering::SeqCst);
            Ok("undone".into())
        }
    }

    use super::super::Executed;

    fn target() -> Target {
        Target {
            item_id: "file:x".into(),
            kind: Kind::File,
            uri: Some("x".into()),
            port: None,
            origin: None,
        }
    }

    fn engine(risk: Risk, drift: bool) -> (ActionEngine, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let (fake, runs, undos) = Fake::make(risk, drift);
        let cfg = GateConfig {
            min_dwell: Duration::from_millis(40),
            ..GateConfig::default()
        };
        let e = ActionEngine::new(vec![fake], UndoLog::open_in_memory().unwrap(), cfg);
        (e, runs, undos)
    }

    fn wait_dwell() {
        std::thread::sleep(Duration::from_millis(60));
    }

    #[test]
    fn safe_actions_run_with_one_call_and_are_logged() {
        let (e, runs, _) = engine(Risk::Safe, false);
        let p = e.prepare("fake", target()).unwrap();
        let done = e.run(&p.ticket).unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(done.undoable);
        let h = e.history(10).unwrap();
        assert_eq!((h.len(), h[0].status), (1, Status::Executed));
    }

    /// The bypass test: every route that could run a confirm/dangerous action
    /// without the confirm step must leave the action unexecuted.
    #[test]
    fn confirm_and_dangerous_actions_cannot_be_run_without_the_confirm_step() {
        for risk in [Risk::Confirm, Risk::Dangerous] {
            let (e, runs, _) = engine(risk, false);

            // 1. Calling the safe-path `run` is refused, and does not burn the ticket.
            let p = e.prepare("fake", target()).unwrap();
            assert!(matches!(
                e.run(&p.ticket),
                Err(ActionError::ConfirmationRequired)
            ));
            // 2. `confirm` before `present` is refused.
            assert!(matches!(
                e.confirm(&p.ticket),
                Err(ActionError::NotPresented)
            ));
            // 3. `confirm` right after `present` (key repeat, scripted) is refused.
            e.present(&p.ticket).unwrap();
            assert!(matches!(e.confirm(&p.ticket), Err(ActionError::TooSoon)));
            assert_eq!(runs.load(Ordering::SeqCst), 0, "{risk:?} ran unconfirmed");

            // 4. Made-up, empty and other actions' tickets run nothing.
            for bad in ["", "t1", "t1-0000000000000000", "../../x"] {
                assert!(matches!(e.run(bad), Err(ActionError::UnknownTicket)));
                assert!(matches!(e.confirm(bad), Err(ActionError::UnknownTicket)));
                assert!(matches!(e.present(bad), Err(ActionError::UnknownTicket)));
            }
            // 5. A cancelled ticket cannot be confirmed later.
            let c = e.prepare("fake", target()).unwrap();
            e.present(&c.ticket).unwrap();
            e.cancel(&c.ticket);
            wait_dwell();
            assert!(matches!(
                e.confirm(&c.ticket),
                Err(ActionError::UnknownTicket)
            ));
            assert_eq!(runs.load(Ordering::SeqCst), 0);

            // 6. Only after present + dwell does confirm run it, exactly once.
            wait_dwell();
            e.confirm(&p.ticket).unwrap();
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            assert!(matches!(
                e.confirm(&p.ticket),
                Err(ActionError::UnknownTicket)
            ));
            assert_eq!(runs.load(Ordering::SeqCst), 1, "a ticket is single use");
        }
    }

    #[test]
    fn nothing_is_logged_as_executed_when_confirmation_is_refused() {
        let (e, _, _) = engine(Risk::Confirm, false);
        let p = e.prepare("fake", target()).unwrap();
        let _ = e.run(&p.ticket);
        let _ = e.confirm(&p.ticket);
        assert!(e.history(10).unwrap().is_empty());
    }

    #[test]
    fn an_expired_ticket_is_dead() {
        let (fake, runs, _) = Fake::make(Risk::Confirm, false);
        let cfg = GateConfig {
            ttl: Duration::from_millis(30),
            min_dwell: Duration::ZERO,
            ..GateConfig::default()
        };
        let e = ActionEngine::new(vec![fake], UndoLog::open_in_memory().unwrap(), cfg);
        let p = e.prepare("fake", target()).unwrap();
        e.present(&p.ticket).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(matches!(
            e.confirm(&p.ticket),
            Err(ActionError::UnknownTicket)
        ));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_confirmation_never_applies_to_a_state_that_differs_from_the_preview() {
        let (e, runs, _) = engine(Risk::Confirm, true);
        let p = e.prepare("fake", target()).unwrap();
        e.present(&p.ticket).unwrap();
        wait_dwell();
        let r = e.confirm(&p.ticket);
        assert!(matches!(r, Err(ActionError::Stale(_))), "{r:?}");
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert!(e.history(10).unwrap().is_empty(), "nothing started");
    }

    #[test]
    fn wrong_kind_and_unknown_action_are_refused_at_prepare() {
        let (e, _, _) = engine(Risk::Safe, false);
        let mut t = target();
        t.kind = Kind::Port;
        assert!(matches!(
            e.prepare("fake", t),
            Err(ActionError::NotApplicable { .. })
        ));
        assert!(matches!(
            e.prepare("rm_rf", target()),
            Err(ActionError::UnknownAction(_))
        ));
    }

    #[test]
    fn undo_needs_its_own_confirmation_and_runs_once() {
        let (e, _, undos) = engine(Risk::Safe, false);
        assert!(matches!(e.prepare_undo(), Err(ActionError::NothingToUndo)));
        let p = e.prepare("fake", target()).unwrap();
        e.run(&p.ticket).unwrap();
        assert!(e.can_undo());

        let u = e.prepare_undo().unwrap();
        assert!(u.is_undo && u.risk == Risk::Confirm);
        assert!(matches!(
            e.run(&u.ticket),
            Err(ActionError::ConfirmationRequired)
        ));
        assert_eq!(undos.load(Ordering::SeqCst), 0);
        e.present(&u.ticket).unwrap();
        wait_dwell();
        let done = e.confirm(&u.ticket).unwrap();
        assert!(done.was_undo);
        assert_eq!(undos.load(Ordering::SeqCst), 1);
        assert!(!e.can_undo());
        assert!(matches!(e.prepare_undo(), Err(ActionError::NothingToUndo)));
        assert_eq!(e.history(1).unwrap()[0].status, Status::Undone);
    }

    #[test]
    fn two_undo_tickets_for_one_entry_cannot_both_run() {
        let (e, _, undos) = engine(Risk::Safe, false);
        let p = e.prepare("fake", target()).unwrap();
        e.run(&p.ticket).unwrap();
        let a = e.prepare_undo().unwrap();
        let b = e.prepare_undo().unwrap();
        for t in [&a.ticket, &b.ticket] {
            e.present(t).unwrap();
        }
        wait_dwell();
        e.confirm(&a.ticket).unwrap();
        assert!(matches!(e.confirm(&b.ticket), Err(ActionError::Stale(_))));
        assert_eq!(undos.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn too_many_open_tickets_drops_the_oldest() {
        let (fake, _, _) = Fake::make(Risk::Safe, false);
        let cfg = GateConfig {
            max_pending: 2,
            ..GateConfig::default()
        };
        let e = ActionEngine::new(vec![fake], UndoLog::open_in_memory().unwrap(), cfg);
        let a = e.prepare("fake", target()).unwrap();
        let _b = e.prepare("fake", target()).unwrap();
        let _c = e.prepare("fake", target()).unwrap();
        assert!(matches!(e.run(&a.ticket), Err(ActionError::UnknownTicket)));
    }
}
