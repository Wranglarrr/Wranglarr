// SPDX-License-Identifier: GPL-3.0-or-later
//! The updater's settings (wranglarr.toml's `[updates]`, overridden from the
//! page), the holds and blocks that keep containers back, and the check and
//! auto-update schedules.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::Updater;
use super::check::repo_digests;
use crate::error::{Error, Result};
use crate::time::now_iso_micros;
use crate::value::truthy;

const CHECK_JOB: &str = "updates:check";
const AUTO_JOB: &str = "updates:auto";
const SELF_JOB: &str = "updates:self";
/// The scheduled jobs on a set minute start up to this many seconds after it.
const JITTER_SECS: u64 = 90;
/// kv: container -> why it is held
const HOLDS: &str = "updates:holds";
/// kv: container -> the image it must not be updated to
const BLOCKED: &str = "updates:blocked";

/// Update settings: wranglarr.toml's `[updates]`, overridden by what was saved on the page.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UpdateSettings {
    pub enabled: bool,
    pub check_schedule: String,
    pub auto_update: bool,
    pub auto_schedule: String,
    pub backup_first: bool,
    /// Update wranglarr itself on `self_schedule`.
    pub self_update: bool,
    pub self_schedule: String,
    /// "config" or "ui"
    pub source: &'static str,
}

impl UpdateSettings {
    /// `defaults` with each key of the saved object `ui` that has the right type.
    pub fn merged(mut self, ui: Option<&Value>) -> UpdateSettings {
        let Some(o) = ui.and_then(Value::as_object).filter(|o| !o.is_empty()) else {
            self.source = "config";
            return self;
        };
        let b = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        let s = |k: &str, d: &str| o.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        UpdateSettings {
            enabled: b("enabled", self.enabled),
            check_schedule: s("check_schedule", &self.check_schedule),
            auto_update: b("auto_update", self.auto_update),
            auto_schedule: s("auto_schedule", &self.auto_schedule),
            backup_first: b("backup_first", self.backup_first),
            self_update: b("self_update", self.self_update),
            self_schedule: s("self_schedule", &self.self_schedule),
            source: "ui",
        }
    }
}

impl Updater {
    pub(crate) async fn settings(&self) -> Result<UpdateSettings> {
        let c = &self.app.cfg().updates;
        let d = UpdateSettings {
            enabled: c.enabled,
            check_schedule: c.check_schedule.clone(),
            auto_update: c.auto_update,
            auto_schedule: c.auto_schedule.clone(),
            backup_first: c.backup_first,
            self_update: c.self_update,
            self_schedule: c.self_schedule.clone(),
            source: "config",
        };
        Ok(d.merged(self.app.state().get_value("updates:settings").await?.as_ref()))
    }

    /// Save `s` as the page's settings (its `source` is ignored), then reschedule.
    pub(crate) async fn save_settings(&self, s: &UpdateSettings) -> Result<UpdateSettings> {
        for expr in [&s.check_schedule, &s.auto_schedule, &s.self_schedule] {
            if !expr.trim().is_empty() {
                crate::sched::parse(expr).map_err(|e| Error::invalid(format!("invalid schedule: {e}")))?;
            }
        }
        self.app
            .state()
            .put(
                "updates:settings",
                &json!({"enabled": s.enabled, "check_schedule": s.check_schedule.trim(), "auto_update": s.auto_update,
                    "auto_schedule": s.auto_schedule.trim(), "backup_first": s.backup_first,
                    "self_update": s.self_update, "self_schedule": s.self_schedule.trim()}),
            )
            .await?;
        self.reschedule().await;
        self.settings().await
    }

    pub(crate) async fn reset_settings(&self) -> Result<UpdateSettings> {
        self.app.state().delete("updates:settings").await?;
        self.reschedule().await;
        self.settings().await
    }

    /// container -> why it is held
    pub(crate) async fn holds(&self) -> HashMap<String, String> {
        self.app.state().get(HOLDS).await.ok().flatten().unwrap_or_default()
    }

    pub(crate) async fn set_hold(&self, container: &str, hold: bool, reason: &str) -> Result<()> {
        self.target(container)?;
        let (container, reason) = (container.to_owned(), reason.to_owned());
        self.app
            .state()
            .write(move |tx| {
                let mut h: HashMap<String, String> = tx.get(HOLDS)?.unwrap_or_default();
                if hold {
                    h.insert(container, reason);
                } else {
                    h.remove(&container);
                }
                tx.put(HOLDS, &h)
            })
            .await
    }

    /// container -> {digests, image, reason, at}
    pub(crate) async fn blocked(&self) -> Map<String, Value> {
        self.app.state().get(BLOCKED).await.ok().flatten().unwrap_or_default()
    }

    /// Remember that this registry digest must not be applied automatically.
    pub(crate) async fn block(&self, container: &str, image_id: &str, reason: &str) -> Result<()> {
        let digests: Vec<String> = match self.docker()?.image(image_id).await {
            Ok(img) => repo_digests(&img).into_iter().collect(),
            Err(_) => vec![],
        };
        self.put_block(container, image_id, digests, reason).await
    }

