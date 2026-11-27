//! Plugins: separate processes that speak JSON-RPC 2.0 over stdio (brief
//! section 10). See `docs/plugins.md` for the protocol and for what the host
//! can and cannot enforce.

pub mod host;
pub mod integration;
pub mod manifest;
pub mod sdk;
pub mod wire;

pub use host::{Candidate, DragData, HostConfig, HostError, PluginHost, PluginInfo, Timeouts};
pub use integration::{PluginActions, PluginSource};
pub use manifest::{Manifest, ManifestError};
