//! `vercelify-host`: the host manager on a Mac or Windows computer (the
//! cross-platform hosting design). It measures the computer, recommends
//! resources, and makes and runs the one Linux environment that hosts all
//! of its apps with the unchanged Vercelify agent inside. It runs on its
//! own, before and after Desktop: closing Desktop doesn't stop hosting.

pub mod api;
pub mod guest;
pub mod lima;
pub mod manager;
pub mod measure;
pub mod power;
pub mod provider;
pub mod qemu;
pub mod state;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
    /// A prerequisite only the owner can fix on this computer.
    #[error("{0}")]
    Blocked(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Internal(String),
    #[error("this needs the computer owner's token")]
    Unauthorized,
}

impl Error {
    pub fn message(&self) -> &str {
        match self {
            Error::Invalid(m)
            | Error::Conflict(m)
            | Error::NotFound(m)
            | Error::Blocked(m)
            | Error::Unavailable(m)
            | Error::Internal(m) => m,
            Error::Unauthorized => "this needs the computer owner's token",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            Error::Invalid(_) => 400,
            Error::NotFound(_) => 404,
            Error::Conflict(_) => 409,
            Error::Blocked(_) => 412,
            Error::Unavailable(_) => 503,
            Error::Internal(_) => 500,
            Error::Unauthorized => 401,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
