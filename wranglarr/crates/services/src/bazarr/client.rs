// SPDX-License-Identifier: GPL-3.0-or-later
//! Bazarr client with the same surface as ArrClient, so the service does not branch.
//!
//! Bazarr differences that matter here:
//! - API lives at /api, header X-API-KEY, most responses are wrapped in {"data": ...}.
//! - POST /api/system/backups makes a zip synchronously; GET lists them.
//! - The zip download route needs a browser login, not the API key, so zips are
//!   read from the mounted config dir (<config_dir>/backup/).
//! - With Postgres, the zip holds only config.yaml, and Bazarr's own restore
//!   refuses a zip without bazarr.db. So a config restore writes config.yaml into
//!   <config_dir>/config/ directly and restarts Bazarr through the API.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::feed::{self as feeds, FeedPage, FeedQuery, Item, Lookups};
use crate::{Feed, FeedKind};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use wranglarr_core::http::poll_ready;
use wranglarr_core::http::reason;
use wranglarr_core::http::{ArrError, ArrResult, http};
use wranglarr_core::owner;
use wranglarr_core::value::{count, head, int, text, truthy};

/// A request body.
pub enum Body<'a> {
    None,
    Json(&'a Value),
    Form(&'a [(String, String)]),
}

#[derive(Clone)]
pub struct BazarrClient {
    pub(crate) name: String,
    /// `<url>/api`
    pub(crate) api: String,
    pub(crate) api_key: String,
    /// Bazarr's /config, mounted into wranglarr.
    pub(crate) config_dir: Option<PathBuf>,
    http: reqwest::Client,
    /// The backup the last run_backup made.
    last: Arc<Mutex<Option<Value>>>,
}

/// What a restore of a backup zip does.
enum Restore {
    /// SQLite-era backup (has bazarr.db): Bazarr restores it itself.
    ByBazarr(String),
    /// config.yaml written into place.
    Direct,
}

impl BazarrClient {
    pub fn new(name: &str, url: &str, api_key: &str, config_dir: &str) -> BazarrClient {
        BazarrClient {
            name: name.into(),
            api: format!("{}/api", url.trim_end_matches('/')),
            api_key: api_key.into(),
            config_dir: (!config_dir.is_empty()).then(|| PathBuf::from(config_dir)),
            http: http(Duration::from_secs(30)),
            last: Arc::default(),
        }
    }

    /// Any API call. Null for 204 or an empty body; a body that isn't JSON comes
    /// back as a string.
    pub async fn req(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Body<'_>,
        timeout: Option<Duration>,
    ) -> ArrResult<Value> {
        let mut rb = self
            .http
            .request(method.clone(), format!("{}/{}", self.api, path.trim_start_matches('/')))
            .header("X-API-KEY", &self.api_key)
            .query(query);
        rb = match body {
            Body::None => rb,
            Body::Json(j) => rb.json(j),
            Body::Form(f) => rb.form(f),
        };
        if let Some(t) = timeout {
            rb = rb.timeout(t);
        }
        let err = |e: reqwest::Error| ArrError::new(format!("{}: {method} {path}: {}", self.name, reason(&e)));
        let r = rb.send().await.map_err(err)?;
        let status = r.status();
        let json_type = crate::is_json_type(r.headers());
        let bytes = wranglarr_core::http::response_bytes(r)
            .await
            .map_err(|e| ArrError::new(format!("{}: {method} {path}: {e}", self.name)))?;
        if status.as_u16() >= 400 {
            return Err(ArrError::with_status(
                status,
                format!(
                    "{}: {method} {path} -> {}: {}",
                    self.name,
                    status.as_u16(),
                    head(
                        &wranglarr_core::http::redact(String::from_utf8_lossy(&bytes).into_owned(), &[&self.api_key]),
                        200
                    )
                ),
            ));
        }
        if method == Method::GET && !json_type {
            return Err(ArrError::new(format!("{}: {method} {path}: expected JSON content type", self.name)));
        }
        if status == StatusCode::NO_CONTENT || bytes.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned())))
    }

    pub async fn get(&self, path: &str) -> ArrResult<Value> {
        self.req(Method::GET, path, &[], Body::None, None).await
    }

    /// GET, unwrapping {"data": …}.
    pub async fn data(&self, path: &str) -> ArrResult<Value> {
        let mut j = self.get(path).await?;
        if let Some(d) = j.as_object_mut().and_then(|o| o.remove("data")) {
            return Ok(d);
        }
        Ok(j)
    }

    // ---- system ------------------------------------------------------------
    pub async fn ping(&self) -> bool {
        let r = self.http.get(format!("{}/system/ping", self.api)).timeout(Duration::from_secs(5)).send().await;
        matches!(r, Ok(r) if r.status().as_u16() < 500)
    }

    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        poll_ready(timeout, || async { self.status().await.is_ok() }).await
    }

    pub async fn status(&self) -> ArrResult<Value> {
        let status = self.data("system/status").await?;
        if !status.is_object() {
            return Err(ArrError::new(format!("{}: system/status: expected an object from the Bazarr API", self.name)));
        }
        Ok(status_map(status))
    }

    pub async fn restart(&self) -> ArrResult<()> {
        self.req(Method::POST, "system", &[("action", "restart")], Body::None, None).await.map(|_| ())
    }

    pub async fn health(&self) -> Vec<Value> {
        match self.data("system/health").await {
            Ok(h) => h
                .as_array()
                .into_iter()
                .flatten()
                .map(
                    |h| json!({"type": "warning", "message": format!("{}: {}", text(&h["object"]), text(&h["issue"]))}),
                )
                .collect(),
            Err(_) => vec![],
        }
    }

    // ---- backups -----------------------------------------------------------
    fn backup_dir(&self) -> ArrResult<PathBuf> {
        match &self.config_dir {
            Some(d) => Ok(d.join("backup")),
            None => Err(ArrError::new(format!(
                "{}: config_dir is required for bazarr backups (mount its /config)",
                self.name
            ))),
        }
    }

    pub async fn list_backups(&self) -> ArrResult<Vec<Value>> {
        match self.data("system/backups").await? {
            Value::Array(a) if a.iter().all(Value::is_object) => Ok(a),
            _ => Err(ArrError::new(format!(
                "{}: system/backups: expected an array of objects from the Bazarr API",
                self.name
            ))),
        }
    }

    /// Make a backup and wait for it to show up in the list.
    pub async fn run_backup(&self, timeout: Duration) -> ArrResult<Value> {
        let before: HashSet<String> = self.list_backups().await?.iter().map(|b| text(&b["filename"])).collect();
        self.req(Method::POST, "system/backups", &[], Body::None, Some(timeout)).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while tokio::time::Instant::now() < deadline {
            let newest = self
                .list_backups()
                .await?
                .into_iter()
                .filter(|b| !before.contains(&text(&b["filename"])))
                .max_by_key(|b| text(&b["filename"]));
            if let Some(b) = newest {
                *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(b);
                return Ok(json!({"status": "completed"}));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(ArrError::new(format!("{}: no new backup appeared after POST /system/backups", self.name)))
    }

    /// The backup run_backup made, else the newest listed, in the arrs' shape.
    pub async fn newest_manual_backup(&self) -> ArrResult<Option<Value>> {
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let b = match last {
            Some(b) => b,
            None => match self.list_backups().await?.into_iter().max_by_key(|b| text(&b["filename"])) {
                Some(b) => b,
                None => return Ok(None),
            },
        };
        Ok(Some(
            json!({"id": b["filename"], "name": b["filename"], "type": "manual", "time": b["date"], "size": b["size"]}),
        ))
    }

    /// Copy the zip from the mounted config dir.
    pub async fn download_backup(&self, backup: &Value, dest: &Path) -> ArrResult<u64> {
        let name = text(&backup["name"]);
        if !wranglarr_core::store::valid_name(&name) {
            return Err(ArrError::new(format!("{}: unsafe backup name", self.name)));
        }
        let root = self.backup_dir()?;
        let src = root.join(&name);
        if !tokio::fs::metadata(&src).await.is_ok_and(|m| m.is_file()) {
            let cd = self.config_dir.as_deref().map(|p| p.display().to_string()).unwrap_or_default();
            return Err(ArrError::new(format!(
                "{}: {} not found (is {cd} bazarr's /config?)",
                self.name,
                src.display()
            )));
        }
        let relative = name.clone();
        tokio::task::spawn_blocking(move || wranglarr_core::store::checked_path(&root, &relative))
            .await
            .map_err(|e| ArrError::new(e.to_string()))?
            .map_err(|e| ArrError::new(format!("{}: {e}", self.name)))?;
        let io = |e: std::io::Error| ArrError::new(format!("{}: {e}", self.name));
        if let Some(p) = dest.parent() {
            tokio::fs::create_dir_all(p).await.map_err(io)?;
        }
        tokio::fs::copy(&src, dest).await.map_err(io)?;
        Ok(tokio::fs::metadata(dest).await.map_err(io)?.len())
    }

    pub async fn delete_backup(&self, backup_id: &str) -> ArrResult<()> {
        self.req(Method::DELETE, "system/backups", &[("filename", backup_id)], Body::None, None).await.map(|_| ())
    }

    /// Restore config.yaml from a backup zip (a SQLite-era zip goes to Bazarr's
    /// own restore), then restart Bazarr.
    pub async fn restore_backup_zip(&self, zip_path: &Path) -> ArrResult<Value> {
        let cfg_dir = self.config_dir.clone().unwrap_or_else(|| PathBuf::from("/nonexistent")).join("config");
        let backup_dir = self.backup_dir();
        let (name, zip) = (self.name.clone(), zip_path.to_path_buf());
        let how = tokio::task::spawn_blocking(move || write_config(&name, &zip, &cfg_dir, backup_dir))
            .await
            .map_err(|e| ArrError::new(format!("{}: {e}", self.name)))??;
        match how {
            Restore::ByBazarr(file) => {
                self.req(Method::PATCH, "system/backups", &[("filename", &file)], Body::None, None).await?;
                self.restart().await?;
                Ok(json!({"restartRequired": true, "method": "bazarr"}))
            }
            Restore::Direct => {
                self.restart().await?;
                Ok(json!({"restartRequired": true, "method": "direct"}))
            }
        }
    }

    /// Run a backup, copy the zip into `dest_dir`, drop it from Bazarr.
    pub async fn capture_config(&self, dest_dir: &Path) -> ArrResult<PathBuf> {
        self.run_backup(Duration::from_secs(600)).await?;
        let b = self
            .newest_manual_backup()
            .await?
            .ok_or_else(|| ArrError::new(format!("{}: no backup listed after POST /system/backups", self.name)))?;
        let name = text(&b["name"]);
        if !wranglarr_core::store::valid_name(&name) {
            return Err(ArrError::new(format!("{}: unsafe backup name", self.name)));
        }
        let dest = dest_dir.join(&name);
        self.download_backup(&b, &dest).await?;
        if let Err(e) = self.delete_backup(&text(&b["id"])).await {
            tracing::warn!("{}: could not delete {name}: {e}", self.name);
        }
        Ok(dest)
    }

    pub async fn restore_config(&self, path: &Path, _app_stopped: bool) -> ArrResult<Value> {
        self.restore_backup_zip(path).await
    }

    // ---- collection + scope --------------------------------------------------
    async fn total(&self, path: &str) -> ArrResult<i64> {
        let j = self.req(Method::GET, path, &[("start", "0"), ("length", "1")], Body::None, None).await?;
        Ok(match &j {
            Value::Object(o) => match o.get("total") {
                Some(t) if truthy(t) => int(t),
                _ => count(o.get("data").unwrap_or(&Value::Null)) as i64,
            },
            other => count(other) as i64,
        })
    }

    pub async fn collection(&self) -> ArrResult<Value> {
        let mut out = Map::new();
        for (key, path) in [
            ("subtitles_series", "series"),
            ("subtitles_movies", "movies"),
            ("wanted_episodes", "episodes/wanted"),
            ("wanted_movies", "movies/wanted"),
        ] {
            match self.total(path).await {
                Ok(n) => {
                    out.insert(key.into(), json!(n));
                }
                Err(e) => tracing::debug!("{}: {path}: {e}", self.name),
            }
        }
        Ok(Value::Object(out))
    }

    pub async fn scope(&self) -> Value {
        let mut out = Map::new();
        for (key, path) in [("providers", "providers"), ("languages_profiles", "system/languages/profiles")] {
            match self.data(path).await {
                Ok(Value::Array(a)) => {
                    out.insert(key.into(), json!(a.len()));
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("{}: {path}: {e}", self.name),
            }
        }
        // notification providers live in the settings; count the enabled ones
        match self.get("system/settings").await {
            Ok(s) => match enabled_notifications(&s) {
                Some(n) => {
                    out.insert("notification".into(), json!(n));
                }
                None => tracing::debug!("{}: settings: unexpected shape", self.name),
            },
            Err(e) => tracing::debug!("{}: settings: {e}", self.name),
        }
        Value::Object(out)
    }

    // ---- shared resources: not applicable ------------------------------------
    pub fn no_resources(&self, resource: &str) -> ArrError {
        ArrError::new(format!("{}: bazarr has no '{resource}' resources", self.name))
    }
}

/// /system/status in the arrs' shape.
pub fn status_map(d: Value) -> Value {
    let d = if truthy(&d) { d } else { json!({}) };
    let engine = if truthy(&d["database_engine"]) { d["database_engine"].clone() } else { json!("unknown") };
    json!({"version": d["bazarr_version"], "databaseType": engine, "databaseVersion": null,
           "migrationVersion": d["database_migration"], "instanceName": "Bazarr", "branch": d["branch"],
           "osName": d["operating_system"], "raw": d})
}

/// The enabled notification providers in /system/settings; None when it isn't shaped like that.
pub fn enabled_notifications(s: &Value) -> Option<usize> {
    let s = match s {
        Value::Null => return Some(0),
        Value::Object(o) => o,
        _ => return None,
    };
    let notif = match s.get("notifications") {
        Some(v) if truthy(v) => v.as_object()?,
        _ => return Some(0),
    };
    let mut n = 0;
    for x in notif.get("providers").and_then(Value::as_array).into_iter().flatten() {
        if truthy(x.as_object()?.get("enabled").unwrap_or(&Value::Null)) {
            n += 1;
        }
    }
    Some(n)
}

/// The file work of a restore (blocking).
fn write_config(name: &str, zip_path: &Path, cfg_dir: &Path, backup_dir: ArrResult<PathBuf>) -> ArrResult<Restore> {
    let io = |e: std::io::Error| ArrError::new(format!("{name}: {e}"));
    if !cfg_dir.is_dir() {
        return Err(ArrError::new(format!(
            "{name}: {} not found; config_dir must be bazarr's /config",
            cfg_dir.display()
        )));
    }
    let zip_name = zip_path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let f = std::fs::File::open(zip_path).map_err(io)?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| ArrError::new(format!("{name}: {zip_name}: {e}")))?;
    let names: Vec<String> = z
        .file_names()
        .map(|n| n.map(std::borrow::Cow::into_owned))
        .collect::<Result<_, _>>()
        .map_err(|e| ArrError::new(format!("{name}: {zip_name}: {e}")))?;
    let Some(cfg) = names.iter().find(|n| matches!(n.to_lowercase().as_str(), "config.yaml" | "config.ini")).cloned()
    else {
        return Err(ArrError::new(format!("{name}: {zip_name} has no config.yaml")));
    };
    if names.iter().any(|n| n.to_lowercase() == "bazarr.db") {
        // SQLite-era backup: let Bazarr do the whole thing itself.
        let target = backup_dir?.join(&zip_name);
        if !target.exists() {
            wranglarr_core::files::copy_file(zip_path, &target).map_err(io)?;
            if let Some(p) = target.parent() {
                owner::like(&target, p);
            }
        }
        return Ok(Restore::ByBazarr(zip_name));
    }
    let mut data = vec![];
    z.by_name(&cfg)
        .map_err(|e| ArrError::new(format!("{name}: {zip_name}: {e}")))?
        .read_to_end(&mut data)
        .map_err(io)?;
    let dest = cfg_dir.join(&cfg);
    let reference = if dest.exists() { dest.clone() } else { cfg_dir.to_path_buf() };
    if dest.exists() {
        let keep = cfg_dir.join(format!("{cfg}.pre-wranglarr"));
        wranglarr_core::files::copy_file(&dest, &keep).map_err(io)?;
        owner::like(&keep, &reference);
    }
    wranglarr_core::files::write_atomic(&dest, &data, Some(&reference)).map_err(io)?;
    tracing::info!("{name}: wrote {} from {zip_name}", dest.display());
    Ok(Restore::Direct)
}

#[async_trait::async_trait]
impl crate::Service for BazarrClient {
    fn link_source(&self) -> Option<&dyn crate::link::LinkSource> {
        Some(self)
    }
    fn def(&self) -> &'static crate::ServiceDef {
        &crate::bazarr::DEF
    }
    fn name(&self) -> &str {
        &self.name
    }
    async fn ping(&self) -> bool {
        BazarrClient::ping(self).await
    }
    async fn wait_ready(&self, timeout: Duration) -> bool {
        BazarrClient::wait_ready(self, timeout).await
    }
    async fn status(&self) -> ArrResult<Value> {
        BazarrClient::status(self).await
    }
    async fn collection(&self) -> ArrResult<Value> {
        BazarrClient::collection(self).await
    }
    async fn scope(&self) -> Value {
        BazarrClient::scope(self).await
    }
    async fn health(&self) -> Vec<Value> {
        BazarrClient::health(self).await
    }
    async fn restart(&self) -> ArrResult<()> {
        BazarrClient::restart(self).await
    }
    async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        BazarrClient::restore_config(self, path, app_stopped).await
    }
    async fn capture_config(&self, dest_dir: &Path) -> ArrResult<Option<std::path::PathBuf>> {
        BazarrClient::capture_config(self, dest_dir).await.map(Some)
    }
    fn no_resources(&self, resource: &str) -> ArrError {
        BazarrClient::no_resources(self, resource)
    }
    fn feed(&self) -> Option<&dyn Feed> {
        Some(self)
    }
}

