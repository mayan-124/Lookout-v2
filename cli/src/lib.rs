//! `lookout` command line: wire format, argument parsing, and the named pipe
//! that connects it to the running app. Read-only; no network.

pub mod cmd;
pub mod proto;
#[cfg(windows)]
pub mod winpipe;

#[cfg(test)]
mod tests;
