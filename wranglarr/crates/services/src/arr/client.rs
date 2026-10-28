// SPDX-License-Identifier: GPL-3.0-or-later
//! Client for the *arr HTTP API (Radarr, Sonarr, Lidarr, Readarr, Prowlarr, Whisparr).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Local, TimeDelta, Utc};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::arr::{ArrDef, Calendar, Collection, Lists, Record};
use crate::feed::{
    self as feeds, FeedPage, FeedQuery, Item, Lookups, iso, item, join, parse_v, some_path, text, window,
};
use crate::{Feed, FeedKind, Service, ServiceDef};
use wranglarr_core::http::{
    ArrError, ArrResult, check_zip_async, http, redact, response_bytes, response_text, safe_url,
};
use wranglarr_core::value::truthy;

/// One arr's API client. Its fields are fixed when it's built ([`ArrClient::new`]):
/// the kind, its descriptors and the addresses derived from the URL stay in step.
#[derive(Clone)]
pub struct ArrClient {
    name: String,
    kind: String,
    def: &'static ServiceDef,
    /// What this arr does differently.
    arr_def: &'static ArrDef,
    base: String,
    api: String,
    api_key: String,
    /// The app's /config, mounted read-only.
    config_dir: Option<PathBuf>,
    http: reqwest::Client,
}

pub struct Raw {
    pub status: StatusCode,
    pub content_type: String,
    pub body: bytes::Bytes,
}

impl ArrClient {
    pub fn new(name: &str, kind: &str, url: &str, api_key: &str, config_dir: Option<PathBuf>) -> ArrResult<ArrClient> {
        let (def, arr_def) = crate::get(kind)
            .and_then(|d| Some((d, d.arr?)))
            .ok_or_else(|| ArrError::new(format!("{name}: unsupported app kind {kind:?}")))?;
        let base = url.trim_end_matches('/').to_string();
        Ok(ArrClient {
            name: name.into(),
            kind: kind.into(),
            def,
            arr_def,
            api: format!("{base}/api/{}", arr_def.api_version),
            base,
            api_key: api_key.into(),
            config_dir,
            http: http(Duration::from_secs(30)),
        })
    }

