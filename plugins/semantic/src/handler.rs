//! The helper's side of the index API (see `docs/plugins.md`, "Index API").
//! The model is loaded on first use; a failure to load is remembered (and
//! reported as the reason) until `index_status` asks again.

use std::path::PathBuf;

use lookout_core::crypto::Key;
use lookout_core::plugins::sdk::Handler;
use serde_json::{json, Value};

use crate::embed::{Embedder, Loader};
use crate::store::Store;

enum Model {
    Unloaded,
    Ready(Box<dyn Embedder>),
    Failed(String),
}

pub struct Semantic {
    loader: Loader,
    model: Model,
    store: Option<Store>,
    data_dir: Option<PathBuf>,
    key: Option<Key>,
    min_similarity: f32,
}

/// "my_tax-notes.v2.txt" becomes "my tax notes v2": the words of the name.
fn name_words(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.chars()
        .map(|c| if matches!(c, '_' | '-' | '.') { ' ' } else { c })
        .collect()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

impl Semantic {
    pub fn new(loader: Loader) -> Self {
        Self {
            loader,
            model: Model::Unloaded,
            store: None,
            data_dir: None,
            key: None,
            min_similarity: 0.6,
        }
    }

    fn assets(&self) -> Result<PathBuf, String> {
        self.data_dir
            .as_ref()
            .map(|d| d.join("assets"))
            .ok_or_else(|| "not initialized".to_owned())
    }

    fn embedder(&mut self) -> Result<&mut dyn Embedder, String> {
        if matches!(self.model, Model::Unloaded) {
            let dir = self.assets()?;
            self.model = match (self.loader)(&dir) {
                Ok(e) => Model::Ready(e),
                Err(why) => Model::Failed(why),
            };
        }
        match &mut self.model {
            Model::Ready(e) => Ok(e.as_mut()),
            Model::Failed(why) => Err(why.clone()),
            Model::Unloaded => Err("the model did not load".into()),
        }
    }

    fn store(&mut self) -> Result<&mut Store, String> {
        if self.store.is_none() {
            let dir = self.data_dir.clone().ok_or("not initialized")?;
            let _ = std::fs::create_dir_all(&dir);
            self.store = Some(Store::open(Some(dir.join("vectors.log")), self.key.clone()));
        }
        self.store.as_mut().ok_or_else(|| "no store".into())
    }

    fn status(&mut self) -> Value {
        // Ask again: the user may have fixed the files since the last failure.
        if matches!(self.model, Model::Failed(_)) {
            self.model = Model::Unloaded;
        }
        let ready = self.embedder().map(|_| ());
        let count = self.store().map_or(0, |st| st.vector_count());
        match ready {
            Ok(()) => json!({"ready": true, "reason": null, "count": count}),
            Err(why) => json!({"ready": false, "reason": why, "count": count}),
        }
    }

    fn needs(&mut self, params: &Value) -> Result<Value, String> {
        let items = params
            .get("items")
            .and_then(Value::as_array)
            .ok_or("no items")?;
        let store = self.store()?;
        let need: Vec<&str> = items
            .iter()
            .filter(|i| store.mtime_of(s(i, "path")) != i.get("mtime").and_then(Value::as_u64))
            .map(|i| s(i, "path"))
            .collect();
        Ok(json!({"need": need}))
    }

    fn batch(&mut self, params: &Value) -> Result<Value, String> {
        let docs = params
            .get("docs")
            .and_then(Value::as_array)
            .ok_or("no docs")?;
        let mut out = Vec::with_capacity(docs.len());
        for d in docs {
            let path = s(d, "path").to_owned();
            let mtime = d.get("mtime").and_then(Value::as_u64).unwrap_or(0);
            let text = s(d, "text");
            let vector = if text.trim().is_empty() {
                None
            } else {
                let passage = format!("{}. {}", name_words(s(d, "name")), text);
                Some(self.embedder()?.embed(&passage, false)?)
            };
            out.push((path, mtime, vector));
        }
        let n = out.len();
        self.store()?.put_many(out)?;
        Ok(json!({"stored": n}))
    }

    fn search(&mut self, params: &Value) -> Result<Value, String> {
        let text = s(params, "text");
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .min(100) as usize;
        let min = self.min_similarity;
        if self.store()?.vector_count() == 0 {
            return Ok(json!({"hits": []}));
        }
        let q = self.embedder()?.embed(text, true)?;
        let hits: Vec<Value> = self
            .store()?
            .search(&q, limit, min)
            .into_iter()
            .map(|(path, score)| json!({"path": path, "score": score}))
            .collect();
        Ok(json!({"hits": hits}))
    }
}

impl Handler for Semantic {
    fn handle(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "initialize" => {
                self.data_dir = params
                    .get("data_dir")
                    .and_then(Value::as_str)
                    .map(PathBuf::from);
                self.key = params
                    .get("key")
                    .and_then(Value::as_str)
                    .and_then(Key::from_hex);
                if let Some(m) = params
                    .get("config")
                    .and_then(|c| c.get("min_similarity"))
                    .and_then(Value::as_f64)
                {
                    self.min_similarity = (m as f32).clamp(0.0, 0.99);
                }
                Ok(json!({"ok": true}))
            }
            "index_status" => Ok(self.status()),
            "index_needs" => self.needs(&params),
            "index_batch" => self.batch(&params),
            "index_search" => self.search(&params),
            "index_remove" => {
                let paths: Vec<String> = params
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let n = self.store()?.remove_many(&paths)?;
                Ok(json!({"removed": n}))
            }
            "index_list" => {
                let after = params
                    .get("after")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(1000)
                    .min(5000);
                let paths = self.store()?.list(after.as_deref(), limit as usize);
                Ok(json!({"paths": paths}))
            }
            "index_delete" => {
                let n = self.store()?.clear();
                Ok(json!({"deleted": n}))
            }
            other => Err(format!("unsupported method {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::DIM;

    /// Bag-of-words hashing embedder: texts that share words are similar.
    struct Fake;
    impl Embedder for Fake {
        fn embed(&mut self, text: &str, _q: bool) -> Result<Vec<f32>, String> {
            let mut v = vec![0f32; DIM];
            for w in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
                if w.len() > 2 {
                    let h = w
                        .bytes()
                        .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
                    v[(h % DIM as u64) as usize] += 1.0;
                }
            }
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
            Ok(v.into_iter().map(|x| x / n).collect())
        }
    }

    fn make(dir: &std::path::Path, fail: Option<&'static str>) -> Semantic {
        let mut h = Semantic::new(Box::new(move |_| match fail {
            Some(why) => Err(why.to_owned()),
            None => Ok(Box::new(Fake)),
        }));
        let key = Key::generate().unwrap().to_hex();
        h.handle(
            "initialize",
            json!({"data_dir": dir, "key": key, "config": {"min_similarity": 0.3}}),
        )
        .unwrap();
        h
    }

    fn doc(path: &str, text: &str) -> Value {
        json!({"path": path, "name": "n.txt", "mtime": 5, "text": text})
    }

    #[test]
    fn indexes_searches_and_skips_up_to_date_files() {
        let d = tempfile::tempdir().unwrap();
        let mut h = make(d.path(), None);
        h.handle(
            "index_batch",
            json!({"docs": [doc("a", "garden tomatoes watering schedule"),
                            doc("b", "mortgage interest rate refinancing"), doc("c", "  ")]}),
        )
        .unwrap();
        let hits = h
            .handle(
                "index_search",
                json!({"text": "tomatoes watering", "limit": 5}),
            )
            .unwrap();
        assert_eq!(hits["hits"][0]["path"], "a");
        assert_eq!(hits["hits"].as_array().unwrap().len(), 1);
        let need = h
            .handle(
                "index_needs",
                json!({"items": [{"path": "a", "mtime": 5}, {"path": "c", "mtime": 5},
                                 {"path": "a2", "mtime": 5}, {"path": "b", "mtime": 6}]}),
            )
            .unwrap();
        assert_eq!(need["need"], json!(["a2", "b"]));
    }

    #[test]
    fn the_index_survives_a_restart_and_a_torn_tail_and_can_be_deleted() {
        let d = tempfile::tempdir().unwrap();
        let key = Key::generate().unwrap().to_hex();
        let boot = |key: &str| {
            let mut h = Semantic::new(Box::new(|_| Ok(Box::new(Fake))));
            h.handle(
                "initialize",
                json!({"data_dir": d.path(), "key": key, "config": {}}),
            )
            .unwrap();
            h
        };
        let mut h = boot(&key);
        h.handle(
            "index_batch",
            json!({"docs": [doc("a", "alpha beta gamma")]}),
        )
        .unwrap();
        h.handle(
            "index_batch",
            json!({"docs": [doc("b", "delta epsilon zeta")]}),
        )
        .unwrap();
        h.handle("index_remove", json!({"paths": ["a"]})).unwrap();
        drop(h);
        let log = d.path().join("vectors.log");
        let mut bytes = std::fs::read(&log).unwrap();
        bytes.extend_from_slice(&[9, 0, 0, 0, 1, 2]); // a torn frame
        std::fs::write(&log, bytes).unwrap();
        let mut h = boot(&key);
        assert_eq!(
            h.handle("index_list", json!({})).unwrap()["paths"],
            json!(["b"])
        );
        // Another key cannot read it: starts empty rather than failing.
        let other = Key::generate().unwrap().to_hex();
        let mut h2 = boot(&other);
        assert_eq!(
            h2.handle("index_list", json!({})).unwrap()["paths"],
            json!([])
        );
        let mut h3 = boot(&key);
        h3.handle("index_delete", Value::Null).unwrap();
        assert!(!log.exists() || h3.handle("index_list", json!({})).unwrap()["paths"] == json!([]));
    }

    #[test]
    fn a_model_that_cannot_load_gives_a_reason_and_leaves_the_rest_working() {
        let d = tempfile::tempdir().unwrap();
        let mut h = make(d.path(), Some("model.onnx is missing"));
        let st = h.handle("index_status", Value::Null).unwrap();
        assert_eq!(st["ready"], false);
        assert!(st["reason"].as_str().unwrap().contains("missing"));
        assert!(h
            .handle(
                "index_batch",
                json!({"docs": [doc("a", "some words here")]})
            )
            .is_err());
        assert!(h.handle("index_search", json!({"text": "x y"})).is_ok()); // empty index: nothing to say
        assert!(h.handle("index_needs", json!({"items": []})).is_ok());
        // Files without text need no model.
        h.handle("index_batch", json!({"docs": [doc("e", "")]}))
            .unwrap();
    }

    #[test]
    fn name_words_drop_the_extension_and_separators() {
        assert_eq!(name_words("my_tax-notes.v2.txt"), "my tax notes v2");
    }
}
