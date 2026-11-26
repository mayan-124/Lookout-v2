//! Ranking regression test: runs the known queries in `fixtures/ranking/cases.json`
//! against an in-memory index and checks which result comes first.

// Test code: a failed setup step should fail the test loudly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use lookout_core::name_index::{NameIndex, SearchParams};
use lookout_core::query;
use lookout_core::ranking::RankingWeights;
use serde::Deserialize;

#[derive(Deserialize)]
struct FixtureFile {
    path: String,
    age_days: u32,
    #[serde(default)]
    frecency: u8,
}

#[derive(Deserialize)]
struct Case {
    query: String,
    top: Option<String>,
    top_absent: Option<String>,
}

#[derive(Deserialize)]
struct Fixture {
    now: u32,
    files: Vec<FixtureFile>,
    folders: Vec<String>,
    cases: Vec<Case>,
}

const ROOT: &str = "/fixture";

fn build(fx: &Fixture) -> NameIndex {
    let mut idx = NameIndex::new();
    // Folders are a year old so that recency does not dominate name matching.
    let folder_time = fx.now - 365 * 86_400;
    let root = idx.add_root(Path::new(ROOT), folder_time).unwrap();
    let ensure_dir = |idx: &mut NameIndex, rel: &Path| -> u32 {
        let mut dir = root;
        let mut path = PathBuf::from(ROOT);
        for part in rel.components() {
            path.push(part);
            dir = match idx.dir_id_for_path(&path) {
                Some(id) => id,
                None => idx
                    .add_dir(dir, &part.as_os_str().to_string_lossy(), folder_time)
                    .unwrap(),
            };
        }
        dir
    };
    for folder in &fx.folders {
        ensure_dir(&mut idx, Path::new(folder));
    }
    for f in &fx.files {
        let p = Path::new(&f.path);
        let dir = ensure_dir(&mut idx, p.parent().unwrap_or(Path::new("")));
        let name = p.file_name().unwrap().to_string_lossy();
        idx.add_file(dir, &name, fx.now - f.age_days * 86_400)
            .unwrap();
        if f.frecency > 0 {
            assert!(idx.set_frecency(&Path::new(ROOT).join(p), f.frecency));
        }
    }
    idx
}

fn top_names(idx: &NameIndex, fx: &Fixture, text: &str) -> Vec<String> {
    let q = query::parse(text);
    let tokens: Vec<Vec<u8>> = q.tokens.iter().map(|t| t.as_bytes().to_vec()).collect();
    let params = SearchParams {
        tokens: &tokens,
        kind: q.kind.as_ref(),
        limit: 10,
        now: fx.now,
        weights: RankingWeights::default(),
        time: None,
        exts: &[],
        filter: None,
    };
    idx.search(&params, &|| false)
        .iter()
        .filter_map(|h| idx.entry_info(h.entry))
        .map(|i| i.name)
        .collect()
}

#[test]
fn known_queries_rank_the_expected_result_first() {
    let fx: Fixture =
        serde_json::from_str(include_str!("../../fixtures/ranking/cases.json")).unwrap();
    let idx = build(&fx);
    let mut failures = Vec::new();
    for case in &fx.cases {
        let got = top_names(&idx, &fx, &case.query);
        if let Some(expected) = &case.top {
            if got.first() != Some(expected) {
                failures.push(format!(
                    "{:?}: expected {expected:?} first, got {got:?}",
                    case.query
                ));
            }
        }
        if let Some(absent) = &case.top_absent {
            if got.contains(absent) {
                failures.push(format!(
                    "{:?}: {absent:?} must not appear, got {got:?}",
                    case.query
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "ranking regressions:\n{}",
        failures.join("\n")
    );
}

#[test]
fn the_fixture_is_sensitive_to_weights() {
    // Guard against a fixture that passes whatever the weights are.
    let fx: Fixture =
        serde_json::from_str(include_str!("../../fixtures/ranking/cases.json")).unwrap();
    let idx = build(&fx);
    let q = query::parse("invoice");
    let tokens: Vec<Vec<u8>> = q.tokens.iter().map(|t| t.as_bytes().to_vec()).collect();
    let run = |weights| {
        let p = SearchParams {
            tokens: &tokens,
            kind: None,
            limit: 2,
            now: fx.now,
            weights,
            time: None,
            exts: &[],
            filter: None,
        };
        idx.search(&p, &|| false)
            .iter()
            .filter_map(|h| idx.entry_info(h.entry))
            .map(|i| i.name)
            .collect::<Vec<_>>()
    };
    let with = run(RankingWeights::default());
    let without = run(RankingWeights {
        w_frecency: 0.0,
        ..RankingWeights::default()
    });
    assert_eq!(with[0], "invoice march.pdf");
    assert_eq!(
        without[0], "invoice april.pdf",
        "an exact tie falls to the later entry"
    );
}
