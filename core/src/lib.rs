//! Lookout core: everything that does not depend on the UI shell.
//!
//! Phase 3 adds encryption at rest (`crypto`, `securedb`, `storage`), the
//! persistent name index (`names_store`) and the privacy controls.
//!
//! Phase 2 adds the action engine (`actions`): preview, confirmation gate,
//! undo log, open and kill-port actions.
//!
//! Phase 1: item model, query parsing and ranking, the in-memory name index
//! with its crawler and watcher, the calculator, unit conversion, apps and
//! file sources, frecency, and the query engine with generation-based
//! cancellation.

pub mod actions;
pub mod answers;
pub mod apps;
#[cfg(windows)]
pub mod autostart;
pub mod calc;
pub mod cleanup;
pub mod cli;
pub mod cliaccess;
pub mod content;
pub mod crypto;
pub mod dates;
#[cfg(windows)]
pub mod diskusage;
pub mod dupes;
pub mod engine;
pub mod exclude;
pub mod fixture_image;
pub mod frecency;
pub mod history;
pub mod index_service;
pub mod item;
pub mod latency;
pub mod lookout;
pub mod memory;
pub mod name_index;
pub mod names_store;
pub mod notes;
pub mod notify;
pub mod operators;
pub mod pathquery;
pub mod pause;
pub mod placement;
pub mod plugins;
pub mod preview;
pub mod query;
pub mod quick;
pub mod quickkeys;
pub mod ranking;
pub mod recent;
#[cfg(windows)]
pub mod screengrab;
pub mod securedb;
pub mod semantic;
pub mod settings;
pub mod setup;
pub mod share;
pub mod sources;
pub mod storage;
pub mod synthetic;
pub mod thumbnail;
pub mod timefilter;
pub mod timers;
pub mod uimode;
pub mod units;
pub mod winfocus;
pub mod zones;
