// SPDX-License-Identifier: GPL-3.0-or-later
//! Current downloads across qBittorrent / SABnzbd / NZBGet, for the Home page.
//!
//! Each download is tagged with the arr that sent it, from its category:
//! Connect apps gives every arr a category named after it (radarr, sonarr, ...).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::app::App;
use crate::config::{AppConfig, Config};
use crate::service;
use crate::service::AnyClient;
use crate::service::AppConfigExt;
use crate::service::Roles;

use crate::error::Result;

/// Several open tabs share one round of requests.
pub const TTL: Duration = Duration::from_secs(4);

/// Common hand-made categories -> the kind of arr that uses them.
fn alias(category: &str) -> Option<&'static str> {
    crate::service::with(Roles::LIBRARY)
        .find(|d| d.arr.is_some_and(|a| a.category_aliases.contains(&category)))
        .map(|d| d.id)
}

/// Active first, then waiting, then stopped.
fn order(state: &str) -> u8 {
    match state {
        "downloading" => 0,
        "processing" => 1,
        "checking" => 2,
        "metadata" => 3,
        "stalled" => 4,
        "error" => 5,
        "queued" => 6,
        "paused" => 7,
        _ => 9,
    }
}

/// The arr app a category belongs to: its name, else its kind, else a known
/// alias, else one whose name or kind is part of it (tv-sonarr, radarr4k, ...).
pub fn arr_for(cfg: &Config, category: &str) -> Option<Value> {
    let c = category.trim().to_lowercase();
    if c.is_empty() {
        return None;
    }
    let arrs: Vec<&AppConfig> = cfg.all_apps().filter(|a| a.has(Roles::ARR)).collect();
    let tests: [&dyn Fn(&AppConfig) -> bool; 4] =
        [&|a| a.name.to_lowercase() == c, &|a| a.kind == c, &|a| alias(&c) == Some(a.kind.as_str()), &|a| {
            c.contains(&a.name.to_lowercase()) || c.contains(&a.kind)
        }];
    tests.iter().find_map(|t| arrs.iter().find(|a| t(a))).map(|a| json!({"name": a.name, "kind": a.kind}))
}

/// Kept between polls: qBittorrent logs in once, not every time.
#[derive(Default)]
pub struct DownloadsState {
    clients: Mutex<HashMap<String, AnyClient>>,
    cache: Mutex<Option<(Instant, Value)>>,
}

/// The merged download queue, as a handle on the service.
#[derive(Clone)]
pub struct Downloads {
    app: App,
}

impl App {
    pub fn downloads(&self) -> Downloads {
        Downloads { app: self.clone() }
    }
}

impl Downloads {
    pub fn apps(&self) -> Vec<AppConfig> {
        self.app.cfg().all_apps().filter(|a| a.has(Roles::DOWNLOADS) && a.enabled).cloned().collect()
    }

    fn st(&self) -> &DownloadsState {
        self.app.downloads_state()
    }

    async fn client(&self, app: &AppConfig) -> Result<AnyClient> {
        if let Some(c) = self.st().clients.lock().ok().and_then(|m| m.get(&app.name).cloned()) {
            return Ok(c);
        }
        let c = service::client(app).await?;
        if let Ok(mut m) = self.st().clients.lock() {
            m.insert(app.name.clone(), c.clone());
        }
        Ok(c)
    }

    fn drop_client(&self, name: &str) {
        if let Ok(mut m) = self.st().clients.lock() {
            m.remove(name);
        }
    }

    async fn one(&self, app: &AppConfig) -> (Value, Vec<Value>) {
        let got = match self.client(app).await {
            Ok(c) => c.downloads().await.map_err(crate::error::Error::from),
            Err(e) => Err(e),
        };
        let mut info = json!({"name": app.name, "kind": app.kind, "ok": false, "error": "", "count": 0, "speed": 0});
        let items = match got {
            Ok(items) => items,
            Err(e) => {
                self.drop_client(&app.name); // a stale session: log in again next time
                let msg = e.to_string();
                let msg = msg.strip_prefix(&format!("{}: ", app.name)).unwrap_or(&msg);
                info["error"] = json!(msg.chars().take(200).collect::<String>());
                return (info, vec![]);
            }
        };
        let speed: i64 = items.iter().map(|d| d.speed).sum();
        let out: Vec<Value> = items
            .into_iter()
            .map(|d| {
                let arr = arr_for(self.app.cfg(), &d.category);
                let mut v = serde_json::to_value(&d).unwrap_or_else(|_| json!({}));
                if let Some(o) = v.as_object_mut() {
                    o.insert("client".into(), json!(app.name));
                    o.insert("client_kind".into(), json!(app.kind));
                    o.insert("arr".into(), arr.unwrap_or(Value::Null));
                }
                v
            })
            .collect();
        info["ok"] = json!(true);
        info["count"] = json!(out.len());
        info["speed"] = json!(speed);
        (info, out)
    }

    /// Every client's downloads, merged; cached for a few seconds unless `refresh`.
    pub async fn list(&self, refresh: bool) -> Value {
        if !refresh
            && let Some((at, v)) = self.st().cache.lock().ok().and_then(|c| c.clone())
            && at.elapsed() < TTL
        {
            return v;
        }
        let apps = self.apps();
        let got = futures::future::join_all(apps.iter().map(|a| self.one(a))).await;
        let mut clients = vec![];
        let mut items = vec![];
        for (info, its) in got {
            clients.push(info);
            items.extend(its);
        }
        let f = |v: &Value, k: &str| v[k].as_f64().unwrap_or(0.0);
        items.sort_by(|a, b| {
            order(a["state"].as_str().unwrap_or(""))
                .cmp(&order(b["state"].as_str().unwrap_or("")))
                .then(f(b, "speed").total_cmp(&f(a, "speed")))
                .then(f(b, "progress").total_cmp(&f(a, "progress")))
        });
        let speed: i64 = clients.iter().map(|c| c["speed"].as_i64().unwrap_or(0)).sum();
        let out = json!({
            "at": crate::time::now_iso_micros(),
            "clients": clients, "items": items, "speed": speed,
        });
        if let Ok(mut c) = self.st().cache.lock() {
            *c = Some((Instant::now(), out.clone()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_to_arrs() {
        let cfg = crate::service::apps::parse(
            r#"
[[suite]]
name = "m"
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "http://radarr:7878"
api_key = "k"
[[suite.app]]
name = "Sonarr4K"
kind = "sonarr"
url = "http://sonarr4k:8989"
api_key = "k"
"#,
            |_| None,
        )
        .expect("config");
        let hit = |c: &str| arr_for(&cfg, c).map(|v| v["name"].as_str().unwrap_or("").to_string());
        assert_eq!(hit("radarr").as_deref(), Some("radarr"));
        assert_eq!(hit(" sonarr4k ").as_deref(), Some("Sonarr4K"));
        assert_eq!(hit("sonarr").as_deref(), Some("Sonarr4K"), "by kind");
        assert_eq!(hit("Movies").as_deref(), Some("radarr"), "an alias");
        assert_eq!(hit("tv").as_deref(), Some("Sonarr4K"));
        assert_eq!(hit("radarr-uhd").as_deref(), Some("radarr"), "part of it");
        assert_eq!(hit("music"), None, "no lidarr");
        assert_eq!(hit(""), None);
        assert_eq!(arr_for(&cfg, "radarr"), Some(json!({"name": "radarr", "kind": "radarr"})));
    }

    #[test]
    fn active_first() {
        assert!(order("downloading") < order("queued") && order("paused") < order("whatever"));
    }
}
