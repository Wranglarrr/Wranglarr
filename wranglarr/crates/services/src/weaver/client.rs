// SPDX-License-Identifier: GPL-3.0-or-later
//! Weaver's APIs: its NZBGet JSON-RPC API for the queue and history (what the arrs
//! use too), and its GraphQL API for what that one can't do (its version, its
//! categories, its API keys). Both take the key wranglarr created; this file also
//! creates it ([`mint`]).

use std::path::Path;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::{KEY_FILE, KEY_NAME, RPC_USER};
use crate::downloads::{Download, status_map};
use crate::file_app::FileApp;
use crate::nzbget::NzbgetClient;
use crate::{AppConfigExt, BoxFuture};
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::{Error, Result};
use wranglarr_core::http::{ArrError, ArrResult, poll_ready, reason, redact};
use wranglarr_core::value::text;

/// Weaver's clients: the arrs' NZBGet client and its GraphQL API, on the key
/// wranglarr created (read from `api_key_file`, created on first use, created
/// again when Weaver refuses it).
pub struct WeaverClient {
    app: AppConfig,
    core: FileApp,
    key: Mutex<Option<String>>,
}

impl WeaverClient {
    pub fn new(app: &AppConfig) -> WeaverClient {
        WeaverClient {
            app: app.clone(),
            core: FileApp::new(&app.name, &app.url, &app.config_dir, Duration::from_secs(15)),
            key: Mutex::new(None),
        }
    }

    fn name(&self) -> &str {
        &self.app.name
    }

    /// The key: the one in use, else read or created.
    async fn key(&self) -> ArrResult<String> {
        let mut k = self.key.lock().await;
        if let Some(k) = k.as_ref() {
            return Ok(k.clone());
        }
        let got = crate::api_key(&self.app).await.map_err(|e| ArrError::new(e.to_string()))?;
        *k = Some(got.clone());
        Ok(got)
    }

    /// Weaver refused `stale`: create another key (once, whoever asks first).
    async fn renew(&self, stale: &str) -> ArrResult<String> {
        let mut k = self.key.lock().await;
        if let Some(cur) = k.as_ref()
            && cur != stale
        {
            return Ok(cur.clone());
        }
        let got = mint_key(&self.app, Some(stale)).await.map_err(|e| ArrError::new(e.to_string()))?;
        *k = Some(got.clone());
        Ok(got)
    }

    fn nzb(&self, key: &str) -> NzbgetClient {
        NzbgetClient::new(&self.app.name, &self.app.url, RPC_USER, key, &self.app.config_dir)
    }

    /// `call` on the NZBGet API; once more on a new key when Weaver refuses the key.
    async fn rpc<T, F, Fut>(&self, call: F) -> ArrResult<T>
    where
        F: Fn(NzbgetClient) -> Fut,
        Fut: std::future::Future<Output = ArrResult<T>>,
    {
        let key = self.key().await?;
        match call(self.nzb(&key)).await {
            Err(e) if e.status() == Some(StatusCode::UNAUTHORIZED) => {
                let key = self.renew(&key).await?;
                call(self.nzb(&key)).await
            }
            r => r,
        }
    }

    /// One GraphQL operation with the key: its `data`. GraphQL errors fail it.
    pub async fn gql(&self, query: &str, variables: Value) -> ArrResult<Value> {
        let key = self.key().await?;
        match self.gql_with(&key, query, &variables).await {
            Err(e) if e.status() == Some(StatusCode::UNAUTHORIZED) => {
                let key = self.renew(&key).await?;
                self.gql_with(&key, query, &variables).await
            }
            r => r,
        }
    }

    async fn gql_with(&self, key: &str, query: &str, variables: &Value) -> ArrResult<Value> {
        let what = "graphql";
        let r = self
            .core
            .http
            .post(format!("{}/graphql", self.core.base))
            .bearer_auth(key)
            .json(&json!({"query": query, "variables": variables}))
            .send()
            .await
            .map_err(|e| ArrError::new(format!("{}: {what}: {}", self.name(), reason(&e))))?;
        let status = r.status();
        let body = wranglarr_core::http::response_text(r)
            .await
            .map_err(|e| ArrError::new(format!("{}: {what}: {e}", self.name())))?;
        self.core.check(what, status, &redact(body.clone(), &[key]))?;
        graphql_data(self.name(), &body)
    }

