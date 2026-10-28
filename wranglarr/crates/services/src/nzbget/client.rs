// SPDX-License-Identifier: GPL-3.0-or-later
//! NZBGet's JSON-RPC API: status, queue and categories; its config is copied as files.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::downloads::{Download, parse_json, rows, status_map};
use crate::file_app::FileApp;
use wranglarr_core::http::{ArrError, ArrResult, poll_ready, reason};
use wranglarr_core::value::{count, int, text, truthy};

/// NZBGet history status of a job still being worked on.
const NZBGET_PP: &[&str] = &[
    "PP_QUEUED",
    "LOADING_PARS",
    "VERIFYING_SOURCES",
    "REPAIRING",
    "VERIFYING_REPAIRED",
    "RENAMING",
    "UNPACKING",
    "MOVING",
    "EXECUTING_SCRIPT",
    "PP_FINISHED",
];

// ---- NZBGet ------------------------------------------------------------------------------

#[derive(Clone)]
pub struct NzbgetClient {
    pub(crate) core: FileApp,
    pub(crate) name: String,
    username: String,
    password: String,
}

impl NzbgetClient {
    pub fn new(name: &str, url: &str, username: &str, password: &str, config_dir: &str) -> NzbgetClient {
        NzbgetClient {
            core: FileApp::new(name, url, config_dir, Duration::from_secs(15)),
            name: name.into(),
            username: username.into(),
            password: password.into(),
        }
    }

    fn post(&self, method: &str, params: &[Value]) -> reqwest::RequestBuilder {
        let rb = self
            .core
            .http
            .post(format!("{}/jsonrpc", self.core.base))
            .json(&json!({"method": method, "params": params, "id": 1}));
        if self.username.is_empty() { rb } else { rb.basic_auth(&self.username, Some(&self.password)) }
    }

    /// One JSON-RPC call: its result (Null when there is none).
    pub async fn rpc(&self, method: &str, params: &[Value]) -> ArrResult<Value> {
        let r = self
            .post(method, params)
            .send()
            .await
            .map_err(|e| ArrError::new(format!("{}: {method}: {}", self.name, reason(&e))))?;
        let status = r.status();
        let body = wranglarr_core::http::response_text(r)
            .await
            .map_err(|e| ArrError::new(format!("{}: {method}: {e}", self.name)))?;
        self.core.check(method, status, &wranglarr_core::http::redact(body.clone(), &[&self.password]))?;
        let mut j = parse_json(&self.name, method, &body)?;
        if truthy(&j["error"]) {
            return Err(ArrError::new(format!(
                "{}: {method}: {}",
                self.name,
                wranglarr_core::http::redact(text(&j["error"]), &[&self.password])
            )));
        }
        Ok(j.get_mut("result").map(Value::take).unwrap_or_default())
    }

