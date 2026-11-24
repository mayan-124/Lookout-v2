//! Text of .docx, .xlsx, .pptx and .pdf files, read by a helper plugin.
//!
//! The daemon reads the file (read-only, at most [`MAX_OFFICE_BYTES`]) and
//! sends the bytes to the helper; the helper opens nothing. What comes back
//! is plain text that goes through `secrets::mask` like any other text.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::plugins::wire::WireError;
use crate::plugins::{HostError, PluginHost};

pub const PLUGIN_ID: &str = "com.lookout.office-text";
/// Larger files are skipped without being read.
pub const MAX_OFFICE_BYTES: u64 = 50 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// What the helper made of one file.
#[derive(Debug, PartialEq, Eq)]
pub enum OfficeOutcome {
    Text(String),
    /// Not indexed, for this reason (encrypted, too big, corrupt, ...).
    Skipped(String),
}

pub trait OfficeText: Send + Sync {
    /// False while the helper is off or not approved: files are left alone.
    fn available(&self) -> bool;
    /// `Err`: the helper could not be reached or died; try again another day.
    fn extract(&self, ext: &str, bytes: &[u8]) -> Result<OfficeOutcome, String>;
}

/// Starts the error of an `extract` call that ended with the helper dead
/// (garbled output included), as opposed to one that merely failed or timed
/// out. A timeout starts with [`TIMEOUT_PREFIX`] instead (the helper is killed).
pub const CRASH_PREFIX: &str = "helper crashed: ";

pub const TIMEOUT_PREFIX: &str = "helper timed out: ";

pub fn is_crash(err: &str) -> bool {
    err.starts_with(CRASH_PREFIX)
}

pub fn is_timeout(err: &str) -> bool {
    err.starts_with(TIMEOUT_PREFIX)
}

pub fn is_office_ext(ext: &str) -> bool {
    matches!(ext, "docx" | "xlsx" | "pptx" | "pdf")
}

pub fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Reaches the helper through the plugin host.
pub struct PluginOffice(pub Arc<PluginHost>);

impl OfficeText for PluginOffice {
    fn available(&self) -> bool {
        self.0.is_usable(PLUGIN_ID)
    }

    fn extract(&self, ext: &str, bytes: &[u8]) -> Result<OfficeOutcome, String> {
        let params = json!({"ext": ext, "data": b64_encode(bytes)});
        let reply = self
            .0
            .service_call(PLUGIN_ID, "extract_office", params, CALL_TIMEOUT, &|| false)
            .map_err(|e| match e {
                // The call timeout kills the helper too (see `PluginHost::call`).
                HostError::Wire(WireError::Rpc { .. }) | HostError::BadReply(_) => e.to_string(),
                HostError::Wire(WireError::Timeout) => format!("{TIMEOUT_PREFIX}{e}"),
                HostError::Wire(_) => format!("{CRASH_PREFIX}{e}"),
                _ => e.to_string(),
            })?;
        if let Some(why) = reply.get("skipped").and_then(Value::as_str) {
            return Ok(OfficeOutcome::Skipped(why.to_owned()));
        }
        Ok(OfficeOutcome::Text(
            reply
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ))
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for n in 0..20u8 {
            let data: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(b64_decode(&b64_encode(&data)).unwrap(), data, "{n}");
        }
        assert_eq!(b64_encode(b"Man"), "TWFu");
        assert_eq!(b64_encode(b"Ma"), "TWE=");
        assert!(b64_decode("a b").is_none());
    }
}
