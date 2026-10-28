// SPDX-License-Identifier: GPL-3.0-or-later
//! Seerr (the successor of Overseerr and Jellyseerr): requests for movies and
//! shows, handed to Radarr / Sonarr.
//!
//! wranglarr backs up its /app/config (settings.json) plus its Postgres
//! database, and adds the stack's Radarr and Sonarr to it (connect). Its API
//! answers to the API key in settings.json, but only once Seerr's own setup is
//! done: that setup signs in with Plex, Jellyfin or Emby, which only the user
//! can do.

use std::path::Path;
use std::time::Duration;

use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::feed::{self as feeds, FeedPage, FeedQuery, Lookups};
use crate::file_app::FileApp;
use crate::{Feed, FeedKind};
use wranglarr_core::http::{ArrError, ArrResult};
use wranglarr_core::http::{poll_ready, reason};
use wranglarr_core::value::{count, head, truthy};

pub fn media_server(kind: &Value) -> &'static str {
    match kind.as_i64() {
        Some(1) => "Plex",
        Some(2) => "Jellyfin",
        Some(3) => "Emby",
        Some(4) => "none",
        _ => "?",
    }
}

const NOT_SET_UP: &str = "Seerr isn't set up yet: open it and sign in with Plex, Jellyfin or Emby";

#[derive(Clone)]
pub struct SeerrClient {
    pub(crate) core: FileApp,
    pub(crate) name: String,
    pub(crate) api_key: String,
    /// `<url>/api/v1`
    pub(crate) api: String,
}

impl SeerrClient {
    pub fn new(name: &str, url: &str, api_key: &str, config_dir: &str) -> SeerrClient {
        let core = FileApp::new(name, url, config_dir, Duration::from_secs(15));
        SeerrClient { api: format!("{}/api/v1", core.base), core, name: name.into(), api_key: api_key.into() }
    }

    /// Any API call: its JSON answer (Null for an empty body).
    pub async fn req(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        timeout: Option<Duration>,
    ) -> ArrResult<Value> {
        let mut rb = self
            .core
            .http
            .request(method.clone(), format!("{}/{}", self.api, path.trim_start_matches('/')))
            .header("X-Api-Key", &self.api_key)
            .query(query);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        if let Some(t) = timeout {
            rb = rb.timeout(t);
        }
        let r =
            rb.send().await.map_err(|e| ArrError::new(format!("{}: {method} {path}: {}", self.name, reason(&e))))?;
        let status = r.status();
        let json_type = crate::is_json_type(r.headers());
        let text = wranglarr_core::http::response_text(r)
            .await
            .map_err(|e| ArrError::new(format!("{}: {method} {path}: {e}", self.name)))?;
        if matches!(status.as_u16(), 401 | 403) && !self.initialized().await {
            return Err(ArrError::with_status(status, format!("{}: {NOT_SET_UP}", self.name)));
        }
        if status.as_u16() >= 400 {
            let text = wranglarr_core::http::redact(text, &[&self.api_key]);
            let mut detail = head(&text, 200);
            if let Some(m) =
                serde_json::from_str::<Value>(&text).ok().and_then(|j| j.get("message").filter(|m| truthy(m)).cloned())
            {
                detail = super::text(&m);
            }
            let detail = wranglarr_core::http::redact(detail, &[&self.api_key]);
            return Err(ArrError::with_status(
                status,
                format!("{}: {method} {path} -> {}: {detail}", self.name, status.as_u16()),
            ));
        }
        if method == Method::GET && !json_type {
            return Err(ArrError::new(format!("{}: {method} {path}: expected JSON content type", self.name)));
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| ArrError::new(format!("{}: {method} {path}: bad JSON: {e}", self.name)))
    }

