//! A plugin used only by tests. `--mode` selects how it misbehaves (crash,
//! hang, garbage, rogue ...); `--marker <file>` is what its `execute` creates,
//! so a test can tell whether the action really ran.

use lookout_core::plugins::sdk::{serve, Handler};
use serde_json::{json, Value};

struct TestPlugin {
    mode: String,
    marker: String,
    state: String,
}

fn hang() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

impl Handler for TestPlugin {
    fn handle(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "initialize" => {
                if self.mode == "crash_init" {
                    std::process::exit(2);
                }
                if self.mode == "hang_init" {
                    hang();
                }
                Ok(json!({"protocol": 1}))
            }
            "query" => {
                match self.mode.as_str() {
                    "crash" => std::process::exit(3),
                    "crash_once" => {
                        if !std::path::Path::new(&self.state).exists() {
                            let _ = std::fs::write(&self.state, "x");
                            std::process::exit(3);
                        }
                    }
                    "hang" => hang(),
                    "garbage" => {
                        println!("this is {{ not json");
                        return Ok(Value::Null);
                    }
                    "huge" => {
                        let blob = "x".repeat(5 * 1024 * 1024);
                        println!("{blob}");
                        return Ok(Value::Null);
                    }
                    "rogue" => {
                        println!(r#"{{"jsonrpc":"2.0","id":99,"method":"execute","params":{{}}}}"#);
                        println!(r#"{{"jsonrpc":"2.0","method":"execute","params":{{}}}}"#);
                        println!(r#"{{"jsonrpc":"2.0","method":"run_action","params":{{}}}}"#);
                    }
                    "info" => {
                        let child = std::process::Command::new("cmd")
                            .args(["/C", "exit"])
                            .spawn();
                        let title = format!(
                            "pid={} child={}",
                            std::process::id(),
                            if child.is_ok() { "ok" } else { "refused" }
                        );
                        return Ok(json!({"items": [item(&title)]}));
                    }
                    "foreign_path" => {
                        return Ok(json!({"items": [
                            {"id": "1", "title": "x", "kind": "file", "uri": "C:\\Windows\\win.ini", "action": "touch"}
                        ]}));
                    }
                    _ => {}
                }
                let rest = params.get("rest").and_then(Value::as_str).unwrap_or("");
                if self.mode == "select"
                    && params["candidates"].as_array().is_none_or(Vec::is_empty)
                {
                    return Ok(json!({"select": {"text": rest, "kind": "file"}}));
                }
                if self.mode == "select" {
                    let items: Vec<Value> = params["candidates"]
                        .as_array()
                        .map(|c| {
                            c.iter()
                                .map(|c| {
                                    json!({"id": c["path"], "title": c["path"], "kind": "file",
                                           "uri": c["path"], "action": "touch"})
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    return Ok(json!({"items": items}));
                }
                Ok(json!({"items": [item("Touch marker")]}))
            }
            "get_actions" => Ok(json!({"actions": [
                {"id": "touch", "label": "Touch marker", "risk": "safe"}
            ]})),
            "preview" => {
                if self.mode == "preview_hang" {
                    hang();
                }
                Ok(
                    json!({"title": "Touch marker", "lines": [format!("create {}", self.marker)],
                          "undoable": true, "risk": "safe"}),
                )
            }
            "execute" => {
                std::fs::write(&self.marker, "ran").map_err(|e| e.to_string())?;
                Ok(json!({"message": "touched", "undo_token": "tok"}))
            }
            "undo" => {
                if params["dry_run"].as_bool() == Some(true) {
                    return Ok(json!({"title": "Remove marker", "lines": []}));
                }
                let _ = std::fs::remove_file(&self.marker);
                Ok(json!({"message": "removed"}))
            }
            m if m.starts_with("index_") => match (m, self.mode.as_str()) {
                ("index_search", "index_crash") => std::process::exit(4),
                ("index_search", "index_hang") => hang(),
                ("index_search", "index_hog") => {
                    // Touches 400 MB; a job memory limit below that kills the process.
                    let mut v = vec![0u8; 400 << 20];
                    for i in (0..v.len()).step_by(4096) {
                        v[i] = 1;
                    }
                    Ok(json!({"hits": [], "touched": v.iter().filter(|b| **b == 1).count()}))
                }
                ("index_search", _) => Ok(json!({"hits": [{"path": "C:/x/a.txt", "score": 0.9}]})),
                ("index_status", _) => {
                    Ok(json!({"ready": false, "reason": "model missing", "count": 0}))
                }
                ("index_needs", _) => Ok(json!({"need": []})),
                ("index_list", _) => Ok(json!({"paths": []})),
                _ => Ok(json!({})),
            },
            _ => Err(format!("unknown method {method}")),
        }
    }
}

fn item(title: &str) -> Value {
    json!({"id": "1", "title": title, "kind": "custom", "action": "touch", "params": {"n": 1}})
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default()
    };
    let mut plugin = TestPlugin {
        mode: arg("--mode"),
        marker: arg("--marker"),
        state: arg("--state"),
    };
    serve(&mut plugin);
}
