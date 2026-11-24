//! The JSON-RPC side of the file tools.

use std::path::{Path, PathBuf};

use lookout_core::plugins::sdk::Handler;
use serde_json::{json, Value};

use crate::parse::{self, human};
use crate::{metadata, rename};

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "jfif", "bmp", "gif", "tif", "tiff"];

pub struct FileTools;

fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_image(p: &Path) -> bool {
    IMAGE_EXT.contains(&ext(p).as_str())
}

/// `<stem> (<tag>).<ext>` next to the source, never an existing file.
pub fn unique_output(src: &Path, tag: &str, extension: &str) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = src.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut n = 1;
    loop {
        let label = if n == 1 {
            tag.to_owned()
        } else {
            format!("{tag} {n}")
        };
        let candidate = dir.join(format!("{stem} ({label}).{extension}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

fn regular_file(p: &Path) -> Result<u64, String> {
    let meta = std::fs::symlink_metadata(p).map_err(|_| format!("'{}' is gone", p.display()))?;
    if !meta.is_file() {
        return Err(format!("'{}' is not a plain file", p.display()));
    }
    Ok(meta.len())
}

enum Op {
    Compress {
        src: PathBuf,
        out: PathBuf,
        target: u64,
        size: u64,
    },
    Strip {
        src: PathBuf,
        out: PathBuf,
        size: u64,
        removed: usize,
    },
    Rename {
        folder: PathBuf,
        plan: rename::Plan,
    },
}

fn item_path(params: &Value) -> Result<PathBuf, String> {
    params["items"][0]["uri"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "no file was named".to_owned())
}

fn plan_op(action: &str, params: &Value) -> Result<Op, String> {
    let path = item_path(params)?;
    match action {
        "compress_image" => {
            let target = params["params"]["target_bytes"]
                .as_u64()
                .ok_or("no target size")?;
            let size = regular_file(&path)?;
            if !is_image(&path) {
                return Err(
                    "that is not a picture type this tool handles (png, jpg, bmp, gif, tif)".into(),
                );
            }
            if size <= target {
                return Err(format!(
                    "it is already {} (under {})",
                    human(size),
                    human(target)
                ));
            }
            Ok(Op::Compress {
                out: unique_output(&path, "compressed", "jpg"),
                src: path,
                target,
                size,
            })
        }
        "strip_metadata" => {
            let size = regular_file(&path)?;
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let (_, removed) = metadata::strip(&bytes)?;
            if removed == 0 {
                return Err("this picture has no metadata to remove".into());
            }
            let e = ext(&path);
            Ok(Op::Strip {
                out: unique_output(&path, "no metadata", &e),
                src: path,
                size,
                removed,
            })
        }
        "rename_files" => {
            let pattern = params["params"]["pattern"].as_str().ok_or("no pattern")?;
            if !path.is_dir() {
                return Err(format!("'{}' is not a folder", path.display()));
            }
            let plan = rename::plan(&path, pattern)?;
            Ok(Op::Rename { folder: path, plan })
        }
        other => Err(format!("unknown action {other}")),
    }
}

fn preview(op: &Op) -> Value {
    match op {
        Op::Compress {
            src,
            out,
            target,
            size,
        } => {
            let lossy = ext(src) != "jpg" && ext(src) != "jpeg" && ext(src) != "jfif";
            json!({
                "title": format!("Compress {} to under {}", name(src), human(*target)),
                "lines": [
                    format!("Now: {}", human(*size)),
                    format!("New file: {}", out.display()),
                    "The original is not changed.".to_owned(),
                ],
                "warnings": if lossy { vec!["The result is a JPEG; transparency is lost.".to_owned()] } else { vec![] },
                "undoable": true, "risk": "confirm",
            })
        }
        Op::Strip {
            src,
            out,
            size,
            removed,
        } => json!({
            "title": format!("Remove metadata from {}", name(src)),
            "lines": [
                format!("{} of metadata (location, camera, comments) will be left out; the picture itself is copied unchanged.", human(*removed as u64)),
                format!("Now: {}", human(*size)),
                format!("New file: {}", out.display()),
                "The original is not changed.".to_owned(),
            ],
            "warnings": [], "undoable": true, "risk": "confirm",
        }),
        Op::Rename { folder, plan } => {
            let mut lines: Vec<String> = plan
                .pairs
                .iter()
                .map(|(o, n)| format!("{}  →  {}", name(o), name(n)))
                .collect();
            if plan.unchanged > 0 {
                lines.push(format!(
                    "({} files already have the right name)",
                    plan.unchanged
                ));
            }
            json!({
                "title": format!("Rename {} files in {}", plan.pairs.len(), folder.display()),
                "lines": lines, "warnings": [], "undoable": true, "risk": "confirm",
            })
        }
    }
}

fn execute(op: &Op) -> Result<Value, String> {
    match op {
        Op::Compress {
            src, out, target, ..
        } => {
            let shrunk = crate::image::shrink(src, *target)?;
            let len = write_new(out, &shrunk.bytes)?;
            Ok(json!({
                "message": format!("Saved {} ({}, {}×{}, quality {:.0}%)", name(out), human(len),
                                   shrunk.width, shrunk.height, shrunk.quality * 100.0),
                "undo_token": json!({"created": out, "len": len}).to_string(),
            }))
        }
        Op::Strip { src, out, .. } => {
            let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
            let (clean, removed) = metadata::strip(&bytes)?;
            let len = write_new(out, &clean)?;
            Ok(json!({
                "message": format!("Saved {} ({} of metadata left out)", name(out), human(removed as u64)),
                "undo_token": json!({"created": out, "len": len}).to_string(),
            }))
        }
        Op::Rename { plan, .. } => {
            rename::apply(&plan.pairs)?;
            let back: Vec<[&PathBuf; 2]> = plan.pairs.iter().map(|(o, n)| [n, o]).collect();
            Ok(json!({
                "message": format!("Renamed {} files", plan.pairs.len()),
                "undo_token": json!({"pairs": back}).to_string(),
            }))
        }
    }
}

/// Creates `path` (it must not exist) and writes `bytes`.
fn write_new(path: &Path, bytes: &[u8]) -> Result<u64, String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("could not create {}: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    Ok(bytes.len() as u64)
}

fn undo(token: &str, dry_run: bool) -> Result<Value, String> {
    let t: Value =
        serde_json::from_str(token).map_err(|_| "the undo information is damaged".to_owned())?;
    if let Some(created) = t["created"].as_str() {
        let (path, len) = (
            PathBuf::from(created),
            t["len"].as_u64().unwrap_or(u64::MAX),
        );
        if regular_file(&path).ok() != Some(len) {
            return Err("the new file was changed or is gone; it is left alone".into());
        }
        if dry_run {
            return Ok(json!({"title": format!("Delete {}", name(&path)),
                             "lines": [format!("{}", path.display()), "Your original is not touched.".to_owned()],
                             "warnings": []}));
        }
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        return Ok(json!({"message": format!("Deleted {}", name(&path))}));
    }
    let pairs: Vec<(PathBuf, PathBuf)> = t["pairs"]
        .as_array()
        .ok_or("the undo information is damaged")?
        .iter()
        .filter_map(|p| Some((PathBuf::from(p[0].as_str()?), PathBuf::from(p[1].as_str()?))))
        .collect();
    if pairs.is_empty() {
        return Err("the undo information is damaged".into());
    }
    if dry_run {
        let sources: Vec<String> = pairs
            .iter()
            .map(|(o, _)| o.to_string_lossy().to_lowercase())
            .collect();
        for (old, new) in &pairs {
            if !old.is_file() {
                return Err(format!(
                    "'{}' is gone, so the renames cannot be reverted",
                    name(old)
                ));
            }
            if new.exists() && !sources.contains(&new.to_string_lossy().to_lowercase()) {
                return Err(format!(
                    "'{}' exists again, so the renames cannot be reverted",
                    name(new)
                ));
            }
        }
        let lines: Vec<String> = pairs
            .iter()
            .map(|(o, n)| format!("{}  →  {}", name(o), name(n)))
            .collect();
        return Ok(
            json!({"title": format!("Rename {} files back", pairs.len()), "lines": lines, "warnings": []}),
        );
    }
    rename::apply(&pairs)?;
    Ok(json!({"message": format!("Renamed {} files back", pairs.len())}))
}

fn query(params: &Value) -> Value {
    let verb = params["verb"].as_str().unwrap_or("");
    let rest = params["rest"].as_str().unwrap_or("");
    let candidates: Vec<(PathBuf, bool)> = params["candidates"]
        .as_array()
        .map(|c| {
            c.iter()
                .filter_map(|c| Some((PathBuf::from(c["path"].as_str()?), c["kind"] == "folder")))
                .collect()
        })
        .unwrap_or_default();
    let subject = match verb {
        "compress" => parse::compress(rest).map(|c| c.subject),
        "strip" => parse::strip(rest),
        "rename" => parse::rename(rest).map(|(s, _)| s),
        _ => None,
    };
    let Some(subject) = subject else {
        return json!({"items": []});
    };
    if candidates.is_empty() {
        return json!({"select": {"text": subject}});
    }
    let mut items = Vec::new();
    for (path, is_folder) in candidates.iter().take(5) {
        let uri = path.to_string_lossy();
        let parent = path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        match verb {
            "compress" if !is_folder && is_image(path) => {
                let Some(c) = parse::compress(rest) else {
                    continue;
                };
                let size = regular_file(path).map(human).unwrap_or_default();
                items.push(json!({
                    "id": format!("compress:{uri}"), "kind": "file", "uri": uri,
                    "title": format!("Compress {} to under {}", name(path), human(c.target_bytes)),
                    "subtitle": format!("{parent} · now {size}"),
                    "action": "compress_image", "params": {"target_bytes": c.target_bytes},
                }));
            }
            "strip"
                if !is_folder && matches!(ext(path).as_str(), "jpg" | "jpeg" | "jfif" | "png") =>
            {
                items.push(json!({
                    "id": format!("strip:{uri}"), "kind": "file", "uri": uri,
                    "title": format!("Remove metadata from {}", name(path)),
                    "subtitle": parent, "action": "strip_metadata",
                }));
            }
            "rename" if *is_folder => {
                let Some((_, pattern)) = parse::rename(rest) else {
                    continue;
                };
                // The live preview: what the first files would become right now.
                let subtitle = match rename::plan(path, &pattern) {
                    Ok(p) => {
                        let first: Vec<String> = p
                            .pairs
                            .iter()
                            .take(3)
                            .map(|(o, n)| format!("{} → {}", name(o), name(n)))
                            .collect();
                        let more = p.pairs.len().saturating_sub(3);
                        format!(
                            "{}{}",
                            first.join(", "),
                            if more > 0 {
                                format!(" (+{more} more)")
                            } else {
                                String::new()
                            }
                        )
                    }
                    Err(e) => e,
                };
                items.push(json!({
                    "id": format!("rename:{uri}:{pattern}"), "kind": "folder", "uri": uri,
                    "title": format!("Rename files in {} to “{pattern}”", name(path)),
                    "subtitle": subtitle,
                    "action": "rename_files", "params": {"pattern": pattern},
                }));
            }
            _ => {}
        }
    }
    json!({"items": items})
}

impl Handler for FileTools {
    fn handle(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "initialize" => Ok(json!({"protocol": 1})),
            "query" => Ok(query(&params)),
            "get_actions" => {
                let folder = params["items"][0]["kind"] == "folder";
                Ok(if folder {
                    json!({"actions": [{"id": "rename_files", "label": "Rename files", "risk": "confirm"}]})
                } else {
                    json!({"actions": [
                        {"id": "compress_image", "label": "Compress image", "risk": "confirm"},
                        {"id": "strip_metadata", "label": "Remove metadata", "risk": "confirm"},
                    ]})
                })
            }
            "preview" => {
                let action = params["action_id"].as_str().unwrap_or("");
                Ok(preview(&plan_op(action, &params)?))
            }
            "execute" => {
                let action = params["action_id"].as_str().unwrap_or("");
                execute(&plan_op(action, &params)?)
            }
            "undo" => undo(
                params["undo_token"].as_str().unwrap_or(""),
                params["dry_run"].as_bool().unwrap_or(true),
            ),
            other => Err(format!("unknown method {other}")),
        }
    }
}
