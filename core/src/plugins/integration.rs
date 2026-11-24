//! Plugins as ordinary sources and actions: results come from [`PluginSource`],
//! and every action goes through the same gate and undo log as the built-in
//! ones ([`PluginActions`] is the gate's provider).

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};

use super::host::{split_action_id, Candidate, HostError, PluginHost};
use crate::actions::gate::Authorized;
use crate::actions::{
    Action, ActionError, ActionProvider, Executed, Plan, Preview, Target, UndoPlan,
};
use crate::item::{Item, Kind};
use crate::query::{self, ParsedQuery};
use crate::sources::{QueryContext, Source, Tier};

fn to_action_error(e: HostError) -> ActionError {
    match e {
        HostError::Disabled | HostError::NotApproved | HostError::Unknown => {
            ActionError::Refused(e.to_string())
        }
        other => ActionError::Failed(other.to_string()),
    }
}

fn fnv(parts: &[&str]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0xff)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Gives the gate the actions of every usable plugin.
pub struct PluginActions(pub Arc<PluginHost>);

impl ActionProvider for PluginActions {
    fn find(&self, id: &str) -> Option<Arc<dyn Action>> {
        let (plugin, action) = split_action_id(id)?;
        if !self.0.is_usable(plugin) {
            return None;
        }
        Some(Arc::new(PluginAction {
            host: self.0.clone(),
            plugin: plugin.to_owned(),
            action: action.to_owned(),
            full: id.to_owned(),
            label: OnceLock::new(),
        }))
    }
}

struct PluginAction {
    host: Arc<PluginHost>,
    plugin: String,
    action: String,
    full: String,
    label: OnceLock<String>,
}

impl Action for PluginAction {
    fn id(&self) -> &str {
        &self.full
    }

    fn label(&self) -> &str {
        self.label.get_or_init(|| {
            self.host
                .label_of(&self.plugin, &self.action)
                .unwrap_or_else(|| self.action.replace('_', " "))
        })
    }

    fn applies_to(&self, _kind: &Kind) -> bool {
        true
    }

    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        // Only an item this plugin itself produced, for this very action.
        let origin = target
            .origin
            .as_ref()
            .filter(|o| o.plugin_id == self.plugin && o.action_id == self.action)
            .ok_or_else(|| ActionError::NotApplicable {
                action: self.full.clone(),
            })?;
        let item = json!({
            "id": target.item_id,
            "kind": target.kind,
            "uri": target.uri,
        });
        let reply = self
            .host
            .plan(&self.plugin, &self.action, &item, &origin.params)
            .map_err(to_action_error)?;
        let fingerprint = fnv(&[
            &format!("{:?}", reply.risk),
            &reply.title,
            &reply.lines.join("\n"),
            &reply.warnings.join("\n"),
        ]);
        Ok(Plan {
            risk: reply.risk,
            preview: Preview {
                from: Some(self.host.from_line(&self.plugin)),
                title: reply.title,
                lines: reply.lines,
                warnings: reply.warnings,
            },
            undoable: reply.undoable,
            snapshot: json!({"plugin": self.plugin, "action": self.action,
                             "item": item, "params": origin.params}),
            fingerprint,
        })
    }

    fn execute(&self, plan: &Plan, proof: &Authorized) -> Result<Executed, ActionError> {
        let empty = Value::Null;
        let item = plan.snapshot.get("item").unwrap_or(&empty);
        let params = plan.snapshot.get("params").unwrap_or(&empty);
        let done = self
            .host
            .execute(&self.plugin, &self.action, item, params, proof)
            .map_err(to_action_error)?;
        Ok(Executed {
            message: done.message,
            undo_token: done.undo_token,
        })
    }

    fn plan_undo(&self, token: &str) -> Result<UndoPlan, ActionError> {
        let r = self
            .host
            .plan_undo(&self.plugin, token)
            .map_err(to_action_error)?;
        let fingerprint = fnv(&[&r.title, &r.lines.join("\n"), &r.warnings.join("\n")]);
        Ok(UndoPlan {
            risk: r.risk,
            preview: Preview {
                from: Some(self.host.from_line(&self.plugin)),
                title: r.title,
                lines: r.lines,
                warnings: r.warnings,
            },
            fingerprint,
        })
    }

    fn undo(
        &self,
        token: &str,
        _plan: &UndoPlan,
        proof: &Authorized,
    ) -> Result<String, ActionError> {
        self.host
            .undo(&self.plugin, token, proof)
            .map_err(to_action_error)
    }
}

/// Routes a query whose first word is one of a plugin's triggers to it.
pub struct PluginSource {
    host: Arc<PluginHost>,
    plugin: String,
    source_id: &'static str,
    /// Finds files by name for plugins that act on files the user picks.
    files: Option<Arc<dyn Source>>,
}

impl PluginSource {
    pub fn new(
        host: Arc<PluginHost>,
        plugin: &str,
        files: Option<Arc<dyn Source>>,
    ) -> Option<Self> {
        let source_id = host.source_id(plugin)?;
        Some(Self {
            host,
            plugin: plugin.to_owned(),
            source_id,
            files,
        })
    }
}

impl Source for PluginSource {
    fn id(&self) -> &'static str {
        self.source_id
    }

    fn tier(&self) -> Tier {
        Tier::Smart
    }

    fn query(&self, q: &ParsedQuery, ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        let raw = q.raw.trim();
        let (verb, rest) = match raw.split_once(char::is_whitespace) {
            Some((v, r)) => (v.to_lowercase(), r.trim()),
            None => (raw.to_lowercase(), ""),
        };
        let wanted = self
            .host
            .manifest(&self.plugin)
            .is_some_and(|m| m.triggers.iter().any(|t| t.eq_ignore_ascii_case(&verb)));
        if !wanted || !self.host.is_usable(&self.plugin) || ctx.is_cancelled() {
            return;
        }
        let search = |text: &str| -> Vec<Candidate> {
            let Some(files) = &self.files else {
                return Vec::new();
            };
            let parsed = query::parse(text);
            let mut found = Vec::new();
            files.query(&parsed, ctx, &mut |items| {
                found = items
                    .into_iter()
                    .filter(|i| matches!(i.kind, Kind::File | Kind::Folder))
                    .filter_map(|i| {
                        Some(Candidate {
                            path: i.uri?,
                            is_folder: i.kind == Kind::Folder,
                        })
                    })
                    .collect();
            });
            found
        };
        if let Ok(items) = self.host.query(&self.plugin, raw, &verb, rest, &search) {
            if !items.is_empty() && !ctx.is_cancelled() {
                emit(items);
            }
        }
    }
}
