//! First-run setup: the folders it offers.

use std::path::{Path, PathBuf};

/// One row of the setup's folder checklist.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FolderChoice {
    pub path: String,
    pub checked: bool,
}

fn same(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// The suggested folders that exist, then any other folder already chosen.
/// With nothing chosen yet every suggestion is ticked; otherwise exactly the
/// chosen folders are.
pub fn folder_choices(suggested: &[PathBuf], chosen: &[PathBuf]) -> Vec<FolderChoice> {
    let mut paths: Vec<&PathBuf> = Vec::new();
    for p in suggested.iter().filter(|p| p.is_dir()).chain(chosen) {
        if !paths.iter().any(|q| same(q, p)) {
            paths.push(p);
        }
    }
    paths
        .into_iter()
        .map(|p| FolderChoice {
            path: p.to_string_lossy().into_owned(),
            checked: chosen.is_empty() || chosen.iter().any(|c| same(c, p)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_folders_that_exist_are_suggested_and_all_are_ticked_at_first() {
        let dir = tempfile::tempdir().unwrap();
        let (docs, desk) = (dir.path().join("Documents"), dir.path().join("Desktop"));
        std::fs::create_dir(&docs).unwrap();
        std::fs::create_dir(&desk).unwrap();
        let file = dir.path().join("Downloads");
        std::fs::write(&file, "not a folder").unwrap();
        let suggested = [
            docs.clone(),
            desk.clone(),
            file,
            dir.path().join("Pictures"),
        ];
        let got = folder_choices(&suggested, &[]);
        let paths: Vec<_> = got.iter().map(|c| PathBuf::from(&c.path)).collect();
        assert_eq!(paths, [docs, desk]);
        assert!(got.iter().all(|c| c.checked));
    }

    #[test]
    fn running_setup_again_ticks_what_is_chosen_and_keeps_other_folders() {
        let dir = tempfile::tempdir().unwrap();
        let (docs, desk) = (dir.path().join("Documents"), dir.path().join("Desktop"));
        std::fs::create_dir(&docs).unwrap();
        std::fs::create_dir(&desk).unwrap();
        // Chosen: one suggestion in other letter case, and a drive that is away.
        let upper = PathBuf::from(docs.to_string_lossy().to_uppercase());
        let away = PathBuf::from(r"Q:\Archive");
        let got = folder_choices(&[docs.clone(), desk], &[upper, away.clone()]);
        let ticked: Vec<bool> = got.iter().map(|c| c.checked).collect();
        assert_eq!(ticked, [true, false, true]);
        assert_eq!(got.len(), 3, "no duplicate for the same folder");
        assert_eq!(PathBuf::from(&got[2].path), away);
    }
}