    /// The app's name in wranglarr.toml.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its kind's id (`radarr`, `sonarr`, …).
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// What wranglarr knows about its kind.
    pub fn def(&self) -> &'static ServiceDef {
        self.def
    }

    /// What this arr does differently from the others.
    pub fn arr_def(&self) -> &'static ArrDef {
        self.arr_def
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{}/{}", self.api, path.trim_start_matches('/'))
        }
    }

    /// Any request; an error status becomes an ArrError with the arr's reason.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        timeout: Option<Duration>,
    ) -> ArrResult<Raw> {
        let display_path = safe_url(path);
        let mut rb = self.http.request(method.clone(), self.url(path)).header("X-Api-Key", &self.api_key);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        if let Some(t) = timeout {
            rb = rb.timeout(t);
        }
        let r = rb
            .send()
            .await
            .map_err(|e| ArrError::new(format!("{}: {method} {display_path}: {}", self.name, e.without_url())))?;
        let status = r.status();
        let content_type = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let body = response_bytes(r)
            .await
            .map_err(|e| ArrError::new(format!("{}: {method} {display_path}: {e}", self.name)))?;
        if status.as_u16() >= 400 {
            let safe_body = redact(String::from_utf8_lossy(&body).into_owned(), &[&self.api_key]);
            let detail = error_detail(status, &safe_body, &self.base);
            return Err(ArrError::with_status(
                status,
                format!("{}: {method} {display_path} -> {}: {detail}", self.name, status.as_u16()),
            ));
        }
        Ok(Raw { status, content_type, body })
    }

    /// JSON answer; only mutation responses may have an empty body.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        timeout: Option<Duration>,
    ) -> ArrResult<Value> {
        let requires_json = method == Method::GET;
        let r = self.send(method, path, body, timeout).await?;
        let media_type = r.content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let is_json = media_type == "application/json" || media_type.ends_with("+json");
        if is_json && !r.body.is_empty() {
            serde_json::from_slice(&r.body)
                .map_err(|e| ArrError::new(format!("{}: {}: bad JSON: {e}", self.name, safe_url(path))))
        } else if requires_json {
            Err(ArrError::new(format!(
                "{}: {}: expected JSON response (content type {:?})",
                self.name,
                safe_url(path),
                media_type
            )))
        } else {
            Ok(Value::Null)
        }
    }

    pub async fn get(&self, path: &str) -> ArrResult<Value> {
        self.call(Method::GET, path, None, None).await
    }
    pub async fn post(&self, path: &str, body: &Value) -> ArrResult<Value> {
        self.call(Method::POST, path, Some(body), None).await
    }
    pub async fn put(&self, path: &str, body: &Value) -> ArrResult<Value> {
        self.call(Method::PUT, path, Some(body), None).await
    }
    pub async fn delete(&self, path: &str) -> ArrResult<Value> {
        self.call(Method::DELETE, path, None, None).await
    }
    /// An arr API list, preserving all fields while rejecting incompatible shapes.
    pub async fn list(&self, path: &str) -> ArrResult<Vec<Value>> {
        match self.get(path).await? {
            Value::Array(a) if a.iter().all(Value::is_object) => Ok(a),
            _ => Err(ArrError::new(format!(
                "{}: {}: expected an array of objects from the {} API",
                self.name,
                safe_url(path),
                self.arr_def.api_version
            ))),
        }
    }

    // ---- system ------------------------------------------------------------
    pub async fn ping(&self) -> bool {
        matches!(self.http.get(format!("{}/ping", self.base)).timeout(Duration::from_secs(5)).send().await,
                 Ok(r) if r.status() == StatusCode::OK)
    }

    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if self.ping().await && self.status().await.is_ok() {
                return true;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        false
    }

    pub async fn status(&self) -> ArrResult<Value> {
        let status = self.get("system/status").await?;
        if !status.is_object() {
            return Err(ArrError::new(format!(
                "{}: system/status: expected an object from the {} API",
                self.name, self.arr_def.api_version
            )));
        }
        Ok(status)
    }

    pub async fn restart(&self) -> ArrResult<()> {
        self.post("system/restart", &json!({})).await.map(|_| ())
    }

    // ---- backups -----------------------------------------------------------
    /// Trigger the app's own Backup task and wait for it to finish.
    pub async fn run_backup(&self, timeout: Duration) -> ArrResult<Value> {
        let cmd = self.post("command", &json!({"name": "Backup"})).await?;
        let cid = cmd["id"].clone();
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            let cmd = self.get(&format!("command/{cid}")).await?;
            match cmd["status"].as_str() {
                Some("completed") => return Ok(cmd),
                Some(st @ ("failed" | "aborted" | "cancelled")) => {
                    let why = cmd["message"].as_str().or(cmd["exception"].as_str()).unwrap_or("no reason given");
                    return Err(ArrError::new(format!("{}: Backup command {st}: {why}", self.name)));
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(ArrError::new(format!("{}: Backup command timed out", self.name)))
    }

    pub async fn newest_manual_backup(&self) -> ArrResult<Option<Value>> {
        let all = self.list("system/backup").await?;
        Ok(all
            .into_iter()
            .filter(|b| b["type"] == "manual")
            .max_by(|a, b| a["time"].as_str().unwrap_or("").cmp(b["time"].as_str().unwrap_or(""))))
    }

    /// Copy the zip from the app's mounted /config (`Backups/<type>/<name>`), else
    /// download it from `/backup/<type>/<name>`. The download only works where the
    /// app lets wranglarr in without its login page.
    pub async fn download_backup(&self, backup: &Value, dest: &Path) -> ArrResult<u64> {
        let io = |e: std::io::Error| ArrError::new(format!("{}: {e}", self.name));
        if let Some(p) = dest.parent() {
            tokio::fs::create_dir_all(p).await.map_err(io)?;
        }
        let typ = backup["type"].as_str().unwrap_or("manual");
        let name = backup["name"].as_str().unwrap_or_default();
        if !wranglarr_core::store::valid_name(name) || !wranglarr_core::store::valid_name(typ) {
            return Err(ArrError::new(format!("{}: unsafe backup name", self.name)));
        }
        if let Some(cd) = &self.config_dir {
            let local = cd.join("Backups").join(typ).join(name);
            if tokio::fs::metadata(&local).await.is_ok_and(|m| m.is_file()) {
                let (root, relative) = (cd.clone(), format!("Backups/{typ}/{name}"));
                tokio::task::spawn_blocking(move || wranglarr_core::store::checked_path(&root, &relative))
                    .await
                    .map_err(|e| ArrError::new(e.to_string()))?
                    .map_err(|e| ArrError::new(format!("{}: {e}", self.name)))?;
                tokio::fs::copy(&local, dest).await.map_err(io)?;
                return check_zip_async(&self.name, dest).await;
            }
        }
        let url = match backup["path"].as_str() {
            Some(p) if p.starts_with('/') => format!("{}{p}", self.base),
            _ => format!("{}/backup/{typ}/{name}", self.base),
        };
        let display_url = safe_url(&url);
        let r = self
            .http
            .get(&url)
            .query(&[("apikey", &self.api_key)])
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .map_err(|e| ArrError::new(format!("{}: download {display_url}: {}", self.name, e.without_url())))?;
        if r.status().is_redirection() || r.status() == StatusCode::UNAUTHORIZED {
            return Err(ArrError::new(format!(
                "{}: the backup download went to the app's login page; mount its /config into wranglarr \
                 (compose.yaml does) so the zip is read from disk",
                self.name
            )));
        }
        if r.status().as_u16() >= 400 {
            return Err(ArrError::with_status(
                r.status(),
                format!("{}: download {display_url} -> {}", self.name, r.status().as_u16()),
            ));
        }
        let parent = dest.parent().ok_or_else(|| ArrError::new("backup destination has no parent"))?;
        let parent = parent.to_path_buf();
        let tmp = tokio::task::spawn_blocking(move || tempfile::NamedTempFile::new_in(parent))
            .await
            .map_err(|e| ArrError::new(e.to_string()))?
            .map_err(io)?;
        let (tmp, file) = tokio::task::spawn_blocking(move || {
            let file = tmp.reopen()?;
            Ok::<_, std::io::Error>((tmp, file))
        })
        .await
        .map_err(|e| ArrError::new(e.to_string()))?
        .map_err(io)?;
        let mut output = tokio::fs::File::from_std(file);
        let mut response = r;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ArrError::new(format!("{}: download: {}", self.name, e.without_url())))?
        {
            use tokio::io::AsyncWriteExt;
            output.write_all(&chunk).await.map_err(io)?;
        }
        use tokio::io::AsyncWriteExt;
        output.flush().await.map_err(io)?;
        drop(output);
        let destination = dest.to_path_buf();
        tokio::task::spawn_blocking(move || tmp.persist(destination).map_err(|e| e.error))
            .await
            .map_err(|e| ArrError::new(e.to_string()))?
            .map_err(io)?;
        check_zip_async(&self.name, dest).await
    }

    /// Upload a backup zip: the app restores config.xml (and its SQLite db, if
    /// present), then is restarted when it says it needs one to apply them.
    pub async fn restore_backup_zip(&self, zip: &Path) -> ArrResult<Value> {
        let data = tokio::fs::read(zip).await.map_err(|e| ArrError::new(format!("{}: {e}", self.name)))?;
        let fname = zip.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(fname)
            .mime_str("application/zip")
            .map_err(|e| ArrError::new(e.to_string()))?;
        let form = reqwest::multipart::Form::new().part("restore", part);
        let r = self
            .http
            .post(self.url("system/backup/restore/upload"))
            .header("X-Api-Key", &self.api_key)
            .multipart(form)
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .map_err(|e| ArrError::new(format!("{}: restore upload: {}", self.name, e.without_url())))?;
        let status = r.status();
        let text = response_text(r).await.map_err(|e| ArrError::new(format!("{}: restore upload: {e}", self.name)))?;
        if status.as_u16() >= 400 {
            return Err(ArrError::with_status(
                status,
                format!(
                    "{}: POST system/backup/restore/upload -> {}: {}",
                    self.name,
                    status.as_u16(),
                    error_detail(status, &redact(text.clone(), &[&self.api_key]), &self.base)
                ),
            ));
        }
        let out: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        // it answers {"restartRequired": true} and waits for the restart its UI would send
        if truthy(&out["restartRequired"]) {
            self.restart().await?;
        }
        Ok(out)
    }

    /// Run the app's Backup task, pull the zip, drop it from the app.
    pub async fn capture_config(&self, dest_dir: &Path) -> ArrResult<PathBuf> {
        self.run_backup(Duration::from_secs(600)).await?;
        let b = self
            .newest_manual_backup()
            .await?
            .ok_or_else(|| ArrError::new(format!("{}: no manual backup listed after Backup command", self.name)))?;
        let name = b["name"].as_str().unwrap_or("backup.zip");
        if !wranglarr_core::store::valid_name(name) {
            return Err(ArrError::new(format!("{}: unsafe backup name", self.name)));
        }
        let dest = dest_dir.join(name);
        self.download_backup(&b, &dest).await?;
        if let Err(e) = self.delete(&format!("system/backup/{}", b["id"])).await {
            tracing::warn!("{}: could not delete {} from the app: {e}", self.name, b["name"]);
        }
        Ok(dest)
    }

    pub async fn restore_config(&self, path: &Path, _app_stopped: bool) -> ArrResult<Value> {
        self.restore_backup_zip(path).await
    }

    // ---- collection + scope --------------------------------------------------
    /// Library size/scope: counts and bytes on disk, as its [`Collection`] says.
    pub async fn collection(&self) -> ArrResult<Value> {
        match &self.arr_def.collection {
            Collection::Library(lib) => Ok(lib.counts(&self.list(lib.endpoint).await?)),
            Collection::Counts(lists) => {
                let mut out = Map::new();
                for (key, endpoint) in *lists {
                    out.insert((*key).into(), json!(self.list(endpoint).await?.len()));
                }
                Ok(Value::Object(out))
            }
        }
    }

    /// Configuration scope: how much *settings* the backup covers.
    pub async fn scope(&self) -> Value {
        let mut out = Map::new();
        let a = self.arr_def;
        for ep in a.scope.iter().copied().chain(a.scope_custom_formats.then_some("customformat")) {
            match self.get(ep).await {
                Ok(Value::Array(a)) => {
                    out.insert(ep.into(), json!(a.len()));
                }
                Ok(_) => tracing::debug!("{}: scope {ep}: unexpected shape", self.name),
                Err(e) => tracing::debug!("{}: scope {ep}: {e}", self.name),
            }
        }
        if let Ok(roots) = self.list("rootfolder").await {
            out.insert("rootfolder".into(), json!(roots.len()));
            out.insert(
                "root_free_space".into(),
                json!(roots.iter().map(|r| r["freeSpace"].as_i64().unwrap_or(0)).sum::<i64>()),
            );
        }
        Value::Object(out)
    }

    pub async fn health(&self) -> Vec<Value> {
        self.list("health").await.unwrap_or_default()
    }

    // ---- shared resources (download clients / notifications) ------------------
    pub async fn test_resource(&self, resource: &str, body: &Value) -> ArrResult<()> {
        self.call(Method::POST, &format!("{resource}/test"), Some(body), Some(Duration::from_secs(120)))
            .await
            .map(|_| ())
    }

    /// Create or update by name. Returns ("created"|"updated", resource).
    pub async fn upsert_resource(&self, resource: &str, body: &Value) -> ArrResult<(&'static str, Value)> {
        let existing = self.list(resource).await?;
        let name = &body["name"];
        if let Some(cur) = existing.iter().find(|r| &r["name"] == name) {
            let mut merged = cur.as_object().cloned().unwrap_or_default();
            for (k, v) in body.as_object().into_iter().flatten() {
                if k != "fields" {
                    merged.insert(k.clone(), v.clone());
                }
            }
            // keep fields the app already has, override the ones we set
            let ours: Map<String, Value> = body["fields"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|f| f.get("value").is_some())
                .filter_map(|f| Some((f["name"].as_str()?.to_string(), f["value"].clone())))
                .collect();
            let mut fields: Vec<Value> = cur["fields"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| {
                    let mut f = f.clone();
                    if let Some(v) = f["name"].as_str().and_then(|n| ours.get(n)) {
                        f["value"] = v.clone();
                    }
                    f
                })
                .collect();
            let known: Vec<Value> = fields.iter().map(|f| f["name"].clone()).collect();
            fields.extend(
                body["fields"].as_array().into_iter().flatten().filter(|f| !known.contains(&f["name"])).cloned(),
            );
            merged.insert("fields".into(), Value::Array(fields));
            let id = &cur["id"];
            return Ok(("updated", self.put(&format!("{resource}/{id}"), &Value::Object(merged)).await?));
        }
        Ok(("created", self.post(resource, body).await?))
    }
}