    pub async fn ping(&self) -> bool {
        let r = self.core.http.get(format!("{}/readyz", self.core.base)).timeout(Duration::from_secs(5)).send().await;
        matches!(r, Ok(r) if r.status().is_success())
    }

    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        poll_ready(timeout, || self.ping()).await
    }

    pub async fn status(&self) -> ArrResult<Value> {
        let d = self.gql("{ version }", json!({})).await?;
        Ok(status_map(d["version"].clone(), "Weaver", Value::Null))
    }

    /// `{enabled, username}` of its login.
    pub async fn login_status(&self) -> ArrResult<Value> {
        Ok(self.gql("{ adminLoginStatus { enabled username } }", json!({})).await?["adminLoginStatus"].take())
    }

    pub async fn collection(&self) -> ArrResult<Value> {
        self.rpc(|c| async move { c.collection().await }).await
    }

    pub async fn downloads(&self) -> ArrResult<Vec<Download>> {
        self.rpc(|c| async move { c.downloads().await }).await
    }

    pub async fn download_root(&self) -> ArrResult<String> {
        self.rpc(|c| async move { c.download_root().await }).await
    }

    /// Add the missing categories. Each one's folder is `<complete folder>/<name>`.
    pub async fn ensure_categories(&self, names: &[String]) -> ArrResult<Vec<String>> {
        let have: Vec<String> = self.gql("{ categories { name } }", json!({})).await?["categories"]
            .as_array()
            .ok_or_else(|| ArrError::new(format!("{}: categories: expected a list", self.name())))?
            .iter()
            .map(|c| text(&c["name"]).to_lowercase())
            .collect();
        let mut added = vec![];
        for n in names.iter().filter(|n| !have.contains(&n.to_lowercase())) {
            self.gql(
                "mutation($input: CategoryInput!) { addCategory(input: $input) { id } }",
                json!({"input": {"name": n}}),
            )
            .await?;
            added.push(n.clone());
        }
        Ok(added)
    }

    pub async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        self.core.restore_config(path, app_stopped).await
    }
}

/// `data` of a GraphQL answer, or its first error.
fn graphql_data(name: &str, body: &str) -> ArrResult<Value> {
    let mut j: Value =
        serde_json::from_str(body).map_err(|e| ArrError::new(format!("{name}: graphql: bad JSON: {e}")))?;
    if let Some(e) = j["errors"].as_array().and_then(|e| e.first()) {
        let code = text(&e["extensions"]["code"]);
        let msg = text(&e["message"]);
        return Err(ArrError::new(if code.is_empty() {
            format!("{name}: {msg}")
        } else {
            format!("{name}: {msg} ({code})")
        }));
    }
    Ok(j.get_mut("data").map(Value::take).unwrap_or_default())
}

// ---- the key: created with Weaver's login -------------------------------------------------

/// A browser session in Weaver: its cookie and the CSRF token that goes with it.
pub(crate) struct Session {
    base: String,
    cookie: String,
    csrf: String,
}

/// Sign in to Weaver with a login, as its pages do. `None`: the login is wrong.
pub(crate) async fn sign_in(
    c: &reqwest::Client,
    base: &str,
    user: &str,
    pass: &str,
    timeout: Duration,
) -> std::result::Result<Option<Session>, String> {
    if user.is_empty() || pass.is_empty() {
        return Ok(None);
    }
    let r = c
        .post(format!("{base}/api/login"))
        .header("Origin", base)
        .json(&json!({"username": user, "password": pass}))
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| reason(&e))?;
    let status = r.status();
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Ok(None);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err("Weaver refuses logins for now (too many attempts)".into());
    }
    if !status.is_success() {
        return Err(format!("login -> {}", status.as_u16()));
    }
    let cookie = session_cookie(r.headers()).ok_or("login: Weaver set no session cookie")?;
    let j: Value = r.json().await.map_err(|e| reason(&e))?;
    let csrf = text(&j["csrfToken"]);
    if csrf.is_empty() {
        return Err("login: Weaver sent no CSRF token".into());
    }
    Ok(Some(Session { base: base.into(), cookie, csrf }))
}

