// SPDX-License-Identifier: GPL-3.0-or-later
//! SABnzbd's API: status, queue and history; its config is copied as files.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::downloads::{Download, hms, parse_json, status_map};
use crate::file_app::FileApp;
use wranglarr_core::http::{ArrError, ArrResult, poll_ready, reason};
use wranglarr_core::value::{float, head, int, or, text, truthy};

/// SABnzbd history status of a job still being worked on.
const SAB_PP: &[&str] =
    &["Queued", "QuickCheck", "Verifying", "Repairing", "Fetching", "Extracting", "Moving", "Running"];

// ---- SABnzbd ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SabnzbdClient {
    pub(crate) core: FileApp,
    pub(crate) name: String,
    api_key: String,
}

impl SabnzbdClient {
    pub fn new(name: &str, url: &str, api_key: &str, config_dir: &str) -> SabnzbdClient {
        SabnzbdClient {
            core: FileApp::new(name, url, config_dir, Duration::from_secs(15)),
            name: name.into(),
            api_key: api_key.into(),
        }
    }

    /// `GET /api?mode=<mode>&…`: the JSON answer; `"status": false` is an error.
    pub async fn api(&self, mode: &str, params: &[(&str, &str)]) -> ArrResult<Value> {
        let r = self
            .core
            .http
            .get(format!("{}/api", self.core.base))
            .query(params)
            .query(&[("mode", mode), ("output", "json"), ("apikey", &self.api_key)])
            .send()
            .await
            .map_err(|e| ArrError::new(format!("{}: {mode}: {}", self.name, reason(&e))))?;
        let status = r.status();
        let body = wranglarr_core::http::response_text(r)
            .await
            .map_err(|e| ArrError::new(format!("{}: {mode}: {e}", self.name)))?;
        self.core.check(mode, status, &wranglarr_core::http::redact(body.clone(), &[&self.api_key]))?;
        let j = parse_json(&self.name, mode, &body)?;
        if j.get("status") == Some(&Value::Bool(false)) {
            return Err(ArrError::new(format!(
                "{}: {mode}: {}",
                self.name,
                wranglarr_core::http::redact(text(&j["error"]), &[&self.api_key])
            )));
        }
        Ok(j)
    }

    pub async fn ping(&self) -> bool {
        let r = self
            .core
            .http
            .get(format!("{}/api", self.core.base))
            .query(&[("mode", "version"), ("output", "json")])
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        matches!(r, Ok(r) if r.status() == StatusCode::OK)
    }

    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        poll_ready(timeout, || self.ping()).await
    }

    pub async fn status(&self) -> ArrResult<Value> {
        let v = self.api("version", &[]).await?;
        Ok(status_map(v["version"].clone(), "SABnzbd", Value::Null))
    }

    pub async fn collection(&self) -> ArrResult<Value> {
        let q = self.api("queue", &[("limit", "1")]).await?;
        let h = self.api("history", &[("limit", "1")]).await?;
        Ok(sab_collection(&q["queue"], &h["history"]))
    }

    /// The queue, plus jobs in history still being verified / repaired / unpacked.
    pub async fn downloads(&self) -> ArrResult<Vec<Download>> {
        let q = self.api("queue", &[("limit", "100")]).await?;
        let h = self.api("history", &[("limit", "20")]).await.unwrap_or_default();
        Ok(sab_downloads(&q["queue"], &h["history"]))
    }

    pub async fn download_root(&self) -> ArrResult<String> {
        let j = self.api("get_config", &[("section", "misc"), ("keyword", "complete_dir")]).await?;
        Ok(text(&j["config"]["misc"]["complete_dir"]))
    }

    /// Create missing categories (folder = name, under complete_dir). Returns the ones created.
    pub async fn ensure_categories(&self, names: &[String]) -> ArrResult<Vec<String>> {
        let cats = self.api("get_cats", &[]).await?;
        let categories = cats["categories"]
            .as_array()
            .filter(|rows| rows.iter().all(Value::is_string))
            .ok_or_else(|| ArrError::new(format!("{}: get_cats: expected an array of category names", self.name)))?;
        let have: HashSet<String> = categories.iter().map(|c| text(c).to_lowercase()).collect();
        let mut added = vec![];
        for n in names {
            let n = n.to_lowercase();
            if !have.contains(&n) {
                self.api("set_config", &[("section", "categories"), ("name", &n), ("dir", &n)]).await?;
                added.push(n);
            }
        }
        Ok(added)
    }

    pub async fn health(&self) -> Vec<Value> {
        match self.api("warnings", &[]).await {
            Ok(w) => sab_warnings(&w),
            Err(_) => vec![],
        }
    }

    pub async fn restart(&self) -> ArrResult<()> {
        Err(self.core.restart_error())
    }
    pub async fn scope(&self) -> Value {
        json!({})
    }
    pub async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        self.core.restore_config(path, app_stopped).await
    }
}