    pub async fn get(&self, path: &str) -> ArrResult<Value> {
        self.req(Method::GET, path, &[], None, None).await
    }
    pub async fn get_query(&self, path: &str, query: &[(&str, &str)]) -> ArrResult<Value> {
        self.req(Method::GET, path, query, None, None).await
    }
    pub async fn post(&self, path: &str, body: &Value) -> ArrResult<Value> {
        self.req(Method::POST, path, &[], Some(body), None).await
    }
    pub async fn put(&self, path: &str, body: &Value) -> ArrResult<Value> {
        self.req(Method::PUT, path, &[], Some(body), None).await
    }
    pub async fn delete(&self, path: &str) -> ArrResult<Value> {
        self.req(Method::DELETE, path, &[], None, None).await
    }

    /// GET <api>/<path> without the key and without checking the status: the JSON.
    async fn open_json(&self, path: &str, timeout: u64) -> Result<(u16, Value), String> {
        let r = self
            .core
            .http
            .get(format!("{}/{path}", self.api))
            .timeout(Duration::from_secs(timeout))
            .send()
            .await
            .map_err(|e| reason(&e))?;
        let status = r.status().as_u16();
        if !crate::is_json_type(r.headers()) {
            return Err("expected JSON content type".into());
        }
        let j = wranglarr_core::http::response_json(r).await.map_err(|e| e.to_string())?;
        Ok((status, j))
    }

    /// /settings/public (no key needed); {} when Seerr doesn't answer it.
    async fn public(&self) -> Value {
        match self.open_json("settings/public", 8).await {
            Ok((200, j @ Value::Object(_))) => j,
            _ => json!({}),
        }
    }

    pub async fn initialized(&self) -> bool {
        truthy(&self.public().await["initialized"])
    }

    pub async fn ping(&self) -> bool {
        let r = self.core.http.get(format!("{}/status", self.api)).timeout(Duration::from_secs(5)).send().await;
        matches!(r, Ok(r) if r.status() == reqwest::StatusCode::OK)
    }

    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        poll_ready(timeout, || self.ping()).await
    }

    pub async fn status(&self) -> ArrResult<Value> {
        let (code, st) =
            self.open_json("status", 10).await.map_err(|e| ArrError::new(format!("{}: status: {e}", self.name)))?;
        if code != 200 || !st.is_object() {
            return Err(ArrError::new(format!(
                "{}: status: expected an object from the Seerr v1 API (HTTP {code})",
                self.name
            )));
        }
        Ok(json!({"version": st["version"], "databaseType": null, "instanceName": "Seerr",
                  "branch": st["commitTag"], "osName": null, "migrationVersion": null, "databaseVersion": null,
                  "updateAvailable": st["updateAvailable"], "restartRequired": st["restartRequired"]}))
    }

    pub async fn collection(&self) -> ArrResult<Value> {
        if !self.initialized().await {
            return Ok(json!({}));
        }
        let c = self.get("request/count").await?;
        let n = |k: &str| c.get(k).cloned().unwrap_or(json!(0));
        Ok(json!({"requests": n("total"), "pending": n("pending"), "available": n("available")}))
    }

    pub async fn scope(&self) -> Value {
        let p = self.public().await;
        if !truthy(&p["initialized"]) {
            return json!({});
        }
        let mut out = Map::new();
        out.insert("media_server".into(), json!(media_server(&p["mediaServerType"])));
        for k in ["radarr", "sonarr"] {
            if let Ok(v) = self.get(&format!("settings/{k}")).await {
                out.insert(k.into(), json!(count(&v)));
            }
        }
        Value::Object(out)
    }

    pub async fn health(&self) -> Vec<Value> {
        if !truthy(&self.public().await["initialized"]) {
            return vec![json!({"type": "warning", "source": "setup",
                               "message": "Not set up yet: open Seerr and sign in with Plex, Jellyfin or Emby"})];
        }
        match self.open_json("status", 8).await {
            Ok((_, st)) if truthy(&st["restartRequired"]) => {
                vec![
                    json!({"type": "warning", "source": "restart", "message": "Seerr asks for a restart to apply settings"}),
                ]
            }
            _ => vec![],
        }
    }

    pub async fn restart(&self) -> ArrResult<()> {
        Err(self.core.restart_error())
    }
    pub async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        self.core.restore_config(path, app_stopped).await
    }

    // ---- Radarr / Sonarr entries ---------------------------------------------------
    pub async fn servers(&self, kind: &str) -> ArrResult<Vec<Value>> {
        match self.get(&format!("settings/{kind}")).await? {
            Value::Array(a) if a.iter().all(Value::is_object) => Ok(a),
            _ => Err(ArrError::new(format!(
                "{}: settings/{kind}: expected an array of objects from the Seerr v1 API",
                self.name
            ))),
        }
    }

    /// Seerr tries the connection; the answer has the profiles and root folders.
    pub async fn test_server(&self, kind: &str, body: &Value) -> ArrResult<Value> {
        let r = self
            .req(Method::POST, &format!("settings/{kind}/test"), &[], Some(body), Some(Duration::from_secs(60)))
            .await?;
        Ok(if truthy(&r) { r } else { json!({}) })
    }

    pub async fn save_server(&self, kind: &str, body: &Value, sid: Option<i64>) -> ArrResult<Value> {
        match sid {
            None => self.post(&format!("settings/{kind}"), body).await,
            Some(id) => self.put(&format!("settings/{kind}/{id}"), body).await,
        }
    }
}

