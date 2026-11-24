//! A harmless process for the kill-port tests: listens on `127.0.0.1:<port>`
//! and does nothing, then exits by itself after two minutes so that it can
//! never be left running by a test that died.
//!
//! Usage: `test_listener <port> [label...]`

use std::net::TcpListener;
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let Some(port) = std::env::args().nth(1).and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("usage: test_listener <port> [label...]");
        return ExitCode::from(2);
    };
    let _listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("test_listener: cannot listen on {port}: {e}");
            return ExitCode::from(1);
        }
    };
    std::thread::sleep(Duration::from_secs(120));
    ExitCode::SUCCESS
}
