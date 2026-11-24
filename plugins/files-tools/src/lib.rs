//! File tools as a Lookout plugin: compress an image to a size, batch rename,
//! strip image metadata (see `docs/plugins.md`).

pub mod handler;
#[cfg(windows)]
pub mod image;
pub mod metadata;
pub mod parse;
pub mod rename;