pub fn sab_collection(q: &Value, h: &Value) -> Value {
    json!({
        "queue": int(or(or(&q["noofslots_total"], &q["noofslots"]), &json!(0))),
        "history": int(&h["noofslots"]),
        "paused": i64::from(truthy(&q["paused"])),
    })
}

pub fn sab_downloads(q: &Value, h: &Value) -> Vec<Download> {
    let paused = truthy(&q["paused"]);
    let mut speed = (float(&q["kbpersec"]) * 1024.0) as i64;
    let mut out = vec![];
    for sl in q["slots"].as_array().into_iter().flatten() {
        let st = text(&sl["status"]);
        let size = (float(&sl["mb"]) * 1024.0 * 1024.0) as i64;
        let left = (float(&sl["mbleft"]) * 1024.0 * 1024.0) as i64;
        let state = if paused || st == "Paused" {
            "paused"
        } else if st == "Downloading" {
            "downloading"
        } else if matches!(st.as_str(), "Fetching" | "Grabbing" | "Propagating") {
            "metadata"
        } else if st == "Checking" {
            "checking"
        } else {
            "queued"
        };
        let mut item = Download {
            id: text(&sl["nzo_id"]),
            name: text(&sl["filename"]),
            category: text(&sl["cat"]),
            state: state.into(),
            state_text: if paused && st != "Paused" { "paused (whole queue)".into() } else { st.to_lowercase() },
            progress: float(&sl["percentage"]) / 100.0,
            size,
            done: size - left,
            eta: if state == "downloading" { hms(&sl["timeleft"]) } else { None },
            ..Download::default()
        }
        .clamped();
        if state == "downloading" && speed != 0 {
            // SABnzbd reports one speed for the queue
            item.speed = speed;
            speed = 0;
        }
        out.push(item);
    }
    for sl in h["slots"].as_array().into_iter().flatten() {
        let st = text(&sl["status"]);
        if SAB_PP.contains(&st.as_str()) {
            let action = text(&sl["action_line"]);
            let bytes = int(&sl["bytes"]);
            out.push(
                Download {
                    id: text(&sl["nzo_id"]),
                    name: text(&sl["name"]),
                    category: text(&sl["category"]),
                    state: "processing".into(),
                    state_text: head(&if action.is_empty() { st.to_lowercase() } else { action }, 80),
                    progress: 1.0,
                    size: bytes,
                    done: bytes,
                    ..Download::default()
                }
                .clamped(),
            );
        }
    }
    out
}

