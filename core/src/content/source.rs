//! The Content source: full-text and screenshot-text hits as ordinary items.

use std::path::Path;
use std::sync::Arc;

use super::extract::{self, FileClass};
use super::secrets;
use super::store::{ContentHit, ContentQuery, ContentStore, DocKind};
use crate::index_service::unix_now;
use crate::item::{Item, Kind, ScoreFeatures};
use crate::query::ParsedQuery;
use crate::ranking::{self, RankingWeights};
use crate::sources::{QueryContext, Source, Tier};

/// Content matches never outrank a good file-name match: their match quality
/// is scaled down by this factor.
const CONTENT_MATCH_SCALE: f32 = 0.6;
pub const CONTENT_LIMIT: usize = 30;
/// Only this many top results get a snippet (each one re-reads its file).
const SNIPPET_RESULTS: usize = 10;
/// Snippet length in characters, and the lead-in before the first match.
const SNIPPET_LEN: usize = 160;
const SNIPPET_LEAD: usize = 50;
/// Wrap matched words in the `snippet` metadata string.
pub const MARK_START: char = '\u{1}';
pub const MARK_END: char = '\u{2}';

pub struct ContentSource {
    store: Arc<ContentStore>,
    weights: RankingWeights,
    clock: fn() -> u32,
}

impl ContentSource {
    pub fn new(store: Arc<ContentStore>, weights: RankingWeights) -> Self {
        Self {
            store,
            weights,
            clock: unix_now,
        }
    }

    pub fn with_clock(mut self, clock: fn() -> u32) -> Self {
        self.clock = clock;
        self
    }

    fn item(&self, hit: &ContentHit, best: f32, snippet: Option<String>) -> Item {
        let name = Path::new(&hit.path)
            .file_name()
            .map_or_else(|| hit.path.clone(), |n| n.to_string_lossy().into_owned());
        let mut item = Item::new(format!("file:{}", hit.path), Kind::File, name, self.id());
        let shown = snippet
            .as_ref()
            .map(|m| m.replace([MARK_START, MARK_END], ""))
            .unwrap_or_else(|| hit.preview.clone());
        item.subtitle = Some(if shown.is_empty() {
            hit.path.clone()
        } else {
            format!("{} — {shown}", hit.path)
        });
        if let Some(marked) = snippet {
            item.metadata
                .insert("snippet".into(), serde_json::Value::String(marked));
        }
        item.uri = Some(hit.path.clone());
        item.modified_at = (hit.mtime != 0).then_some(hit.mtime);
        if hit.kind == DocKind::Ocr {
            item.metadata
                .insert("ocr".into(), serde_json::Value::Bool(true));
        }
        item.metadata.insert(
            "text_preview".into(),
            serde_json::Value::String(hit.preview.clone()),
        );
        let features = ScoreFeatures {
            match_quality: CONTENT_MATCH_SCALE * (hit.score / best.max(f32::EPSILON)),
            frecency: 0.0,
            recency: ranking::recency(u32::try_from(hit.mtime).unwrap_or(0), (self.clock)()),
            semantic: None,
        };
        item.score = self.weights.score(&features);
        item.score_features = features;
        item
    }
}

impl Source for ContentSource {
    fn id(&self) -> &'static str {
        "content"
    }

    fn tier(&self) -> Tier {
        Tier::Smart
    }

    fn query(&self, q: &ParsedQuery, ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.tokens.is_empty() || !matches!(q.kind, None | Some(Kind::File | Kind::Screenshot)) {
            return;
        }
        let query = ContentQuery {
            text: &q.text,
            images_only: q.kind == Some(Kind::Screenshot),
            range: q.time.map(|t| (t.from, t.to)),
            limit: CONTENT_LIMIT,
        };
        let Ok(hits) = self.store.search(&query) else {
            return;
        };
        if ctx.is_cancelled() {
            return;
        }
        let best = hits.iter().map(|h| h.score).fold(0.0, f32::max);
        // First the results with their stored previews, so nothing waits on
        // file reads; then the same results again with snippets filled in.
        emit(hits.iter().map(|h| self.item(h, best, None)).collect());
        let mut snippets: Vec<Option<String>> = Vec::new();
        for h in hits.iter().take(SNIPPET_RESULTS) {
            if ctx.is_cancelled() {
                return;
            }
            snippets.push(snippet_for(h, &q.tokens));
        }
        if snippets.iter().any(Option::is_some) {
            emit(
                hits.iter()
                    .enumerate()
                    .map(|(i, h)| self.item(h, best, snippets.get(i).cloned().flatten()))
                    .collect(),
            );
        }
    }
}