// ---- the home page lists ----------------------------------------------------------------

#[async_trait::async_trait]
impl Feed for ArrClient {
    async fn list(&self, kind: FeedKind, st: &FeedQuery, _lookups: &Lookups) -> ArrResult<FeedPage> {
        // the lists its descriptor names (Prowlarr's indexers, a library's calendar)
        if !self.def.feeds.contains(&kind) {
            return Err(feeds::no_list(&self.name, kind));
        }
        match (kind, &self.arr_def.lists) {
            (FeedKind::Upcoming, Some(lists)) => upcoming(self, lists, st).await,
            (FeedKind::Missing, Some(lists)) => missing(self, lists, st).await,
            (FeedKind::Indexers, _) => indexers(self, st).await,
            _ => Err(feeds::no_list(&self.name, kind)),
        }
    }
}

fn has(r: &Value, k: &str) -> bool {
    r.get(k).is_some()
}

fn records(j: &Value) -> Vec<Value> {
    j["records"].as_array().cloned().unwrap_or_default()
}

async fn get_query(c: &ArrClient, path: &str, params: &[(&str, String)]) -> ArrResult<Value> {
    c.get(&feeds::qs(&format!("{}/{path}", c.api), params)).await
}

/// "S01E02 · title"
fn episode(e: &Value) -> String {
    let code = match (e["seasonNumber"].as_i64(), e["episodeNumber"].as_i64()) {
        (Some(s), Some(n)) => format!("S{s:02}E{n:02}"),
        _ => String::new(),
    };
    join(&[code, text(&e["title"])])
}

