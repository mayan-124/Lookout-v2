//! The action engine (brief section 7.3).
//!
//! An [`Action`] only describes and performs work. Nothing outside this module
//! can run one: the only way in is [`gate::ActionEngine`], which hands out
//! single-use tickets and refuses to run a `confirm` or `dangerous` action
//! until the ticket has been presented to the user and confirmed. `execute`
//! needs an [`gate::Authorized`] value that only the gate can create, so even
//! code inside this crate cannot call it by accident.

#[cfg(windows)]
pub mod disk;
pub mod gate;
pub mod kill_port;
pub mod open;
pub mod system;
pub mod undo_log;

use std::sync::Arc;

use serde::Serialize;

use crate::item::Kind;
pub use gate::{ActionEngine, Authorized, Done, GateConfig, Prepared};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Runs on one keypress; nothing is lost if it was a mistake.
    Safe,
    /// Changes something the user may care about: preview and explicit confirm.
    Confirm,
    /// Like `Confirm`, but the effect is wide or hard to reverse.
    Dangerous,
}

impl Risk {
    pub fn needs_confirm(self) -> bool {
        self != Risk::Safe
    }
}

/// What the user is shown before anything happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preview {
    /// Written by Lookout, never by a plugin: where the text below comes from
    /// ("From plugin: ..."). Shown above everything else on the card.
    pub from: Option<String>,
    pub title: String,
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
}

/// What an action is asked to act on. Built by `Lookout` from an item the
/// backend itself produced, never from text sent by the webview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub item_id: String,
    pub kind: Kind,
    pub uri: Option<String>,
    pub port: Option<u16>,
    /// Set when a plugin produced this item; only that plugin's own actions accept it.
    pub origin: Option<PluginOrigin>,
}

/// Where a plugin item came from and what the plugin asked to do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginOrigin {
    pub plugin_id: String,
    pub action_id: String,
    pub params: serde_json::Value,
}

/// Finds actions that are not known at start-up (plugin actions).
pub trait ActionProvider: Send + Sync {
    fn find(&self, id: &str) -> Option<Arc<dyn Action>>;
}

/// The result of looking at the world right now.
#[derive(Debug, Clone)]
pub struct Plan {
    pub risk: Risk,
    pub preview: Preview,
    /// Whether `execute` will return an undo token.
    pub undoable: bool,
    /// What `execute` will act on, recorded in the log.
    pub snapshot: serde_json::Value,
    /// Identifies the exact state that was previewed. The gate plans again at
    /// confirm time and refuses to run if this changed.
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct UndoPlan {
    pub risk: Risk,
    pub preview: Preview,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct Executed {
    pub message: String,
    pub undo_token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("unknown action '{0}'")]
    UnknownAction(String),
    #[error("'{action}' does not apply to this item")]
    NotApplicable { action: String },
    #[error("that confirmation has expired or was already used; start again")]
    UnknownTicket,
    #[error("this action needs confirmation; it cannot run without it")]
    ConfirmationRequired,
    #[error("the preview was not shown yet, so it cannot be confirmed")]
    NotPresented,
    #[error("confirmed too quickly after the preview appeared; press the button again")]
    TooSoon,
    #[error("{0}")]
    Stale(String),
    #[error("{0}")]
    NothingToDo(String),
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("this action cannot be undone")]
    NotUndoable,
    #[error(transparent)]
    Log(#[from] undo_log::LogError),
}

/// One kind of work. Implementations must not change anything in `plan`.
pub trait Action: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn applies_to(&self, kind: &Kind) -> bool;

    /// Looks at the real state now and says what `execute` would do.
    fn plan(&self, target: &Target) -> Result<Plan, ActionError>;

    /// Does the work described by a plan the gate just re-checked.
    fn execute(&self, plan: &Plan, proof: &Authorized) -> Result<Executed, ActionError>;

    /// Describes what undoing `token` would do now.
    fn plan_undo(&self, _token: &str) -> Result<UndoPlan, ActionError> {
        Err(ActionError::NotUndoable)
    }

    fn undo(
        &self,
        _token: &str,
        _plan: &UndoPlan,
        _proof: &Authorized,
    ) -> Result<String, ActionError> {
        Err(ActionError::NotUndoable)
    }
}
