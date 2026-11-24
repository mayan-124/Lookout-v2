//! The core's side of the helper's index API: typed calls over
//! [`PluginHost::service_call`]. Every error leaves the core working; callers
//! treat any `Err` as "no semantic results right now".

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::plugins::{HostError, PluginHost};

/// Id of the bundled helper's manifest.
pub const PLUGIN_ID: &str = "com.lookout.semantic";
/// Files the user places in `<plugin data>/assets/` (see `docs/semantic.md`).
pub const ASSET_FILES: [&str; 3] = ["model.onnx", "tokenizer.json", "onnxruntime.dll"];

#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error("{0}")]
    Host(#[from] HostError),
    #[error("the helper answered something unexpected: {0}")]
    Reply(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct HelperStatus {
    pub ready: bool,
    pub reason: Option<String>,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: String,
    pub similarity: f32,
}

/// A document handed to the helper: masked text only, never a file handle.
#[derive(Debug, Clone)]
pub struct SemDoc {
    pub path: String,
    pub name: String,
    pub mtime: u64,
    pub text: String,
}

#[derive(Clone)]
pub struct SemanticClient {
    host: Arc<PluginHost>,
    id: String,
    assets: PathBuf,
    data: PathBuf,
}

const BATCH_TIMEOUT: Duration = Duration::from_secs(30);
const QUICK_TIMEOUT: Duration = Duration::from_secs(10);
/// A cold search starts the process, checks the hashes and loads the model.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(8);

fn never() -> bool {
    false
}

impl SemanticClient {
    /// `data_root` is Lookout's data folder (the one `HostConfig::data_dir` names).
    pub fn new(host: Arc<PluginHost>, id: &str, data_root: &Path) -> Self {
        let data = data_root.join("plugins").join(id);
        Self {
            host,
            id: id.to_owned(),
            assets: super::setup::assets_dir(data_root, id),
            data,
        }
    }

    /// Where the user puts the model, tokenizer and runtime.
    pub fn assets_dir(&self) -> &Path {
        &self.assets
    }

    /// The plugin is turned on and its permissions accepted.
    pub fn usable(&self) -> bool {
        self.host.is_usable(&self.id)
    }

    /// A cheap check, without starting the helper, for the commonest reason
    /// nothing happens: the files are not there.
    pub fn missing_reason(&self) -> Option<String> {
        let missing: Vec<&str> = ASSET_FILES
            .iter()
            .copied()
            .filter(|f| !self.assets.join(f).is_file())
            .collect();
        (!missing.is_empty()).then(|| {
            format!(
                "Missing {} in {} (see docs/semantic.md)",
                missing.join(", "),
                self.assets.display()
            )
        })
    }

    fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, SemanticError> {
        Ok(self
            .host
            .service_call(&self.id, method, params, timeout, cancelled)?)
    }

    pub fn status(&self, cancelled: &dyn Fn() -> bool) -> Result<HelperStatus, SemanticError> {
        let v = self.call("index_status", Value::Null, SEARCH_TIMEOUT, cancelled)?;
        Ok(HelperStatus {
            ready: v.get("ready").and_then(Value::as_bool).unwrap_or(false),
            reason: v.get("reason").and_then(Value::as_str).map(str::to_owned),
            count: v.get("count").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    /// Which of these files have no up-to-date vector.
    pub fn needs(
        &self,
        items: &[(String, u64)],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<String>, SemanticError> {
        let list: Vec<Value> = items
            .iter()
            .map(|(p, m)| json!({"path": p, "mtime": m}))
            .collect();
        let v = self.call(
            "index_needs",
            json!({"items": list}),
            QUICK_TIMEOUT,
            cancelled,
        )?;
        strings(&v, "need")
    }

    /// Stores vectors for these documents. Returns how many were stored.
    pub fn index_batch(
        &self,
        docs: &[SemDoc],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<u64, SemanticError> {
        let list: Vec<Value> = docs
            .iter()
            .map(|d| json!({"path": d.path, "name": d.name, "mtime": d.mtime, "text": d.text}))
            .collect();
        let v = self.call(
            "index_batch",
            json!({"docs": list}),
            BATCH_TIMEOUT,
            cancelled,
        )?;
        Ok(v.get("stored").and_then(Value::as_u64).unwrap_or(0))
    }

    pub fn remove(&self, paths: &[String]) -> Result<(), SemanticError> {
        self.call(
            "index_remove",
            json!({"paths": paths}),
            QUICK_TIMEOUT,
            &never,
        )?;
        Ok(())
    }

    /// Indexed paths in ascending order, `limit` at a time, after `after`.
    pub fn list(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>, SemanticError> {
        let v = self.call(
            "index_list",
            json!({"after": after, "limit": limit}),
            QUICK_TIMEOUT,
            &never,
        )?;
        strings(&v, "paths")
    }

    /// Nearest files to `text`, best first. `cancelled` abandons the wait for
    /// the helper (the helper itself finishes what it started).
    pub fn search(
        &self,
        text: &str,
        limit: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<Hit>, SemanticError> {
        let v = self.call(
            "index_search",
            json!({"text": text, "limit": limit}),
            SEARCH_TIMEOUT,
            cancelled,
        )?;
        let list = v
            .get("hits")
            .and_then(Value::as_array)
            .ok_or_else(|| SemanticError::Reply("no hits".into()))?;
        Ok(list
            .iter()
            .filter_map(|h| {
                Some(Hit {
                    path: h.get("path")?.as_str()?.to_owned(),
                    similarity: h.get("score")?.as_f64()? as f32,
                })
            })
            .collect())
    }

    /// "Delete my index": the helper forgets every vector, and whatever file
    /// is left on disk is removed too (the helper may be off or broken).
    pub fn delete_all(&self) -> std::io::Result<()> {
        let _ = self.call("index_delete", Value::Null, QUICK_TIMEOUT, &never);
        for name in ["vectors.log", "vectors.log.tmp"] {
            match std::fs::remove_file(self.data.join(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

fn strings(v: &Value, key: &str) -> Result<Vec<String>, SemanticError> {
    Ok(v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| SemanticError::Reply(format!("no {key}")))?
        .iter()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect())
}