    pub async fn ping(&self) -> bool {
        let r = self.post("version", &[]).timeout(Duration::from_secs(5)).send().await;
        matches!(r, Ok(r) if matches!(r.status().as_u16(), 200 | 401))
    }

    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        poll_ready(timeout, || self.ping()).await
    }

    pub async fn download_root(&self) -> ArrResult<String> {
        let opts = rows(self.rpc("config", &[]).await?);
        Ok(opts.iter().find(|o| o["Name"] == "DestDir").map(|o| text(&o["Value"])).unwrap_or_default())
    }

    /// Add missing categories to nzbget.conf (DestDir empty = `<DestDir>/<name>`) and reload.
    pub async fn ensure_categories(&self, names: &[String]) -> ArrResult<Vec<String>> {
        // Refuse to replace the on-disk configuration if its existing rows cannot be read.
        let config = self.rpc("loadconfig", &[]).await?;
        let mut opts = match config {
            Value::Array(rows)
                if rows.iter().all(|r| {
                    r.is_object() && r["Name"].as_str().is_some_and(|n| !n.is_empty()) && r["Value"].is_string()
                }) =>
            {
                rows
            }
            _ => {
                return Err(ArrError::new(format!(
                    "{}: loadconfig: expected an array of configuration objects",
                    self.name
                )));
            }
        };
        let (have, mut idx) = nzbget_categories(&opts);
        let added: Vec<String> = names.iter().filter(|n| !have.contains(&n.to_lowercase())).cloned().collect();
        if added.is_empty() {
            return Ok(vec![]);
        }
        for n in &added {
            idx += 1;
            opts.push(json!({"Name": format!("Category{idx}.Name"), "Value": n}));
            opts.push(json!({"Name": format!("Category{idx}.DestDir"), "Value": ""}));
            opts.push(json!({"Name": format!("Category{idx}.Unpack"), "Value": "yes"}));
        }
        if !truthy(&self.rpc("saveconfig", &[Value::Array(opts)]).await?) {
            return Err(ArrError::new(format!("{}: saveconfig refused", self.name)));
        }
        self.rpc("reload", &[]).await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        if !self.wait_ready(Duration::from_secs(120)).await {
            return Err(ArrError::new(format!("{}: did not come back after reload", self.name)));
        }
        Ok(added)
    }

    pub async fn status(&self) -> ArrResult<Value> {
        Ok(status_map(self.rpc("version", &[]).await?, "NZBGet", Value::Null))
    }

    pub async fn downloads(&self) -> ArrResult<Vec<Download>> {
        let groups = rows(self.rpc("listgroups", &[json!(0)]).await?);
        let st = self.rpc("status", &[]).await?;
        Ok(nzbget_downloads(&groups, &st))
    }

    pub async fn collection(&self) -> ArrResult<Value> {
        let groups = self.rpc("listgroups", &[json!(0)]).await?;
        let hist = self.rpc("history", &[json!(false)]).await?;
        let st = self.rpc("status", &[]).await?;
        Ok(json!({
            "queue": count(&groups),
            "history": count(&hist),
            "paused": i64::from(truthy(&st["DownloadPaused"])),
            "size_on_disk": int(&st["DownloadedSizeMB"]) * 1024 * 1024,
        }))
    }

    pub async fn restart(&self) -> ArrResult<()> {
        Err(self.core.restart_error())
    }
    pub async fn scope(&self) -> Value {
        json!({})
    }
    pub async fn health(&self) -> Vec<Value> {
        vec![]
    }
    pub async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        self.core.restore_config(path, app_stopped).await
    }
}

/// The category names in nzbget.conf (lower case) and the highest CategoryN.
pub fn nzbget_categories(opts: &[Value]) -> (HashSet<String>, i64) {
    let (mut have, mut idx) = (HashSet::new(), 0);
    for o in opts {
        let n = text(&o["Name"]);
        let l = n.to_lowercase();
        if l.starts_with("category") && l.ends_with(".name") {
            have.insert(text(&o["Value"]).to_lowercase());
            if let Some(i) = n.get(8..n.len().saturating_sub(5)).and_then(|x| x.trim().parse::<i64>().ok()) {
                idx = idx.max(i);
            }
        }
    }
    (have, idx)
}

pub fn nzbget_downloads(groups: &[Value], st: &Value) -> Vec<Download> {
    let paused = truthy(&st["DownloadPaused"]);
    let mut speed = int(&st["DownloadRate"]);
    let mut out = vec![];
    for g in groups {
        let status = text(&g["Status"]);
        let hi_lo = |hi: &str, lo: &str| (int(&g[hi]) << 32) + int(&g[lo]);
        let mut size = hi_lo("FileSizeHi", "FileSizeLo");
        if size == 0 {
            size = int(&g["FileSizeMB"]) * 1024 * 1024;
        }
        let left = if g.get("RemainingSizeLo").is_some() {
            hi_lo("RemainingSizeHi", "RemainingSizeLo")
        } else {
            int(&g["RemainingSizeMB"]) * 1024 * 1024
        };
        let state = if NZBGET_PP.contains(&status.as_str()) {
            "processing"
        } else if status == "PAUSED" || (paused && matches!(status.as_str(), "QUEUED" | "DOWNLOADING")) {
            "paused"
        } else if status == "DOWNLOADING" {
            "downloading"
        } else if status == "FETCHING" {
            "metadata"
        } else {
            "queued"
        };
        let mut item = Download {
            id: text(&g["NZBID"]),
            name: text(&g["NZBName"]),
            category: text(&g["Category"]),
            state: state.into(),
            state_text: status.to_lowercase().replace('_', " "),
            progress: if size != 0 { 1.0 - left as f64 / size as f64 } else { 0.0 },
            size,
            done: size - left,
            ..Download::default()
        }
        .clamped();
        if state == "downloading" && speed != 0 {
            item.speed = speed;
            item.eta = (left != 0).then(|| (left as f64 / speed as f64) as i64);
            speed = 0;
        }
        out.push(item);
    }
    out
}

