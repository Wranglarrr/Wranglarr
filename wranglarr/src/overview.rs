// SPDX-License-Identifier: GPL-3.0-or-later
//! The dashboard overview of a suite: each app's status, collection, scope and
//! health, asked live and cached for a few minutes. It runs on the request, not
//! on the job worker.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::app::App;
use crate::config::AppConfig;
use crate::error::Result;
use crate::service::AnyClient;
use crate::time::now_iso_micros;

const OVERVIEW_TTL: Duration = Duration::from_secs(300);

/// suite -> (fetched at, overview). A run that ends drops its suites'.
#[derive(Default)]
pub struct OverviewCache(Mutex<HashMap<String, (Instant, Value)>>);

impl OverviewCache {
    /// The suite's overview, unless it is older than [`OVERVIEW_TTL`].
    pub(crate) fn fresh(&self, suite: &str) -> Option<Value> {
        let (at, v) = self.0.lock().ok()?.get(suite).cloned()?;
        (at.elapsed() < OVERVIEW_TTL).then_some(v)
    }

    pub(crate) fn put(&self, suite: &str, overview: Value) {
        if let Ok(mut c) = self.0.lock() {
            c.insert(suite.to_string(), (Instant::now(), overview));
        }
    }

    pub(crate) fn invalidate<'a>(&self, suites: impl IntoIterator<Item = &'a String>) {
        if let Ok(mut c) = self.0.lock() {
            for s in suites {
                c.remove(s);
            }
        }
    }
}

impl App {
    /// The suite's overview, from the cache unless it is older than five minutes
    /// or `refresh` is set.
    pub async fn overview(&self, suite_name: &str, refresh: bool) -> Result<Value> {
        if !refresh && let Some(v) = self.overview_cache().fresh(suite_name) {
            return Ok(v);
        }
        let suite = self.suite(suite_name)?.clone();
        let all: Vec<&AppConfig> = self.cfg().suites.iter().flat_map(|s| s.apps.iter()).collect();
        let mut apps = vec![];
        for app in &suite.apps {
            let mut dbs = vec![];
            for db in &app.databases {
                dbs.push(json!({"name": db, "size": self.pg().database_size(db).await}));
            }
            let mut info =
                json!({"name": app.name, "kind": app.kind, "url": app.url, "online": false, "databases": dbs});
            let got: std::result::Result<Value, String> = async {
                let c = crate::service::client(app).await.map_err(|e| e.to_string())?;
                let st = c.status().await.map_err(|e| e.to_string())?;
                let collection = c.collection().await.map_err(|e| e.to_string())?;
                let scope = c.scope().await;
                let mut health: Vec<Value> = c
                    .health()
                    .await
                    .into_iter()
                    .filter(|h| matches!(h["type"].as_str(), Some("warning" | "error")))
                    .collect();
                health.extend(self.vpn_bypass(app, &all, &c).await);
                health.sort_by_key(|h| h["type"] != "error"); // errors first
                Ok(json!({"online": true, "version": st["version"], "database_type": st["databaseType"],
                          "instance_name": st["instanceName"], "branch": st["branch"],
                          "collection": collection, "scope": scope, "health": health}))
            }
            .await;
            match got {
                Ok(Value::Object(o)) => {
                    if let Some(i) = info.as_object_mut() {
                        i.extend(o);
                    }
                }
                Ok(_) => {}
                Err(e) => info["error"] = json!(e),
            }
            apps.push(info);
        }
        let pg = if suite.apps.iter().any(|a| !a.databases.is_empty()) {
            json!({"version": self.pg().server_version().await})
        } else {
            Value::Null
        };
        let out = json!({"suite": suite_name, "fetched": now_iso_micros(), "apps": apps, "postgres": pg});
        self.overview_cache().put(suite_name, out.clone());
        Ok(out)
    }

    async fn vpn_bypass(&self, app: &AppConfig, all: &[&AppConfig], c: &AnyClient) -> Vec<Value> {
        let vpn = self.cfg().vpn.as_ref();
        if crate::vpn::bypass_side(vpn, app, all).is_none() {
            return vec![];
        }
        let Some(arr) = c.arr() else { return vec![] };
        match arr.get("indexer").await {
            Ok(v) => crate::vpn::bypass(vpn, app, all, v.as_array().map(Vec::as_slice).unwrap_or(&[])),
            Err(e) => {
                tracing::debug!("{}: vpn check: {e}", app.name);
                vec![]
            }
        }
    }
}
