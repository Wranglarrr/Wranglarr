// SPDX-License-Identifier: GPL-3.0-or-later
//! Errors as the HTTP API answers them: the status for the error's kind and
//! `{"error": "<message>"}` (wiki/API.md, Errors).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::error::Error;

/// An [`Error`] on its way out as an HTTP answer. Handlers return
/// [`ApiResult`], and `?` turns any error that converts to [`Error`] into this.
#[derive(Debug)]
pub struct ApiError(pub Error);

/// What a handler returns.
pub type ApiResult<T> = std::result::Result<T, ApiError>;

impl ApiError {
    /// The status an error of this kind answers with.
    pub fn status(&self) -> StatusCode {
        status_of(&self.0)
    }

    pub fn into_inner(self) -> Error {
        self.0
    }
}

/// The status for an error's kind.
pub fn status_of(e: &Error) -> StatusCode {
    match e {
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::Invalid(_) => StatusCode::BAD_REQUEST,
        Error::Conflict(_) => StatusCode::CONFLICT,
        Error::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        Error::Forbidden(_) => StatusCode::FORBIDDEN,
        Error::TooMany(_) => StatusCode::TOO_MANY_REQUESTS,
        Error::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
        Error::Upstream(_) => StatusCode::BAD_GATEWAY,
        Error::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl<E: Into<Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!("{}", with_causes(&self.0));
        }
        (status, axum::Json(serde_json::json!({ "error": self.0.message() }))).into_response()
    }
}

/// The message, then each cause behind it that adds something, for the log.
fn with_causes(e: &Error) -> String {
    let mut out = e.to_string();
    let mut cause = std::error::Error::source(e);
    while let Some(c) = cause {
        let text = c.to_string();
        if !out.ends_with(&text) {
            out.push_str(&format!(" (caused by: {text})"));
        }
        cause = c.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ArrError;

    #[test]
    fn statuses() {
        let cases = [
            (Error::not_found("x"), 404),
            (Error::invalid("x"), 400),
            (Error::conflict("x"), 409),
            (Error::Unauthorized("x".into()), 401),
            (Error::Forbidden("x".into()), 403),
            (Error::TooMany("x".into()), 429),
            (Error::Unprocessable("x".into()), 422),
            (Error::upstream("x"), 502),
            // the app's own status stays the app's: wranglarr answers 502
            (Error::from(ArrError::with_status(StatusCode::NOT_FOUND, "x")), 502),
            (Error::internal("x"), 500),
        ];
        for (e, want) in cases {
            assert_eq!(ApiError(e).status().as_u16(), want);
        }
    }

    #[tokio::test]
    async fn answers_the_message_as_json() {
        let r = ApiError::from(ArrError::new("radarr: timed out")).into_response();
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(r.into_body(), 1024).await.expect("body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("json"),
            serde_json::json!({"error": "radarr: timed out"})
        );
    }

    #[test]
    fn logs_the_causes() {
        let e = Error::from(std::io::Error::other("disk full")).map_msg(|m| format!("writing x: {m}"));
        assert_eq!(with_causes(&e), "writing x: disk full");
        let e = Error::caused("state db", std::io::Error::other("locked")).map_msg(|_| "saving".into());
        assert_eq!(with_causes(&e), "saving (caused by: locked)");
    }
}
