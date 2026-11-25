use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cmd::{self, Command, ConnectError, Transport};
use crate::proto::*;

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

// ---------------------------------------------------------------- arguments

#[test]
fn search_collects_words_and_flags_in_any_order() {
    let c = cmd::parse(&args("search --json budget report --limit 5 --kind files")).unwrap();
    assert_eq!(
        c,
        Command::Search {
            query: "budget report".into(),
            limit: Some(5),
            json: true,
            kind: Some(KindFilter::Files),
        }
    );
    let c = cmd::parse(&args("search pdf from last week --limit=3")).unwrap();
    assert!(
        matches!(c, Command::Search { ref query, limit: Some(3), .. } if query == "pdf from last week")
    );
}

#[test]
fn double_dash_ends_options() {
    let c = cmd::parse(&args("search -- --json x")).unwrap();
    assert!(matches!(c, Command::Search { ref query, json: false, .. } if query == "--json x"));
}

#[test]
fn bad_arguments_are_errors() {
    for bad in [
        "",
        "frobnicate",
        "search",
        "search --json",
        "search x --limit",
        "search x --limit abc",
        "search x --limit 0",
        "search x --kind banana",
        "search x --nope",
        "status extra",
    ] {
        assert!(cmd::parse(&args(bad)).is_err(), "{bad:?} should be refused");
    }
}

#[test]
fn help_and_version_parse() {
    assert_eq!(cmd::parse(&args("--help")).unwrap(), Command::Help);
    assert_eq!(cmd::parse(&args("--version")).unwrap(), Command::Version);
    assert_eq!(
        cmd::parse(&args("status --json")).unwrap(),
        Command::Status { json: true }
    );
}

#[test]
fn limits_are_capped() {
    assert_eq!(clamp_limit(None), 10);
    assert_eq!(clamp_limit(Some(1)), 1);
    assert_eq!(clamp_limit(Some(50)), 50);
    assert_eq!(clamp_limit(Some(5000)), 50);
    assert_eq!(clamp_limit(Some(0)), 1);
    let c = cmd::parse(&args("search x --limit 999")).unwrap();
    assert!(matches!(
        c,
        Command::Search {
            limit: Some(50),
            ..
        }
    ));
}

// ------------------------------------------------------------------- output

struct Canned(Result<String, ConnectError>);

impl Transport for Canned {
    fn call(&self, _: &str) -> Result<String, ConnectError> {
        self.0.clone()
    }
}

fn run_with(t: &dyn Transport, line: &str) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cmd::run(&args(line), t, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn sample() -> Vec<ResultLine> {
    vec![
        ResultLine {
            kind: "file".into(),
            title: "budget.xlsx".into(),
            subtitle: Some("C:\\docs\\budget.xlsx".into()),
            path: Some("C:\\docs\\budget.xlsx".into()),
            score: 1.5,
            modified: Some(1_700_000_000),
        },
        ResultLine {
            kind: "clipboard".into(),
            title: "tab\tand\nnewline".into(),
            subtitle: None,
            path: None,
            score: 0.25,
            modified: None,
        },
    ]
}

#[test]
fn json_fields_are_stable() {
    let t = Canned(Ok(Response::results(sample()).to_line()));
    let (code, out, _) = run_with(&t, "search budget --json");
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        r#"{"kind":"file","title":"budget.xlsx","subtitle":"C:\\docs\\budget.xlsx","path":"C:\\docs\\budget.xlsx","score":1.5,"modified":1700000000}"#
    );
    // unknown modified time: the field is left out; subtitle and path stay, as null
    assert_eq!(
        lines[1],
        r#"{"kind":"clipboard","title":"tab\tand\nnewline","subtitle":null,"path":null,"score":0.25}"#
    );
}

#[test]
fn status_json_fields_are_stable() {
    let s = StatusLine {
        apps: 3,
        files: 40,
        folders: 5,
        indexing: false,
        paused: false,
        state: "idle".into(),
    };
    let t = Canned(Ok(Response::status(s).to_line()));
    let (code, out, _) = run_with(&t, "status --json");
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        r#"{"apps":3,"files":40,"folders":5,"indexing":false,"paused":false,"state":"idle"}"#
    );
    let (_, plain, _) = run_with(&t, "status");
    assert_eq!(plain, "apps: 3\nfiles: 40\nfolders: 5\nindex: idle\n");
}

#[test]
fn plain_output_is_one_line_per_result() {
    let t = Canned(Ok(Response::results(sample()).to_line()));
    let (_, out, _) = run_with(&t, "search budget");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "file\tbudget.xlsx\tC:\\docs\\budget.xlsx");
    assert_eq!(lines[1], "clipboard\ttab and newline\t");
}

