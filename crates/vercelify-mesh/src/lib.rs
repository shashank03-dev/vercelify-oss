//! Vercelify's private network (design §13): addressing, node keys, the
//! coordinator's certificates and signed peer maps. The data plane daemon
//! (`vercelify-mesh`) and the coordinator in the agent both build on this.

pub mod addr;
pub mod cert;
#[cfg(all(feature = "daemon", target_os = "linux"))]
pub mod daemon;
pub mod dns;
#[cfg(feature = "daemon")]
pub mod http;
pub mod keys;
pub mod net;
pub mod proto;
pub mod seal;
pub mod state;
#[cfg(all(feature = "daemon", target_os = "linux"))]
pub mod tun;
#[cfg(feature = "daemon")]
pub mod wg;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MeshError {
    #[error("{0}")]
    Invalid(String),
    #[error("signature does not verify")]
    BadSignature,
    #[error("certificate expired or not yet valid")]
    Expired,
}

pub type Result<T> = std::result::Result<T, MeshError>;
