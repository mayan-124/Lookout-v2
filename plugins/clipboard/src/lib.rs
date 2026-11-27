//! Clipboard history as a Lookout plugin (see `docs/plugins.md`).

pub mod handler;
pub mod image;
pub mod monitor;
pub mod smart;
pub mod store;
pub mod text_actions;
pub mod text_more;
#[cfg(windows)]
pub mod win;
