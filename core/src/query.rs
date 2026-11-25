//! Query parsing (brief section 7.5, step 2). Phase 1 supports free text and
//! the `kind:` filter. Time phrases and action verbs arrive with later phases.

use crate::item::Kind;

/// A time window `[from, to)` in unix seconds, from a phrase like "last week".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub from: u64,
    pub to: u64,
}

/// Which understood filters to ignore, set when the user removes a label or
/// asks for the exact words.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skip {
    pub kind: bool,
    pub date: bool,
}

impl Skip {
    pub const ALL: Skip = Skip {
        kind: true,
        date: true,
    };

    /// From the names the frontend sends: `"kind"`, `"date"`, or `"all"`.
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Skip {
        let has = |n: &str| names.iter().any(|s| s.as_ref() == n);
        Skip {
            kind: has("kind") || has("all"),
            date: has("date") || has("all"),
        }
    }
}

/// What the parser understood from natural words, shown as removable labels.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Understood {
    /// For example "PDF".
    pub kind: Option<String>,
    /// For example "modified last week".
    pub date: Option<String>,
}

impl Understood {
    pub fn is_empty(&self) -> bool {
        self.kind.is_none() && self.date.is_none()
    }
}

/// File-kind words (singular or plural) and the extensions they stand for.
/// They count only next to a date phrase, so "code review" and "music" stay
/// ordinary searches.
const KIND_WORDS: &[(&[&str], &str, &[&str])] = &[
    (&["pdf", "pdfs"], "PDF", &["pdf"]),
    (
        &["image", "images", "photo", "photos"],
        "Images",
        &[
            "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "heic",
        ],
    ),
    (
        &["screenshot", "screenshots"],
        "Screenshots",
        &["png", "jpg", "jpeg", "bmp", "webp"],
    ),
    (
        &["document", "documents", "doc", "docs"],
        "Documents",
        &["doc", "docx", "odt", "rtf", "txt", "md", "pdf"],
    ),
    (
        &["spreadsheet", "spreadsheets"],
        "Spreadsheets",
        &["xls", "xlsx", "xlsm", "csv", "ods"],
    ),
    (
        &["presentation", "presentations", "slides"],
        "Presentations",
        &["ppt", "pptx", "odp", "key"],
    ),
    (
        &["video", "videos"],
        "Videos",
        &["mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v"],
    ),
    (
        &["audio", "music"],
        "Audio",
        &["mp3", "wav", "flac", "m4a", "aac", "ogg", "wma"],
    ),
    (
        &["code"],
        "Code",
        &[
            "rs", "py", "js", "ts", "tsx", "jsx", "java", "c", "cpp", "h", "hpp", "cs", "go", "rb",
            "php", "swift", "kt", "svelte", "html", "css", "json", "toml", "yaml", "yml", "sh",
            "ps1",
        ],
    ),
];

#[allow(clippy::type_complexity)]
fn kind_word(
    word: &str,
) -> Option<&'static (
    &'static [&'static str],
    &'static str,
    &'static [&'static str],
)> {
    KIND_WORDS
        .iter()
        .find(|(names, _, _)| names.contains(&word))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    /// The input exactly as typed (calculator and unit sources read this).
    pub raw: String,
    /// Free text with filters removed.
    pub text: String,
    /// Lowercased free-text words; every one must match.
    pub tokens: Vec<String>,
    pub kind: Option<Kind>,
    pub time: Option<TimeRange>,
    /// Lowercase file extensions from a kind word ("pdf", "photos"); empty when
    /// there is none. Only files with one of these extensions match.
    pub exts: &'static [&'static str],
    /// The labels for `exts` and `time`.
    pub understood: Understood,
    /// The input ended with whitespace after at least one word (quick keys
    /// fire only on an exact word plus a trailing space).
    pub trailing_space: bool,
}

impl ParsedQuery {
    /// Nothing to search for: no words and no time window.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.time.is_none()
    }
}

/// Number of leading words in a phrase like "screenshot with" or "image that
/// says", if the rest of the query is not empty.
fn image_phrase_len(words: &[String]) -> Option<usize> {
    const NOUNS: &[&str] = &[
        "screenshot",
        "screenshots",
        "image",
        "images",
        "picture",
        "pictures",
        "photo",
        "photos",
    ];
    let w = |i: usize| words.get(i).map(String::as_str);
    if !NOUNS.contains(&w(0)?) {
        return None;
    }
    let len = match (w(1)?, w(2)) {
        ("with" | "containing" | "saying" | "showing" | "of", _) => 2,
        ("that", Some("says" | "has" | "contains")) => 3,
        _ => return None,
    };
    (words.len() > len).then_some(len)
}

