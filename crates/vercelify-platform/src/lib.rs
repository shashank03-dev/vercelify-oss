//! The Vercelify platform: everything Coolify used to do for us, as a library
//! the agent mounts on the main server.
//!
//! Design: `docs/design/SYSTEM-DESIGN.md`. This crate owns the state
//! (`store`), secrets at rest (`secrets`) and identifiers (`ids`); builds,
//! deployments and the reconciler build on those.

pub mod error;
pub mod ids;
pub mod secrets;
pub mod store;

pub use error::{Error, Result};
pub use store::Store;

/// Current time as RFC 3339 UTC with seconds, the format of every timestamp
/// in the platform database and API.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
