// SPDX-License-Identifier: GPL-3.0-or-later
//! One error type for the whole service: what kind of failure it is, the message
//! people read, and for an internal error the cause behind it, kept for the log.
//! The web layer turns the kind into an HTTP status (wiki/API.md, Errors).

use crate::http::ArrError;

/// The cause behind an internal error.
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No such suite, version, channel, …
    #[error("{0}")]
    NotFound(String),
    /// The request itself is wrong.
    #[error("{0}")]
    Invalid(String),
    /// Something else is using it, or it already exists.
    #[error("{0}")]
    Conflict(String),
    /// Not signed in.
    #[error("{0}")]
    Unauthorized(String),
    /// Signed in, but not allowed.
    #[error("{0}")]
    Forbidden(String),
    /// Too many attempts.
    #[error("{0}")]
    TooMany(String),
    /// A body, query or path value of the wrong shape.
    #[error("{0}")]
    Unprocessable(String),
    /// An app, Docker or a registry answered with an error or not at all.
    #[error(transparent)]
    Upstream(ArrError),
    /// A bug or an environment problem.
    #[error("{message}")]
    Internal {
        message: String,
        #[source]
        source: Option<Source>,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn message(&self) -> &str {
        match self {
            Error::NotFound(m)
            | Error::Invalid(m)
            | Error::Conflict(m)
            | Error::Unauthorized(m)
            | Error::Forbidden(m)
            | Error::TooMany(m)
            | Error::Unprocessable(m) => m,
            Error::Upstream(e) => e.message(),
            Error::Internal { message, .. } => message,
        }
    }

    pub fn not_found(m: impl Into<String>) -> Self {
        Error::NotFound(m.into())
    }
    pub fn invalid(m: impl Into<String>) -> Self {
        Error::Invalid(m.into())
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        Error::Conflict(m.into())
    }
    pub fn upstream(m: impl Into<String>) -> Self {
        Error::Upstream(ArrError::new(m))
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Error::Internal { message: m.into(), source: None }
    }

    /// An internal error whose message is `context: cause`, keeping the cause.
    pub fn caused(context: &str, cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        let message = if context.is_empty() { cause.to_string() } else { format!("{context}: {cause}") };
        Error::Internal { message, source: Some(Box::new(cause)) }
    }

    /// Same kind, new message (for adding context like "radarr: …").
    pub fn map_msg(self, f: impl FnOnce(String) -> String) -> Self {
        match self {
            Error::NotFound(m) => Error::NotFound(f(m)),
            Error::Invalid(m) => Error::Invalid(f(m)),
            Error::Conflict(m) => Error::Conflict(f(m)),
            Error::Unauthorized(m) => Error::Unauthorized(f(m)),
            Error::Forbidden(m) => Error::Forbidden(f(m)),
            Error::TooMany(m) => Error::TooMany(f(m)),
            Error::Unprocessable(m) => Error::Unprocessable(f(m)),
            Error::Upstream(e) => Error::Upstream(e.map_message(f)),
            Error::Internal { message, source } => Error::Internal { message: f(message), source },
        }
    }

    /// The app behind an upstream error answered 404.
    pub fn is_upstream_not_found(&self) -> bool {
        matches!(self, Error::Upstream(e) if e.is_not_found())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::caused("", e)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::caused("state db", e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::caused("json", e)
    }
}

impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Self {
        Error::caused("task", e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn causes_are_kept_and_messages_unchanged() {
        let e = Error::from(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert_eq!(e.to_string(), "gone");
        assert_eq!(e.source().map(ToString::to_string).as_deref(), Some("gone"));
        let e = Error::from(serde_json::from_str::<u8>("x").expect_err("not json"));
        assert!(e.to_string().starts_with("json: expected value"), "{e}");
        assert!(e.source().is_some());
        let e = Error::from(rusqlite::Error::QueryReturnedNoRows).map_msg(|m| format!("radarr: {m}"));
        assert_eq!(e.to_string(), "radarr: state db: Query returned no rows");
        assert!(e.source().is_some());
    }

    #[test]
    fn upstream_status_survives_context() {
        let e = Error::from(ArrError::with_status(reqwest::StatusCode::NOT_FOUND, "radarr: GET tag/3 -> 404: x"))
            .map_msg(|m| format!("undo: {m}"));
        assert!(e.is_upstream_not_found());
        assert_eq!(e.to_string(), "undo: radarr: GET tag/3 -> 404: x");
        assert!(!Error::upstream("radarr: timed out").is_upstream_not_found());
        assert!(!Error::from(ArrError::with_status(reqwest::StatusCode::BAD_GATEWAY, "x")).is_upstream_not_found());
    }
}
