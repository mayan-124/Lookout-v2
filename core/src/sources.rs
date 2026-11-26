//! Sources turn a parsed query into items (brief section 7.4). Phase 1 has
//! four, all in the `Instant` tier: calculator, unit conversion, apps and file names.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use crate::apps::AppEntry;
use crate::calc;
use crate::frecency::FrecencyStore;
use crate::index_service::{read, unix_now, SharedIndex};
use crate::item::{Item, Kind, ScoreFeatures};
use crate::name_index::{typo_word, SearchParams, INITIALS_QUALITY, TYPO_QUALITY};
use crate::query::ParsedQuery;
use crate::ranking::{self, RankingWeights};
use crate::units;

/// Calculator and conversion answers outrank every file or app match.
pub const ANSWER_SCORE: f32 = 100.0;
/// Added to app scores so that an app beats a file of the same name.
pub const APP_BONUS: f32 = 0.25;
/// Most file results a single query returns.
pub const FILE_LIMIT: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Must answer from memory in under 20 ms.
    Instant,
    Smart,
}

/// Lets a source notice that its query has been superseded.
#[derive(Clone)]
pub struct QueryContext {
    generation: u64,
    current: Arc<AtomicU64>,
}

impl QueryContext {
    pub fn new(generation: u64, current: Arc<AtomicU64>) -> Self {
        Self {
            generation,
            current,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// True once a newer query generation has been submitted. Sources should
    /// stop work when this is true; anything they emit afterwards is dropped.
    pub fn is_cancelled(&self) -> bool {
        self.current.load(Ordering::Acquire) != self.generation
    }
}

pub trait Source: Send + Sync {
    fn id(&self) -> &'static str;
    fn tier(&self) -> Tier;
    /// Calls `emit` with this source's results. Each call replaces the
    /// source's earlier results for this query, so a source can refine its
    /// answer progressively by emitting a longer list.
    fn query(&self, q: &ParsedQuery, ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>));
}

pub struct CalculatorSource;

impl Source for CalculatorSource {
    fn id(&self) -> &'static str {
        "calculator"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        // "2026-12-25" is a date, not a subtraction.
        let today = chrono::Local::now().date_naive();
        if q.kind.is_some() || crate::dates::answer(&q.raw, today).is_some() {
            return;
        }
        let Some(value) = calc::try_evaluate(&q.raw) else {
            return;
        };
        let text = calc::format_number(value);
        let mut item = Item::new(
            format!("calc:{}", q.raw),
            Kind::Custom("calculator".into()),
            text.clone(),
            self.id(),
        );
        item.subtitle = Some(format!("{} =", q.raw));
        item.uri = Some(text);
        item.score = ANSWER_SCORE;
        item.score_features.match_quality = 1.0;
        emit(vec![item]);
    }
}

pub struct UnitsSource;

impl Source for UnitsSource {
    fn id(&self) -> &'static str {
        "units"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.kind.is_some() {
            return;
        }
        let Some(conv) = units::convert(&q.raw) else {
            return;
        };
        let text = conv.result_text();
        let mut item = Item::new(
            format!("convert:{}", q.raw),
            Kind::Custom("conversion".into()),
            text.clone(),
            self.id(),
        );
        item.subtitle = Some(conv.summary());
        item.uri = Some(text);
        item.score = ANSWER_SCORE;
        item.score_features.match_quality = 1.0;
        emit(vec![item]);
    }
}

/// The app list: Store apps are added to it after start.
pub type SharedApps = Arc<RwLock<Vec<AppEntry>>>;

pub struct AppsSource {
    apps: SharedApps,
    frecency: Arc<FrecencyStore>,
    weights: RankingWeights,
    clock: fn() -> u32,
}

impl AppsSource {
    pub fn new(apps: Vec<AppEntry>, frecency: Arc<FrecencyStore>, weights: RankingWeights) -> Self {
        Self {
            apps: Arc::new(RwLock::new(apps)),
            frecency,
            weights,
            clock: unix_now,
        }
    }

    pub fn shared(&self) -> SharedApps {
        self.apps.clone()
    }

