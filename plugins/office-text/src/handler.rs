//! The plugin protocol: `initialize` and `extract_office`.

use std::time::Instant;

use lookout_core::content::office::b64_decode;
use lookout_core::plugins::sdk::Handler;
use serde_json::{json, Value};

use crate::extract::{extract_office, Limits};

pub struct Helper;

impl Handler for Helper {
    fn handle(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "initialize" => Ok(json!({"ok": true})),
            "extract_office" => {
                let ext = params.get("ext").and_then(Value::as_str).unwrap_or("");
                let data = params.get("data").and_then(Value::as_str).unwrap_or("");
                let Some(bytes) = b64_decode(data) else {
                    return Ok(json!({"skipped": "the data was not valid base64"}));
                };
                let started = Instant::now();
                Ok(match extract_office(&bytes, ext, &Limits::default()) {
                    Ok(text) => json!({"text": text, "ms": started.elapsed().as_millis() as u64}),
                    Err(why) => json!({"skipped": why}),
                })
            }
            other => Err(format!("unknown method {other}")),
        }
    }
}
