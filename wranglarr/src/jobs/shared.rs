// SPDX-License-Identifier: GPL-3.0-or-later
//! The shared-settings sync: the `[[shared]]` resources from wranglarr.toml
//! (download clients, indexers, …) created or updated in each app they apply to.

use std::collections::HashSet;

use serde_json::json;

use crate::app::App;
use crate::error::{Error, Result};
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::state::RunStatus;
use crate::value::text;

impl App {
    pub async fn sync_shared(&self, suite: &str, app_name: Option<&str>) -> Result<i64> {
        let s = self.suite(suite)?;
        let names: Vec<String> = match app_name {
            Some(a) => vec![s.app(a).ok_or_else(|| Error::not_found("no such app"))?.name.clone()],
            None => s.apps.iter().map(|a| a.name.clone()).collect(),
        };
        let (app, sname) = (self.clone(), suite.to_string());
        self.submit(suite, "sync", app_name.unwrap_or("all"), HashSet::new(), move |rl| async move {
            app.sync_job(&rl, &names, &sname).await
        })
        .await
    }

    async fn sync_job(&self, rl: &RunLog, names: &[String], suite_name: &str) -> JobResult {
        let suite = self.suite(suite_name)?.clone();
        let mut results = vec![];
        let mut failures = 0;
        for name in names {
            let Some(app) = suite.app(name) else { continue };
            let items: Vec<_> = self.cfg().shared.iter().filter(|r| r.applies_to(&app.name, &app.kind)).collect();
            if items.is_empty() {
                rl.append(format!("{}: no shared resources apply", app.name)).await;
                continue;
            }
            let c = match crate::service::client(app).await {
                Ok(c) => c,
                Err(e) => {
                    rl.append(format!("{}: {e}", app.name)).await;
                    failures += 1;
                    continue;
                }
            };
            for r in items {
                let body = crate::service::arr::shared::body(r, &app.kind);
                let res = async {
                    if r.test {
                        c.test_resource(&r.resource, &body).await?;
                    }
                    c.upsert_resource(&r.resource, &body).await
                }
                .await;
                match res {
                    Ok((action, saved)) => {
                        rl.append(format!(
                            "{}: {action} {} {:?} (id {})",
                            app.name,
                            r.label(),
                            r.name,
                            text(&saved["id"])
                        ))
                        .await;
                        results
                            .push(json!({"app": app.name, "resource": r.resource, "name": r.name, "action": action}));
                    }
                    Err(e) => {
                        rl.append(format!("{}: {} '{}' FAILED: {e}", app.name, r.label(), r.name)).await;
                        failures += 1;
                        results.push(json!({"app": app.name, "resource": r.resource, "name": r.name,
                                            "action": "failed", "error": e.to_string()}));
                    }
                }
            }
        }
        let status = RunStatus::of_steps(failures > 0, !results.is_empty());
        Ok(JobOutcome::new(status, json!({"results": results})))
    }
}