pub fn sab_warnings(w: &Value) -> Vec<Value> {
    w["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .take(3)
        .map(|x| {
            // dicts since SABnzbd 3; plain strings before
            let t = if x.is_string() { text(x) } else { text(&x["text"]) };
            json!({"type": "warning", "message": head(&t, 120)})
        })
        .collect()
}

#[async_trait::async_trait]
impl crate::Service for SabnzbdClient {
    fn def(&self) -> &'static crate::ServiceDef {
        &crate::sabnzbd::DEF
    }
    fn name(&self) -> &str {
        &self.name
    }
    async fn ping(&self) -> bool {
        SabnzbdClient::ping(self).await
    }
    async fn wait_ready(&self, timeout: Duration) -> bool {
        SabnzbdClient::wait_ready(self, timeout).await
    }
    async fn status(&self) -> ArrResult<Value> {
        SabnzbdClient::status(self).await
    }
    async fn collection(&self) -> ArrResult<Value> {
        SabnzbdClient::collection(self).await
    }
    async fn scope(&self) -> Value {
        SabnzbdClient::scope(self).await
    }
    async fn health(&self) -> Vec<Value> {
        SabnzbdClient::health(self).await
    }
    async fn restart(&self) -> ArrResult<()> {
        SabnzbdClient::restart(self).await
    }
    async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        SabnzbdClient::restore_config(self, path, app_stopped).await
    }
    fn download_client(&self) -> Option<&dyn crate::DownloadClient> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl crate::DownloadClient for SabnzbdClient {
    async fn downloads(&self) -> ArrResult<Vec<Download>> {
        SabnzbdClient::downloads(self).await
    }
    async fn download_root(&self) -> ArrResult<String> {
        SabnzbdClient::download_root(self).await
    }
    async fn ensure_categories(&self, names: &[String]) -> ArrResult<Vec<String>> {
        SabnzbdClient::ensure_categories(self, names).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sab_items() {
        let q = json!({"paused": false, "kbpersec": "100.0", "noofslots_total": 3, "slots": [
            {"nzo_id": "n1", "filename": "One", "cat": "sonarr", "status": "Downloading", "mb": "100", "mbleft": "25",
             "percentage": "75", "timeleft": "0:01:00"},
            {"nzo_id": "n2", "filename": "Two", "cat": "radarr", "status": "Downloading", "mb": "10", "mbleft": "10",
             "percentage": "0", "timeleft": "0:10:00"},
            {"nzo_id": "n3", "filename": "Three", "status": "Propagating", "mb": "1", "mbleft": "1", "percentage": "0"},
        ]});
        let h = json!({"noofslots": 120, "slots": [
            {"nzo_id": "h1", "name": "Old", "category": "tv", "status": "Completed", "bytes": 5},
            {"nzo_id": "h2", "name": "Busy", "category": "tv", "status": "Extracting", "action_line": "Unpacking 1/2", "bytes": 7},
            {"nzo_id": "h3", "name": "Wait", "status": "QuickCheck", "bytes": "9"},
        ]});
        let out = sab_downloads(&q, &h);
        assert_eq!(out.len(), 5);
        assert_eq!(
            (out[0].speed, out[0].eta, out[0].done, out[0].progress),
            (102400, Some(60), 75 * 1024 * 1024, 0.75)
        );
        assert_eq!((out[1].speed, out[1].eta), (0, Some(600)));
        assert_eq!((out[2].state.as_str(), out[2].state_text.as_str(), out[2].eta), ("metadata", "propagating", None));
        assert_eq!(
            (out[3].state.as_str(), out[3].state_text.as_str(), out[3].size, out[3].progress),
            ("processing", "Unpacking 1/2", 7, 1.0)
        );
        assert_eq!(out[4].state_text, "quickcheck");
        assert_eq!(sab_collection(&q, &h), json!({"queue": 3, "history": 120, "paused": 0}));

        let paused = sab_downloads(
            &json!({"paused": true, "slots": [{"status": "Downloading"}, {"status": "Paused"}]}),
            &Value::Null,
        );
        assert_eq!((paused[0].state.as_str(), paused[0].state_text.as_str()), ("paused", "paused (whole queue)"));
        assert_eq!(paused[1].state_text, "paused");
        assert_eq!(
            sab_collection(&json!({"noofslots": 2, "paused": true}), &json!({})),
            json!({"queue": 2, "history": 0, "paused": 1})
        );
    }

    #[test]
    fn sab_health() {
        let w = json!({"warnings": [{"text": "a"}, {"text": "b"}, "old style", {"text": "d"}]});
        let h = sab_warnings(&w);
        assert_eq!(h.len(), 3);
        assert_eq!(h[0], json!({"type": "warning", "message": "a"}));
        assert_eq!(h[2]["message"], "old style");
    }
}
