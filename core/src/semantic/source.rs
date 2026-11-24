//! The semantic source: files whose content means what the query says, found
//! by the helper's vectors. It is a smart-tier source, so its results arrive
//! after the instant ones; it answers only queries of two or more words that do
//! not start with a plugin's trigger word, waits a moment so a fast typist
//! never starts the helper, and gives up when the next keystroke arrives.
//! Whatever goes wrong (helper off, model missing, a crash) means no results,
//! silently; the reason is shown in the Plugins panel.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::client::SemanticClient;
use crate::content::store::ContentStore;
use crate::index_service::unix_now;
use crate::item::{Item, Kind, ScoreFeatures};
use crate::query::ParsedQuery;
use crate::ranking::{self, RankingWeights};
use crate::sources::{QueryContext, Source, Tier};

/// Wait this long without a newer keystroke before asking the helper.
const DEBOUNCE: Duration = Duration::from_millis(150);
const LIMIT: usize = 10;
/// Semantic matches count for at most this much match quality, below a good
/// name match (1.0) and below the full-text matches' 0.6.
const SEMANTIC_MATCH_SCALE: f32 = 0.5;
const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff"];

pub struct SemanticSource {
    client: SemanticClient,
    store: Arc<ContentStore>,
    weights: RankingWeights,
    /// First words that belong to plugins; such queries are theirs.
    triggers: Vec<String>,
    /// Hits below this similarity are dropped by the source as well (the
    /// helper applies the user's configured minimum first).
    min_similarity: Box<dyn Fn() -> f32 + Send + Sync>,
}

impl SemanticSource {
    pub fn new(
        client: SemanticClient,
        store: Arc<ContentStore>,
        weights: RankingWeights,
        triggers: Vec<String>,
        min_similarity: Box<dyn Fn() -> f32 + Send + Sync>,
    ) -> Self {
        Self {
            client,
            store,
            weights,
            triggers: triggers.into_iter().map(|t| t.to_lowercase()).collect(),
            min_similarity,
        }
    }

    fn is_trigger_query(&self, q: &ParsedQuery) -> bool {
        let first = q.raw.split_whitespace().next().unwrap_or("").to_lowercase();
        self.triggers.contains(&first)
    }
}

impl Source for SemanticSource {
    fn id(&self) -> &'static str {
        "semantic"
    }

    fn tier(&self) -> Tier {
        Tier::Smart
    }

    fn query(&self, q: &ParsedQuery, ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.tokens.len() < 2
            || !matches!(q.kind, None | Some(Kind::File | Kind::Screenshot))
            || self.is_trigger_query(q)
            || !self.client.usable()
        {
            return;
        }
        let until = Instant::now() + DEBOUNCE;
        while Instant::now() < until {
            if ctx.is_cancelled() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let cancelled = || ctx.is_cancelled();
        let Ok(hits) = self.client.search(&q.text, LIMIT * 2, &cancelled) else {
            return;
        };
        if ctx.is_cancelled() {
            return;
        }
        let min = (self.min_similarity)().clamp(0.0, 0.95);
        let images_only = q.kind == Some(Kind::Screenshot);
        let now = unix_now();
        let mut items = Vec::new();
        for hit in hits {
            if hit.similarity < min {
                continue;
            }
            let path = Path::new(&hit.path);
            let is_image = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)));
            if images_only && !is_image {
                continue;
            }
            // Only files the content index still has: deleted, excluded or
            // moved-out-of-root files drop out here even if a vector is left.
            let Some(mtime) = self.store.indexed_mtime(&hit.path) else {
                continue;
            };
            let name = path
                .file_name()
                .map_or_else(|| hit.path.clone(), |n| n.to_string_lossy().into_owned());
            let mut item = Item::new(format!("file:{}", hit.path), Kind::File, name, self.id());
            item.subtitle = Some(format!("{} — similar meaning", hit.path));
            item.uri = Some(hit.path.clone());
            item.modified_at = (mtime != 0).then_some(mtime);
            item.metadata
                .insert("similarity".into(), serde_json::json!(hit.similarity));
            let features = ScoreFeatures {
                match_quality: SEMANTIC_MATCH_SCALE
                    * ((hit.similarity - min) / (1.0 - min)).clamp(0.0, 1.0),
                frecency: 0.0,
                recency: ranking::recency(u32::try_from(mtime).unwrap_or(0), now),
                semantic: Some(hit.similarity),
            };
            item.score = self.weights.score(&features);
            item.score_features = features;
            items.push(item);
            if items.len() >= LIMIT {
                break;
            }
        }
        if !items.is_empty() {
            emit(items);
        }
    }
}
