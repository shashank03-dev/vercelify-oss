//! Vercelify's private network (design §13): addressing, node keys, the
//! coordinator's certificates and signed peer maps. The data plane daemon
//! (`vercelify-mesh`) and the coordinator in the agent both build on this.

pub mod addr;
pub mod beacon;
pub mod cert;
#[cfg(all(feature = "daemon", any(target_os = "linux", target_os = "macos")))]
pub mod daemon;
pub mod disco;
pub mod dns;
#[cfg(feature = "daemon")]
pub mod http;
pub mod keys;
pub mod mdns;
pub mod net;
#[cfg(feature = "daemon")]
pub mod netstack;
#[cfg(all(feature = "daemon", any(target_os = "linux", target_os = "macos")))]
pub mod pair_client;
pub mod pake;
pub mod proto;
pub mod relay;
#[cfg(feature = "daemon")]
pub mod relay_client;
pub mod seal;
#[cfg(feature = "daemon")]
pub mod socks;
pub mod state;
pub mod stun;
#[cfg(all(feature = "daemon", target_os = "linux"))]
pub mod tun;
#[cfg(all(feature = "daemon", target_os = "macos"))]
pub mod utun;
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