pub fn parse(input: &str) -> ParsedQuery {
    parse_with(input, Skip::default())
}

/// Like [`parse`], ignoring the filters named in `skip`.
pub fn parse_with(input: &str, skip: Skip) -> ParsedQuery {
    parse_opts(input, &chrono::Local::now(), skip)
}

/// Like [`parse`] with an explicit "now" (and time zone), so tests are exact.
pub fn parse_at<Tz: chrono::TimeZone>(input: &str, now: &chrono::DateTime<Tz>) -> ParsedQuery {
    parse_opts(input, now, Skip::default())
}

pub fn parse_opts<Tz: chrono::TimeZone>(
    input: &str,
    now: &chrono::DateTime<Tz>,
    skip: Skip,
) -> ParsedQuery {
    let mut kind = None;
    let mut words = Vec::new();
    for word in input.split_whitespace() {
        let lower = word.to_lowercase();
        match lower.strip_prefix("kind:").and_then(Kind::from_filter) {
            Some(k) => kind = Some(k),
            None => words.push(lower),
        }
    }
    // "screenshot with invoice 4821" means: images whose text contains "invoice 4821".
    if let Some(skip) = image_phrase_len(&words) {
        words.drain(..skip);
        kind = Some(Kind::Screenshot);
    }
    let mut understood = Understood::default();
    let mut exts: &'static [&'static str] = &[];
    let mut time = None;
    if !skip.date {
        let has_kind_word = !skip.kind && words.iter().any(|w| kind_word(w).is_some());
        if let Some((range, phrase)) = crate::timefilter::extract(&mut words, now, has_kind_word) {
            time = Some(range);
            understood.date = Some(format!("modified {phrase}"));
            // A kind word is a filter only next to a date phrase.
            if !skip.kind {
                if let Some(at) = words.iter().position(|w| kind_word(w).is_some()) {
                    if let Some((_, label, e)) = kind_word(&words[at]) {
                        exts = e;
                        understood.kind = Some((*label).to_owned());
                        words.remove(at);
                    }
                }
            }
        }
    }
    let text = words.join(" ");
    ParsedQuery {
        raw: input.trim().to_owned(),
        text,
        tokens: words,
        kind,
        time,
        exts,
        understood,
        trailing_space: !input.trim().is_empty() && input.ends_with(char::is_whitespace),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_lowercases_words() {
        let q = parse("  Annual   REPORT.pdf ");
        assert_eq!(q.tokens, ["annual", "report.pdf"]);
        assert_eq!(q.text, "annual report.pdf");
        assert_eq!(q.raw, "Annual   REPORT.pdf");
        assert_eq!(q.kind, None);
    }

    #[test]
    fn extracts_kind_filter_anywhere() {
        let q = parse("kind:folder photos");
        assert_eq!(q.kind, Some(Kind::Folder));
        assert_eq!(q.tokens, ["photos"]);
        let q = parse("photos KIND:File");
        assert_eq!(q.kind, Some(Kind::File));
        assert_eq!(q.tokens, ["photos"]);
    }

    #[test]
    fn unknown_filter_value_stays_free_text() {
        let q = parse("kind:banana x");
        assert_eq!(q.kind, None);
        assert_eq!(q.tokens, ["kind:banana", "x"]);
    }

    #[test]
    fn screenshot_phrases_become_an_image_text_search() {
        let q = parse("Screenshot with invoice 4821");
        assert_eq!(q.kind, Some(Kind::Screenshot));
        assert_eq!(q.tokens, ["invoice", "4821"]);
        let q = parse("image that says hello");
        assert_eq!(
            (q.kind, q.tokens),
            (Some(Kind::Screenshot), vec!["hello".to_owned()])
        );
        let q = parse("kind:screenshot receipt");
        assert_eq!(
            (q.kind, q.tokens),
            (Some(Kind::Screenshot), vec!["receipt".to_owned()])
        );
        // Nothing after the phrase, or just the noun: plain words.
        assert_eq!(parse("screenshot with").kind, None);
        assert_eq!(parse("screenshot notes").kind, None);
    }

    #[test]
    fn time_phrases_are_removed_from_the_words_and_become_a_window() {
        use chrono::TimeZone;
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 11, 18, 10, 0, 0)
            .unwrap();
        let q = parse_at("Invoice last week", &now);
        assert_eq!(q.tokens, ["invoice"]);
        let t = q.time.unwrap();
        assert_eq!(t.to - t.from, 7 * 86_400);
        let q = parse_at("yesterday", &now);
        assert!(q.tokens.is_empty() && q.time.is_some() && !q.is_empty());
        let q = parse_at("kind:file taxes in march", &now);
        assert_eq!(
            (q.kind, q.tokens),
            (Some(Kind::File), vec!["taxes".to_owned()])
        );
        assert!(q.time.is_some());
    }

    fn at() -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc
            .with_ymd_and_hms(2026, 11, 18, 10, 0, 0)
            .unwrap()
    }

    #[test]
    fn kind_words_with_a_date_become_extension_and_date_filters() {
        for (input, label, ext, rest) in [
            ("pdf last week", "PDF", "pdf", vec![]),
            ("Invoice  PDFs   YESTERDAY", "PDF", "pdf", vec!["invoice"]),
            ("images from July", "Images", "png", vec![]),
            ("photos in july", "Images", "heic", vec![]),
            ("screenshots this month", "Screenshots", "png", vec![]),
            ("docs last 7 days", "Documents", "docx", vec![]),
            ("document today", "Documents", "txt", vec![]),
            ("spreadsheets today", "Spreadsheets", "xlsx", vec![]),
            ("slides today", "Presentations", "pptx", vec![]),
            ("presentation today", "Presentations", "key", vec![]),
            ("videos today", "Videos", "mkv", vec![]),
            ("music today", "Audio", "flac", vec![]),
            ("audio today", "Audio", "mp3", vec![]),
            ("code today budget", "Code", "rs", vec!["budget"]),
            ("pdf 2024", "PDF", "pdf", vec![]),
        ] {
            let q = parse_at(input, &at());
            assert_eq!(q.understood.kind.as_deref(), Some(label), "{input}");
            assert!(q.exts.contains(&ext), "{input}");
            assert!(q.time.is_some(), "{input}");
            assert_eq!(q.tokens, rest, "{input}");
        }
        let q = parse_at("pdf last week", &at());
        assert_eq!(q.understood.date.as_deref(), Some("modified last week"));
        assert!(!q.is_empty());
    }

    #[test]
    fn kind_words_need_whole_words_and_a_date() {
        for input in [
            "pdf",
            "code review",
            "music",
            "pdfx last week",
            "mypdf today",
        ] {
            let q = parse_at(input, &at());
            assert!(q.exts.is_empty(), "{input}");
            assert!(q.understood.kind.is_none(), "{input}");
        }
        // "opened" is plain text; nothing special happens.
        let q = parse_at("opened last", &at());
        assert!(q.time.is_none() && q.tokens == ["opened", "last"]);
        let q = parse_at("invoice opened yesterday", &at());
        assert_eq!(q.tokens, ["invoice", "opened"]);
    }

    #[test]
    fn removing_a_label_reruns_without_that_filter() {
        let all = parse_opts("pdf last week", &at(), Skip::default());
        assert!(all.time.is_some() && !all.exts.is_empty());
        let no_kind = parse_opts("pdf last week", &at(), Skip::from_names(&["kind"]));
        assert!(no_kind.time.is_some() && no_kind.exts.is_empty());
        assert_eq!(no_kind.tokens, ["pdf"]);
        let no_date = parse_opts("pdf last week", &at(), Skip::from_names(&["date"]));
        assert!(no_date.time.is_none() && no_date.exts.is_empty());
        assert_eq!(no_date.tokens, ["pdf", "last", "week"]);
        let exact = parse_opts("pdf last week", &at(), Skip::from_names(&["all"]));
        assert!(exact.understood.is_empty() && exact.time.is_none());
        assert_eq!(exact.text, "pdf last week");
    }

    #[test]
    fn ordinary_queries_are_unchanged() {
        for input in [
            "annual report",
            "report.pdf",
            "march",
            "march madness",
            "may notes",
            "notes may",
            "taxes",
            "code",
            "pdf",
            "photos",
            "last",
            "this is the week",
            "past tense",
            "last of us",
            "2024",
            "2018 plan",
            "invoice 4821",
            "screenshot notes",
            "vscode",
            "calc 2+2",
            "docs",
            "slides deck",
        ] {
            let q = parse_at(input, &at());
            let words: Vec<String> = input.split_whitespace().map(str::to_lowercase).collect();
            assert_eq!(q.tokens, words, "{input}");
            assert!(q.time.is_none() && q.kind.is_none(), "{input}");
            assert!(q.exts.is_empty() && q.understood.is_empty(), "{input}");
        }
        let q = parse_at("kind:folder photos", &at());
        assert_eq!(
            (q.kind, q.tokens),
            (Some(Kind::Folder), vec!["photos".to_owned()])
        );
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(parse("   ").is_empty());
        assert!(parse("kind:app").is_empty());
    }
}
