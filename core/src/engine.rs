//! Query pipeline (brief section 7.5): every keystroke is a new *generation*;
//! sources run concurrently; one merger thread coalesces their results into
//! ranked updates (at most one per flush interval) and drops anything that
//! belongs to a generation other than the current one.
//!
//! Staleness is enforced in three places: sources can poll
//! [`QueryContext::is_cancelled`] and stop early; the merger ignores messages
//! whose generation is not the one it is building; and it re-checks the live
//! generation immediately before every delivery, so a result for a superseded
//! query cannot reach the sink even if its source ignored cancellation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::item::Item;
use crate::query::{self, ParsedQuery, Skip, Understood};
use crate::sources::{QueryContext, Source};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Update {
    pub generation: u64,
    /// Merged, ranked results so far (replaces any earlier update of this generation).
    pub items: Vec<Item>,
    /// True once every source has finished.
    pub done: bool,
    /// Date and kind words the query was understood to contain.
    pub filters: Understood,
}

pub type Sink = Arc<dyn Fn(Update) + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    pub flush_interval: Duration,
    pub max_results: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_millis(16),
            max_results: 50,
        }
    }
}

enum Msg {
    New {
        generation: u64,
        sources: usize,
        understood: Understood,
        exts: &'static [&'static str],
    },
    Batch {
        generation: u64,
        source: &'static str,
        items: Vec<Item>,
    },
    Done {
        generation: u64,
    },
    Shutdown,
}

pub struct Engine {
    sources: Vec<Arc<dyn Source>>,
    current: Arc<AtomicU64>,
    tx: Sender<Msg>,
    merger: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(
        sources: Vec<Arc<dyn Source>>,
        sink: Sink,
        config: EngineConfig,
    ) -> Result<Self, std::io::Error> {
        let current = Arc::new(AtomicU64::new(0));
        let (tx, rx) = mpsc::channel();
        let live = current.clone();
        let merger = std::thread::Builder::new()
            .name("lookout-merge".into())
            .spawn(move || merge_loop(&rx, &sink, &live, config))?;
        Ok(Self {
            sources,
            current,
            tx,
            merger: Some(merger),
        })
    }

    /// The newest generation submitted so far.
    pub fn current_generation(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    /// Starts a query. Returns `false` (and does nothing) unless `generation`
    /// is newer than every earlier submission. Older work is cancelled.
    pub fn submit(&self, generation: u64, text: &str) -> bool {
        self.submit_with(generation, text, Skip::default())
    }

    /// Like [`submit`](Self::submit), ignoring the natural-word filters in `skip`.
    pub fn submit_with(&self, generation: u64, text: &str, skip: Skip) -> bool {
        let previous = self.current.fetch_max(generation, Ordering::AcqRel);
        if generation <= previous {
            return false;
        }
        let parsed = Arc::new(query::parse_with(text, skip));
        // What follows "note:" is shown to the notes alone.
        let private = crate::notes::is_private(text);
        let wanted: Vec<&Arc<dyn Source>> = self
            .sources
            .iter()
            .filter(|s| !parsed.is_empty() && (!private || s.id() == "notes"))
            .collect();
        let runnable = wanted.len();
        // Sent before any source starts, so the merger always sees `New` first.
        if self
            .tx
            .send(Msg::New {
                generation,
                sources: runnable,
                understood: parsed.understood.clone(),
                exts: parsed.exts,
            })
            .is_err()
        {
            return false;
        }
        if runnable == 0 {
            return true;
        }
        for source in wanted {
            let ctx = QueryContext::new(generation, self.current.clone());
            if ctx.is_cancelled() {
                // Superseded while we were starting; the newer submit owns the merger now.
                break;
            }
            let (source, parsed, tx) = (source.clone(), parsed.clone(), self.tx.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("lookout-src-{}", source.id()))
                .spawn(move || run_source(&*source, &parsed, &ctx, &tx));
            if spawned.is_err() {
                // Count the source as finished so the query can still complete.
                let _ = self.tx.send(Msg::Done { generation });
            }
        }
        true
    }
}

fn run_source(source: &dyn Source, parsed: &ParsedQuery, ctx: &QueryContext, tx: &Sender<Msg>) {
    let generation = ctx.generation();
    source.query(parsed, ctx, &mut |items| {
        let _ = tx.send(Msg::Batch {
            generation,
            source: source.id(),
            items,
        });
    });
    let _ = tx.send(Msg::Done { generation });
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(h) = self.merger.take() {
            let _ = h.join();
        }
    }
}