/// `name=value` of the session cookie Weaver set.
fn session_cookie(h: &HeaderMap) -> Option<String> {
    h.get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .find(|kv| kv.trim_start().starts_with("weaver_session="))
        .map(|kv| kv.trim().to_string())
}

impl Session {
    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in [("origin", &self.base), ("cookie", &self.cookie), ("x-weaver-csrf", &self.csrf)] {
            if let Ok(v) = HeaderValue::from_str(v) {
                h.insert(k, v);
            }
        }
        h
    }

    async fn gql(&self, c: &reqwest::Client, query: &str, variables: Value) -> std::result::Result<Value, String> {
        let r = c
            .post(format!("{}/graphql", self.base))
            .headers(self.headers())
            .json(&json!({"query": query, "variables": variables}))
            .send()
            .await
            .map_err(|e| reason(&e))?;
        let status = r.status();
        let body = wranglarr_core::http::response_text(r).await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("graphql -> {}", status.as_u16()));
        }
        graphql_data("weaver", &body).map_err(|e| e.message().to_string())
    }

    /// End the session (best effort).
    pub(crate) async fn sign_out(&self, c: &reqwest::Client, timeout: Duration) {
        let _ = c.post(format!("{}/api/logout", self.base)).headers(self.headers()).timeout(timeout).send().await;
    }
}

/// [`Credentials::MintedKey`](crate::Credentials)'s `mint` for Weaver.
pub fn mint<'a>(app: &'a AppConfig, stale: Option<&'a str>) -> BoxFuture<'a, Result<String>> {
    Box::pin(mint_key(app, stale))
}

/// One key is created at a time, so callers that all find the file empty get one key.
static MINTING: Mutex<()> = Mutex::const_new(());
/// The last failure to create one, by key file: tried again only after [`RETRY_AFTER`]
/// (the key watcher asks every minute, and each try is a login attempt in Weaver).
static FAILED: std::sync::Mutex<Vec<(String, std::time::Instant, String)>> = std::sync::Mutex::new(Vec::new());
const RETRY_AFTER: Duration = Duration::from_secs(300);

/// Sign in with Weaver's login (`username` / `password`: WEAVER_USER / WEAVER_PASS),
/// replace the key named [`KEY_NAME`] with a new one, and keep it in `api_key_file`.
/// `stale`: the key Weaver refused, if that is why.
async fn mint_key(app: &AppConfig, stale: Option<&str>) -> Result<String> {
    let _one = MINTING.lock().await;
    let name = &app.name;
    let path = app.api_key_file.clone();
    if path.is_empty() {
        return Err(Error::invalid(format!("{name}: api_key_file is required (where wranglarr keeps its key)")));
    }
    // made while this one waited, or written by an earlier run
    let current = {
        let a = app.clone();
        tokio::task::spawn_blocking(move || a.api_key_blocking()).await?.ok()
    };
    if let Some(k) = current
        && Some(k.as_str()) != stale
    {
        return Ok(k);
    }
    if let Some(why) = FAILED.lock().ok().and_then(|f| {
        f.iter().find(|(p, at, _)| *p == path && at.elapsed() < RETRY_AFTER).map(|(_, _, why)| why.clone())
    }) {
        return Err(Error::invalid(why));
    }
    let made = create(app, &path).await;
    if let Ok(mut f) = FAILED.lock() {
        f.retain(|(p, _, _)| *p != path);
        if let Err(e) = &made {
            f.push((path.clone(), std::time::Instant::now(), e.to_string()));
        }
    }
    made
}