/// One calendar / Wanted → Missing record as an item, with its link into the arr's UI.
pub fn shape(record: Record, r: &Value) -> Item {
    let downloaded = || if truthy(&r["hasFile"]) { "downloaded".to_string() } else { String::new() };
    match record {
        Record::Episode => {
            let s = &r["series"];
            let title = if truthy(&s["title"]) { &s["title"] } else { &r["seriesTitle"] };
            item(title, episode(r), &r["airDateUtc"], downloaded(), "", some_path("/series/", &s["titleSlug"]), false)
        }
        Record::Album => {
            let mut sub = text(&r["title"]);
            if truthy(&r["albumType"]) && r["albumType"] != "Album" {
                sub += &format!(" ({})", text(&r["albumType"]).to_lowercase());
            }
            let path = some_path("/album/", &r["foreignAlbumId"]);
            item(&r["artist"]["artistName"], sub, &r["releaseDate"], String::new(), "", path, true)
        }
        Record::Book => item(
            &r["author"]["authorName"],
            text(&r["title"]),
            &r["releaseDate"],
            String::new(),
            "",
            String::new(),
            true,
        ),
        Record::Movie => {
            let mut title = if truthy(&r["title"]) { text(&r["title"]) } else { "?".into() };
            if truthy(&r["year"]) {
                title += &format!(" ({})", text(&r["year"]));
            }
            let when = ["releaseDate", "digitalRelease", "physicalRelease", "inCinemas"]
                .iter()
                .map(|k| &r[*k])
                .find(|v| truthy(v))
                .unwrap_or(&Value::Null);
            item(&json!(title), String::new(), when, downloaded(), "", some_path("/movie/", &r["titleSlug"]), true)
        }
    }
}

