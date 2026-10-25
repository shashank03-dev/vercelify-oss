//! Vercelify's private network (design §13): addressing, node keys, the
//! coordinator's certificates and signed peer maps. The data plane daemon
//! (`vercelify-mesh`) and the coordinator in the agent both build on this.

pub mod addr;
pub mod cert;
pub mod keys;

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
