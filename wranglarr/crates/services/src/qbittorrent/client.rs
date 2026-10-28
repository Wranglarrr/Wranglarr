// SPDX-License-Identifier: GPL-3.0-or-later
//! qBittorrent's API: status, transfer stats and torrents; its config is copied as files.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::downloads::{Download, parse_json, rows, status_map};
use crate::file_app::FileApp;
use wranglarr_core::http::{ArrError, ArrResult, poll_ready, reason};
use wranglarr_core::value::{float, head, int, text};

/// qBittorrent state -> ours.
pub fn qb_state(st: &str) -> Option<&'static str> {
    Some(match st {
        "downloading" | "forcedDL" => "downloading",
        "metaDL" | "forcedMetaDL" => "metadata",
        "stalledDL" => "stalled",
        "queuedDL" => "queued",
        "pausedDL" | "stoppedDL" => "paused",
        "checkingDL" | "checkingUP" | "checkingResumeData" | "allocating" => "checking",
        "moving" => "processing",
        "error" | "missingFiles" => "error",
        _ => return None,
    })
}

/// qBittorrent state -> words.
pub fn qb_text(st: &str) -> Option<&'static str> {
    Some(match st {
        "downloading" => "downloading",
        "forcedDL" => "downloading (forced)",
        "metaDL" | "forcedMetaDL" => "fetching metadata",
        "stalledDL" => "stalled, no peers",
        "queuedDL" => "queued",
        "pausedDL" => "paused",
        "stoppedDL" => "stopped",
        "checkingDL" | "checkingUP" | "checkingResumeData" => "checking",
        "allocating" => "allocating space",
        "moving" => "moving files",
        "error" => "error",
        "missingFiles" => "files missing",
        _ => return None,
    })
}

// ---- qBittorrent ---------------------------------------------------------------------

#[derive(Clone)]
pub struct QbittorrentClient {
    pub(crate) core: FileApp,
    pub(crate) name: String,
    username: String,
    password: String,
    /// `SID=…` (or `QBT_SID_<port>=…` on 5.1+), shared by clones.
    sid: Arc<Mutex<String>>,
}

impl QbittorrentClient {
    pub fn new(name: &str, url: &str, username: &str, password: &str, config_dir: &str) -> QbittorrentClient {
        QbittorrentClient {
            core: FileApp::new(name, url, config_dir, Duration::from_secs(15)),
            name: name.into(),
            username: username.into(),
            password: password.into(),
            sid: Arc::default(),
        }
    }

    fn sid(&self) -> String {
        self.sid.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_sid(&self, v: String) {
        *self.sid.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }

    /// Sign in, unless there's a session already. `path` names the request in errors.
    async fn login(&self, path: &str) -> ArrResult<()> {
        if !self.sid().is_empty() {
            return Ok(());
        }
        let base = &self.core.base;
        let r = self
            .core
            .http
            .post(format!("{base}/api/v2/auth/login"))
            .form(&[("username", &self.username), ("password", &self.password)])
            .header("Referer", base)
            .send()
            .await
            .map_err(|e| ArrError::new(format!("{}: {path}: {}", self.name, reason(&e))))?;
        let status = r.status();
        let cookies: Vec<String> =
            r.headers().get_all("set-cookie").iter().filter_map(|h| h.to_str().ok()).map(str::to_string).collect();
        let body = wranglarr_core::http::response_text(r)
            .await
            .map_err(|e| ArrError::new(format!("{}: login: {e}", self.name)))?;
        // qBittorrent < 5.1: 200 "Ok." + SID cookie; 5.1+: 204 + QBT_SID_<port> cookie
        let ok = (status == StatusCode::OK && body.trim() == "Ok.") || status == StatusCode::NO_CONTENT;
        if !ok {
            return Err(ArrError::new(format!(
                "{}: login failed ({} {}); check QBITTORRENT_USER / QBITTORRENT_PASSWORD",
                self.name,
                status.as_u16(),
                head(wranglarr_core::http::redact(body.trim().to_string(), &[&self.password]).as_str(), 40)
            )));
        }
        // keep the session cookie ourselves: cookie jars drop cookies for bare-IP hosts
        match session_cookie(&cookies) {
            Some(c) => self.set_sid(c),
            None => {
                return Err(ArrError::new(format!(
                    "{}: login answered {} but set no session cookie",
                    self.name,
                    status.as_u16()
                )));
            }
        }
        Ok(())
    }

    /// GET /api/v2/<path>, signing in (again) as needed. The body as text.
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> ArrResult<String> {
        self.send(path, || self.core.http.get(format!("{}/api/v2/{path}", self.core.base)).query(query)).await
    }

    /// POST a form to /api/v2/<path>, signing in (again) as needed.
    async fn post_form(&self, path: &str, form: &[(&str, &str)]) -> ArrResult<String> {
        self.send(path, || self.core.http.post(format!("{}/api/v2/{path}", self.core.base)).form(form)).await
    }

    /// The request `build` makes, with the session; signs in again once when the
    /// session has ended. The body as text.
    async fn send(&self, path: &str, build: impl Fn() -> reqwest::RequestBuilder) -> ArrResult<String> {
        let mut attempt = 1;
        let (status, body) = loop {
            self.login(path).await?;
            let r = build()
                .header("Cookie", self.sid())
                .header("Referer", &self.core.base)
                .send()
                .await
                .map_err(|e| ArrError::new(format!("{}: {path}: {}", self.name, reason(&e))))?;
            let status = r.status();
            if !matches!(status.as_u16(), 401 | 403) || attempt == 2 {
                let body = wranglarr_core::http::response_text(r)
                    .await
                    .map_err(|e| ArrError::new(format!("{}: {path}: {e}", self.name)))?;
                break (status, body);
            }
            attempt += 1;
            self.set_sid(String::new()); // session ended (qBittorrent restarted or timed out): sign in again
        };
        self.core.check(path, status, &body)?;
        Ok(body)
    }

    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> ArrResult<Value> {
        parse_json(&self.name, path, &self.get(path, query).await?)
    }

    pub async fn ping(&self) -> bool {
        let r = self
            .core
            .http
            .get(format!("{}/api/v2/app/version", self.core.base))
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        matches!(r, Ok(r) if matches!(r.status().as_u16(), 200 | 401 | 403))
    }

    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        poll_ready(timeout, || self.ping()).await
    }

