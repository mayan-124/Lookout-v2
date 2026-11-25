//! The wire format between the command line and the running app.
//! One request line in, one response line out, both JSON. Version 1.

use std::collections::VecDeque;
use std::str::FromStr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
/// Requests longer than this are refused.
pub const MAX_REQUEST_BYTES: usize = 4096;
/// Requests per second the app accepts.
pub const MAX_REQUESTS_PER_SEC: usize = 5;
pub const DEFAULT_LIMIT: usize = 10;
pub const MAX_LIMIT: usize = 50;

/// Which kind of result `--kind` keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindFilter {
    Files,
    Apps,
    Clipboard,
    Screens,
    Meaning,
}

impl FromStr for KindFilter {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "files" => Ok(Self::Files),
            "apps" => Ok(Self::Apps),
            "clipboard" => Ok(Self::Clipboard),
            "screens" => Ok(Self::Screens),
            "meaning" => Ok(Self::Meaning),
            other => Err(format!(
                "unknown kind '{other}' (use files, apps, clipboard, screens or meaning)"
            )),
        }
    }
}

/// Clamps a requested limit to `1..=MAX_LIMIT`; `None` means the default.
pub fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    /// `search` or `status`.
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<KindFilter>,
}

impl Request {
    pub fn search(query: &str, limit: Option<usize>, kind: Option<KindFilter>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            cmd: "search".into(),
            query: Some(query.to_owned()),
            limit,
            kind,
        }
    }

    pub fn status() -> Self {
        Self {
            v: PROTOCOL_VERSION,
            cmd: "status".into(),
            query: None,
            limit: None,
            kind: None,
        }
    }

    /// The request as one line, newline included.
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// One search result. Field names and order are the stable output of
/// `lookout search --json`. `modified` (Unix seconds) is left out when unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultLine {
    pub kind: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub path: Option<String>,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
}

/// Stable output of `lookout status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusLine {
    pub apps: usize,
    pub files: usize,
    pub folders: usize,
    pub indexing: bool,
    pub paused: bool,
    /// `indexing`, `paused` or `idle`.
    pub state: String,
}

/// Why the app said no.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    TooLarge,
    RateLimited,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Results {
        ok: bool,
        results: Vec<ResultLine>,
    },
    Status {
        ok: bool,
        status: StatusLine,
    },
    Error {
        ok: bool,
        code: ErrorCode,
        error: String,
    },
}

impl Response {
    pub fn results(results: Vec<ResultLine>) -> Self {
        Self::Results { ok: true, results }
    }
    pub fn status(status: StatusLine) -> Self {
        Self::Status { ok: true, status }
    }
    pub fn error(code: ErrorCode, error: impl Into<String>) -> Self {
        Self::Error {
            ok: false,
            code,
            error: error.into(),
        }
    }
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"ok":false,"code":"unavailable","error":"internal error"}"#.into()
        });
        s.push('\n');
        s
    }
}

/// What the app does for a request. Implemented by the app over its search.
pub trait Backend: Send + Sync {
    fn search(
        &self,
        query: &str,
        limit: usize,
        kind: Option<KindFilter>,
    ) -> Result<Vec<ResultLine>, String>;
    fn status(&self) -> Result<StatusLine, String>;
    /// Called once per request. The query text is never passed on, only its length.
    fn record(&self, cmd: &str, query_len: usize, ok: bool);
}

/// At most `max` requests in any `window`.
pub struct RateLimiter {
    max: usize,
    window: Duration,
    hits: VecDeque<Instant>,
}

impl RateLimiter {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: VecDeque::new(),
        }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .hits
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            self.hits.pop_front();
        }
        if self.hits.len() >= self.max {
            return false;
        }
        self.hits.push_back(now);
        true
    }
}

/// Turns one received request into a response. Size and rate limits are
/// checked here, so they hold on any transport.
pub struct Service<B: Backend> {
    backend: B,
    limiter: std::sync::Mutex<RateLimiter>,
}

impl<B: Backend> Service<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            limiter: std::sync::Mutex::new(RateLimiter::new(
                MAX_REQUESTS_PER_SEC,
                Duration::from_secs(1),
            )),
        }
    }

    /// `raw` is the request as received (without the trailing newline).
    pub fn handle(&self, raw: &[u8], now: Instant) -> Response {
        let allowed = self
            .limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .allow(now);
        if !allowed {
            self.backend.record("refused", raw.len(), false);
            return Response::error(
                ErrorCode::RateLimited,
                "too many requests; at most 5 per second",
            );
        }
        if raw.len() > MAX_REQUEST_BYTES {
            self.backend.record("refused", raw.len(), false);
            return Response::error(ErrorCode::TooLarge, "request is larger than 4 KB");
        }
        let Ok(req) = serde_json::from_slice::<Request>(raw) else {
            self.backend.record("invalid", raw.len(), false);
            return Response::error(ErrorCode::BadRequest, "could not read the request");
        };
        if req.v != PROTOCOL_VERSION {
            self.backend.record("invalid", raw.len(), false);
            return Response::error(ErrorCode::BadRequest, "unsupported protocol version");
        }
        match req.cmd.as_str() {
            "search" => {
                let query = req.query.unwrap_or_default();
                let q = query.trim();
                if q.is_empty() {
                    self.backend.record("search", 0, false);
                    return Response::error(ErrorCode::BadRequest, "the query is empty");
                }
                let out = self.backend.search(q, clamp_limit(req.limit), req.kind);
                self.backend.record("search", query.len(), out.is_ok());
                match out {
                    Ok(r) => Response::results(r),
                    Err(e) => Response::error(ErrorCode::Unavailable, e),
                }
            }
            "status" => {
                let out = self.backend.status();
                self.backend.record("status", 0, out.is_ok());
                match out {
                    Ok(s) => Response::status(s),
                    Err(e) => Response::error(ErrorCode::Unavailable, e),
                }
            }
            _ => {
                self.backend.record("invalid", raw.len(), false);
                Response::error(ErrorCode::BadRequest, "unknown command")
            }
        }
    }
}
