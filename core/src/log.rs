//! A small rotating log of errors and warnings (`logs/` in Lookout's data folder).
//!
//! Only fixed sentences written in the source can be logged, plus the kind of
//! an I/O error and the source position of a panic. There is no way to pass it
//! a path, a search or file text.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// One file holds this much; with the previous one the log stays under 1 MB.
const MAX_FILE: u64 = 500 * 1024;
const NAME: &str = "lookout.log";
const OLD: &str = "lookout.old.log";

static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The log folder below Lookout's data folder.
pub fn folder(data: &Path) -> PathBuf {
    data.join("logs")
}

/// Starts writing into `dir`. Until then lines go to standard error only.
pub fn init(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    *DIR.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
}

pub fn warn(what: &'static str) {
    write("WARN", what, "");
}

pub fn error(what: &'static str) {
    write("ERROR", what, "");
}

/// Adds only the kind of the failure: its message may name a file.
pub fn error_io(what: &'static str, e: &io::Error) {
    write("ERROR", what, &format!(" ({:?})", e.kind()));
}

/// Records where a panic happened. Its message is dropped: it may quote data.
pub fn panic_at(file: &str, line: u32) {
    // Dependency paths hold the build machine's user folder: keep the file name only.
    let name = file.rsplit(['\\', '/']).next().unwrap_or_default();
    write("ERROR", "internal fault", &format!(" ({name} line {line})"));
}

/// Logs every panic, then calls `after`. Replaces the default hook.
pub fn install_panic_hook(after: impl Fn() + Send + Sync + 'static) {
    std::panic::set_hook(Box::new(move |info| {
        match info.location() {
            Some(l) => panic_at(l.file(), l.line()),
            None => error("internal fault"),
        }
        after();
    }));
}

fn write(level: &str, what: &str, detail: &str) {
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("{time} {level} {what}{detail}");
    // A closed standard error must never be a failure of its own.
    let _ = writeln!(io::stderr(), "lookout: {line}");
    let guard = DIR.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(dir) = guard.as_ref() {
        let _ = append(dir, &line, MAX_FILE);
    }
}

fn append(dir: &Path, line: &str, max: u64) -> io::Result<()> {
    let path = dir.join(NAME);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() + line.len() as u64 >= max) {
        std::fs::rename(&path, dir.join(OLD))?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

/// The newest `count` error lines, oldest first.
pub fn last_errors(dir: &Path, count: usize) -> Vec<String> {
    let mut lines: Vec<String> = [OLD, NAME]
        .iter()
        .filter_map(|n| std::fs::read(dir.join(n)).ok())
        .flat_map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                // A line holding a path was not written by this module.
                .filter(|l| l.contains(" ERROR ") && !l.contains(['\\', '/']))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    lines.drain(..lines.len().saturating_sub(count));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_rotates_and_stays_small() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..200 {
            append(dir.path(), &format!("t ERROR number {n}"), 1000).unwrap();
        }
        let total: u64 = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= 2000, "{total}");
        let last = last_errors(dir.path(), 20);
        assert_eq!(last.len(), 20);
        assert!(last[19].ends_with("number 199"));
    }
}
