//! Tests that need the real model, tokenizer and runtime. Set
//! `LOOKOUT_SEMANTIC_ASSETS` to a folder holding `model.onnx`,
//! `tokenizer.json` and `onnxruntime.dll` (the pinned files); without it they
//! print a note and pass, so CI without the 35 MB model stays green.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use lookout_semantic::embed::load_from_assets;
use lookout_semantic::tokenizer::Tokenizer;

fn assets() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("LOOKOUT_SEMANTIC_ASSETS")?);
    p.is_absolute().then_some(p)
}

#[test]
fn matches_the_reference_tokenizer() {
    let Some(dir) = assets() else {
        eprintln!("skipped: LOOKOUT_SEMANTIC_ASSETS is not set");
        return;
    };
    let tok = Tokenizer::from_json(&std::fs::read(dir.join("tokenizer.json")).unwrap()).unwrap();
    let reference: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/tokens.json")).unwrap();
    assert!(reference.len() >= 30);
    let mut wrong = Vec::new();
    for r in &reference {
        let text = r["text"].as_str().unwrap();
        let want: Vec<u32> = r["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_u64().unwrap() as u32)
            .collect();
        let got = tok.encode(text, 512);
        if got != want {
            wrong.push(format!("{text:?}\n  want {want:?}\n  got  {got:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

fn cos(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[test]
fn similar_sentences_score_clearly_higher_than_unrelated_ones() {
    let Some(dir) = assets() else {
        eprintln!("skipped: LOOKOUT_SEMANTIC_ASSETS is not set");
        return;
    };
    let mut e = load_from_assets(&dir).unwrap();
    let mut doc = |t: &str| e.embed(t, false).unwrap();
    let cat = doc("A cat is sleeping on the warm sofa.");
    let kitten = doc("A kitten naps on the couch.");
    let tax = doc("Quarterly corporate tax filing deadlines and penalties.");
    let norm: f32 = cat.iter().map(|x| x * x).sum();
    assert!((norm - 1.0).abs() < 1e-3, "vectors are unit length");
    let (near, far) = (cos(&cat, &kitten), cos(&cat, &tax));
    eprintln!("similar {near:.3}, unrelated {far:.3}");
    assert!(near > far + 0.2, "similar {near} vs unrelated {far}");
    // A query (with the instruction prefix) finds the matching passage.
    let q = e.embed("where does the pet rest", true).unwrap();
    assert!(cos(&q, &cat) > cos(&q, &tax) + 0.1);
}

#[test]
fn a_wrong_file_is_refused_before_anything_is_loaded() {
    let Some(dir) = assets() else {
        eprintln!("skipped: LOOKOUT_SEMANTIC_ASSETS is not set");
        return;
    };
    let copy = tempfile::tempdir().unwrap();
    for f in ["model.onnx", "tokenizer.json", "onnxruntime.dll"] {
        std::fs::copy(dir.join(f), copy.path().join(f)).unwrap();
    }
    let mut bytes = std::fs::read(copy.path().join("model.onnx")).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(copy.path().join("model.onnx"), bytes).unwrap();
    let err = load_from_assets(copy.path()).err().unwrap();
    assert!(
        err.contains("model.onnx") && err.contains("pinned SHA-256"),
        "{err}"
    );
}