/// The calendar: dated releases (the next few, months ahead) or the next days' episodes.
async fn upcoming(c: &ArrClient, lists: &Lists, st: &FeedQuery) -> ArrResult<FeedPage> {
    let releases = lists.calendar == Calendar::Releases;
    let (start, end) = window(if releases { st.release_days } else { st.days });
    let t = |b: bool| if b { "true".to_string() } else { "false".to_string() };
    let params = [
        ("start", iso(start)),
        ("end", iso(end)),
        ("unmonitored", t(st.unmonitored)),
        ("includeSeries", t(true)),
        ("includeArtist", t(true)),
        ("includeAuthor", t(true)),
    ];
    let rows = get_query(c, "calendar", &params).await?;
    let mut items = Vec::new();
    for r in rows.as_array().into_iter().flatten() {
        let record = lists.record(r);
        let dates = record.dates();
        if dates.is_empty() {
            items.push(shape(record, r));
        }
        // a film can have several dates; show the ones in the window
        for (field, label) in dates {
            if let Some(d) = parse_v(&r[*field])
                && start <= d
                && d < end
            {
                items.push(Item { sub: label.to_string(), when: text(&r[*field]), ..shape(record, r) });
            }
        }
    }
    items.sort_by(|a, b| {
        let k = |i: &Item| if i.when.is_empty() { "9".to_string() } else { i.when.clone() };
        k(a).cmp(&k(b))
    });
    let total = items.len();
    if releases {
        let n = st.release_next.min(st.limit);
        items.truncate(n);
        return Ok(FeedPage {
            note: Some(format!("next {n} · {} days", st.release_days)),
            days: Some(st.release_days),
            next: Some(n),
            ..FeedPage::new("Upcoming", items, total)
        });
    }
    items.truncate(st.limit);
    Ok(FeedPage {
        note: Some(format!("next {} days", st.days)),
        days: Some(st.days),
        ..FeedPage::new("Upcoming", items, total)
    })
}