    pub fn app_count(&self) -> usize {
        self.apps
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn item(&self, app: &AppEntry, match_quality: f32, now: i64) -> Item {
        let path = app.path.to_string_lossy().into_owned();
        let id = format!("app:{path}");
        let features = ScoreFeatures {
            match_quality,
            frecency: ranking::frecency_from_level(ranking::frecency_level(
                self.frecency.score(&id, now),
            )),
            recency: 0.0,
            semantic: None,
        };
        let mut item = Item::new(id, Kind::App, app.name.clone(), self.id());
        item.subtitle = Some("Application".into());
        item.uri = Some(path);
        item.score = self.weights.score(&features) + APP_BONUS;
        item.score_features = features;
        item
    }
}

/// Whether `token` is the first letters of the first words of `name`:
/// "vs" and "vsc" both match "visual studio code".
fn initials_prefix(name: &[u8], token: &[u8]) -> bool {
    let mut firsts = name
        .split(|b| matches!(b, b' ' | b'.' | b'_' | b'-'))
        .filter(|w| !w.is_empty())
        .map(|w| w[0]);
    token.iter().all(|&t| firsts.next() == Some(t))
}

impl Source for AppsSource {
    fn id(&self) -> &'static str {
        "apps"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        // Operators ask for files; apps have no extension, size or folder.
        if q.is_empty() || !q.ops.is_empty() || !matches!(q.kind, None | Some(Kind::App)) {
            return;
        }
        let folded: Vec<String> = q.tokens.iter().map(|t| ranking::fold_lower(t)).collect();
        let tokens: Vec<&[u8]> = folded.iter().map(String::as_bytes).collect();
        let now = i64::from((self.clock)());
        let apps = self.apps.read().unwrap_or_else(PoisonError::into_inner);
        let mut items = Vec::new();
        for app in apps.iter() {
            if ctx.is_cancelled() {
                return;
            }
            let name = app.lower.as_bytes();
            let name = name
                .strip_suffix(b".exe")
                .or_else(|| name.strip_suffix(b".lnk"))
                .unwrap_or(name);
            let mut m = ranking::match_quality_tiered(name, &tokens, &|| 0);
            // "chrome" is the whole product name of "Google Chrome": the last word counts as exact.
            if let (Some(sp), [t]) = (name.iter().rposition(|&b| b == b' '), tokens.as_slice()) {
                if name[sp + 1..] == **t {
                    let exact = ranking::match_quality_tiered(&name[sp + 1..], &tokens, &|| 0);
                    m = m.into_iter().chain(exact).reduce(f32::max);
                }
            }
            let Some(m) = m else {
                continue;
            };
            items.push(self.item(app, m, now));
        }
        // Like file names: only when nothing matched, try initials ("vsc") and a typo.
        if items.is_empty() {
            if let [t] = tokens.as_slice() {
                if t.iter().all(u8::is_ascii_lowercase) {
                    for app in apps.iter() {
                        let quality = if (2..=4).contains(&t.len()) {
                            initials_prefix(app.lower.as_bytes(), t).then_some(INITIALS_QUALITY)
                        } else if t.len() >= 5 {
                            typo_word(app.lower.as_bytes(), t).map(|_| TYPO_QUALITY)
                        } else {
                            None
                        };
                        if let Some(q) = quality {
                            items.push(self.item(app, q, now));
                        }
                    }
                }
            }
        }
        items.sort_by(|a, b| b.score.total_cmp(&a.score));
        items.truncate(FILE_LIMIT);
        emit(items);
    }
}

pub struct FilesSource {
    index: SharedIndex,
    weights: RankingWeights,
    clock: fn() -> u32,
}

impl FilesSource {
    pub fn new(index: SharedIndex, weights: RankingWeights) -> Self {
        Self {
            index,
            weights,
            clock: unix_now,
        }
    }

    /// Replaces the clock; tests use a fixed time.
    pub fn with_clock(mut self, clock: fn() -> u32) -> Self {
        self.clock = clock;
        self
    }
}

impl Source for FilesSource {
    fn id(&self) -> &'static str {
        "files"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.is_empty() {
            return;
        }
        // `name:` words rank like typed words; the filter keeps them in the name.
        let tokens: Vec<Vec<u8>> = q
            .tokens
            .iter()
            .map(|t| ranking::fold_lower(t))
            .chain(q.ops.names.iter().cloned())
            .map(String::into_bytes)
            .collect();
        let items = {
            let idx = read(&self.index);
            let filter = (!q.ops.is_empty()).then(|| idx.filter(&q.ops));
            let params = SearchParams {
                tokens: &tokens,
                kind: q.kind.as_ref(),
                limit: FILE_LIMIT,
                now: (self.clock)(),
                weights: self.weights,
                time: q.time.map(|t| {
                    (
                        u32::try_from(t.from).unwrap_or(u32::MAX),
                        u32::try_from(t.to).unwrap_or(u32::MAX),
                    )
                }),
                exts: q.exts,
                filter: filter.as_ref(),
            };
            let (hits, corrected) = idx.search_noted(&params, &|| ctx.is_cancelled());
            if ctx.is_cancelled() {
                return;
            }
            hits.iter()
                .filter_map(|h| {
                    let info = idx.entry_info(h.entry)?;
                    let path = info.path.to_string_lossy().into_owned();
                    let (prefix, kind) = if info.is_dir {
                        ("folder", Kind::Folder)
                    } else {
                        ("file", Kind::File)
                    };
                    let mut item =
                        Item::new(format!("{prefix}:{path}"), kind, info.name, self.id());
                    item.subtitle = Some(path.clone());
                    item.uri = Some(path);
                    item.modified_at = (info.modified != 0).then_some(u64::from(info.modified));
                    item.score = h.score;
                    item.score_features = h.features;
                    if let Some(word) = &corrected {
                        item.metadata.insert(
                            "corrected_word".into(),
                            serde_json::Value::String(word.clone()),
                        );
                    }
                    Some(item)
                })
                .collect::<Vec<_>>()
        };
        emit(items);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_service::{new_shared_index, write};
    use std::path::{Path, PathBuf};

