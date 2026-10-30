// SPDX-License-Identifier: GPL-3.0-or-later
//! Inline pre-update backups and completeness checks for affected suites.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::super::{Target, Updater};
use crate::app::App;
use crate::error::Result;
use crate::jobs::{JobResult, RunLog};
use crate::state::RunStatus;

/// Postgres swaps require a fully successful backup of every affected suite.
fn postgres_backup_incomplete(
    status: RunStatus,
    suite: &crate::config::SuiteConfig,
    manifest: &crate::manifest::Manifest,
) -> bool {
    if status != RunStatus::Ok || manifest.globals.as_ref().is_none_or(|g| g.file.is_empty()) {
        return true;
    }
    suite.apps.iter().filter(|a| !a.databases.is_empty()).any(|a| {
        manifest.apps.get(&a.name).is_none_or(|entry| {
            a.databases.iter().any(|name| !entry.databases.iter().any(|d| d.name == *name && !d.file.is_empty()))
        })
    })
}

impl Updater {
    pub(super) fn suites_to_back_up(&self, targets: &[Target]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for t in targets {
            if t.role == "app" {
                if let Some(s) = &t.suite {
                    out.insert(s.clone());
                }
            } else if t.role == "postgres" {
                for s in &self.app.cfg().suites {
                    if s.apps.iter().any(|a| !a.databases.is_empty()) {
                        out.insert(s.name.clone());
                    }
                }
            }
        }
        out
    }

    /// Back up affected suites; returns the containers whose app failed to back
    /// up, and the version made for each suite.
    pub(super) async fn pre_backup(
        &self,
        rl: &RunLog,
        targets: &[Target],
    ) -> Result<(HashSet<String>, HashMap<String, String>)> {
        let (mut bad, mut made) = (HashSet::new(), HashMap::new());
        for suite in self.suites_to_back_up(targets) {
            rl.append(format!("pre-update backup of suite {suite}")).await;
            let res = pre_update_backup(&self.app, rl, &suite).await?;
            let version = res.result["version"].as_str().unwrap_or("").to_string();
            if !version.is_empty() {
                made.insert(suite.clone(), version.clone());
            }
            let (s, v) = (suite.clone(), version);
            let m = self.app.blocking(move |app| Ok(app.store().read_manifest(&s, &v))).await?.unwrap_or_default();
            for t in targets {
                if t.role != "app" || t.suite.as_deref() != Some(suite.as_str()) {
                    continue;
                }
                let a = t.app.as_ref().and_then(|ap| m.apps.get(&ap.name));
                if a.is_none_or(|a| !a.ok()) {
                    bad.insert(t.container.clone());
                }
            }
            if postgres_backup_incomplete(res.status, self.app.suite(&suite)?, &m) {
                bad.extend(targets.iter().filter(|t| t.role == "postgres").map(|t| t.container.clone()));
            }
        }
        Ok((bad, made))
    }
}

/// The suite backup, run inline on the update's own run: the single worker can't wait
/// for a run queued behind this one. Its result holds the `version` made.
async fn pre_update_backup(app: &App, rl: &RunLog, suite: &str) -> JobResult {
    app.backup_job(rl.clone(), suite, wranglarr_core::manifest::PRE_UPDATE).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_update_requires_a_complete_backup() {
        let suite = crate::config::SuiteConfig {
            name: "suite".into(),
            apps: vec![crate::config::AppConfig {
                name: "radarr".into(),
                databases: vec!["radarr".into()],
                ..Default::default()
            }],
            schedule: String::new(),
            enabled: true,
            keep_last: 0,
            keep_days: 0,
        };
        let mut manifest = crate::manifest::Manifest::default();
        assert!(postgres_backup_incomplete(RunStatus::Ok, &suite, &manifest));
        manifest.globals = Some(crate::manifest::Globals { file: "globals.sql".into(), ..Default::default() });
        let mut entry = crate::manifest::AppBackup::new("radarr", "radarr", None);
        entry.databases.push(crate::manifest::DatabaseDump {
            name: "radarr".into(),
            file: "radarr/radarr.dump".into(),
            ..Default::default()
        });
        manifest.apps.insert("radarr".into(), entry);
        assert!(!postgres_backup_incomplete(RunStatus::Ok, &suite, &manifest));
        for status in [RunStatus::Partial, RunStatus::Failed, RunStatus::Running, RunStatus::Queued] {
            assert!(postgres_backup_incomplete(status, &suite, &manifest));
        }
    }
}
