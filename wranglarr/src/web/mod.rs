// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTP side: HTML pages in the arr-style shell, and the JSON API.
//!
//! Every module adds its routes in [`router`]. Handlers return [`ApiResult`]; an
//! error answers `{"error": …}` ([`ApiError`]), and the `error_pages` layer turns
//! that into the error page for non-API paths.

mod auth;
mod configs;
mod connect;
mod connections;
mod error;
mod events;
mod home;
mod keys;
mod notify;
mod proxy;
mod suites;
mod system;
pub mod tpl;
mod updates;

use std::collections::BTreeMap;

use axum::Router;
use axum::body::{Body as AxBody, Bytes};
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::app::App;
use crate::error::{Error, Result};
pub use error::{ApiError, ApiResult, status_of};

/// What a page handler returns.
pub type Page = ApiResult<Html<String>>;

pub fn router(app: App) -> Router {
    Router::new()
        .merge(suites::routes())
        .merge(system::routes())
        .route("/static/{*path}", get(static_file))
        .route("/api/health", get(health))
        .merge(notify::routes())
        .merge(connections::routes())
        .merge(updates::routes())
        .merge(connect::routes())
        .merge(home::routes())
        .merge(proxy::routes())
        .merge(configs::routes())
        .merge(events::routes())
        .merge(keys::routes())
        .fallback(fallback)
        .merge(auth::routes())
        .layer(middleware::from_fn_with_state(app.clone(), error_pages))
        .layer(middleware::from_fn_with_state(app.clone(), auth::guard))
        .with_state(app)
}

// ---- who is asking ------------------------------------------------------------

/// Set by the login guard on every request it lets through.
#[derive(Clone, Debug, Default)]
pub struct Who {
    /// The signed-in account (None: auth is off, or a public path).
    pub user: Option<String>,
    /// "session" or "api-key"
    pub via: &'static str,
    /// The session token, when signed in with the cookie.
    pub token: String,
}

/// What pages need to know about the request.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub path: String,
    pub query: String,
    /// The host name the browser used (the passkeys' relying party).
    pub host: String,
    pub headers: HeaderMap,
    pub who: Who,
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// The host part of `host[:port]` (or `[v6]:port`).
pub fn hostname(h: &str) -> String {
    let h = h.trim();
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("").to_lowercase();
    }
    h.rsplit_once(':').filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit())).map_or(h, |(a, _)| a).to_lowercase()
}

/// The host name the browser used: trusted X-Forwarded-Host's first entry, else
/// Host. The login guard removes untrusted forwarded headers first.
pub fn rp_id(h: &HeaderMap) -> String {
    let fwd = header_str(h, "x-forwarded-host").split(',').next().unwrap_or("").trim();
    for c in [fwd, header_str(h, "host")] {
        if !c.is_empty() {
            return hostname(c);
        }
    }
    String::new()
}