#[async_trait::async_trait]
impl crate::Service for SeerrClient {
    fn link_source(&self) -> Option<&dyn crate::link::LinkSource> {
        Some(self)
    }
    fn def(&self) -> &'static crate::ServiceDef {
        &crate::seerr::DEF
    }
    fn name(&self) -> &str {
        &self.name
    }
    async fn ping(&self) -> bool {
        SeerrClient::ping(self).await
    }
    async fn wait_ready(&self, timeout: Duration) -> bool {
        SeerrClient::wait_ready(self, timeout).await
    }
    async fn status(&self) -> ArrResult<Value> {
        SeerrClient::status(self).await
    }
    async fn collection(&self) -> ArrResult<Value> {
        SeerrClient::collection(self).await
    }
    async fn scope(&self) -> Value {
        SeerrClient::scope(self).await
    }
    async fn health(&self) -> Vec<Value> {
        SeerrClient::health(self).await
    }
    async fn restart(&self) -> ArrResult<()> {
        SeerrClient::restart(self).await
    }
    async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        SeerrClient::restore_config(self, path, app_stopped).await
    }
    fn feed(&self) -> Option<&dyn Feed> {
        Some(self)
    }
}

// ---- the home page list: the latest requests ----------------------------------------------

fn request_status(s: &Value) -> Option<&'static str> {
    Some(match s.as_i64()? {
        1 => "pending approval",
        2 => "approved",
        3 => "declined",
        4 => "failed",
        5 => "completed",
        _ => return None,
    })
}

fn media_status(s: &Value) -> Option<&'static str> {
    Some(match s.as_i64()? {
        3 => "processing",
        4 => "partly available",
        5 => "available",
        _ => return None,
    })
}

#[async_trait::async_trait]
impl Feed for SeerrClient {
    async fn list(&self, kind: FeedKind, st: &FeedQuery, cache: &Lookups) -> ArrResult<FeedPage> {
        match kind {
            FeedKind::Requests => self.requests(st, cache).await,
            _ => Err(feeds::no_list(&self.name, kind)),
        }
    }
}

impl SeerrClient {
    /// A film's or show's title and year, looked up once (`kind`: movie or tv).
    async fn title(&self, cache: &Lookups, kind: &str, tmdb: &Value) -> String {
        let id = wranglarr_core::value::text(tmdb);
        if id.is_empty() {
            return "unknown title".into();
        }
        let key = format!("{kind}:{id}");
        if let Some(t) = cache.title(&key) {
            return t;
        }
        match self.get(&format!("/{kind}/{id}")).await {
            Ok(d) => {
                let year: String = feeds::or_text(&d["releaseDate"], &d["firstAirDate"]).chars().take(4).collect();
                let name = feeds::or_text(&d["title"], &d["name"]);
                let mut t = if name.is_empty() { format!("TMDB {id}") } else { name };
                if !year.is_empty() {
                    t += &format!(" ({year})");
                }
                cache.keep_title(key, t.clone());
                t
            }
            Err(e) => {
                tracing::debug!("feeds: seerr title {key}: {e}");
                format!("TMDB {id}")
            }
        }
    }

