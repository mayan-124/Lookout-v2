//! Turning text into a 384-number vector with bge-small-en-v1.5.
//!
//! Following the model card (BAAI/bge-small-en-v1.5): the sentence embedding
//! is the hidden state of the `[CLS]` token (CLS pooling, not mean pooling),
//! L2-normalised; a *query* is prefixed with
//! "Represent this sentence for searching relevant passages: " and a passage
//! is embedded as it is; input is cut at 512 tokens.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

use crate::assets::{self, Pins};
use crate::tokenizer::Tokenizer;

pub const DIM: usize = 384;
pub const MAX_TOKENS: usize = 512;
pub const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

pub trait Embedder: Send {
    /// A unit-length vector of [`DIM`] numbers.
    fn embed(&mut self, text: &str, is_query: bool) -> Result<Vec<f32>, String>;
}

/// Opens the assets folder and returns a ready embedder, or why it cannot.
pub type Loader = Box<dyn FnMut(&Path) -> Result<Box<dyn Embedder>, String> + Send>;

pub struct OrtEmbedder {
    // Kept alive for the session's lifetime.
    _env: ort::environment::Environment,
    session: Session,
    tokenizer: Tokenizer,
    has_token_types: bool,
}

/// The real loader: checks the pinned hashes, then loads the runtime from its
/// absolute path in `dir`, never by name.
pub fn load_from_assets(dir: &Path) -> Result<Box<dyn Embedder>, String> {
    let v = assets::verify(dir, &assets::pinned())?;
    Ok(Box::new(OrtEmbedder::new(
        &v.runtime,
        &v.model,
        &v.tokenizer,
    )?))
}

/// Same, with other pins (tests with small stand-in files).
pub fn load_with_pins(dir: &Path, pins: &Pins) -> Result<Box<dyn Embedder>, String> {
    let v = assets::verify(dir, pins)?;
    Ok(Box::new(OrtEmbedder::new(
        &v.runtime,
        &v.model,
        &v.tokenizer,
    )?))
}

impl OrtEmbedder {
    /// `runtime` must be an absolute path to `onnxruntime.dll`.
    pub fn new(runtime: &Path, model: &[u8], tokenizer: &[u8]) -> Result<Self, String> {
        if !runtime.is_absolute() {
            return Err("the runtime path is not absolute".into());
        }
        let tokenizer = Tokenizer::from_json(tokenizer)?;
        let env = ort::init_from(runtime)
            .map_err(|e| format!("cannot load the runtime: {e}"))?
            .with_telemetry(false)
            .build()
            .map_err(|e| format!("cannot start the runtime: {e}"))?;
        let fail = |what: &str, e: String| format!("cannot {what}: {e}");
        let mut b = Session::builder(&env).map_err(|e| fail("create a session", e.to_string()))?;
        b = b
            .with_intra_threads(1)
            .map_err(|e| fail("configure the session", e.to_string()))?;
        b = b
            .with_inter_threads(1)
            .map_err(|e| fail("configure the session", e.to_string()))?;
        b = b
            .with_memory_pattern(false)
            .map_err(|e| fail("configure the session", e.to_string()))?;
        let session: Session = b
            .commit_from_memory(model)
            .map_err(|e| fail("load the model", e.to_string()))?;
        let has_token_types = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");
        Ok(Self {
            _env: env,
            session,
            tokenizer,
            has_token_types,
        })
    }
}

fn unit(mut v: Vec<f32>) -> Result<Vec<f32>, String> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm < 1e-9 {
        return Err("the model produced an empty vector".into());
    }
    v.iter_mut().for_each(|x| *x /= norm);
    Ok(v)
}

impl Embedder for OrtEmbedder {
    fn embed(&mut self, text: &str, is_query: bool) -> Result<Vec<f32>, String> {
        let prefixed;
        let text = if is_query {
            prefixed = format!("{QUERY_PREFIX}{text}");
            prefixed.as_str()
        } else {
            text
        };
        let ids = self.tokenizer.encode(text, MAX_TOKENS);
        let n = ids.len();
        let ids: Vec<i64> = ids.into_iter().map(i64::from).collect();
        let tensor = |data: Vec<i64>| {
            Tensor::from_array(([1usize, n], data)).map_err(|e| format!("tensor: {e}"))
        };
        let mut inputs = ort::inputs![
            "input_ids" => tensor(ids)?,
            "attention_mask" => tensor(vec![1; n])?,
        ];
        if self.has_token_types {
            inputs.push(("token_type_ids".into(), tensor(vec![0; n])?.into()));
        }
        let out = self
            .session
            .run(inputs)
            .map_err(|e| format!("the model failed: {e}"))?;
        let (shape, data) = out[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("unexpected model output: {e}"))?;
        // [1, tokens, 384]: the [CLS] row is the first one. [1, 384]: already pooled.
        let ok = match shape.len() {
            3 => shape[2] as usize == DIM && data.len() >= DIM,
            2 => shape[1] as usize == DIM && data.len() >= DIM,
            _ => false,
        };
        if !ok {
            return Err(format!("unexpected model output shape {shape:?}"));
        }
        unit(data[..DIM].to_vec())
    }
}