impl Ctx {
    pub fn from_parts(p: &Parts) -> Ctx {
        Ctx {
            path: p.uri.path().to_string(),
            query: p.uri.query().unwrap_or("").to_string(),
            host: rp_id(&p.headers),
            headers: p.headers.clone(),
            who: p.extensions.get::<Who>().cloned().unwrap_or_default(),
        }
    }
    pub fn header(&self, name: &str) -> &str {
        header_str(&self.headers, name)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Ctx {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(p: &mut Parts, _: &S) -> std::result::Result<Ctx, Self::Rejection> {
        Ok(Ctx::from_parts(p))
    }
}

// ---- the shell ------------------------------------------------------------------

/// The common context for the arr-style shell (sidebar + header), merged
/// with the page's own.
pub async fn shell(app: &App, ctx: &Ctx, page: &str, extra: Value) -> Result<Value> {
    let running = app.state().active_suites().await.unwrap_or_else(|e| {
        tracing::warn!("nav: could not read the active runs: {e}");
        Default::default()
    });
    // the newest version of each suite and the plan are files: read them off the runtime
    let (latest, plan) = app
        .blocking(|app| {
            let latest: Vec<_> = app.cfg().suites.iter().map(|s| app.store().latest(&s.name)).collect();
            Ok((latest, app.restore_plan()))
        })
        .await?;
    let nav: Vec<Value> = app
        .cfg()
        .suites
        .iter()
        .zip(latest)
        .map(|(s, m)| {
            json!({"name": s.name, "apps": s.apps.iter().map(|a| a.name.clone()).collect::<Vec<_>>(),
                   "running": running.contains(&s.name), "status": m.as_ref().map(|m| m.status),
                   "version": m.as_ref().map(|m| &m.version)})
        })
        .collect();
    let mut out = json!({
        "request": {"url": {"hostname": ctx.host, "path": ctx.path}},
        "page": page,
        "user": ctx.who.user,
        "auth_on": app.auth_enabled(),
        "nav_suites": nav,
        "vpn_configured": app.cfg().vpn.is_some(),
        "restore_plan": plan,
        "updates_available": app.updates_available().await,
    });
    if let (Some(o), Value::Object(e)) = (out.as_object_mut(), extra) {
        o.extend(e);
    }
    Ok(out)
}

pub fn render(name: &str, ctx: &Value) -> Result<Html<String>> {
    tpl::render(name, minijinja::Value::from(minijinja::value::Serde(ctx))).map(Html)
}

/// A page in the shell.
pub async fn page(app: &App, ctx: &Ctx, template: &str, page_key: &str, extra: Value) -> Result<Html<String>> {
    render(template, &shell(app, ctx, page_key, extra).await?)
}

// ---- request bodies and queries ----------------------------------------------------

/// A JSON body. A bad or missing body is a 422 `{"error": …}`;
/// an empty body reads as `null`, so `Body<Option<T>>` makes it optional.
///
/// A body must say it is JSON (`application/json`, or a `+json` type): a page on
/// another origin can send `text/plain` (or a form) without a CORS preflight, and
/// that must not reach a JSON endpoint. That's a 415. An empty body needs no
/// Content-Type (`fetch` and `curl -X POST` send none), as it carries nothing.
pub struct Body<T>(pub T);

/// `application/json` or `application/<something>+json`, any parameters.
pub fn is_json(content_type: &str) -> bool {
    let essence = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    essence == "application/json"
        || essence.strip_prefix("application/").is_some_and(|sub| sub.len() > 5 && sub.ends_with("+json"))
}

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = Response;
    async fn from_request(req: Request, state: &S) -> std::result::Result<Self, Response> {
        let json_type = is_json(header_str(req.headers(), "content-type"));
        let bytes = Bytes::from_request(req, state).await.map_err(IntoResponse::into_response)?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return serde_json::from_slice(b"null")
                .map(Body)
                .map_err(|e| ApiError(unprocessable(&body_error(&e))).into_response());
        }
        if !json_type {
            let msg = "send the body as JSON (Content-Type: application/json)";
            return Err((StatusCode::UNSUPPORTED_MEDIA_TYPE, axum::Json(json!({"error": msg}))).into_response());
        }
        serde_json::from_slice(&bytes).map(Body).map_err(|e| ApiError(unprocessable(&body_error(&e))).into_response())
    }
}

fn body_error(e: &serde_json::Error) -> String {
    match e.classify() {
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => "the body isn't valid JSON".into(),
        _ => {
            let msg = e.to_string();
            // serde's "… at line 1 column 12" means nothing to the caller
            msg.split(" at line ").next().unwrap_or(&msg).to_string()
        }
    }
}

pub fn unprocessable(msg: &str) -> Error {
    Error::Unprocessable(msg.to_string())
}

/// The 422 messages for a request that isn't shaped right.
pub const NO_BODY: &str = "a JSON object is required";
pub const NOT_AN_OBJECT: &str = "the body must be a JSON object";
pub const WHOLE_NUMBER: &str = "must be a whole number";
pub const TRUE_OR_FALSE: &str = "must be true or false";

/// `"<field>: "<value>" isn't a whole number"`.
pub fn not_whole(k: &str, v: &str) -> Error {
    unprocessable(&format!("{k}: {v:?} isn't a whole number"))
}

/// A query string read loosely: unknown keys ignored, flags as words or 0/1.
#[derive(Debug, Default, Clone)]
pub struct Q(pub BTreeMap<String, String>);

