//! Argument parsing and output for the `lookout` command. Pure: the pipe is
//! behind [`Transport`], so all of it is testable without the app.

use std::io::Write;

use crate::proto::{ErrorCode, KindFilter, Request, Response, ResultLine, StatusLine, MAX_LIMIT};

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_NOT_RUNNING: i32 = 2;
pub const EXIT_OFF: i32 = 3;
pub const EXIT_REFUSED: i32 = 4;

pub const NOT_RUNNING: &str = "Lookout is not running. Start it first.";
pub const ACCESS_OFF: &str = "Command-line access is off in Lookout settings.";

pub const HELP: &str = "lookout: search the running Lookout app from a terminal (read-only)

USAGE:
    lookout search <query...> [--limit N] [--json] [--kind KIND]
    lookout status [--json]
    lookout --version
    lookout --help

OPTIONS:
    --limit N     results to show, 1 to 50 (default 10)
    --json        one JSON object per line: kind, title, subtitle, path, score, modified
    --kind KIND   files, apps, clipboard, screens or meaning

The query understands the same date and kind words as the search bar, for
example: lookout search pdf from last week

Lookout must be running, and Settings > Privacy > Command line access must be on.
Exit codes: 0 ok, 1 error, 2 Lookout not running, 3 access off, 4 request refused.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Status {
        json: bool,
    },
    Search {
        query: String,
        limit: Option<usize>,
        json: bool,
        kind: Option<KindFilter>,
    },
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Err("missing command".into());
    };
    match first.as_str() {
        "--help" | "-h" | "help" => Ok(Command::Help),
        "--version" | "-V" | "version" => Ok(Command::Version),
        "status" => {
            let mut json = false;
            for a in &args[1..] {
                match a.as_str() {
                    "--json" => json = true,
                    other => return Err(format!("unexpected argument '{other}'")),
                }
            }
            Ok(Command::Status { json })
        }
        "search" => parse_search(&args[1..]),
        other => Err(format!("unknown command '{other}'")),
    }
}

fn parse_search(args: &[String]) -> Result<Command, String> {
    let (mut words, mut limit, mut json, mut kind) = (Vec::new(), None, false, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        match name {
            "--" => words.extend(it.by_ref().cloned()),
            "--json" if inline.is_none() => json = true,
            "--limit" => {
                let v = inline
                    .or_else(|| it.next().cloned())
                    .ok_or("--limit needs a number")?;
                let n: usize = v
                    .parse()
                    .map_err(|_| format!("--limit needs a number, not '{v}'"))?;
                if n == 0 {
                    return Err("--limit must be at least 1".into());
                }
                limit = Some(n.min(MAX_LIMIT));
            }
            "--kind" => {
                let v = inline
                    .or_else(|| it.next().cloned())
                    .ok_or("--kind needs a value")?;
                kind = Some(v.parse::<KindFilter>()?);
            }
            _ if name.starts_with("--") => return Err(format!("unknown option '{name}'")),
            _ => words.push(a.clone()),
        }
    }
    let query = words.join(" ");
    if query.trim().is_empty() {
        return Err("search needs a query".into());
    }
    Ok(Command::Search {
        query,
        limit,
        json,
        kind,
    })
}

/// Why a request could not be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    NotRunning,
    Off,
    Io(String),
}

/// Sends one request line and returns the response line.
pub trait Transport {
    fn call(&self, request_line: &str) -> Result<String, ConnectError>;
}

/// One line of plain output: kind, title, then the path or, without one, the subtitle.
pub fn plain_line(r: &ResultLine) -> String {
    let clean = |s: &str| s.replace(['\t', '\r', '\n'], " ");
    let tail = r.path.as_deref().or(r.subtitle.as_deref()).unwrap_or("");
    format!("{}\t{}\t{}", clean(&r.kind), clean(&r.title), clean(tail))
}

pub fn plain_status(s: &StatusLine) -> String {
    format!(
        "apps: {}\nfiles: {}\nfolders: {}\nindex: {}",
        s.apps, s.files, s.folders, s.state
    )
}

/// Runs the command line `args` (without the program name). Returns the exit code.
pub fn run(
    args: &[String],
    transport: &dyn Transport,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let cmd = match parse(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "lookout: {e}\nTry 'lookout --help'.");
            return EXIT_ERROR;
        }
    };
    let (request, json) = match cmd {
        Command::Help => {
            let _ = writeln!(out, "{HELP}");
            return EXIT_OK;
        }
        Command::Version => {
            let _ = writeln!(out, "lookout {}", env!("CARGO_PKG_VERSION"));
            return EXIT_OK;
        }
        Command::Status { json } => (Request::status(), json),
        Command::Search {
            query,
            limit,
            json,
            kind,
        } => (Request::search(&query, limit, kind), json),
    };
    let line = match transport.call(&request.to_line()) {
        Ok(l) => l,
        Err(ConnectError::NotRunning) => {
            let _ = writeln!(err, "{NOT_RUNNING}");
            return EXIT_NOT_RUNNING;
        }
        Err(ConnectError::Off) => {
            let _ = writeln!(err, "{ACCESS_OFF}");
            return EXIT_OFF;
        }
        Err(ConnectError::Io(e)) => {
            let _ = writeln!(err, "lookout: could not talk to Lookout: {e}");
            return EXIT_ERROR;
        }
    };
    match serde_json::from_str::<Response>(line.trim()) {
        Ok(Response::Results { results, .. }) => {
            for r in &results {
                let text = if json {
                    serde_json::to_string(r).unwrap_or_default()
                } else {
                    plain_line(r)
                };
                let _ = writeln!(out, "{text}");
            }
            EXIT_OK
        }
        Ok(Response::Status { status, .. }) => {
            let text = if json {
                serde_json::to_string(&status).unwrap_or_default()
            } else {
                plain_status(&status)
            };
            let _ = writeln!(out, "{text}");
            EXIT_OK
        }
        Ok(Response::Error { code, error, .. }) => {
            let _ = writeln!(err, "lookout: {error}");
            match code {
                ErrorCode::TooLarge | ErrorCode::RateLimited => EXIT_REFUSED,
                ErrorCode::BadRequest | ErrorCode::Unavailable => EXIT_ERROR,
            }
        }
        Err(_) => {
            let _ = writeln!(
                err,
                "lookout: Lookout sent an answer this version cannot read."
            );
            EXIT_ERROR
        }
    }
}
