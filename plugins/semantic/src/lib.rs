//! The semantic-search helper, a Lookout plugin (see `docs/semantic.md`): it
//! receives masked text over the plugin protocol, embeds it with
//! bge-small-en-v1.5 (int8) through ONNX Runtime, and keeps an encrypted flat
//! int8 vector file. It reads no files of the user's.

pub mod assets;
pub mod embed;
pub mod handler;
pub mod store;
pub mod tokenizer;
