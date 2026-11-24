//! Open by path: a query that looks like a path (`C:\...`, `\\server\share`,
//! `%APPDATA%`, `~\`, or `appdata\...`) gets a top result that opens it. Only
//! the existing safe open action is used; nothing here launches anything.

use std::path::{Path, PathBuf};

use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};

/// True if `text` is written as a path rather than as words to search for.
pub fn looks_like_path(text: &str) -> bool {
    let t = text.trim().trim_start_matches('"');
    let b = t.as_bytes();
    if t.starts_with("\\\\") || t.starts_with("//") {
        return t.len() > 2;
    }
    // Drive letter: `C:`, `c:\x`, `D:/x`.
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return b.len() == 2 || b[2] == b'\\' || b[2] == b'/';
    }
    if let Some(rest) = t.strip_prefix('%') {
        return rest.contains('%');
    }
    if t == "~" || t.starts_with("~\\") || t.starts_with("~/") {
        return true;
    }
    // `appdata\local` is shorthand for the folder in the user's profile.
    let lower = t.to_ascii_lowercase();
    lower.starts_with("appdata\\") || lower.starts_with("appdata/")
}

/// Expands `%VAR%`, a leading `~` and a leading `appdata\`, and normalises
/// separators. Unknown variables are left as typed.
pub fn expand_with(text: &str, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let t = text.trim().trim_matches('"');
    let mut out = String::new();
    let mut rest = t;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 && env(&after[..end]).is_some() => {
                out.push_str(&env(&after[..end]).unwrap_or_default());
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let mut s = out.replace('/', "\\");
    let profile = env("USERPROFILE");
    if let Some(p) = &profile {
        if s == "~" || s.starts_with("~\\") {
            s = format!("{p}{}", &s[1..]);
        } else if s.to_ascii_lowercase().starts_with("appdata\\") {
            s = format!("{p}\\{s}");
        }
    }
    // `C:` alone means the drive's current folder; the root is what is meant.
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    PathBuf::from(s)
}

pub fn expand(text: &str) -> PathBuf {
    expand_with(text, &|name| std::env::var(name).ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathTarget {
    Folder(PathBuf),
    File(PathBuf),
    /// Nothing at `asked`; `nearest` is the closest existing parent folder.
    Missing {
        asked: PathBuf,
        nearest: Option<PathBuf>,
    },
}

pub fn resolve(path: &Path) -> PathTarget {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => PathTarget::Folder(path.to_path_buf()),
        Ok(_) => PathTarget::File(path.to_path_buf()),
        Err(_) => PathTarget::Missing {
            asked: path.to_path_buf(),
            nearest: path
                .ancestors()
                .skip(1)
                .find(|p| !p.as_os_str().is_empty() && p.is_dir())
                .map(Path::to_path_buf),
        },
    }
}

/// Tab completion: extends the last folder name in `text` to the one folder
/// that starts with it (or to the common start of several). The typed form
/// (`%APPDATA%\Mic`) is kept. `None` when nothing would change.
pub fn complete(text: &str) -> Option<String> {
    if !looks_like_path(text) {
        return None;
    }
    let cut = text.rfind(['\\', '/'])? + 1;
    let (head, partial) = text.split_at(cut);
    let dir = expand(head);
    let want = partial.to_lowercase();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_lowercase().starts_with(&want))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    let first = names.first()?;
    let done = if names.len() == 1 {
        format!("{first}\\")
    } else {
        let common = names.iter().skip(1).fold(first.len(), |len, n| {
            first
                .chars()
                .zip(n.chars())
                .take(len)
                .take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
                .map(|(a, _)| a.len_utf8())
                .sum::<usize>()
                .min(len)
        });
        if common > partial.len() {
            first[..common].to_owned()
        } else {
            // Several folders and nothing more in common: go to the first one.
            format!("{first}\\")
        }
    };
    let next = format!("{head}{done}");
    (next != text).then_some(next)
}

/// The item for a query that looks like a path.
pub fn item_for(text: &str, source: &str) -> Option<Item> {
    if !looks_like_path(text) {
        return None;
    }
    let item = match resolve(&expand(text)) {
        PathTarget::Folder(p) => open_item(
            Kind::Folder,
            "folder",
            &p,
            format!("Open {}", p.display()),
            None,
            source,
        ),
        PathTarget::File(p) => open_item(
            Kind::File,
            "file",
            &p,
            format!("Open {}", p.display()),
            None,
            source,
        ),
        PathTarget::Missing { asked, nearest } => match nearest {
            Some(parent) => open_item(
                Kind::Folder,
                "folder",
                &parent,
                "Not found".to_owned(),
                Some(format!(
                    "{} (nearest folder: {})",
                    asked.display(),
                    parent.display()
                )),
                source,
            ),
            None => {
                let mut i = Item::new(
                    format!("path:{}", asked.display()),
                    Kind::Custom("path".into()),
                    "Not found",
                    source,
                );
                i.subtitle = Some(asked.display().to_string());
                i
            }
        },
    };
    let mut item = item;
    item.score = ANSWER_SCORE;
    Some(item)
}

fn open_item(
    kind: Kind,
    prefix: &str,
    path: &Path,
    title: String,
    subtitle: Option<String>,
    source: &str,
) -> Item {
    let p = path.to_string_lossy().into_owned();
    let mut i = Item::new(format!("{prefix}:{p}"), kind, title, source);
    i.subtitle = Some(subtitle.unwrap_or_else(|| p.clone()));
    i.uri = Some(p);
    i
}

pub struct PathSource;

impl Source for PathSource {
    fn id(&self) -> &'static str {
        "path"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if ctx.is_cancelled() {
            return;
        }
        if let Some(item) = item_for(&q.raw, self.id()) {
            emit(vec![item]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "APPDATA" => Some(r"C:\Users\u\AppData\Roaming".into()),
            "USERPROFILE" => Some(r"C:\Users\u".into()),
            _ => None,
        }
    }

    #[test]
    fn detects_paths_and_leaves_words_alone() {
        for yes in [
            r"C:\Users",
            "c:/work",
            "D:",
            r"\\server\share",
            "%APPDATA%",
            r"%APPDATA%\Code",
            r"~\Documents",
            "~",
            r"appdata\local",
            r#""C:\Program Files""#,
        ] {
            assert!(looks_like_path(yes), "{yes}");
        }
        for no in [
            "report",
            "2+2",
            "10 km to miles",
            "c:x",
            "%",
            "50% off",
            "\\\\",
            "appdata",
            "~notes",
            "kind:file",
        ] {
            assert!(!looks_like_path(no), "{no}");
        }
    }

    #[test]
    fn expands_variables_tilde_and_appdata_shorthand() {
        let x = |t: &str| expand_with(t, &env).to_string_lossy().into_owned();
        assert_eq!(x(r"%APPDATA%\Code"), r"C:\Users\u\AppData\Roaming\Code");
        assert_eq!(x(r"~\Documents"), r"C:\Users\u\Documents");
        assert_eq!(x("~"), r"C:\Users\u");
        assert_eq!(x(r"AppData\Local"), r"C:\Users\u\AppData\Local");
        assert_eq!(x("C:/a/b"), r"C:\a\b");
        assert_eq!(x("D:"), r"D:\");
        assert_eq!(x(r"%NOPE%\x"), r"%NOPE%\x");
    }

    #[test]
    fn resolves_folder_file_and_missing_with_nearest_parent() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let file = sub.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(resolve(&sub), PathTarget::Folder(sub.clone()));
        assert_eq!(resolve(&file), PathTarget::File(file.clone()));
        let gone = sub.join("no").join("deeper.txt");
        assert_eq!(
            resolve(&gone),
            PathTarget::Missing {
                asked: gone,
                nearest: Some(sub)
            }
        );
    }

    #[test]
    fn items_open_through_the_normal_file_and_folder_targets() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().to_string_lossy().into_owned();
        if !looks_like_path(&text) {
            return; // temp dir is not drive-rooted on this machine
        }
        let i = item_for(&text, "path").unwrap();
        assert_eq!(i.kind, Kind::Folder);
        assert!(i.title.starts_with("Open "));
        assert_eq!(i.score, ANSWER_SCORE);
        let missing = item_for(&format!(r"{text}\nothing\here"), "path").unwrap();
        assert_eq!(missing.title, "Not found");
        assert_eq!(missing.uri.as_deref(), Some(text.as_str()));
        assert!(item_for("hello", "path").is_none());
    }

    #[test]
    fn tab_completes_the_next_folder_name() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["Documents", "Downloads", "Music"] {
            std::fs::create_dir(dir.path().join(n)).unwrap();
        }
        std::fs::write(dir.path().join("Muffin.txt"), "x").unwrap();
        let base = format!("{}\\", dir.path().display());
        if !looks_like_path(&base) {
            return;
        }
        assert_eq!(
            complete(&format!("{base}mu")),
            Some(format!("{base}Music\\"))
        );
        // Two matches share "Do": no longer start to add, so the first is taken.
        assert_eq!(
            complete(&format!("{base}Do")),
            Some(format!("{base}Documents\\"))
        );
        assert_eq!(
            complete(&format!("{base}Dow")),
            Some(format!("{base}Downloads\\"))
        );
        assert_eq!(complete(&format!("{base}zzz")), None);
        assert_eq!(complete("hello"), None);
    }
}