impl Q {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str)
    }
    /// `true/1/yes/on` (any case); a bad value is a 422.
    pub fn flag(&self, k: &str) -> Result<bool> {
        match self.get(k) {
            None => Ok(false),
            Some(v) => crate::value::flag_word(v).ok_or_else(|| unprocessable(&format!("{k}: {TRUE_OR_FALSE}"))),
        }
    }
    pub fn int(&self, k: &str, default: i64) -> Result<i64> {
        match self.get(k) {
            None => Ok(default),
            Some(v) => v.trim().parse().map_err(|_| not_whole(k, v)),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Q {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(p: &mut Parts, s: &S) -> std::result::Result<Q, Self::Rejection> {
        let m = Query::<BTreeMap<String, String>>::from_request_parts(p, s).await.map(|q| q.0).unwrap_or_default();
        Ok(Q(m))
    }
}

/// A path segment parsed as a whole number: a bad one is a 422.
pub fn path_int(v: &str, name: &str) -> Result<i64> {
    v.parse().map_err(|_| not_whole(name, v))
}

// ---- static files, health, errors --------------------------------------------------

pub async fn static_file(Path(path): Path<String>, uri: Uri, headers: HeaderMap) -> Response {
    let Some(f) = tpl::Static::get(&path) else {
        return ApiError(Error::not_found("Not Found")).into_response();
    };
    let etag = format!("\"{}\"", hex::encode(&f.metadata.sha256_hash()[..12]));
    if header_str(&headers, "if-none-match") == etag {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    // the pages link these with ?v=<hash of every file>, so a tagged copy never changes
    let cache =
        if uri.query().is_some_and(|q| q.contains("v=")) { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mut r = Response::new(AxBody::from(f.data.into_owned()));
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(mime.as_ref()) {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    r
}

async fn health(State(app): State<App>, ctx: Ctx) -> axum::Json<Value> {
    if app.auth_enabled() && ctx.who.user.is_none() {
        return axum::Json(json!({"ok": true})); // the details need a sign-in
    }
    let vpn = app.vpn_ok().await;
    axum::Json(json!({"ok": true, "version": crate::VERSION, "postgres": app.pg().available().await,
                      "docker": app.docker_ok().await, "vpn": vpn,
                      "updates_available": app.updates_available().await}))
}

async fn fallback() -> ApiError {
    ApiError(Error::not_found("Not Found"))
}

fn reason(s: StatusCode) -> String {
    s.canonical_reason().unwrap_or("Error").to_string()
}

/// Errors answer the same way everywhere: `{"error": …}` under /api/ and
/// /static/, the error page elsewhere. Covers axum's own answers too (405,
/// rejected paths), which come without a JSON body.
async fn error_pages(State(app): State<App>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let ctx = Ctx::from_parts(&parts);
    let resp = next.run(Request::from_parts(parts, body)).await;
    let status = resp.status();
    if !(status.is_client_error() || status.is_server_error()) || status == StatusCode::NOT_MODIFIED {
        return resp;
    }
    let api = ctx.path.starts_with("/api/") || ctx.path.starts_with("/static/");
    let is_json = header_str(resp.headers(), "content-type").starts_with("application/json");
    // API JSON already has its final representation. Buffering it here both
    // wastes memory and turns a body over the limit into an empty response.
    if api && is_json {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
    let detail = if is_json {
        serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v.get("error").cloned()).unwrap_or(Value::Null)
    } else {
        let t = String::from_utf8_lossy(&bytes).trim().to_string();
        Value::String(if t.is_empty() { reason(status) } else { t })
    };
    let detail = if detail.is_null() { Value::String(reason(status)) } else { detail };
    // These describe the old representation, not the generated JSON or HTML.
    for name in [
        header::CONTENT_LENGTH,
        header::CONTENT_ENCODING,
        header::TRANSFER_ENCODING,
        header::ETAG,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
        header::TRAILER,
    ] {
        parts.headers.remove(name);
    }
    if api {
        parts.headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        return Response::from_parts(parts, AxBody::from(json!({"error": detail}).to_string()));
    }
    let html = match page(&app, &ctx, "error.html", "", json!({"status": status.as_u16(), "detail": detail})).await {
        Ok(h) => h.0,
        Err(e) => format!("<h1>{}</h1><p>{}</p>", status.as_u16(), crate::format::html_escape(e.message())),
    };
    parts.headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    Response::from_parts(parts, AxBody::from(html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[test]
    fn json_content_types() {
        for t in ["application/json", "Application/JSON; charset=utf-8", "application/merge-patch+json"] {
            assert!(is_json(t), "{t}");
        }
        for t in [
            "",
            "text/plain",
            "text/plain; a=application/json",
            "application/x-www-form-urlencoded",
            "multipart/form-data; boundary=x",
            "application/+json",
            "application/jsonx",
            "text/json",
        ] {
            assert!(!is_json(t), "{t}");
        }
    }

    #[tokio::test]
    async fn json_bodies_need_a_json_content_type() {
        #[derive(serde::Deserialize)]
        struct In {
            #[allow(dead_code)]
            x: i64,
        }
        let router: Router =
            Router::new().route("/need", axum::routing::post(|Body(_): Body<In>| async { "ok" })).route(
                "/maybe",
                axum::routing::post(
                    |Body(b): Body<Option<In>>| async move { if b.is_some() { "some" } else { "none" } },
                ),
            );
        let send = |path: &'static str, ct: Option<&'static str>, body: &'static str| {
            let router = router.clone();
            async move {
                let mut b = Request::builder().method("POST").uri(path);
                if let Some(ct) = ct {
                    b = b.header(header::CONTENT_TYPE, ct);
                }
                let r = router.oneshot(b.body(AxBody::from(body)).expect("request")).await.expect("response");
                let status = r.status();
                let bytes = axum::body::to_bytes(r.into_body(), 1000).await.expect("body");
                (status, String::from_utf8_lossy(&bytes).into_owned())
            }
        };
        assert_eq!(send("/need", Some("application/json"), r#"{"x":1}"#).await.0, StatusCode::OK);
        // a cross-site "simple" request: no preflight, so it must not get in
        let (status, body) = send("/need", Some("text/plain"), r#"{"x":1}"#).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(body.contains("application/json"), "{body}");
        assert_eq!(send("/need", None, r#"{"x":1}"#).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            send("/need", Some("application/x-www-form-urlencoded"), "x=1").await.0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        // an empty body carries nothing: no Content-Type needed
        assert_eq!(send("/maybe", None, "").await, (StatusCode::OK, "none".into()));
        assert_eq!(send("/maybe", Some("text/plain"), " ").await, (StatusCode::OK, "none".into()));
        assert_eq!(send("/need", None, "").await.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(send("/maybe", Some("application/json"), r#"{"x":2}"#).await, (StatusCode::OK, "some".into()));
    }

    #[tokio::test]
    async fn large_json_errors_pass_through_unchanged() {
        let (_dir, app) = crate::app::tests::app().await;
        let router = Router::new()
            .route(
                "/api/error",
                get(|| async { (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "x".repeat(2 << 20)}))) }),
            )
            .layer(middleware::from_fn_with_state(app, error_pages));
        let response = router
            .oneshot(Request::builder().uri("/api/error").body(AxBody::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 3 << 20).await.expect("body");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).expect("json")["error"].as_str().expect("error").len(),
            2 << 20
        );
    }

    #[tokio::test]
    async fn rewritten_errors_drop_old_representation_headers() {
        let (_dir, app) = crate::app::tests::app().await;
        let router = Router::new()
            .route(
                "/api/error",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        [
                            (header::CONTENT_ENCODING, "gzip"),
                            (header::ETAG, "old"),
                            (header::WWW_AUTHENTICATE, "Bearer"),
                            (header::RETRY_AFTER, "10"),
                        ],
                        "bad request",
                    )
                }),
            )
            .layer(middleware::from_fn_with_state(app, error_pages));
        let response = router
            .oneshot(Request::builder().uri("/api/error").body(AxBody::empty()).expect("request"))
            .await
            .expect("response");
        assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
        assert!(!response.headers().contains_key(header::ETAG));
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        assert_eq!(response.headers()[header::RETRY_AFTER], "10");
        let bytes = axum::body::to_bytes(response.into_body(), 1000).await.expect("body");
        assert_eq!(serde_json::from_slice::<Value>(&bytes).expect("json"), json!({"error": "bad request"}));
    }
}
