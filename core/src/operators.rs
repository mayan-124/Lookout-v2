//! Typed search operators: `ext:`, `size:`, `in:` and `name:`. A token is an
//! operator only when its name is known and its value reads; anything else stays
//! an ordinary word, so a query without operators is parsed as before.

use std::path::{Path, PathBuf};

use crate::ranking;

/// Size code of a file whose size is not known; no size filter accepts it.
pub const SIZE_UNKNOWN: u32 = u32::MAX;
const SIZE_MAX: u32 = u32::MAX - 1;

/// A file size in four bytes: twice the whole KiB, plus one if bytes remain.
/// Comparisons with any whole number of KiB stay exact, up to 2 TiB.
pub fn size_code(bytes: u64) -> u32 {
    let code = (bytes / 1024)
        .saturating_mul(2)
        .saturating_add(u64::from(!bytes.is_multiple_of(1024)));
    u32::try_from(code).map_or(SIZE_MAX, |c| c.min(SIZE_MAX))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Operators {
    /// Lowercase extensions without the dot; a file needs one of them.
    pub exts: Vec<String>,
    /// Allowed [`size_code`]s, both ends included.
    pub size: Option<(u32, u32)>,
    /// Folder names or path fragments; a file must lie below each.
    pub folders: Vec<String>,
    /// Folded words the file name must contain.
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub ops: Operators,
    /// Lowercase words left for the ordinary search.
    pub words: Vec<String>,
    /// The recognised operators exactly as typed, one per chip.
    pub typed: Vec<String>,
    /// Something looked like an operator but was not understood.
    pub invalid: bool,
}

const KEYS: [&str; 4] = ["ext", "size", "in", "name"];
/// `word:` forms that are addresses, not attempts at an operator.
const SCHEMES: [&str; 12] = [
    "http",
    "https",
    "ftp",
    "ftps",
    "file",
    "mailto",
    "tel",
    "shell",
    "steam",
    "localhost",
    "ssh",
    "git",
];

/// Splits at white space, except inside the quotes of `in:"My Folder"`.
fn tokens(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = input.trim_start();
    while !rest.is_empty() {
        let quoted = KEYS.iter().find_map(|k| {
            let name = rest.get(..k.len())?;
            let mark = rest.get(k.len()..k.len() + 2)?;
            (name.eq_ignore_ascii_case(k) && mark == ":\"").then_some(k.len() + 2)
        });
        // After an opening quote, white space counts only once it is closed.
        let from = quoted.map_or(0, |open| {
            rest[open..].find('"').map_or(rest.len(), |i| open + i + 1)
        });
        let end = rest[from..]
            .find(char::is_whitespace)
            .map_or(rest.len(), |i| from + i);
        out.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    out
}

fn unquote(v: &str) -> &str {
    match v.strip_prefix('"') {
        // An unclosed quote is still being typed: search with what is there.
        Some(v) => v.strip_suffix('"').unwrap_or(v),
        None => v,
    }
}

fn extensions(v: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for part in v.split(',') {
        let e = part.strip_prefix('.').unwrap_or(part);
        if e.is_empty() {
            // "pdf," while the next one is typed.
            if part.is_empty() {
                continue;
            }
            return None;
        }
        let ok = e.len() <= 16
            && e.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '+' | '~' | '.'));
        if !ok {
            return None;
        }
        out.push(ranking::fold_lower(e));
    }
    (!out.is_empty()).then_some(out)
}

/// "100mb", "1.5gb" as whole KiB.
fn kib(v: &str) -> Option<u64> {
    let at = v.len().checked_sub(2)?;
    let (number, unit) = (v.get(..at)?, v.get(at..)?);
    let factor = match unit {
        "kb" => 1.0,
        "mb" => 1024.0,
        "gb" => 1024.0 * 1024.0,
        _ => return None,
    };
    if !number.starts_with(|c: char| c.is_ascii_digit())
        || !number.chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        return None;
    }
    let n: f64 = number.parse().ok()?;
    let total = (n * factor).round();
    (total <= f64::from(SIZE_MAX / 2)).then_some(total as u64)
}

