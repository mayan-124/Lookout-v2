//! Web search: "g words" or "search the web for words" gives one result that
//! opens the chosen engine's results page in the default browser, through the
//! `open_url` action. Off until an engine is chosen in Settings. Nothing is
//! sent while typing: this only writes an address. Text that looks like a
//! secret is never put in one.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::actions::quick::{KIND_URL, OPEN_URL};
use crate::content::secrets;
use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::quick;
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};

pub const SOURCE: &str = "websearch";
/// Longer text is pasted content, not a search.
const MAX_CHARS: usize = 200;
const COMMANDS: [&str; 2] = ["g ", "search the web for "];
/// First words Lookout answers itself: no fallback for them.
const OWN_WORDS: &[&str] = &[
    "help", "kill", "timer", "timers", "note", "notes", "disk", "settings", "setup",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
    #[default]
    Off,
    DuckDuckGo,
    Google,
    Bing,
}

impl SearchEngine {
    /// Name and results address; `None` while off.
    fn parts(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Off => None,
            Self::DuckDuckGo => Some(("DuckDuckGo", "https://duckduckgo.com/?q=")),
            Self::Google => Some(("Google", "https://www.google.com/search?q=")),
            Self::Bing => Some(("Bing", "https://www.bing.com/search?q=")),
        }
    }
}

/// The engine chosen now, shared with the running source.
#[derive(Debug, Default)]
pub struct Choice(AtomicU8);

impl Choice {
    pub fn set(&self, e: SearchEngine) {
        self.0.store(e as u8, Ordering::Release);
    }

    pub fn get(&self) -> SearchEngine {
        match self.0.load(Ordering::Acquire) {
            1 => SearchEngine::DuckDuckGo,
            2 => SearchEngine::Google,
            3 => SearchEngine::Bing,
            _ => SearchEngine::Off,
        }
    }
}

/// The words of a typed search command, or `None` if the text is not one.
pub fn command(typed: &str) -> Option<&str> {
    let t = typed.trim();
    COMMANDS.iter().find_map(|c| {
        let head = t.get(..c.len())?;
        let rest = t[c.len()..].trim();
        (head.eq_ignore_ascii_case(c) && !rest.is_empty()).then_some(rest)
    })
}

/// Everything but unreserved characters, as UTF-8 bytes.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The result row that searches `words` with `engine`; `None` while off, or
/// for text that must not leave the PC.
fn row(engine: SearchEngine, words: &str) -> Option<Item> {
    let (name, base) = engine.parts()?;
    let words = quick::strip_controls(words);
    let words = words.trim();
    if words.is_empty() || words.chars().count() > MAX_CHARS || secrets::mask(words).1 > 0 {
        return None;
    }
    let url = quick::parse_web_url(&format!("{base}{}", percent_encode(words)))?;
    // The id never holds the words: ids reach the action log.
    let mut item = Item::new(
        format!("websearch:{}", name.to_ascii_lowercase()),
        Kind::Custom(KIND_URL.into()),
        format!("Search the web for \u{201c}{words}\u{201d}"),
        SOURCE,
    );
    item.subtitle = Some(format!("{name} · opens in your default browser"));
    item.uri = Some(url.url);
    item.score = ANSWER_SCORE;
    item.score_features.match_quality = 1.0;
    item.metadata
        .insert("default_action".into(), OPEN_URL.into());
    Some(item)
}

/// Whether a first word belongs to an add-on that is in use.
pub type Claimed = Box<dyn Fn(&str) -> bool + Send + Sync>;

pub struct WebSearchSource {
    choice: Arc<Choice>,
    claimed: Claimed,
}

impl WebSearchSource {
    pub fn new(choice: Arc<Choice>, claimed: Claimed) -> Self {
        Self { choice, claimed }
    }
}

