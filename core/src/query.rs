//! Query parsing (brief section 7.5, step 2). Phase 1 supports free text and
//! the `kind:` filter. Time phrases and action verbs arrive with later phases.

use crate::item::Kind;

/// A time window `[from, to)` in unix seconds, from a phrase like "last week".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub from: u64,
    pub to: u64,
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
    parse_at(input, &chrono::Local::now())
}

/// Like [`parse`] with an explicit "now" (and time zone), so tests are exact.
pub fn parse_at<Tz: chrono::TimeZone>(input: &str, now: &chrono::DateTime<Tz>) -> ParsedQuery {
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
    let time = crate::timefilter::extract(&mut words, now);
    let text = words.join(" ");
    ParsedQuery {
        raw: input.trim().to_owned(),
        text,
        tokens: words,
        kind,
        time,
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

    #[test]
    fn empty_input_is_empty() {
        assert!(parse("   ").is_empty());
        assert!(parse("kind:app").is_empty());
    }
}
