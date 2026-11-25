//! Command-line access: answers `lookout search` and `lookout status` from the
//! running app (see the `lookout-cli` crate for the pipe). Read-only: nothing
//! here opens a file, writes the clipboard or runs an action.

use std::sync::Arc;

use lookout_cli::proto::{Backend, KindFilter, ResultLine, StatusLine};

use crate::content::secrets;
use crate::item::{Item, Kind};
use crate::lookout::Lookout;

/// Gives the current search state, or `None` while it is not running.
pub type Current = Arc<dyn Fn() -> Option<Arc<Lookout>> + Send + Sync>;

pub struct CliBackend {
    current: Current,
}

impl CliBackend {
    pub fn new(current: Current) -> Self {
        Self { current }
    }

    fn lookout(&self) -> Result<Arc<Lookout>, String> {
        (self.current)().ok_or_else(|| "Lookout search is not available right now.".to_owned())
    }
}

fn kind_name(kind: &Kind) -> String {
    match kind {
        Kind::File => "file".into(),
        Kind::Folder => "folder".into(),
        Kind::App => "app".into(),
        Kind::Clipboard => "clipboard".into(),
        Kind::Screenshot => "screenshot".into(),
        Kind::Process => "process".into(),
        Kind::Port => "port".into(),
        Kind::Project => "project".into(),
        Kind::Snippet => "snippet".into(),
        Kind::Custom(name) => name.clone(),
    }
}

/// The stable output line for a result. Text that is not a file name or path
/// (clipboard entries, answers, plugin results) goes through the secret mask
/// once more, so a secret can never be printed even if a source missed it.
pub fn to_line(item: &Item) -> ResultLine {
    let is_file_like = matches!(item.kind, Kind::File | Kind::Folder | Kind::App);
    let clean = |s: &str| {
        if is_file_like {
            s.to_owned()
        } else {
            secrets::mask(s).0.into_owned()
        }
    };
    let path = match item.kind {
        Kind::File | Kind::Folder | Kind::Screenshot => item.uri.clone(),
        _ => None,
    };
    ResultLine {
        kind: kind_name(&item.kind),
        title: clean(&item.title),
        subtitle: item.subtitle.as_deref().map(clean),
        path,
        score: (f64::from(item.score) * 1000.0).round() / 1000.0,
        modified: item.modified_at,
    }
}

impl Backend for CliBackend {
    fn search(
        &self,
        query: &str,
        limit: usize,
        kind: Option<KindFilter>,
    ) -> Result<Vec<ResultLine>, String> {
        let l = self.lookout()?;
        Ok(l.cli_search(query, kind, limit)
            .iter()
            .map(to_line)
            .collect())
    }

    fn status(&self) -> Result<StatusLine, String> {
        let s = self.lookout()?.status();
        let state = if s.paused {
            "paused"
        } else if s.indexing {
            "indexing"
        } else {
            "idle"
        };
        Ok(StatusLine {
            apps: s.apps,
            files: s.files,
            folders: s.folders,
            indexing: s.indexing,
            paused: s.paused,
            state: state.into(),
        })
    }

    fn record(&self, cmd: &str, query_len: usize, ok: bool) {
        if let Ok(l) = self.lookout() {
            l.record_cli(cmd, query_len, ok);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_text_is_masked_and_files_are_left_alone() {
        let mut clip = Item::new(
            "clip:1",
            Kind::Clipboard,
            "card 4111 1111 1111 1111",
            "clipboard",
        );
        clip.subtitle = Some("pay 4111 1111 1111 1111".into());
        clip.uri = Some("secret-uri".into());
        let line = to_line(&clip);
        assert!(!line.title.contains("4111") && !line.subtitle.unwrap().contains("4111"));
        assert_eq!(line.path, None);
        assert_eq!(line.kind, "clipboard");

        let mut file = Item::new(r"file:C:\a\b.txt", Kind::File, "b.txt", "files");
        file.uri = Some(r"C:\a\b.txt".into());
        file.modified_at = Some(5);
        file.score = 1.23456;
        let line = to_line(&file);
        assert_eq!(line.path.as_deref(), Some(r"C:\a\b.txt"));
        assert_eq!((line.modified, line.score), (Some(5), 1.235));
    }

    #[test]
    fn without_a_running_search_the_backend_says_so() {
        let b = CliBackend::new(Arc::new(|| None));
        assert!(b.search("x", 10, None).is_err());
        assert!(b.status().is_err());
        b.record("search", 1, true); // does nothing, does not panic
    }
}
