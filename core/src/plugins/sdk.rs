//! The plugin side of the protocol: a read-eval-answer loop over stdio. The
//! bundled plugins and the test plugin use it; a third-party plugin may speak
//! the protocol by any other means.

use std::io::BufRead;

use serde_json::{json, Value};

pub trait Handler {
    /// Answers a request. `Err` becomes a JSON-RPC error.
    fn handle(&mut self, method: &str, params: Value) -> Result<Value, String>;

    /// A notification from the host (`pause_changed`); no answer is sent.
    fn notify(&mut self, _method: &str, _params: Value) {}
}

/// Tells the host that results may have changed (a notification).
pub fn items_changed() {
    println!("{}", json!({"jsonrpc": "2.0", "method": "items_changed"}));
}

/// Serves requests until the host sends `shutdown` or closes stdin.
pub fn serve(handler: &mut dyn Handler) {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = msg.get("id").cloned() else {
            handler.notify(method, params);
            continue;
        };
        let answer = if method == "shutdown" {
            json!({"jsonrpc": "2.0", "id": id, "result": null})
        } else {
            match handler.handle(method, params) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(message) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}})
                }
            }
        };
        println!("{answer}");
        if method == "shutdown" {
            return;
        }
    }
}
