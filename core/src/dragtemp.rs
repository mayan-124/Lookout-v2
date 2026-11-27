//! The temp folder for pictures dragged out of the clipboard history. The files
//! in it are not encrypted (another program has to read them), so they are
//! short-lived: gone after [`MAX_AGE`] and when Lookout closes.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MAX_AGE: Duration = Duration::from_secs(60 * 60);

pub fn dir() -> PathBuf {
    std::env::temp_dir().join("Lookout-drag")
}

/// Removes files written `max_age` ago or longer.
pub fn sweep(dir: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age >= max_age));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Removes every file, then the folder.
pub fn clear(dir: &Path) {
    sweep(dir, Duration::ZERO);
    let _ = std::fs::remove_dir(dir);
}

/// Whether `path` is a file directly inside `dir`.
pub fn contains(dir: &Path, path: &Path) -> bool {
    path.parent() == Some(dir) && path.file_name().is_some() && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_files_go_new_ones_stay_and_clearing_removes_all() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("drag");
        std::fs::create_dir(&dir).unwrap();
        let (old, new) = (dir.join("old.png"), dir.join("new.png"));
        std::fs::write(&old, "o").unwrap();
        std::fs::write(&new, "n").unwrap();
        let past = std::time::SystemTime::now() - MAX_AGE - Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(past)
            .unwrap();
        sweep(&dir, MAX_AGE);
        assert!(!old.exists() && new.exists());
        assert!(contains(&dir, &new));
        assert!(!contains(&dir, &old));
        assert!(!contains(root.path(), &new));
        clear(&dir);
        assert!(!dir.exists());
    }
}