struct MergeState {
    generation: u64,
    per_source: HashMap<&'static str, Vec<Item>>,
    remaining: usize,
    understood: Understood,
    exts: &'static [&'static str],
    dirty: bool,
    last_flush: Option<Instant>,
}

fn flush(state: &mut MergeState, sink: &Sink, live: &AtomicU64, max_results: usize, done: bool) {
    state.dirty = false;
    state.last_flush = Some(Instant::now());
    // A newer query was submitted: whatever we hold is stale, deliver nothing.
    if live.load(Ordering::Acquire) != state.generation {
        return;
    }
    let mut items: Vec<Item> = state.per_source.values().flatten().cloned().collect();
    items.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.title.cmp(&b.title))
    });
    // The same file can come from the name index and from its contents: keep the best.
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| seen.insert(i.id.clone()));
    // A kind word ("pdf", "photos") means files of that type only.
    if !state.exts.is_empty() {
        items.retain(|i| {
            i.uri.as_deref().is_some_and(|u| {
                std::path::Path::new(u)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| state.exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
            })
        });
    }
    items.truncate(max_results);
    sink(Update {
        generation: state.generation,
        items,
        done,
        filters: state.understood.clone(),
    });
}

fn merge_loop(rx: &Receiver<Msg>, sink: &Sink, live: &AtomicU64, cfg: EngineConfig) {
    let mut state = MergeState {
        generation: 0,
        per_source: HashMap::new(),
        remaining: 0,
        understood: Understood::default(),
        exts: &[],
        dirty: false,
        last_flush: None,
    };
    loop {
        let wait = match (state.dirty, state.last_flush) {
            (true, Some(t)) => cfg.flush_interval.saturating_sub(t.elapsed()),
            _ => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(Msg::New {
                generation,
                sources,
                understood,
                exts,
            }) => {
                if generation < state.generation {
                    continue;
                }
                state.generation = generation;
                state.per_source.clear();
                state.remaining = sources;
                state.understood = understood;
                state.exts = exts;
                state.dirty = false;
                state.last_flush = None;
                if sources == 0 {
                    flush(&mut state, sink, live, cfg.max_results, true);
                }
            }
            Ok(Msg::Batch {
                generation,
                source,
                items,
            }) => {
                if generation == state.generation {
                    // An empty batch changes nothing visible unless it clears earlier results.
                    let had_results = state.per_source.get(source).is_some_and(|v| !v.is_empty());
                    if had_results || !items.is_empty() {
                        state.per_source.insert(source, items);
                        state.dirty = true;
                    }
                }
            }
            Ok(Msg::Done { generation }) => {
                if generation == state.generation && state.remaining > 0 {
                    state.remaining -= 1;
                    if state.remaining == 0 {
                        flush(&mut state, sink, live, cfg.max_results, true);
                    }
                }
            }
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // First results go out immediately; later ones at most once per interval.
        if state.dirty
            && state
                .last_flush
                .is_none_or(|t| t.elapsed() >= cfg.flush_interval)
        {
            flush(&mut state, sink, live, cfg.max_results, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Kind;
    use crate::sources::Tier;
    use std::sync::mpsc::sync_channel;
    use std::sync::Mutex;

    type Behaviour = dyn Fn(&ParsedQuery, &QueryContext, &mut dyn FnMut(Vec<Item>)) + Send + Sync;

    struct TestSource {
        id: &'static str,
        behaviour: Box<Behaviour>,
    }

    impl Source for TestSource {
        fn id(&self) -> &'static str {
            self.id
        }

        fn tier(&self) -> Tier {
            Tier::Instant
        }

        fn query(&self, q: &ParsedQuery, ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
            (self.behaviour)(q, ctx, emit);
        }
    }

    fn source(
        id: &'static str,
        f: impl Fn(&ParsedQuery, &QueryContext, &mut dyn FnMut(Vec<Item>)) + Send + Sync + 'static,
    ) -> Arc<dyn Source> {
        Arc::new(TestSource {
            id,
            behaviour: Box::new(f),
        })
    }

    fn item(title: &str, score: f32) -> Item {
        let mut i = Item::new(format!("test:{title}"), Kind::File, title, "test");
        i.score = score;
        i
    }

    /// Collects every update the sink receives, in delivery order.
    struct Recorder {
        updates: Mutex<Vec<Update>>,
        tx: Sender<()>,
    }

    fn recorder() -> (Arc<Recorder>, Receiver<()>, Sink) {
        let (tx, rx) = mpsc::channel();
        let rec = Arc::new(Recorder {
            updates: Mutex::new(Vec::new()),
            tx,
        });
        let r = rec.clone();
        let sink: Sink = Arc::new(move |u| {
            r.updates.lock().unwrap().push(u);
            let _ = r.tx.send(());
        });
        (rec, rx, sink)
    }

    impl Recorder {
        fn all(&self) -> Vec<Update> {
            self.updates.lock().unwrap().clone()
        }
    }

    /// Blocks until a `done` update for `generation` has been delivered.
    fn wait_done(rec: &Recorder, rx: &Receiver<()>, generation: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if rec
                .all()
                .iter()
                .any(|u| u.generation == generation && u.done)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no done update for generation {generation}"
            );
            let _ = rx.recv_timeout(Duration::from_millis(50));
        }
    }

    fn assert_generations_never_go_backwards(updates: &[Update]) {
        for pair in updates.windows(2) {
            assert!(
                pair[0].generation <= pair[1].generation,
                "generation went backwards: {} then {}",
                pair[0].generation,
                pair[1].generation
            );
        }
    }

    #[test]
    fn old_generation_results_are_discarded_even_from_a_source_that_ignores_cancellation() {
        // The "slow" source blocks until released, then emits regardless of cancellation.
        let (gate_tx, gate_rx) = sync_channel::<()>(0);
        let gate_rx = Mutex::new(gate_rx);
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (finished_tx, finished_rx) = mpsc::channel::<()>();
        let started_tx = Mutex::new(started_tx);
        let finished_tx = Mutex::new(finished_tx);

        let src = source("test", move |q, _ctx, emit| {
            if q.text == "slow" {
                let _ = started_tx.lock().unwrap().send(());
                let _ = gate_rx.lock().unwrap().recv();
                emit(vec![item("STALE", 9.0)]);
                let _ = finished_tx.lock().unwrap().send(());
            } else {
                emit(vec![item(&format!("fresh {}", q.text), 1.0)]);
            }
        });
        let (rec, rx, sink) = recorder();
        let engine = Engine::new(vec![src], sink, EngineConfig::default()).unwrap();

        assert!(engine.submit(1, "slow"));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(engine.submit(2, "fast"));
        wait_done(&rec, &rx, 2);

        // Release the stale source and let it finish emitting into the pipeline.
        gate_tx.send(()).unwrap();
        finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // A later generation's `done` proves the merger has drained everything queued before it.
        assert!(engine.submit(3, "marker"));
        wait_done(&rec, &rx, 3);

        let updates = rec.all();
        assert!(
            updates
                .iter()
                .all(|u| u.items.iter().all(|i| i.title != "STALE")),
            "a stale result reached the sink: {updates:?}"
        );
        assert!(
            updates.iter().all(|u| u.generation != 1),
            "generation 1 was delivered"
        );
        let two = updates
            .iter()
            .find(|u| u.generation == 2 && u.done)
            .unwrap();
        assert_eq!(two.items[0].title, "fresh fast");
        assert_generations_never_go_backwards(&updates);
    }

    #[test]
    fn cooperating_sources_see_cancellation() {
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (exit_tx, exit_rx) = mpsc::channel::<bool>();
        let started_tx = Mutex::new(started_tx);
        let exit_tx = Mutex::new(exit_tx);
        let src = source("test", move |q, ctx, _emit| {
            if q.text == "long" {
                let _ = started_tx.lock().unwrap().send(());
                while !ctx.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let _ = exit_tx.lock().unwrap().send(true);
            }
        });
        let (_rec, _rx, sink) = recorder();
        let engine = Engine::new(vec![src], sink, EngineConfig::default()).unwrap();
        engine.submit(1, "long");
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        engine.submit(2, "other");
        assert!(exit_rx.recv_timeout(Duration::from_secs(5)).unwrap());
    }

    #[test]
    fn typing_quickly_delivers_only_the_final_query_after_the_last_keystroke() {
        let src = source("test", |q, ctx, emit| {
            std::thread::sleep(Duration::from_millis(3));
            if !ctx.is_cancelled() {
                emit(vec![item(&format!("result for {}", q.text), 1.0)]);
            }
        });
        let (rec, rx, sink) = recorder();
        let engine = Engine::new(vec![src], sink, EngineConfig::default()).unwrap();
        let mut typed = String::new();
        for g in 1..=60u64 {
            typed.push('x');
            engine.submit(g, &typed);
        }
        wait_done(&rec, &rx, 60);
        let updates = rec.all();
        assert_generations_never_go_backwards(&updates);
        let last = updates.last().unwrap();
        assert_eq!(last.generation, 60);
        assert_eq!(
            last.items[0].title,
            format!("result for {}", "x".repeat(60))
        );
        // Nothing may arrive for an older generation after the final one has been answered.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(rec.all().last().unwrap().generation, 60);
    }

    #[test]
    fn submits_that_are_not_newer_are_ignored() {
        let src = source("test", |_, _, emit| emit(vec![item("a", 1.0)]));
        let (rec, rx, sink) = recorder();
        let engine = Engine::new(vec![src], sink, EngineConfig::default()).unwrap();
        assert!(engine.submit(5, "q"));
        assert!(!engine.submit(5, "q"));
        assert!(!engine.submit(3, "q"));
        assert_eq!(engine.current_generation(), 5);
        wait_done(&rec, &rx, 5);
        assert!(rec.all().iter().all(|u| u.generation == 5));
    }

    #[test]
    fn first_results_are_immediate_and_later_ones_are_coalesced() {
        let src = source("test", |_, _, emit| {
            for n in 1..=300 {
                emit(vec![item(&format!("r{n}"), n as f32)]);
            }
        });
        let (rec, rx, sink) = recorder();
        let engine = Engine::new(vec![src], sink, EngineConfig::default()).unwrap();
        engine.submit(1, "q");
        wait_done(&rec, &rx, 1);
        let updates = rec.all();
        assert!(
            updates.len() <= 4,
            "300 rapid batches produced {} updates",
            updates.len()
        );
        assert!(!updates[0].done || updates.len() == 1);
        let last = updates.last().unwrap();
        assert!(last.done);
        assert_eq!(last.items[0].title, "r300");
    }

    #[test]
    fn results_from_all_sources_are_merged_ranked_and_truncated() {
        let a = source("a", |_, _, emit| {
            emit(vec![item("mid", 5.0), item("low", 1.0)])
        });
        let b = source("b", |_, _, emit| emit(vec![item("top", 9.0)]));
        let (rec, rx, sink) = recorder();
        let cfg = EngineConfig {
            max_results: 2,
            ..EngineConfig::default()
        };
        let engine = Engine::new(vec![a, b], sink, cfg).unwrap();
        engine.submit(1, "q");
        wait_done(&rec, &rx, 1);
        let last = rec.all().into_iter().last().unwrap();
        let titles: Vec<_> = last.items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["top", "mid"]);
    }

    #[test]
    fn empty_batches_do_not_cause_updates_but_clearing_batches_do() {
        let silent = source("silent", |_, _, emit| emit(Vec::new()));
        let shrinking = source("shrinking", |_, _, emit| {
            emit(vec![item("first", 1.0)]);
            std::thread::sleep(Duration::from_millis(40));
            emit(Vec::new()); // the source withdraws its result
        });
        let (rec, rx, sink) = recorder();
        let engine = Engine::new(vec![silent], sink, EngineConfig::default()).unwrap();
        engine.submit(1, "q");
        wait_done(&rec, &rx, 1);
        assert_eq!(
            rec.all().len(),
            1,
            "only the final done update, no empty flush before it"
        );

        let (rec, rx, sink) = recorder();
        let engine = Engine::new(vec![shrinking], sink, EngineConfig::default()).unwrap();
        engine.submit(1, "q");
        wait_done(&rec, &rx, 1);
        let all = rec.all();
        assert_eq!(all[0].items.len(), 1);
        assert!(all.last().unwrap().items.is_empty() && all.last().unwrap().done);
        drop(engine);
    }

    #[test]
    fn empty_query_completes_immediately_with_no_results() {
        let src = source("test", |_, _, emit| emit(vec![item("never", 1.0)]));
        let (rec, rx, sink) = recorder();
        let engine = Engine::new(vec![src], sink, EngineConfig::default()).unwrap();
        engine.submit(1, "   ");
        wait_done(&rec, &rx, 1);
        let u = rec.all();
        assert_eq!(u.len(), 1);
        assert!(u[0].items.is_empty() && u[0].done);
    }

    #[test]
    fn dropping_the_engine_stops_the_merger() {
        let (_rec, _rx, sink) = recorder();
        let engine = Engine::new(Vec::new(), sink, EngineConfig::default()).unwrap();
        drop(engine); // joins the merger thread; hangs here if shutdown is broken
    }
}