#[async_trait::async_trait]
impl crate::Service for NzbgetClient {
    fn def(&self) -> &'static crate::ServiceDef {
        &crate::nzbget::DEF
    }
    fn name(&self) -> &str {
        &self.name
    }
    async fn ping(&self) -> bool {
        NzbgetClient::ping(self).await
    }
    async fn wait_ready(&self, timeout: Duration) -> bool {
        NzbgetClient::wait_ready(self, timeout).await
    }
    async fn status(&self) -> ArrResult<Value> {
        NzbgetClient::status(self).await
    }
    async fn collection(&self) -> ArrResult<Value> {
        NzbgetClient::collection(self).await
    }
    async fn scope(&self) -> Value {
        NzbgetClient::scope(self).await
    }
    async fn health(&self) -> Vec<Value> {
        NzbgetClient::health(self).await
    }
    async fn restart(&self) -> ArrResult<()> {
        NzbgetClient::restart(self).await
    }
    async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        NzbgetClient::restore_config(self, path, app_stopped).await
    }
    fn download_client(&self) -> Option<&dyn crate::DownloadClient> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl crate::DownloadClient for NzbgetClient {
    async fn downloads(&self) -> ArrResult<Vec<Download>> {
        NzbgetClient::downloads(self).await
    }
    async fn download_root(&self) -> ArrResult<String> {
        NzbgetClient::download_root(self).await
    }
    async fn ensure_categories(&self, names: &[String]) -> ArrResult<Vec<String>> {
        NzbgetClient::ensure_categories(self, names).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nzbget_items() {
        let groups = [
            json!({"NZBID": 7, "NZBName": "Big", "Category": "radarr", "Status": "DOWNLOADING",
                   "FileSizeHi": 1, "FileSizeLo": 0, "RemainingSizeHi": 0, "RemainingSizeLo": 1 << 30}),
            json!({"NZBID": 8, "NZBName": "Small", "Status": "QUEUED", "FileSizeMB": 10, "RemainingSizeMB": 10}),
            json!({"NZBID": 9, "NZBName": "Post", "Status": "PP_QUEUED", "FileSizeMB": 1, "RemainingSizeMB": 0}),
            json!({"NZBID": 10, "Status": "FETCHING"}),
        ];
        let out = nzbget_downloads(&groups, &json!({"DownloadRate": 1 << 20, "DownloadPaused": false}));
        assert_eq!(out[0].id, "7");
        assert_eq!(out[0].size, 1 << 32);
        assert_eq!(out[0].progress, 0.75);
        assert_eq!((out[0].speed, out[0].eta), (1 << 20, Some(1024)));
        assert_eq!((out[1].state.as_str(), out[1].size, out[1].done, out[1].progress), ("queued", 10 << 20, 0, 0.0));
        assert_eq!(
            (out[2].state.as_str(), out[2].state_text.as_str(), out[2].progress),
            ("processing", "pp queued", 1.0)
        );
        assert_eq!((out[3].state.as_str(), out[3].progress), ("metadata", 0.0));
        let out = nzbget_downloads(&groups[..2], &json!({"DownloadPaused": true}));
        assert!(out.iter().all(|d| d.state == "paused"));
    }

    #[test]
    fn nzbget_category_index() {
        let opts = [
            json!({"Name": "Category1.Name", "Value": "Movies"}),
            json!({"Name": "Category1.DestDir", "Value": ""}),
            json!({"Name": "category3.name", "Value": "TV"}),
            json!({"Name": "Category.Name", "Value": "x"}),
            json!({"Name": "MainDir", "Value": "/data"}),
        ];
        let (have, idx) = nzbget_categories(&opts);
        assert_eq!(idx, 3);
        assert!(have.contains("movies") && have.contains("tv") && have.contains("x"));
    }
}
