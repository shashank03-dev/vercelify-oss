//! The Vercelify platform: everything Coolify used to do for us, as a library
//! the agent mounts on the main server.
//!
//! Design: `docs/design/SYSTEM-DESIGN.md`. This crate owns the state
//! (`store`), secrets at rest (`secrets`) and identifiers (`ids`); builds,
//! deployments and the reconciler build on those.

pub mod apps;
pub mod build;
pub mod compose;
pub mod deploy;
pub mod deployments;
pub mod detect;
pub mod docker;
pub mod engine;
pub mod env;
pub mod error;
pub mod gate;
pub mod git;
pub mod git_sources;
pub mod git_watch;
pub mod health;
pub mod hooks;
pub mod ids;
pub mod logs;
pub mod nodes;
pub mod previews;
pub mod proc;
pub mod projects;
pub mod proxy;
pub mod reconcile;
pub mod secrets;
pub mod sleeping;
pub mod store;
pub mod tasks;
pub mod validate;
pub mod volumes;

pub use error::{Error, Result};
/// Re-exported so the agent's handlers name the same `Transaction` type.
pub use rusqlite;
pub use store::Store;

/// Current time as RFC 3339 UTC with seconds, the format of every timestamp
/// in the platform database and API.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