/// Monitored, out, no file: only the recent ones (missing_days), newest first; `all` is every missing one.
async fn missing(c: &ArrClient, lists: &Lists, st: &FeedQuery) -> ArrResult<FeedPage> {
    let (lim, days) = (st.limit, st.missing_days);
    let since = (days != 0).then(|| Local::now().fixed_offset() - TimeDelta::days(days));
    let recent = |items: Vec<Item>| -> Vec<Item> {
        items.into_iter().filter(|i| since.is_none_or(|s| feeds::parse(&i.when).unwrap_or(s) >= s)).collect()
    };
    let take = |mut v: Vec<Item>| {
        v.truncate(lim);
        v
    };
    let page = |items: Vec<Item>, total: usize, all: usize| FeedPage {
        all: Some(all),
        path: Some("/wanted/missing".into()),
        ..FeedPage::new("Missing", take(items), total)
    };
    if lists.records.contains(&Record::Movie) {
        // Radarr counts films that aren't out yet as missing, and can't sort by release date: filter and sort here
        let params = [("page", "1".to_string()), ("pageSize", "1000".into()), ("monitored", "true".into())];
        let recs = records(&get_query(c, "wanted/missing", &params).await?);
        if !recs.iter().any(|r| has(r, "series")) {
            // Whisparr v2 has episodes: the generic path below
            let mut items: Vec<Item> = recs
                .iter()
                .filter(|r| r.get("isAvailable").is_none_or(truthy))
                .map(|r| shape(lists.record(r), r))
                .collect();
            items.sort_by(|a, b| b.when.cmp(&a.when));
            let all = items.len();
            let hits = recent(items);
            let total = hits.len();
            return Ok(page(hits, total, all));
        }
    }
    let size = if since.is_some() { lim.max(250) } else { lim };
    let params = [
        ("page", "1".to_string()),
        ("pageSize", size.to_string()),
        ("monitored", "true".into()),
        ("sortKey", lists.missing_sort.into()),
        ("sortDirection", "descending".into()),
        ("includeSeries", "true".into()),
        ("includeArtist", "true".into()),
        ("includeAuthor", "true".into()),
    ];
    let j = get_query(c, "wanted/missing", &params).await?;
    let hits: Vec<Item> = recent(records(&j).iter().map(|r| shape(lists.record(r), r)).collect());
    let every = j["totalRecords"].as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(hits.len());
    let total = if since.is_some() { hits.len() } else { every };
    Ok(page(hits, total, every))
}

/// Prowlarr's indexers, failing ones first.
async fn indexers(c: &ArrClient, st: &FeedQuery) -> ArrResult<FeedPage> {
    let idx = c.get("indexer").await?;
    let status: HashMap<String, Value> = c
        .get("indexerstatus")
        .await?
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| (s["indexerId"].to_string(), s.clone()))
        .collect();
    let now = Utc::now().fixed_offset();
    let mut items = Vec::new();
    for i in idx.as_array().into_iter().flatten() {
        let s = status.get(&i["id"].to_string()).cloned().unwrap_or(Value::Null);
        let till = parse_v(&s["disabledTill"]);
        let (state, tag) = if !truthy(&i["enable"]) {
            ("muted", "off")
        } else if till.is_some_and(|t| t > now) {
            ("err", "failing")
        } else {
            ("", "")
        };
        let sub = join(&[text(&i["protocol"]), text(&i["privacy"])]);
        let when = if state == "err" { &s["mostRecentFailure"] } else { &Value::Null };
        items.push(item(&i["name"], sub, when, tag.into(), state, String::new(), false));
    }
    let rank = |s: &str| match s {
        "err" => 0,
        "" => 1,
        _ => 2,
    };
    items.sort_by_key(|x| (rank(&x.state), x.title.to_lowercase()));
    let failing = items.iter().filter(|x| x.state == "err").count();
    let total = items.len();
    items.truncate(st.limit);
    Ok(FeedPage {
        note: Some(if failing > 0 { format!("{failing} failing") } else { String::new() }),
        path: Some("/".into()),
        ..FeedPage::new("Indexers", items, total)
    })
}

