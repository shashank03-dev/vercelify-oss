//! One error type for the platform, mapped 1:1 onto HTTP statuses by the agent.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 404: the resource doesn't exist.
    NotFound(String),
    /// 409: the request conflicts with the current state (a duplicate name,
    /// a deployment already running, a resource still in use).
    Conflict(String),
    /// 400: the request is malformed or breaks a rule.
    Invalid(String),
    /// 503: a dependency (Docker, a worker, the disk) is not available.
    Unavailable(String),
    /// 500: a bug or an unexpected failure.
    Internal(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn status(&self) -> u16 {
        match self {
            Error::NotFound(_) => 404,
            Error::Conflict(_) => 409,
            Error::Invalid(_) => 400,
            Error::Unavailable(_) => 503,
            Error::Internal(_) => 500,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Error::NotFound(m)
            | Error::Conflict(m)
            | Error::Invalid(m)
            | Error::Unavailable(m)
            | Error::Internal(m) => m,
        }
    }

    pub fn not_found(what: impl fmt::Display) -> Self {
        Error::NotFound(format!("{what} not found"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        use rusqlite::ffi::ErrorCode;
        match &e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound("not found".into()),
            rusqlite::Error::SqliteFailure(f, msg) => {
                let detail = msg.clone().unwrap_or_else(|| f.to_string());
                match (f.code, f.extended_code) {
                    // SQLITE_CONSTRAINT_UNIQUE / PRIMARYKEY
                    (ErrorCode::ConstraintViolation, 2067 | 1555) => {
                        Error::Conflict(format!("already exists ({})", unique_columns(&detail)))
                    }
                    // SQLITE_CONSTRAINT_CHECK
                    (ErrorCode::ConstraintViolation, 275) => Error::Invalid(format!("invalid value ({detail})")),
                    // SQLITE_CONSTRAINT_FOREIGNKEY
                    (ErrorCode::ConstraintViolation, 787) => {
                        Error::Conflict("refers to something that doesn't exist, or is still in use".into())
                    }
                    // RESTRICT on delete reports the plain constraint code.
                    (ErrorCode::ConstraintViolation, _) if detail.starts_with("FOREIGN KEY") => {
                        Error::Conflict("refers to something that doesn't exist, or is still in use".into())
                    }
                    (ErrorCode::ConstraintViolation, _) => Error::Invalid(detail),
                    (ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked, _) => {
                        Error::Unavailable("the platform database is busy, try again".into())
                    }
                    (ErrorCode::DiskFull, _) => Error::Unavailable("the disk is full".into()),
                    _ => Error::Internal(format!("database: {detail}")),
                }
            }
            _ => Error::Internal(format!("database: {e}")),
        }
    }
}

/// "UNIQUE constraint failed: apps.name" -> "apps.name"
fn unique_columns(detail: &str) -> &str {
    detail.rsplit_once(": ").map(|(_, c)| c).unwrap_or(detail)
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Internal(format!("json: {e}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Error::NotFound(e.to_string()),
            _ => Error::Internal(e.to_string()),
        }
    }
}