    fn run(source: &dyn Source, text: &str) -> Vec<Item> {
        let current = Arc::new(AtomicU64::new(1));
        let ctx = QueryContext::new(1, current);
        let q = crate::query::parse(text);
        let mut out = Vec::new();
        source.query(&q, &ctx, &mut |items| out = items);
        out
    }

    #[test]
    fn calculator_answers_arithmetic_only() {
        let items = run(&CalculatorSource, "2+2*3");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "8");
        assert_eq!(items[0].score, ANSWER_SCORE);
        assert!(run(&CalculatorSource, "report").is_empty());
        assert!(run(&CalculatorSource, "kind:file 2+2").is_empty());
    }

    #[test]
    fn units_answers_conversions_only() {
        let items = run(&UnitsSource, "10 km to miles");
        assert_eq!(items[0].title, "6.21371 mi");
        assert!(run(&UnitsSource, "10 km").is_empty());
    }

    fn apps() -> Vec<AppEntry> {
        ["Calculator", "Notepad", "Notes Pro"]
            .iter()
            .map(|n| AppEntry {
                name: (*n).to_owned(),
                lower: n.to_lowercase(),
                path: PathBuf::from(format!("/start/{n}.lnk")),
            })
            .collect()
    }

    #[test]
    fn apps_match_and_frecency_reorders() {
        let store = Arc::new(FrecencyStore::open_in_memory().unwrap());
        let src = AppsSource::new(apps(), store.clone(), RankingWeights::default());
        let before = run(&src, "not");
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].title, "Notepad"); // shorter name wins at equal match
        let now = i64::from(unix_now());
        for _ in 0..5 {
            store
                .record_open(
                    &format!("app:{}", Path::new("/start/Notes Pro.lnk").display()),
                    now,
                )
                .unwrap();
        }
        let after = run(&src, "not");
        assert_eq!(after[0].title, "Notes Pro");
        assert_eq!(after[0].kind, Kind::App);
        assert!(run(&src, "kind:file not").is_empty());
        assert_eq!(run(&src, "kind:app not").len(), 2);
    }

    #[test]
    fn apps_fall_back_to_initials_and_typos_when_nothing_matches() {
        let store = Arc::new(FrecencyStore::open_in_memory().unwrap());
        let apps = ["Visual Studio Code", "Visual Studio 2022", "Calculator"]
            .iter()
            .map(|n| AppEntry {
                name: (*n).to_owned(),
                lower: n.to_lowercase(),
                path: PathBuf::from(format!("/start/{n}.lnk")),
            })
            .collect();
        let src = AppsSource::new(apps, store, RankingWeights::default());
        let titles = |q: &str| {
            run(&src, q)
                .into_iter()
                .map(|i| i.title)
                .collect::<Vec<_>>()
        };
        assert_eq!(titles("vsc"), ["Visual Studio Code"]);
        assert_eq!(titles("vs").len(), 2);
        assert_eq!(titles("calculater"), ["Calculator"]);
        assert!(titles("xyz").is_empty());
        // A real match suppresses the fallbacks.
        assert_eq!(titles("calc"), ["Calculator"]);
    }

    #[test]
    fn files_source_returns_items_with_paths() {
        let index = new_shared_index();
        {
            let mut w = write(&index);
            let root = w.add_root(Path::new("/r"), 0).unwrap();
            let sub = w.add_dir(root, "Reports", 5).unwrap();
            w.add_file(sub, "Q1 Report.xlsx", 10).unwrap();
        }
        let src = FilesSource::new(index, RankingWeights::default()).with_clock(|| 100);
        let items = run(&src, "report");
        assert_eq!(items.len(), 2);
        let file = items.iter().find(|i| i.kind == Kind::File).unwrap();
        assert_eq!(file.title, "Q1 Report.xlsx");
        assert!(file.id.starts_with("file:"));
        let expected = Path::new("/r").join("Reports").join("Q1 Report.xlsx");
        assert_eq!(
            file.uri.as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );
        assert_eq!(file.modified_at, Some(10));
        assert_eq!(run(&src, "kind:folder report").len(), 1);
        assert!(run(&src, "").is_empty());
    }

    #[test]
    fn cancelled_sources_emit_nothing() {
        let index = new_shared_index();
        {
            let mut w = write(&index);
            let root = w.add_root(Path::new("/r"), 0).unwrap();
            w.add_file(root, "report.txt", 0).unwrap();
        }
        let src = FilesSource::new(index, RankingWeights::default());
        let current = Arc::new(AtomicU64::new(2)); // a newer generation exists
        let ctx = QueryContext::new(1, current);
        assert!(ctx.is_cancelled());
        let mut called = false;
        src.query(&crate::query::parse("report"), &ctx, &mut |_| called = true);
        assert!(!called);
    }
}