#[async_trait::async_trait]
impl Service for ArrClient {
    fn def(&self) -> &'static ServiceDef {
        self.def
    }
    fn name(&self) -> &str {
        &self.name
    }
    async fn ping(&self) -> bool {
        ArrClient::ping(self).await
    }
    async fn wait_ready(&self, timeout: Duration) -> bool {
        ArrClient::wait_ready(self, timeout).await
    }
    async fn status(&self) -> ArrResult<Value> {
        ArrClient::status(self).await
    }
    async fn collection(&self) -> ArrResult<Value> {
        ArrClient::collection(self).await
    }
    async fn scope(&self) -> Value {
        ArrClient::scope(self).await
    }
    async fn health(&self) -> Vec<Value> {
        ArrClient::health(self).await
    }
    async fn restart(&self) -> ArrResult<()> {
        ArrClient::restart(self).await
    }
    async fn capture_config(&self, dest_dir: &Path) -> ArrResult<Option<PathBuf>> {
        ArrClient::capture_config(self, dest_dir).await.map(Some)
    }
    async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        ArrClient::restore_config(self, path, app_stopped).await
    }
    fn arr(&self) -> Option<&ArrClient> {
        Some(self)
    }
    fn feed(&self) -> Option<&dyn Feed> {
        Some(self)
    }
}