#[test]
fn exit_codes_and_messages() {
    let (c, _, e) = run_with(&Canned(Err(ConnectError::NotRunning)), "status");
    assert_eq!(
        (c, e.trim()),
        (2, "Lookout is not running. Start it first.")
    );
    let (c, _, e) = run_with(&Canned(Err(ConnectError::Off)), "status");
    assert_eq!(
        (c, e.trim()),
        (3, "Command-line access is off in Lookout settings.")
    );
    let refused = Response::error(ErrorCode::RateLimited, "slow down").to_line();
    assert_eq!(run_with(&Canned(Ok(refused)), "status").0, 4);
    assert_eq!(run_with(&Canned(Err(ConnectError::Off)), "bogus").0, 1);
    let (c, out, _) = run_with(&Canned(Err(ConnectError::Off)), "--version");
    assert_eq!(c, 0);
    assert!(out.starts_with("lookout "));
    let (c, out, _) = run_with(&Canned(Err(ConnectError::Off)), "--help");
    assert_eq!(c, 0);
    assert!(out.contains("--limit"));
}

// ------------------------------------------------------ limits in the service

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<(String, usize, Option<KindFilter>)>>,
    log: Mutex<Vec<(String, usize, bool)>>,
}

impl Backend for Arc<Fake> {
    fn search(
        &self,
        query: &str,
        limit: usize,
        kind: Option<KindFilter>,
    ) -> Result<Vec<ResultLine>, String> {
        self.seen.lock().unwrap().push((query.into(), limit, kind));
        Ok(sample().into_iter().take(limit).collect())
    }
    fn status(&self) -> Result<StatusLine, String> {
        Err("no search".into())
    }
    fn record(&self, cmd: &str, query_len: usize, ok: bool) {
        self.log.lock().unwrap().push((cmd.into(), query_len, ok));
    }
}

#[test]
fn service_caps_the_limit_and_logs_only_the_length() {
    let fake = Arc::new(Fake::default());
    let svc = Service::new(fake.clone());
    let now = Instant::now();
    let req = Request::search("secret words", Some(9999), Some(KindFilter::Apps)).to_line();
    let r = svc.handle(req.trim().as_bytes(), now);
    assert!(matches!(r, Response::Results { .. }));
    assert_eq!(
        fake.seen.lock().unwrap()[0],
        ("secret words".into(), 50, Some(KindFilter::Apps))
    );
    let log = fake.log.lock().unwrap();
    assert_eq!(log[0], ("search".to_owned(), 12, true));
    assert!(!format!("{log:?}").contains("secret"));
}

#[test]
fn service_refuses_big_requests_and_bursts() {
    let fake = Arc::new(Fake::default());
    let svc = Service::new(fake.clone());
    let t0 = Instant::now();
    let big = vec![b'a'; MAX_REQUEST_BYTES + 1];
    assert!(matches!(
        svc.handle(&big, t0),
        Response::Error {
            code: ErrorCode::TooLarge,
            ..
        }
    ));
    assert!(fake.seen.lock().unwrap().is_empty());

    // 1 request used above; four more are fine, the sixth in the same second is refused
    let req = Request::status().to_line();
    for _ in 0..4 {
        assert!(!matches!(
            svc.handle(req.trim().as_bytes(), t0),
            Response::Error {
                code: ErrorCode::RateLimited,
                ..
            }
        ));
    }
    assert!(matches!(
        svc.handle(req.trim().as_bytes(), t0),
        Response::Error {
            code: ErrorCode::RateLimited,
            ..
        }
    ));
    // a second later it works again
    assert!(!matches!(
        svc.handle(req.trim().as_bytes(), t0 + Duration::from_millis(1100)),
        Response::Error {
            code: ErrorCode::RateLimited,
            ..
        }
    ));
}