    pub async fn status(&self) -> ArrResult<Value> {
        let version = self.get("app/version", &[]).await?.trim().to_string();
        let webapi = self.get("app/webapiVersion", &[]).await?.trim().to_string();
        Ok(status_map(json!(version), "qBittorrent", json!(webapi)))
    }

    /// The port it listens on for peers.
    pub async fn listen_port(&self) -> ArrResult<u64> {
        Ok(self.get_json("app/preferences", &[]).await?["listen_port"].as_u64().unwrap_or(0))
    }

    /// Listen on `port` for peers, with no random port and no UPnP: the port a
    /// VPN forwards, as gluetun's port-forwarding command sets it.
    pub async fn set_listen_port(&self, port: u16) -> ArrResult<()> {
        let prefs = json!({"listen_port": port, "random_port": false, "upnp": false}).to_string();
        self.post_form("app/setPreferences", &[("json", &prefs)]).await.map(|_| ())
    }

    pub async fn download_root(&self) -> ArrResult<String> {
        Ok(text(&self.get_json("app/preferences", &[]).await?["save_path"]))
    }

    pub async fn collection(&self) -> ArrResult<Value> {
        Ok(qb_collection(&rows(self.get_json("torrents/info", &[]).await?)))
    }

    /// Torrents not finished yet (seeding ones are left out).
    pub async fn downloads(&self) -> ArrResult<Vec<Download>> {
        let (mut seen, mut out) = (HashSet::new(), vec![]);
        for flt in ["downloading", "errored"] {
            let rows = match self.get_json("torrents/info", &[("filter", flt)]).await {
                Ok(v) => rows(v),
                Err(_) if flt == "errored" => continue, // older qBittorrent: no such filter
                Err(e) => return Err(e),
            };
            qb_downloads(&rows, &mut seen, &mut out);
        }
        Ok(out)
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

/// The session cookie (`SID=…` or `QBT_SID_<port>=…`) from Set-Cookie headers.
pub fn session_cookie(headers: &[String]) -> Option<String> {
    headers.iter().find_map(|h| {
        let (name, rest) = h.split_once('=').unwrap_or((h, ""));
        let name = name.trim();
        (name == "SID" || name.starts_with("QBT_SID"))
            .then(|| format!("{name}={}", rest.split_once(';').map_or(rest, |(v, _)| v)))
    })
}

pub fn qb_collection(t: &[Value]) -> Value {
    let states: Vec<String> = t.iter().map(|x| text(&x["state"])).collect();
    let count = |f: &dyn Fn(&str) -> bool| states.iter().filter(|s| f(s)).count();
    json!({
        "torrents": t.len(),
        "downloading": count(&|s| s.contains("DL") || matches!(s, "downloading" | "metaDL" | "forcedDL")),
        "seeding": count(&|s| s.contains("UP") || matches!(s, "uploading" | "forcedUP" | "stalledUP")),
        "paused": count(&|s| { let l = s.to_lowercase(); l.contains("paused") || l.contains("stopped") }),
        "size_on_disk": t.iter().map(|x| int(&x["completed"])).sum::<i64>(),
    })
}

/// Add the unfinished torrents of one `torrents/info` answer.
pub fn qb_downloads(rows: &[Value], seen: &mut HashSet<String>, out: &mut Vec<Download>) {
    for t in rows {
        let h = text(&t["hash"]);
        let st = text(&t["state"]);
        let progress = float(&t["progress"]);
        if seen.contains(&h) || (progress >= 1.0 && qb_state(&st).is_none()) {
            continue;
        }
        seen.insert(h.clone());
        let size = int(&t["size"]).max(int(&t["total_size"])).max(0); // -1 until it has metadata
        let eta = int(&t["eta"]);
        let done = int(&t["completed"]);
        out.push(
            Download {
                id: h,
                name: text(&t["name"]),
                category: text(&t["category"]),
                state: qb_state(&st).unwrap_or(if st.contains("DL") { "downloading" } else { "queued" }).into(),
                state_text: qb_text(&st).map_or_else(|| st.clone(), str::to_string),
                progress,
                size,
                done: if done != 0 { done } else { (size as f64 * progress) as i64 },
                speed: int(&t["dlspeed"]),
                eta: (eta != 0 && eta < 8_640_000).then_some(eta),
                added: t["added_on"].as_i64(),
            }
            .clamped(),
        );
    }
}

#[async_trait::async_trait]
impl crate::Service for QbittorrentClient {
    fn def(&self) -> &'static crate::ServiceDef {
        &crate::qbittorrent::DEF
    }
    fn name(&self) -> &str {
        &self.name
    }
    async fn ping(&self) -> bool {
        QbittorrentClient::ping(self).await
    }
    async fn wait_ready(&self, timeout: Duration) -> bool {
        QbittorrentClient::wait_ready(self, timeout).await
    }
    async fn status(&self) -> ArrResult<Value> {
        QbittorrentClient::status(self).await
    }
    async fn collection(&self) -> ArrResult<Value> {
        QbittorrentClient::collection(self).await
    }
    async fn scope(&self) -> Value {
        QbittorrentClient::scope(self).await
    }
    async fn health(&self) -> Vec<Value> {
        QbittorrentClient::health(self).await
    }
    async fn restart(&self) -> ArrResult<()> {
        QbittorrentClient::restart(self).await
    }
    async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        QbittorrentClient::restore_config(self, path, app_stopped).await
    }
    fn download_client(&self) -> Option<&dyn crate::DownloadClient> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl crate::DownloadClient for QbittorrentClient {
    async fn downloads(&self) -> ArrResult<Vec<Download>> {
        QbittorrentClient::downloads(self).await
    }
    async fn download_root(&self) -> ArrResult<String> {
        QbittorrentClient::download_root(self).await
    }
    async fn ensure_categories(&self, _names: &[String]) -> ArrResult<Vec<String>> {
        Err(ArrError::new(format!("{}: has no categories to create", self.name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies() {
        assert_eq!(session_cookie(&["SID=abc; HttpOnly; path=/".into()]).as_deref(), Some("SID=abc"));
        assert_eq!(
            session_cookie(&["other=1".into(), "QBT_SID_8080=xyz; SameSite=Strict".into()]).as_deref(),
            Some("QBT_SID_8080=xyz")
        );
        assert_eq!(session_cookie(&["SID=abc".into()]).as_deref(), Some("SID=abc"));
        assert_eq!(session_cookie(&["foo=bar".into()]), None);
    }

    #[test]
    fn qb_counts() {
        let t = [
            json!({"state": "downloading", "completed": 100}),
            json!({"state": "stalledUP", "completed": 5000}),
            json!({"state": "pausedDL", "completed": 0}),
        ];
        // pausedDL counts as downloading and paused
        assert_eq!(
            qb_collection(&t),
            json!({"torrents": 3, "downloading": 2, "seeding": 1, "paused": 1, "size_on_disk": 5100})
        );
    }

    #[test]
    fn qb_items() {
        let rows = [
            json!({"hash": "a", "name": "A", "category": "radarr", "state": "downloading", "progress": 0.5,
                   "size": 1000, "total_size": 1200, "completed": 0, "dlspeed": 50, "eta": 60, "added_on": 1700000000}),
            json!({"hash": "b", "state": "metaDL", "progress": 0, "size": -1, "total_size": -1, "eta": 8640000}),
            json!({"hash": "c", "state": "uploading", "progress": 1}),
            json!({"hash": "d", "state": "weirdDL", "progress": 0.2, "size": 10, "completed": 3}),
            json!({"hash": "a", "state": "downloading", "progress": 0.5}),
        ];
        let (mut seen, mut out) = (HashSet::new(), vec![]);
        qb_downloads(&rows, &mut seen, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[0],
            Download {
                id: "a".into(),
                name: "A".into(),
                category: "radarr".into(),
                state: "downloading".into(),
                state_text: "downloading".into(),
                progress: 0.5,
                size: 1200,
                done: 600,
                speed: 50,
                eta: Some(60),
                added: Some(1700000000),
            }
        );
        assert_eq!(
            (out[1].state.as_str(), out[1].state_text.as_str(), out[1].size, out[1].eta),
            ("metadata", "fetching metadata", 0, None)
        );
        assert_eq!((out[2].state.as_str(), out[2].state_text.as_str(), out[2].done), ("downloading", "weirdDL", 3));
        let v = serde_json::to_value(&out[1]).expect("json");
        assert_eq!(v["eta"], Value::Null);
        assert_eq!(v["added"], Value::Null);
    }
}