/// A short, readable reason from an arr's error body.
fn error_detail(status: StatusCode, text: &str, base: &str) -> String {
    let mut detail: String = text.chars().take(300).collect();
    if let Ok(j) = serde_json::from_str::<Value>(text) {
        match &j {
            Value::Object(o) => {
                if let Some(m) = o.get("message").or(o.get("error")).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    detail = m.to_string();
                }
            }
            Value::Array(a) if a.first().is_some_and(Value::is_object) => {
                detail = a
                    .iter()
                    .map(|x| {
                        x.get("errorMessage").and_then(Value::as_str).map_or_else(|| x.to_string(), str::to_string)
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
            }
            _ => {}
        }
    }
    let head: String = text.chars().take(400).collect();
    if status == StatusCode::BAD_REQUEST && head.contains("<TITLE>Bad Request</TITLE>") {
        // ASP.NET host filtering: the arr's "Allowed Hosts" doesn't list the name we call it by
        let safe_base = safe_url(base);
        let host = safe_base.split_once("//").map_or(safe_base.as_str(), |(_, h)| h).trim_end_matches('/');
        detail = format!(
            "refused the host name {host:?}: add it to Settings → General → Allowed Hosts (or clear that field) and restart the app"
        );
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details() {
        assert_eq!(error_detail(StatusCode::BAD_REQUEST, r#"{"message":"nope"}"#, "http://r"), "nope");
        assert_eq!(
            error_detail(StatusCode::BAD_REQUEST, r#"[{"errorMessage":"a"},{"errorMessage":"b"}]"#, "http://r"),
            "a; b"
        );
        assert!(
            error_detail(StatusCode::BAD_REQUEST, "<TITLE>Bad Request</TITLE>", "http://radarr:7878")
                .contains("\"radarr:7878\"")
        );
    }
    #[test]
    fn shapes_every_record() {
        let e = shape(
            Record::Episode,
            &json!({"series": {"title": "X", "titleSlug": "x"}, "seasonNumber": 2, "episodeNumber": 3,
                    "title": "T", "airDateUtc": "2026-01-01T01:00:00Z", "hasFile": true}),
        );
        assert_eq!(
            e,
            Item {
                title: "X".into(),
                sub: "S02E03 · T".into(),
                when: "2026-01-01T01:00:00Z".into(),
                tag: "downloaded".into(),
                state: String::new(),
                path: "/series/x".into(),
                day: false
            }
        );
        let e = shape(Record::Episode, &json!({"seriesId": 1, "seriesTitle": "Y", "title": ""}));
        assert_eq!((e.title.as_str(), e.sub.as_str(), e.path.as_str()), ("Y", "", ""));
        let a = shape(
            Record::Album,
            &json!({"artist": {"artistName": "Band"}, "title": "LP", "albumType": "EP",
                    "releaseDate": "2026-01-01T00:00:00Z", "foreignAlbumId": "abc"}),
        );
        assert_eq!((a.title.as_str(), a.sub.as_str(), a.path.as_str(), a.day), ("Band", "LP (ep)", "/album/abc", true));
        let a = shape(Record::Album, &json!({"artistId": 3, "title": "Full", "albumType": "Album"}));
        assert_eq!((a.title.as_str(), a.sub.as_str()), ("?", "Full"));
        let b = shape(
            Record::Book,
            &json!({"author": {"authorName": "Ann"}, "title": "Book", "releaseDate": "2026-02-02"}),
        );
        assert_eq!((b.title.as_str(), b.sub.as_str(), b.when.as_str(), b.day), ("Ann", "Book", "2026-02-02", true));
        let m = shape(
            Record::Movie,
            &json!({"title": "Film", "year": 2026, "digitalRelease": "2026-03-03T00:00:00Z",
                    "inCinemas": "2026-01-01T00:00:00Z", "titleSlug": "film"}),
        );
        assert_eq!(
            (m.title.as_str(), m.when.as_str(), m.path.as_str(), m.tag.as_str()),
            ("Film (2026)", "2026-03-03T00:00:00Z", "/movie/film", "")
        );
        assert_eq!(shape(Record::Movie, &json!({})).title, "?");
    }

    #[tokio::test]
    async fn prowlarr_has_no_calendar() {
        let c = ArrClient::new("p", "prowlarr", "http://127.0.0.1:1", "k", None).expect("client");
        let e = Feed::list(&c, FeedKind::Upcoming, &FeedQuery::default(), &Lookups::default()).await.expect_err("none");
        assert_eq!(e.message(), "p: has no upcoming list");
    }

    #[test]
    fn error_urls_exclude_credentials_and_query() {
        assert_eq!(safe_url("https://user:secret@host/api?apikey=secret#secret"), "https://host/api");
        assert_eq!(safe_url("system/status?apikey=secret"), "system/status");
    }

    #[tokio::test]
    async fn bounded_responses_include_chunked_bodies() {
        use axum::{Router, body::Body, routing::get};
        let router = Router::new().route("/length", get(|| async { "x".repeat(16 * 1024 * 1024 + 1) })).route(
            "/chunked",
            get(|| async {
                Body::from_stream(futures::stream::iter(
                    (0..257).map(|_| Ok::<_, std::convert::Infallible>(bytes::Bytes::from(vec![b'x'; 65536]))),
                ))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
        for path in ["length", "chunked"] {
            let response =
                http(Duration::from_secs(5)).get(format!("http://{addr}/{path}")).send().await.expect("response");
            assert!(response_bytes(response).await.expect_err("limit").to_string().contains("exceeds"));
        }
        server.abort();
    }

    #[tokio::test]
    async fn lists_reject_incompatible_responses_and_preserve_unknown_fields() {
        use axum::{Router, routing::get};
        let router = Router::new()
            .route("/api/v3/html", get(|| async { "<html>login</html>" }))
            .route("/api/v3/null", get(|| async { axum::Json(json!(null)) }))
            .route("/api/v3/object", get(|| async { axum::Json(json!({})) }))
            .route("/api/v3/badrow", get(|| async { axum::Json(json!([null])) }))
            .route("/api/v3/good", get(|| async { axum::Json(json!([{"name":"A", "futureField":{"x":1}}])) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
        let c = ArrClient::new("r", "radarr", &format!("http://{addr}"), "k", None).expect("client");
        for path in ["html", "null", "object", "badrow"] {
            assert!(c.list(path).await.is_err(), "{path}");
        }
        assert_eq!(c.list("good").await.expect("list")[0]["futureField"]["x"], 1);
        server.abort();
    }

    #[tokio::test]
    async fn backup_names_cannot_escape_destination() {
        let client = ArrClient::new("radarr", "radarr", "http://127.0.0.1:1", "secret", None).expect("client");
        let t = tempfile::tempdir().expect("tmp");
        let err = client
            .download_backup(&json!({"type": "manual", "name": "../escape.zip"}), &t.path().join("zip"))
            .await
            .expect_err("bad name");
        assert!(err.to_string().contains("unsafe backup name"));
        assert!(!t.path().join("zip").exists());
    }
    #[tokio::test]
    async fn reflected_api_key_is_redacted_from_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let addr = listener.local_addr().expect("addr");
        let router = axum::Router::new().route(
            "/api/v3/system/status",
            axum::routing::get(|| async {
                (axum::http::StatusCode::BAD_REQUEST, "the key secret-api-key was rejected")
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
        let client = ArrClient::new("r", "radarr", &format!("http://{addr}"), "secret-api-key", None).expect("client");
        let err = client.get("system/status").await.expect_err("upstream error");
        assert!(!err.to_string().contains("secret-api-key"));
        assert!(err.to_string().contains("[redacted]"));
        server.abort();
    }
}
