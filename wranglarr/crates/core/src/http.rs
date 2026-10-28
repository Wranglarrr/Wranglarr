// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared plumbing for the API clients: the one upstream error type, the HTTP
//! client settings, bounded response bodies, redaction, readiness polling.

use std::path::Path;
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::Error;

/// An app, Docker or a registry that answered with an error or not at all: the
/// message people read, and the HTTP status when there was an answer.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ArrError {
    message: String,
    status: Option<StatusCode>,
}

impl ArrError {
    /// An error without an HTTP answer behind it (no connection, a bad body, …).
    pub fn new(message: impl Into<String>) -> Self {
        ArrError { message: message.into(), status: None }
    }

    /// The error for an answer with this status.
    pub fn with_status(status: StatusCode, message: impl Into<String>) -> Self {
        ArrError { message: message.into(), status: Some(status) }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// The status the app answered with, when it answered.
    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }

    /// The app answered 404: what was asked for isn't there.
    pub fn is_not_found(&self) -> bool {
        self.status == Some(StatusCode::NOT_FOUND)
    }

    /// The same error with context added to its message ("radarr: …").
    pub fn map_message(self, f: impl FnOnce(String) -> String) -> Self {
        ArrError { message: f(self.message), status: self.status }
    }
}

impl From<ArrError> for Error {
    fn from(e: ArrError) -> Self {
        Error::Upstream(e)
    }
}

pub type ArrResult<T> = Result<T, ArrError>;

/// The size of a backup zip, or an error (and no file) when it isn't one.
pub fn check_zip(name: &str, path: &Path) -> ArrResult<u64> {
    use std::io::Read;
    let mut head = [0u8; 4];
    let ok = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"PK\x03\x04";
    if !ok {
        let _ = std::fs::remove_file(path);
        return Err(ArrError::new(format!("{name}: the backup it handed over is not a zip")));
    }
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| ArrError::new(format!("{name}: {e}")))
}

pub async fn check_zip_async(name: &str, path: &Path) -> ArrResult<u64> {
    let (name, path) = (name.to_string(), path.to_path_buf());
    tokio::task::spawn_blocking(move || check_zip(&name, &path))
        .await
        .map_err(|e| ArrError::new(format!("zip check: {e}")))?
}

/// The shared HTTP client settings.
pub fn http(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder().timeout(timeout).redirect(reqwest::redirect::Policy::none()).build().unwrap_or_default()
}

/// The most an upstream answer's body may hold. Backup downloads stream to disk
/// instead, since they can legitimately be large.
pub const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

/// Why an upstream answer's body couldn't be had.
#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    #[error("upstream response exceeds 16 MiB")]
    TooLarge,
    /// The body broke off; the error carries no URL.
    #[error(transparent)]
    Read(reqwest::Error),
    #[error(transparent)]
    Json(serde_json::Error),
}

impl BodyError {
    /// The message, with a broken-off body worded by `read` (callers word
    /// connection failures their own way).
    pub fn describe(&self, read: impl FnOnce(&reqwest::Error) -> String) -> String {
        match self {
            BodyError::Read(e) => read(e),
            other => other.to_string(),
        }
    }
}

/// The body when it holds at most `limit` bytes, reading no further: None when
/// it's longer. The count holds even when Content-Length is missing or false.
pub async fn read_at_most(mut response: reqwest::Response, limit: usize) -> reqwest::Result<Option<bytes::Bytes>> {
    if response.content_length().is_some_and(|len| len > limit as u64) {
        return Ok(None);
    }
    let mut bytes = bytes::BytesMut::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes.freeze()))
}

/// The body, up to [`RESPONSE_LIMIT`].
pub async fn response_bytes(response: reqwest::Response) -> Result<bytes::Bytes, BodyError> {
    read_at_most(response, RESPONSE_LIMIT)
        .await
        .map_err(|e| BodyError::Read(e.without_url()))?
        .ok_or(BodyError::TooLarge)
}

/// The body as text (invalid UTF-8 replaced), up to [`RESPONSE_LIMIT`].
pub async fn response_text(response: reqwest::Response) -> Result<String, BodyError> {
    Ok(String::from_utf8_lossy(&response_bytes(response).await?).into_owned())
}

/// The body as JSON, up to [`RESPONSE_LIMIT`].
pub async fn response_json(response: reqwest::Response) -> Result<Value, BodyError> {
    serde_json::from_slice(&response_bytes(response).await?).map_err(BodyError::Json)
}

/// Redact known credentials before clipping an upstream diagnostic.
pub fn redact(mut text: String, secrets: &[&str]) -> String {
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, "[redacted]");
    }
    text
}

/// Never include query strings or credentials when a caller supplies a URL.
pub fn safe_url(value: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(value) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }
    value.split(['?', '#']).next().unwrap_or("").to_string()
}

/// The reason of a failed request, without the URL (SABnzbd's has the API key in it).
pub fn reason(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "timed out".into();
    }
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(e);
    let mut last = None;
    while let Some(s) = src {
        last = Some(s.to_string());
        src = s.source();
    }
    last.unwrap_or_else(|| {
        let s = e.to_string();
        e.url().map_or_else(|| s.clone(), |u| s.replace(&format!(" for url ({u})"), "").replace(u.as_str(), "…"))
    })
}

/// Poll `ready` every 2 s until it answers true or `timeout` runs out.
pub async fn poll_ready<F, Fut>(timeout: Duration, mut ready: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if ready().await {
            return true;
        }
        // every 2 s, but not past the deadline
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(left.min(Duration::from_secs(2))).await;
    }
    false
}