/// Sign in, replace the key, write the file.
async fn create(app: &AppConfig, path: &str) -> Result<String> {
    let name = &app.name;
    let path = path.to_string();
    let (user, pass) = crate::credentials(app).await;
    let c = wranglarr_core::http::http(Duration::from_secs(30));
    let base = app.url.trim_end_matches('/');
    let timeout = Duration::from_secs(15);
    let s = sign_in(&c, base, &user, &pass, timeout)
        .await
        .map_err(|e| Error::invalid(format!("{name}: creating its API key: {e}")))?
        .ok_or_else(|| {
            Error::invalid(format!(
                "{name}: WEAVER_USER / WEAVER_PASS don't log in to Weaver, so wranglarr can't create its API key \
                 (Weaver takes them only on its first start)"
            ))
        })?;
    let made = async {
        let keys = s.gql(&c, "{ apiKeys { id name } }", json!({})).await?;
        for k in keys["apiKeys"].as_array().into_iter().flatten().filter(|k| text(&k["name"]) == KEY_NAME) {
            s.gql(&c, "mutation($id: Int!) { deleteApiKey(id: $id) { id } }", json!({"id": k["id"]})).await?;
        }
        let made = s
            .gql(
                &c,
                "mutation($name: String!) { createApiKey(name: $name, scope: ADMIN) { key { id } rawKey } }",
                json!({"name": KEY_NAME}),
            )
            .await?;
        let raw = text(&made["createApiKey"]["rawKey"]);
        if raw.is_empty() {
            return Err("Weaver returned no key".to_string());
        }
        Ok((raw, made["createApiKey"]["key"]["id"].clone()))
    }
    .await;
    s.sign_out(&c, timeout).await;
    let (raw, id) = made.map_err(|e| Error::invalid(format!("{name}: creating its API key: {e}")))?;
    let body = serde_json::to_string_pretty(&json!({"api_key": raw, "id": id, "name": KEY_NAME}))
        .map_err(|e| Error::internal(e.to_string()))?;
    tokio::task::spawn_blocking(move || write_private(Path::new(&path), &body)).await??;
    tracing::info!("{name}: created wranglarr's API key in Weaver");
    Ok(raw)
}

/// Write `body` to `path` readable by its owner only, replacing it whole.
fn write_private(path: &Path, body: &str) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| Error::invalid(format!("{}: no folder", path.display())))?;
    let tmp = dir.join(format!(".{KEY_FILE}.tmp"));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(&tmp).map_err(|e| Error::invalid(format!("{}: {e}", tmp.display())))?;
    f.write_all(body.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| Error::invalid(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))
}

#[async_trait::async_trait]
impl crate::Service for WeaverClient {
    fn def(&self) -> &'static crate::ServiceDef {
        &super::DEF
    }
    fn name(&self) -> &str {
        &self.app.name
    }
    async fn ping(&self) -> bool {
        WeaverClient::ping(self).await
    }
    async fn wait_ready(&self, timeout: Duration) -> bool {
        WeaverClient::wait_ready(self, timeout).await
    }
    async fn status(&self) -> ArrResult<Value> {
        WeaverClient::status(self).await
    }
    async fn collection(&self) -> ArrResult<Value> {
        WeaverClient::collection(self).await
    }
    async fn scope(&self) -> Value {
        json!({})
    }
    async fn health(&self) -> Vec<Value> {
        vec![]
    }
    async fn restart(&self) -> ArrResult<()> {
        Err(self.core.restart_error())
    }
    async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        WeaverClient::restore_config(self, path, app_stopped).await
    }
    fn download_client(&self) -> Option<&dyn crate::DownloadClient> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl crate::DownloadClient for WeaverClient {
    async fn downloads(&self) -> ArrResult<Vec<Download>> {
        WeaverClient::downloads(self).await
    }
    async fn download_root(&self) -> ArrResult<String> {
        WeaverClient::download_root(self).await
    }
    async fn ensure_categories(&self, names: &[String]) -> ArrResult<Vec<String>> {
        WeaverClient::ensure_categories(self, names).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_errors_fail() {
        let e = graphql_data(
            "w",
            r#"{"data":null,"errors":[{"message":"admin scope required","extensions":{"code":"FORBIDDEN"}}]}"#,
        )
        .expect_err("an error");
        assert_eq!(e.message(), "w: admin scope required (FORBIDDEN)");
        assert_eq!(graphql_data("w", r#"{"data":{"version":"0.16.15"}}"#).expect("data")["version"], "0.16.15");
    }

    #[test]
    fn finds_the_session_cookie() {
        let mut h = HeaderMap::new();
        h.append(reqwest::header::SET_COOKIE, HeaderValue::from_static("other=1; Path=/"));
        h.append(reqwest::header::SET_COOKIE, HeaderValue::from_static("weaver_session=abc; HttpOnly; Path=/"));
        assert_eq!(session_cookie(&h).as_deref(), Some("weaver_session=abc"));
    }
}