#[test]
fn exactly_4096_bytes_is_allowed_but_garbage_is_a_bad_request() {
    let svc = Service::new(Arc::new(Fake::default()));
    let now = Instant::now();
    let at_limit = vec![b'x'; MAX_REQUEST_BYTES];
    assert!(matches!(
        svc.handle(&at_limit, now),
        Response::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    assert!(matches!(
        svc.handle(br#"{"v":1,"cmd":"search","query":"  "}"#, now),
        Response::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    assert!(matches!(
        svc.handle(br#"{"v":1,"cmd":"delete"}"#, now),
        Response::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
}

// ---------------------------------------------------------------- the pipe

#[cfg(windows)]
mod pipe {
    use super::*;
    use crate::winpipe::{self, PipeTransport};

    fn unique(tag: &str) -> (String, String) {
        let id = format!("{}-{tag}-{:?}", std::process::id(), Instant::now());
        let id: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
        (
            format!(r"\\.\pipe\lookout-test-{id}"),
            format!(r"Local\lookout-test-running-{id}"),
        )
    }

    fn transport(p: &(String, String)) -> PipeTransport {
        PipeTransport {
            pipe: p.0.clone(),
            marker: p.1.clone(),
        }
    }

    #[test]
    fn not_running_then_off_then_running() {
        let names = unique("states");
        let t = transport(&names);
        let req = Request::status().to_line();

        // nothing running: no pipe, no marker
        assert_eq!(t.call(&req), Err(ConnectError::NotRunning));

        // app running, setting off: marker held, no pipe
        let marker = winpipe::mark_running(&names.1).unwrap();
        assert_eq!(t.call(&req), Err(ConnectError::Off));

        // setting on: the pipe exists and answers
        let fake = Arc::new(Fake::default());
        let listener = winpipe::listen(&names.0, fake.clone()).unwrap();
        let line = t
            .call(&Request::search("hello", Some(1), None).to_line())
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(&line).unwrap(),
            Response::Results { ref results, .. } if results.len() == 1
        ));
        // the whole command works through the pipe
        let (code, out, _) = run_with(&t, "search hello --limit 1 --json");
        assert_eq!(code, 0);
        assert!(out.starts_with(r#"{"kind":"file""#));

        // setting off again: the pipe is gone
        listener.stop();
        assert_eq!(t.call(&req), Err(ConnectError::Off));
        drop(marker);
        assert_eq!(t.call(&req), Err(ConnectError::NotRunning));
    }

    #[test]
    fn pipe_access_list_names_only_the_current_user() {
        let names = unique("acl");
        let _listener = winpipe::listen(&names.0, Arc::new(Fake::default())).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&names.0)
            .unwrap();
        let sddl = winpipe::dacl_sddl_of(&file).unwrap();
        let sid = winpipe::current_user_sid().unwrap();
        assert_eq!(
            sddl.matches("(A;").count(),
            1,
            "one allow entry only: {sddl}"
        );
        // SDDL abbreviates well-known account SIDs: the built-in local Administrator
        // (RID 500, the account a CI runner uses) is written "LA".
        let trustee = if sid.ends_with("-500") {
            "LA"
        } else {
            sid.as_str()
        };
        assert!(
            sddl.contains(&format!(";;;{trustee})")),
            "{sddl} (user {sid})"
        );
        assert!(
            sddl.starts_with("D:P"),
            "protected, nothing inherited: {sddl}"
        );
        assert!(!sddl.contains("(D;") && !sddl.contains(";;;WD)") && !sddl.contains(";;;AU)"));
    }

    #[test]
    fn a_second_pipe_of_the_same_name_is_refused() {
        let names = unique("dup");
        let _a = winpipe::listen(&names.0, Arc::new(Fake::default())).unwrap();
        assert!(winpipe::listen(&names.0, Arc::new(Fake::default())).is_err());
    }

    #[test]
    fn size_and_rate_limits_hold_over_the_pipe() {
        let names = unique("limits");
        let t = transport(&names);
        let _l = winpipe::listen(&names.0, Arc::new(Fake::default())).unwrap();

        let mut big = format!(r#"{{"v":1,"cmd":"search","query":"{}"}}"#, "a".repeat(5000));
        big.push('\n');
        let r: Response = serde_json::from_str(&t.call(&big).unwrap()).unwrap();
        assert!(matches!(
            r,
            Response::Error {
                code: ErrorCode::TooLarge,
                ..
            }
        ));

        // that was request 1; the pipe handles back-to-back requests, then refuses the sixth
        let ok = Request::search("x", None, None).to_line();
        let mut codes = Vec::new();
        for _ in 0..5 {
            let r: Response = serde_json::from_str(&t.call(&ok).unwrap()).unwrap();
            codes.push(matches!(
                r,
                Response::Error {
                    code: ErrorCode::RateLimited,
                    ..
                }
            ));
        }
        assert_eq!(codes, [false, false, false, false, true]);
    }

    #[test]
    fn a_silent_client_does_not_block_the_next_one() {
        let names = unique("silent");
        let t = transport(&names);
        let _l = winpipe::listen(&names.0, Arc::new(Fake::default())).unwrap();
        let _idle = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&names.0)
            .unwrap();
        let started = Instant::now();
        let line = t.call(&Request::search("x", None, None).to_line()).unwrap();
        assert!(line.contains(r#""ok":true"#));
        assert!(started.elapsed() < Duration::from_secs(6));
    }
}