    async fn requests(&self, st: &FeedQuery, cache: &Lookups) -> ArrResult<FeedPage> {
        if !self.initialized().await {
            return Ok(FeedPage {
                empty: Some("Seerr isn't set up yet.".into()),
                ..FeedPage::new("Requests", vec![], 0)
            });
        }
        let take = st.limit.to_string();
        let q = [("take", take.as_str()), ("skip", "0"), ("sort", "added"), ("filter", "all")];
        let j = self.get_query("/request", &q).await?;
        let results = j["results"].as_array().cloned().unwrap_or_default();
        let mut items = Vec::new();
        for r in &results {
            let m = &r["media"];
            let kind =
                [&r["type"], &m["mediaType"]].into_iter().find(|v| truthy(v)).map_or("movie".into(), feeds::text);
            let mut status = request_status(&r["status"]).unwrap_or("");
            if matches!(r["status"].as_i64(), Some(2 | 5))
                && let Some(ms) = media_status(&m["status"])
            {
                status = ms;
            }
            let who = feeds::text(&r["requestedBy"]["displayName"]);
            let seasons: Vec<String> = r["seasons"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| wranglarr_core::value::text(&s["seasonNumber"]))
                .collect();
            let season = if seasons.is_empty() { String::new() } else { format!("season {}", seasons.join(", ")) };
            let k4 = if truthy(&r["is4k"]) { "4K".to_string() } else { String::new() };
            let state = match r["status"].as_i64() {
                Some(1) => "warn",
                Some(3 | 4) => "err",
                _ => "",
            };
            let title = self.title(cache, &kind, &m["tmdbId"]).await;
            let path =
                if truthy(&m["tmdbId"]) { format!("/{kind}/{}", feeds::text(&m["tmdbId"])) } else { String::new() };
            let sub = feeds::join(&[who, season, k4]);
            items.push(feeds::item(&json!(title), sub, &r["createdAt"], status.into(), state, path, false));
        }
        let pending = results.iter().filter(|r| r["status"].as_i64() == Some(1)).count();
        let total = j["pageInfo"]["results"].as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(items.len());
        Ok(FeedPage {
            note: Some(if pending > 0 { format!("{pending} waiting for approval") } else { String::new() }),
            ..FeedPage::new("Requests", items, total)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_servers() {
        assert_eq!(media_server(&json!(2)), "Jellyfin");
        assert_eq!(media_server(&json!(4)), "none");
        assert_eq!(media_server(&Value::Null), "?");
    }

    #[tokio::test]
    async fn servers_reject_non_list_or_non_object_responses() {
        use axum::{Router, routing::get};
        let router = Router::new()
            .route("/api/v1/settings/radarr", get(|| async { axum::Json(json!(null)) }))
            .route("/api/v1/settings/sonarr", get(|| async { axum::Json(json!([null])) }))
            .route("/api/v1/settings/good", get(|| async { axum::Json(json!([{"future": true}])) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
        let c = SeerrClient::new("seerr", &format!("http://{addr}"), "k", "");
        assert!(c.servers("radarr").await.is_err());
        assert!(c.servers("sonarr").await.is_err());
        assert_eq!(c.servers("good").await.expect("servers")[0]["future"], true);
        server.abort();
    }

    #[test]
    fn api_base() {
        let c = SeerrClient::new("seerr", "http://seerr:5055/", "k", "");
        assert_eq!(c.api, "http://seerr:5055/api/v1");
        assert!(c.core.config_dir.is_none());
    }
}