impl Source for WebSearchSource {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.kind.is_some() {
            return;
        }
        if let Some(item) = command(&q.raw).and_then(|w| row(self.choice.get(), w)) {
            emit(vec![item]);
        }
    }

    /// The same row for ordinary words that found nothing else.
    fn fallback(&self, q: &ParsedQuery) -> Option<Item> {
        let first = q.tokens.first()?;
        let filtered =
            q.kind.is_some() || q.time.is_some() || !q.exts.is_empty() || !q.ops.is_empty();
        if filtered
            || command(&q.raw).is_some()
            || crate::pathquery::looks_like_path(&q.raw)
            || quick::parse_web_url(&q.raw).is_some()
            || quick::parse_unc(&q.raw).is_some()
            || OWN_WORDS.contains(&first.trim_end_matches(':'))
            || (self.claimed)(first)
        {
            return None;
        }
        let mut item = row(self.choice.get(), &q.raw)?;
        item.metadata.insert("fallback".into(), true.into());
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn source(engine: SearchEngine) -> WebSearchSource {
        let choice = Arc::new(Choice::default());
        choice.set(engine);
        WebSearchSource::new(choice, Box::new(|w| w == "clip"))
    }

    fn ask(src: &WebSearchSource, text: &str) -> Vec<Item> {
        let q = crate::query::parse(text);
        let ctx = QueryContext::new(1, Arc::new(AtomicU64::new(1)));
        let mut out = Vec::new();
        src.query(&q, &ctx, &mut |i| out.extend(i));
        out
    }

    fn fallback(src: &WebSearchSource, text: &str) -> Option<Item> {
        src.fallback(&crate::query::parse(text))
    }

    #[test]
    fn both_commands_give_an_encoded_address() {
        let src = source(SearchEngine::DuckDuckGo);
        for text in [
            "g c++ & \"rust\" #1",
            "Search The Web For c++ & \"rust\" #1",
        ] {
            let hit = ask(&src, text);
            assert_eq!(hit.len(), 1, "{text}");
            assert_eq!(
                hit[0].uri.as_deref(),
                Some("https://duckduckgo.com/?q=c%2B%2B%20%26%20%22rust%22%20%231")
            );
            assert!(hit[0].title.contains("c++ & \"rust\" #1"));
            assert!(hit[0]
                .subtitle
                .as_deref()
                .unwrap()
                .starts_with("DuckDuckGo"));
            assert_eq!(hit[0].id, "websearch:duckduckgo");
            assert_eq!(hit[0].metadata["default_action"], OPEN_URL);
            assert!(!hit[0].metadata.contains_key("fallback"));
        }
        assert_eq!(percent_encode("é ?"), "%C3%A9%20%3F");
        // Kind and date words are searched as typed.
        assert!(ask(&src, "g pdf last week")[0]
            .title
            .contains("pdf last week"));
    }

    #[test]
    fn each_engine_has_its_own_address_and_off_gives_nothing() {
        let url = |e| {
            ask(&source(e), "g cats")
                .first()
                .and_then(|i| i.uri.clone())
        };
        assert_eq!(url(SearchEngine::Off), None);
        assert_eq!(
            url(SearchEngine::Google).as_deref(),
            Some("https://www.google.com/search?q=cats")
        );
        assert_eq!(
            url(SearchEngine::Bing).as_deref(),
            Some("https://www.bing.com/search?q=cats")
        );
        assert!(fallback(&source(SearchEngine::Off), "cats").is_none());
        assert_eq!(SearchEngine::default(), SearchEngine::Off);
    }

    #[test]
    fn only_the_whole_command_counts() {
        let src = source(SearchEngine::Google);
        for no in ["g", "g ", "gcats", "search the web for", "the g spot"] {
            assert!(ask(&src, no).is_empty(), "{no:?}");
        }
    }

    #[test]
    fn secret_looking_text_is_never_searched() {
        let src = source(SearchEngine::Google);
        // Assembled here so no real-looking secret sits in the source.
        let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        for secret in [
            "password=hunter2".to_owned(),
            "4111 1111 1111 1111".to_owned(),
            key,
        ] {
            assert!(ask(&src, &format!("g {secret}")).is_empty(), "{secret}");
            assert!(fallback(&src, &secret).is_none(), "{secret}");
        }
        assert!(ask(&src, &format!("g {}", "a".repeat(201))).is_empty());
    }

    #[test]
    fn the_fallback_is_for_plain_words_only() {
        let src = source(SearchEngine::Google);
        let hit = fallback(&src, "zebra crossing rules").unwrap();
        assert_eq!(hit.metadata["fallback"], true);
        for no in [
            "",
            "clip zebra",
            "timer abc",
            "kill port 3000",
            "note: zebra",
            "example.com",
            r"C:\Users\x",
            r"\\server\share",
            "ext:pdf zebra",
            "pdf last week",
            "g zebra",
        ] {
            assert!(fallback(&src, no).is_none(), "{no:?}");
        }
    }
}