fn size(v: &str) -> Option<(u32, u32)> {
    let code = |k: u64| u32::try_from(k * 2).ok();
    if let Some((a, b)) = v.split_once("..") {
        let (a, b) = (kib(a)?, kib(b)?);
        return (a <= b).then_some((code(a)?, code(b)?));
    }
    if let Some(r) = v.strip_prefix(">=") {
        Some((code(kib(r)?)?, SIZE_MAX))
    } else if let Some(r) = v.strip_prefix("<=") {
        Some((0, code(kib(r)?)?))
    } else if let Some(r) = v.strip_prefix('>') {
        Some((code(kib(r)?)?.checked_add(1)?, SIZE_MAX))
    } else {
        let r = v.strip_prefix('<')?;
        Some((0, code(kib(r)?)?.checked_sub(1)?))
    }
}

/// A folder fragment with `\` separators and no leading or trailing one.
fn folder(v: &str) -> Option<String> {
    let f = v.trim().replace('/', "\\");
    let f = f.trim_end_matches('\\');
    // A network path keeps its leading backslashes.
    let f = if f.starts_with("\\\\") {
        f
    } else {
        f.trim_start_matches('\\')
    };
    (!f.is_empty() && !f.contains('"')).then(|| ranking::fold_lower(f))
}

impl Operators {
    pub fn is_empty(&self) -> bool {
        self.exts.is_empty()
            && self.size.is_none()
            && self.folders.is_empty()
            && self.names.is_empty()
    }

    /// Reads one lowercase value into the filter; false if it does not read.
    fn add(&mut self, key: &str, value: &str) -> bool {
        match key {
            "ext" => match extensions(value) {
                Some(e) => self.exts.extend(e),
                None => return false,
            },
            "size" => match size(value) {
                // Two size operators narrow each other.
                Some((lo, hi)) => {
                    let (a, b) = self.size.unwrap_or((0, SIZE_MAX));
                    self.size = Some((lo.max(a), hi.min(b)));
                }
                None => return false,
            },
            "in" => match folder(value) {
                Some(f) => self.folders.push(f),
                None => return false,
            },
            _ => {
                let word = value.trim();
                if word.is_empty() || word.contains('"') {
                    return false;
                }
                self.names.push(ranking::fold_lower(word));
            }
        }
        true
    }

    /// Whether a found file passes, for sources that know only its path. The
    /// size is read from the disk, and only when a size was asked for. `in:` is
    /// matched from the indexed folder the file lies in.
    pub fn accepts_path(&self, path: &Path, roots: &[PathBuf]) -> bool {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| ranking::fold_lower(&n.to_string_lossy()));
        if !self.exts.is_empty() && !self.exts.iter().any(|e| has_extension(&name, e)) {
            return false;
        }
        if !self.names.iter().all(|w| name.contains(w)) {
            return false;
        }
        if !self.folders.is_empty() {
            let parent = path.parent().unwrap_or(path);
            // From the indexed folder's own name downwards.
            let base = roots
                .iter()
                .filter(|r| path.starts_with(r))
                .max_by_key(|r| r.as_os_str().len())
                .and_then(|r| r.parent());
            let below = base.and_then(|b| parent.strip_prefix(b).ok());
            let relative = ranking::fold_lower(&below.unwrap_or(parent).to_string_lossy());
            let full = ranking::fold_lower(&parent.to_string_lossy());
            if !self.folders.iter().all(|f| {
                if is_absolute(f) {
                    full.starts_with(f.as_str())
                } else {
                    relative.starts_with(f.as_str()) || relative.contains(&format!("\\{f}"))
                }
            }) {
                return false;
            }
        }
        match self.size {
            Some((lo, hi)) => std::fs::symlink_metadata(path)
                .is_ok_and(|m| m.is_file() && (lo..=hi).contains(&size_code(m.len()))),
            None => true,
        }
    }
}