    /// Remember the image `image_id` (its registry `digests`) as rejected for `container`.
    pub(crate) async fn put_block(
        &self,
        container: &str,
        image_id: &str,
        digests: Vec<String>,
        reason: &str,
    ) -> Result<()> {
        let container = container.to_owned();
        let entry = json!({"digests": digests, "image": image_id, "reason": reason, "at": now_iso_micros()});
        self.app
            .state()
            .write(move |tx| {
                let mut b: Map<String, Value> = tx.get(BLOCKED)?.unwrap_or_default();
                b.insert(container, entry);
                tx.put(BLOCKED, &b)
            })
            .await
    }

    pub(crate) async fn unblock(&self, container: &str) -> Result<()> {
        let container = container.to_owned();
        self.app
            .state()
            .write(move |tx| {
                let mut b: Map<String, Value> = tx.get(BLOCKED)?.unwrap_or_default();
                if b.remove(&container).is_some() {
                    tx.put(BLOCKED, &b)?;
                }
                Ok(())
            })
            .await
    }

    // ---- scheduling ----------------------------------------------------------
    pub(crate) async fn reschedule(&self) {
        let sched = &self.app.sched();
        sched.remove(CHECK_JOB);
        sched.remove(AUTO_JOB);
        sched.remove(SELF_JOB);
        let st = match self.settings().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("updates: settings: {e}");
                return;
            }
        };
        if !st.enabled || self.app.docker().is_none() {
            return;
        }
        let jobs = [
            (CHECK_JOB, &st.check_schedule, true),
            (AUTO_JOB, &st.auto_schedule, st.auto_update),
            (SELF_JOB, &st.self_schedule, st.self_update),
        ];
        for (id, expr, on) in jobs {
            if !on || expr.is_empty() {
                continue;
            }
            let app = self.app.clone();
            // on a set minute (an hourly or daily cron): everyone else's checker runs then too
            let jitter = expr.split_whitespace().next().is_some_and(|m| m.parse::<u32>().is_ok());
            let added = sched.add(id, expr, move || {
                let u = app.updates();
                Box::pin(async move {
                    // start a little after, so the registries aren't asked in the same
                    // second as every other checker
                    if jitter {
                        tokio::time::sleep(std::time::Duration::from_secs(rand::random_range(5..JITTER_SECS))).await;
                    }
                    match id {
                        CHECK_JOB => {
                            if let Err(e) = u.check_all().await {
                                tracing::warn!("scheduled update check: {e}");
                            }
                        }
                        SELF_JOB => u.self_auto().await,
                        _ => u.auto().await,
                    }
                })
            });
            if let Err(e) = added {
                tracing::error!("updates: bad schedule {expr:?}: {e}");
            }
        }
    }

    /// {"check": iso | null, "auto": iso | null, "self": iso | null}
    pub(crate) fn next_runs(&self) -> Value {
        let n = |id: &str| self.app.sched().next_run(id).map(|t| crate::time::iso(&t));
        json!({"check": n(CHECK_JOB), "auto": n(AUTO_JOB), "self": n(SELF_JOB)})
    }

    async fn auto(&self) {
        if let Err(e) = self.check_all().await {
            tracing::warn!("auto-update: check failed: {e}");
            return;
        }
        let names = self.available_unheld().await;
        if names.is_empty() {
            tracing::info!("auto-update: nothing to do");
            return;
        }
        if let Err(e) = self.update(Some(names), "scheduled", false).await {
            tracing::warn!("auto-update skipped: {e}");
        }
    }

    pub(super) async fn available_unheld(&self) -> Vec<String> {
        self.last_check()
            .await
            .into_iter()
            .filter(|(_, s)| s["status"] == "available" && !truthy(&s["held"]))
            .map(|(n, _)| n)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_merge() {
        let d = UpdateSettings {
            enabled: true,
            check_schedule: "0 */6 * * *".into(),
            auto_update: false,
            auto_schedule: "0 5 * * *".into(),
            backup_first: true,
            self_update: false,
            self_schedule: "30 5 * * *".into(),
            source: "config",
        };
        assert_eq!(d.clone().merged(None), d);
        assert_eq!(d.clone().merged(Some(&json!({}))), d);
        let m = d.clone().merged(Some(&json!({"auto_update": true, "check_schedule": "", "enabled": "junk"})));
        assert!(m.auto_update && m.enabled && m.backup_first);
        assert_eq!(m.check_schedule, "");
        assert_eq!(m.auto_schedule, "0 5 * * *");
        assert_eq!(m.source, "ui");
        assert!(!m.self_update, "wranglarr's own update stays off unless turned on");
        let on = d.clone().merged(Some(&json!({"self_update": true, "self_schedule": "0 4 * * 0"})));
        assert!(on.self_update);
        assert_eq!(on.self_schedule, "0 4 * * 0");
        let junk = d.merged(Some(&json!({"self_update": "yes", "self_schedule": 5})));
        assert_eq!((junk.self_update, junk.self_schedule.as_str()), (false, "30 5 * * *"));
    }
}