/// A snippet for plain-text hits only: Office, PDF, OCR and sensitive files keep
/// the stored preview. The file is re-read and masked before anything is cut.
fn snippet_for(hit: &ContentHit, tokens: &[String]) -> Option<String> {
    let path = Path::new(&hit.path);
    if hit.kind != DocKind::Text || extract::classify(path) != FileClass::Text {
        return None;
    }
    let text = extract::read_text(path).ok()??;
    let (masked, _) = secrets::mask(&text);
    snippet(&masked, tokens)
}

/// About [`SNIPPET_LEN`] characters around the first match of any token, with
/// whitespace collapsed and matched words wrapped in [`MARK_START`]/[`MARK_END`].
/// `None` when no token occurs in `text`.
fn snippet(text: &str, tokens: &[String]) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let needles: Vec<Vec<char>> = tokens
        .iter()
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().collect())
        .collect();
    let at = |i: usize, n: &[char]| lower.get(i..i + n.len()) == Some(n);
    let first = (0..lower.len()).find(|&i| needles.iter().any(|n| at(i, n)))?;
    let mut start = first.saturating_sub(SNIPPET_LEAD);
    // Begin at a word start, not in the middle of a word.
    while start > 0 && start < first && !chars[start - 1].is_whitespace() {
        start += 1;
    }
    let end = (start + SNIPPET_LEN).min(chars.len());
    let mut marked = vec![false; end - start];
    for i in start..end {
        for n in &needles {
            if at(i, n) {
                for m in marked.iter_mut().skip(i - start).take(n.len()) {
                    *m = true;
                }
            }
        }
    }
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let (mut in_mark, mut last_space) = (false, true);
    for (k, &c) in chars[start..end].iter().enumerate() {
        let space = c.is_whitespace() || c.is_control();
        if marked[k] != in_mark && !(space && last_space) {
            out.push(if marked[k] { MARK_START } else { MARK_END });
            in_mark = marked[k];
        }
        if space {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    if in_mark {
        out.push(MARK_END);
    }
    let mut out = out.trim_end().to_owned();
    if end < chars.len() {
        out.push('…');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn snippet_centres_on_the_first_match_and_marks_matched_words() {
        let body = format!(
            "{} The Quarterly invoice is due. {}",
            "filler ".repeat(40),
            "tail ".repeat(60)
        );
        let s = snippet(&body, &toks(&["invoice"])).unwrap();
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains(&format!("{MARK_START}invoice{MARK_END}")), "{s}");
        assert!(s.chars().count() < 200);
        assert!(snippet(&body, &toks(&["zebra"])).is_none());
    }

    #[test]
    fn a_secret_next_to_the_match_is_masked_and_sensitive_files_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let body = "Meeting notes about the budget\npassword=hunter2hunter2\nmore budget talk";
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, body).unwrap();
        let hit = |p: &Path| ContentHit {
            path: p.to_string_lossy().into_owned(),
            kind: DocKind::Text,
            mtime: 1,
            preview: String::new(),
            score: 1.0,
        };
        let s = snippet_for(&hit(&txt), &toks(&["budget"])).unwrap();
        assert!(s.contains("budget") && !s.contains("hunter2"), "{s}");
        let env = dir.path().join("prod.env");
        std::fs::write(&env, body).unwrap();
        assert!(snippet_for(&hit(&env), &toks(&["budget"])).is_none());
        let pdf = dir.path().join("a.pdf");
        std::fs::write(&pdf, body).unwrap();
        assert!(snippet_for(&hit(&pdf), &toks(&["budget"])).is_none());
    }

    #[test]
    fn a_file_edited_after_indexing_keeps_its_stored_preview() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "the zebrafinding report").unwrap();
        let store = Arc::new(ContentStore::open_in_ram().unwrap());
        store
            .upsert(&crate::content::store::ContentDoc {
                path: p.to_string_lossy().into_owned(),
                mtime: 1,
                kind: DocKind::Text,
                text: "the zebrafinding report".into(),
            })
            .unwrap();
        store.commit().unwrap();
        std::fs::write(&p, "completely different words now").unwrap();
        let src = ContentSource::new(store, RankingWeights::default());
        let q = crate::query::parse("zebrafinding");
        let ctx = QueryContext::new(1, Arc::new(std::sync::atomic::AtomicU64::new(1)));
        let mut batches: Vec<Vec<Item>> = Vec::new();
        src.query(&q, &ctx, &mut |items| batches.push(items));
        let last = batches.last().unwrap();
        assert_eq!(last.len(), 1);
        assert!(!last[0].metadata.contains_key("snippet"));
        assert!(last[0]
            .subtitle
            .as_deref()
            .unwrap()
            .contains("the zebrafinding report"));
    }
}
