//! Semantic search (Phase 5): the core extracts and masks text, a helper
//! plugin turns it into vectors and answers nearest-neighbour queries. See
//! `docs/semantic.md`.

pub mod client;
pub mod indexer;
pub mod setup;
pub mod source;

pub use client::{HelperStatus, Hit, SemanticClient, SemanticError, PLUGIN_ID};
pub use indexer::{
    SemanticConfig, SemanticIndexer, SemanticProgress, SemanticState, SemanticStatus,
};
pub use source::SemanticSource;
