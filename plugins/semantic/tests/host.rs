//! The real helper process behind the real host: missing files, a wrong hash,
//! and (with `LOOKOUT_SEMANTIC_ASSETS`) the whole path from `index_batch` to a
//! search, plus a restart that keeps the encrypted index. Temp folders only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lookout_core::crypto::KeyStore;
use lookout_core::pause::PauseState;
use lookout_core::plugins::{HostConfig, PluginHost};
use lookout_core::semantic::client::SemDoc;
use lookout_core::semantic::{SemanticClient, PLUGIN_ID};

fn setup(dir: &Path) -> (Arc<PluginHost>, SemanticClient) {
    let plugin = dir.join("plugins").join("semantic");
    fs::create_dir_all(&plugin).unwrap();
    let exe = plugin.join("lookout-semantic.exe");
    let built = env!("CARGO_BIN_EXE_lookout-semantic");
    if fs::hard_link(built, &exe).is_err() {
        fs::copy(built, &exe).unwrap();
    }
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json"),
        plugin.join("plugin.json"),
    )
    .unwrap();
    let mut cfg = HostConfig::new(vec![dir.join("plugins")], dir.join("data"));
    cfg.master_key = Some(
        KeyStore::new(dir.join("lookout.key"))
            .load_or_create()
            .unwrap(),
    );
    let host = PluginHost::start(cfg, PauseState::new());
    host.set_enabled(PLUGIN_ID, true, true).unwrap();
    let client = SemanticClient::new(host.clone(), PLUGIN_ID, &dir.join("data"));
    (host, client)
}

fn never() -> bool {
    false
}

#[test]
fn missing_files_and_wrong_hashes_give_a_reason_and_no_results() {
    let dir = tempfile::tempdir().unwrap();
    let (host, client) = setup(dir.path());
    assert!(client.missing_reason().unwrap().contains("model.onnx"));
    let st = client.status(&never).unwrap();
    assert!(!st.ready);
    assert!(st.reason.unwrap().contains("missing"));
    // Files that are not the pinned ones are refused, naming the file.
    let assets = client.assets_dir().to_path_buf();
    fs::create_dir_all(&assets).unwrap();
    for f in ["model.onnx", "tokenizer.json", "onnxruntime.dll"] {
        fs::write(assets.join(f), b"not the real file").unwrap();
    }
    assert!(client.missing_reason().is_none());
    let reason = client.status(&never).unwrap().reason.unwrap();
    assert!(reason.contains("pinned SHA-256"), "{reason}");
    // Files without text need no model, so the index API still answers.
    let doc = SemDoc {
        path: "C:/a.txt".into(),
        name: "a.txt".into(),
        mtime: 1,
        text: String::new(),
    };
    assert_eq!(client.index_batch(&[doc], &never).unwrap(), 1);
    // A document with text cannot be embedded: an error, nothing stored.
    let doc = SemDoc {
        path: "C:/b.txt".into(),
        name: "b.txt".into(),
        mtime: 1,
        text: "some words".into(),
    };
    assert!(client.index_batch(&[doc], &never).is_err());
    assert!(client.search("some words", 5, &never).unwrap().is_empty());
    host.shutdown();
}

fn real_assets() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOOKOUT_SEMANTIC_ASSETS")?))
}

#[test]
fn indexing_search_restart_and_delete_with_the_real_model() {
    let Some(src) = real_assets() else {
        eprintln!("skipped: LOOKOUT_SEMANTIC_ASSETS is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (host, client) = setup(dir.path());
    fs::create_dir_all(client.assets_dir()).unwrap();
    for f in ["model.onnx", "tokenizer.json", "onnxruntime.dll"] {
        fs::copy(src.join(f), client.assets_dir().join(f)).unwrap();
    }
    let st = client.status(&never).unwrap();
    assert!(st.ready, "{:?}", st.reason);
    let docs: Vec<SemDoc> = [
        (
            "a",
            "pets.txt",
            "My cat likes to sleep on the sofa all afternoon.",
        ),
        (
            "b",
            "tax.txt",
            "Deadlines for filing the corporate income tax return.",
        ),
        (
            "c",
            "trip.txt",
            "Flight to Lisbon departs at dawn; hotel booked near the harbour.",
        ),
    ]
    .iter()
    .map(|(p, n, t)| SemDoc {
        path: (*p).into(),
        name: (*n).into(),
        mtime: 7,
        text: (*t).into(),
    })
    .collect();
    assert_eq!(client.index_batch(&docs, &never).unwrap(), 3);
    let hits = client.search("where the kitten naps", 3, &never).unwrap();
    assert_eq!(hits.first().map(|h| h.path.as_str()), Some("a"), "{hits:?}");
    let need = client
        .needs(&[("a".into(), 7), ("a2".into(), 7)], &never)
        .unwrap();
    assert_eq!(need, vec!["a2".to_owned()]);
    // The vectors are encrypted on disk: no document word appears in the file.
    let log = fs::read(
        dir.path()
            .join("data/plugins")
            .join(PLUGIN_ID)
            .join("vectors.log"),
    )
    .unwrap();
    assert!(!String::from_utf8_lossy(&log).contains("Lisbon"));
    assert!(!log.windows(7).any(|w| w == b"pets.txt"));
    // A fresh process (after a stop) still has them.
    host.set_enabled(PLUGIN_ID, false, false).unwrap();
    host.set_enabled(PLUGIN_ID, true, true).unwrap();
    let t = Instant::now();
    let again = client
        .search("flying to Portugal early", 3, &never)
        .unwrap();
    eprintln!("cold search after restart: {:?}", t.elapsed());
    assert_eq!(
        again.first().map(|h| h.path.as_str()),
        Some("c"),
        "{again:?}"
    );
    client.delete_all().unwrap();
    assert!(client
        .search("where the kitten naps", 3, &never)
        .unwrap()
        .is_empty());
    assert!(client.list(None, 10).unwrap().is_empty());
    host.shutdown();
    let _ = Duration::from_secs(0);
}