// ---- the home page list: the subtitles it still wants -------------------------------------

#[async_trait::async_trait]
impl Feed for BazarrClient {
    async fn list(&self, kind: FeedKind, st: &FeedQuery, _lookups: &Lookups) -> ArrResult<FeedPage> {
        match kind {
            FeedKind::Missing => self.wanted(st).await,
            _ => Err(feeds::no_list(&self.name, kind)),
        }
    }
}

impl BazarrClient {
    async fn wanted(&self, st: &FeedQuery) -> ArrResult<FeedPage> {
        let lim = st.limit.to_string();
        let q = [("start", "0"), ("length", lim.as_str())];
        let ep = self.req(Method::GET, "episodes/wanted", &q, Body::None, None).await?;
        let mv = self.req(Method::GET, "movies/wanted", &q, Body::None, None).await?;
        let langs = |r: &Value| {
            r["missing_subtitles"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| {
                    let n = feeds::or_text(&x["name"], &x["code2"]);
                    if n.is_empty() { "?".to_string() } else { n }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut items: Vec<Item> = ep["data"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                let sub = feeds::join(&[feeds::text(&r["episode_number"]), feeds::text(&r["episodeTitle"])]);
                let path = feeds::some_path("/series/", &r["sonarrSeriesId"]);
                feeds::item(&r["seriesTitle"], sub, &Value::Null, langs(r), "", path, false)
            })
            .collect();
        items.extend(mv["data"].as_array().into_iter().flatten().map(|r| {
            let path = feeds::some_path("/movies/", &r["radarrId"]);
            feeds::item(&r["title"], "movie".into(), &Value::Null, langs(r), "", path, false)
        }));
        items.truncate(st.limit);
        let total = usize::try_from(int(&ep["total"]) + int(&mv["total"])).unwrap_or(0);
        Ok(FeedPage::new("Wanted subtitles", items, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn status_shape() {
        let s = status_map(json!({"bazarr_version": "1.5.2", "database_engine": "PostgreSQL 17.2",
                                  "database_migration": "abc", "operating_system": "Linux"}));
        assert_eq!(s["version"], "1.5.2");
        assert_eq!(s["databaseType"], "PostgreSQL 17.2");
        assert_eq!(s["migrationVersion"], "abc");
        assert_eq!(s["instanceName"], "Bazarr");
        assert_eq!(s["branch"], Value::Null);
        assert_eq!(s["raw"]["operating_system"], "Linux");
        assert_eq!(status_map(Value::Null)["databaseType"], "unknown");
    }

    #[test]
    fn notifications() {
        let s = json!({"notifications": {"providers": [{"enabled": true}, {"enabled": false}, {"name": "x"}, {"enabled": 1}]}});
        assert_eq!(enabled_notifications(&s), Some(2));
        assert_eq!(enabled_notifications(&json!({"general": {}})), Some(0));
        assert_eq!(enabled_notifications(&json!("not json")), None);
        assert_eq!(enabled_notifications(&json!({"notifications": {"providers": ["x"]}})), None);
    }

    fn zip_with(path: &Path, files: &[(&str, &str)]) {
        let f = std::fs::File::create(path).expect("create");
        let mut z = zip::ZipWriter::new(f);
        for (n, body) in files {
            z.start_file(*n, zip::write::SimpleFileOptions::default()).expect("start");
            z.write_all(body.as_bytes()).expect("write");
        }
        z.finish().expect("finish");
    }

    #[test]
    fn writes_config_yaml() {
        let t = tempfile::tempdir().expect("tmp");
        let cfg_dir = t.path().join("config");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        std::fs::write(cfg_dir.join("config.yaml"), "old: true\n").expect("w");
        let zip = t.path().join("bazarr_backup.zip");
        zip_with(&zip, &[("config.yaml", "new: true\n")]);
        let how = write_config("bazarr", &zip, &cfg_dir, Ok(t.path().join("backup"))).expect("restore");
        assert!(matches!(how, Restore::Direct));
        assert_eq!(std::fs::read_to_string(cfg_dir.join("config.yaml")).expect("r"), "new: true\n");
        assert_eq!(std::fs::read_to_string(cfg_dir.join("config.yaml.pre-wranglarr")).expect("r"), "old: true\n");
        assert!(!cfg_dir.join("config.yaml.tmp").exists());

        zip_with(&zip, &[("other.txt", "x")]);
        let e = write_config("bazarr", &zip, &cfg_dir, Ok(t.path().join("backup"))).err().expect("no config");
        assert_eq!(e.message(), "bazarr: bazarr_backup.zip has no config.yaml");

        let e = write_config("bazarr", &zip, &t.path().join("nope"), Ok(t.path().into())).err().expect("no dir");
        assert!(e.message().ends_with("nope not found; config_dir must be bazarr's /config"), "{}", e.message());
    }

    #[test]
    fn sqlite_backup_goes_to_bazarr() {
        let t = tempfile::tempdir().expect("tmp");
        let cfg_dir = t.path().join("config");
        let backup = t.path().join("backup");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        std::fs::create_dir_all(&backup).expect("mkdir");
        let zip = t.path().join("old.zip");
        zip_with(&zip, &[("config.ini", "x"), ("bazarr.db", "db")]);
        let how = write_config("bazarr", &zip, &cfg_dir, Ok(backup.clone())).expect("restore");
        assert!(matches!(how, Restore::ByBazarr(ref f) if f == "old.zip"));
        assert!(backup.join("old.zip").is_file());
    }
    #[test]
    fn direct_restore_does_not_follow_temporary_symlink() {
        let dir = tempfile::tempdir().expect("tmp");
        let config = dir.path().join("config");
        std::fs::create_dir(&config).expect("mkdir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "unchanged").expect("write");
        std::os::unix::fs::symlink(&victim, config.join("config.yaml.tmp")).expect("symlink");
        let archive = dir.path().join("backup.zip");
        zip_with(&archive, &[("config.yaml", "new: true")]);
        write_config("bazarr", &archive, &config, Ok(dir.path().join("backup"))).expect("restore");
        assert_eq!(std::fs::read_to_string(victim).expect("read"), "unchanged");
        assert_eq!(std::fs::read_to_string(config.join("config.yaml")).expect("read"), "new: true");
    }
}