/// A fragment that names a whole path (`c:\…`, `\\server\…`).
pub fn is_absolute(f: &str) -> bool {
    f.starts_with("\\\\") || f.as_bytes().get(1) == Some(&b':')
}

/// Whether a lowercase file name ends in `.ext`.
pub fn has_extension(name: &str, ext: &str) -> bool {
    name.strip_suffix(ext).is_some_and(|r| r.ends_with('.'))
}

/// `word:value` that is none of ours: a mistyped operator, worth one hint.
fn looks_like_operator(word: &str) -> bool {
    word.split_once(':').is_some_and(|(key, value)| {
        (2..=10).contains(&key.len())
            && key.bytes().all(|b| b.is_ascii_lowercase())
            && !value.is_empty()
            && !value.starts_with(['/', '\\'])
            && key != "kind"
            && !SCHEMES.contains(&key)
    })
}

pub fn parse(input: &str) -> Parsed {
    let mut out = Parsed::default();
    for token in tokens(input) {
        let lower = token.to_lowercase();
        let known = lower.split_once(':').filter(|(key, _)| KEYS.contains(key));
        match known {
            Some((key, value)) if out.ops.add(key, unquote(value)) => {
                out.typed.push(token.to_owned());
            }
            _ => {
                let bad_kind = lower
                    .strip_prefix("kind:")
                    .is_some_and(|v| crate::item::Kind::from_filter(v).is_none());
                // "name: john" is prose, or a value not typed yet.
                let bad_value = known.is_some_and(|(_, v)| !v.is_empty());
                out.invalid |= bad_value || bad_kind || looks_like_operator(&lower);
                out.words
                    .extend(lower.split_whitespace().map(str::to_owned));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    fn allows(query: &str, bytes: u64) -> bool {
        let (lo, hi) = parse(query).ops.size.unwrap();
        (lo..=hi).contains(&size_code(bytes))
    }

    #[test]
    fn valid_operators_are_read_in_any_case() {
        let p = parse("EXT:PDF,.Docx Size:>100MB IN:Downloads Name:Report");
        assert_eq!(p.ops.exts, ["pdf", "docx"]);
        assert_eq!(p.ops.folders, ["downloads"]);
        assert_eq!(p.ops.names, ["report"]);
        assert!(p.ops.size.is_some());
        assert!(p.words.is_empty() && !p.invalid);
        assert_eq!(
            p.typed,
            [
                "EXT:PDF,.Docx",
                "Size:>100MB",
                "IN:Downloads",
                "Name:Report"
            ]
        );
    }

    #[test]
    fn operators_combine_with_ordinary_words() {
        let p = parse("annual ext:pdf budget in:work/reports/");
        assert_eq!(p.words, ["annual", "budget"]);
        assert_eq!(p.ops.exts, ["pdf"]);
        assert_eq!(p.ops.folders, [r"work\reports"]);
        assert_eq!(parse("ext:pdf ext:txt").ops.exts, ["pdf", "txt"]);
        assert_eq!(parse("ext:pdf,").ops.exts, ["pdf"]);
    }

    #[test]
    fn size_units_comparisons_and_ranges_are_exact() {
        assert!(allows("size:>100mb", 100 * MB + 1));
        assert!(!allows("size:>100mb", 100 * MB));
        assert!(allows("size:>=100mb", 100 * MB));
        assert!(allows("size:<1gb", GB - 1));
        assert!(!allows("size:<1gb", GB));
        assert!(allows("size:<=1gb", GB));
        assert!(!allows("size:<=1gb", GB + 1));
        assert!(allows("size:<4kb", 0));
        assert!(allows("size:10mb..50mb", 10 * MB));
        assert!(allows("size:10mb..50mb", 50 * MB));
        assert!(!allows("size:10mb..50mb", 10 * MB - 1));
        assert!(!allows("size:10mb..50mb", 50 * MB + 1));
        assert!(allows("size:>1.5gb", 2 * GB));
        assert!(!allows("size:>1.5gb", GB));
        assert!(allows("size:>1mb size:<2mb", MB + KB));
        assert!(!allows("size:>1mb size:<2mb", 3 * MB));
        let (lo, hi) = parse("size:>1kb").ops.size.unwrap();
        assert!(!(lo..=hi).contains(&SIZE_UNKNOWN));
        assert_eq!(size_code(u64::MAX), SIZE_UNKNOWN - 1);
    }

    #[test]
    fn quoted_values_keep_their_spaces() {
        let p = parse(r#"in:"My Projects" tax name:"annual report" 2025"#);
        assert_eq!(p.ops.folders, ["my projects"]);
        assert_eq!(p.ops.names, ["annual report"]);
        assert_eq!(p.words, ["tax", "2025"]);
        assert_eq!(p.typed, [r#"in:"My Projects""#, r#"name:"annual report""#]);
        let typing = parse(r#"in:"My Pro"#);
        assert_eq!(typing.ops.folders, ["my pro"]);
        // Quotes elsewhere are ordinary text.
        assert_eq!(parse(r#"say "a b""#).words, ["say", "\"a", "b\""]);
    }

    #[test]
    fn bad_values_and_unknown_operators_stay_words() {
        for bad in [
            "size:big",
            "size:100mb",
            "size:>10",
            "size:>mb",
            "size:<0kb",
            "size:50mb..10mb",
            "size:>.mb",
            "ext:p*f",
            "type:pdf",
            "folder:work",
            "kind:banana",
        ] {
            let p = parse(&format!("x {bad}"));
            assert!(p.ops.is_empty() && p.typed.is_empty(), "{bad}");
            assert_eq!(p.words, ["x", bad], "{bad}");
            assert!(p.invalid, "{bad}");
        }
        let p = parse("size:big ext:pdf");
        assert_eq!((p.words, p.invalid), (vec!["size:big".to_owned()], true));
        assert_eq!(p.ops.exts, ["pdf"]);
    }

    #[test]
    fn ordinary_text_with_colons_gets_no_hint() {
        for q in [
            "https://example.com/a",
            r"C:\Users\u",
            "c:",
            "meeting 10:30",
            "note: hello",
            "my name: john",
            "ext:",
            "localhost:3000",
            "mailto:a@b.c",
            "kind:folder photos",
            "re",
        ] {
            let p = parse(q);
            assert!(!p.invalid && p.ops.is_empty(), "{q}");
            let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
            assert_eq!(p.words, words, "{q}");
        }
    }

    #[test]
    fn paths_are_filtered_like_index_entries() {
        let roots = [PathBuf::from(r"C:\Users\u\Documents")];
        let file = Path::new(r"C:\Users\u\Documents\Work\Reports\Q1 Report.PDF");
        let ok = |q: &str| parse(q).ops.accepts_path(file, &roots);
        for q in [
            "ext:pdf",
            "name:report",
            "in:documents",
            "in:work",
            "in:work/rep",
            r"in:c:\users\u\documents",
        ] {
            assert!(ok(q), "{q}");
        }
        for q in [
            "ext:df",
            "name:budget",
            "in:users",
            "in:ports",
            "in:q1",
            "size:>1kb",
        ] {
            assert!(!ok(q), "{q}");
        }
    }

    #[test]
    fn size_of_a_real_file_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        std::fs::write(&file, vec![0u8; 3000]).unwrap();
        assert!(parse("size:>2kb").ops.accepts_path(&file, &[]));
        assert!(!parse("size:>3kb").ops.accepts_path(&file, &[]));
    }
}
