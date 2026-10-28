// SPDX-License-Identifier: GPL-3.0-or-later
//! The update and rollback runs: what is queued on the job worker, the backup
//! before an update, and how each container's result becomes the run's status.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::{DOCKER_OFF, SUITE, Target, Updater, self_update_hint, short};
use crate::app::App;
use crate::docker::{DockerError, image_reference};
use crate::error::{Error, Result};
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::state::{NewUpdate, RunStatus, UpdateRow};

fn env_map(env: &Value) -> HashMap<String, String> {
    env.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|e| match e.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (e.to_string(), String::new()),
        })
        .collect()
}

/// The newest image change in `history` (newest first), unless a rollback already undid it.
fn rollback_candidate_in(history: &[UpdateRow]) -> Option<UpdateRow> {
    for u in history {
        if u.status != "ok" {
            continue;
        }
        if u.action == "rollback" {
            return None;
        }
        let old = u.old_image.as_deref().unwrap_or("");
        if (u.action == "update" || u.action == "recreate") && !old.is_empty() && u.old_image != u.new_image {
            return Some(u.clone());
        }
    }
    None
}

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

/// The run's status from the per-container results.
fn run_status(results: &[(String, String)]) -> RunStatus {
    let failed = results.iter().any(|(_, r)| r == "failed");
    let ok = results.iter().any(|(_, r)| ["updated", "recreated", "current"].contains(&r.as_str()));
    let held_back = results.iter().any(|(_, r)| r == "skipped" || r == "refused");
    RunStatus::of_steps(failed || held_back, ok)
}

/// Results in the order they were decided.
#[derive(Default)]
struct Results(Vec<(String, String)>);

impl Results {
    fn set(&mut self, k: &str, v: &str) {
        match self.0.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 = v.to_string(),
            None => self.0.push((k.to_string(), v.to_string())),
        }
    }
    fn set_default(&mut self, k: &str, v: &str) {
        if !self.has(k) {
            self.0.push((k.to_string(), v.to_string()));
        }
    }
    fn has(&self, k: &str) -> bool {
        self.0.iter().any(|(n, _)| n == k)
    }
}

impl Updater {
    /// Queue an update of `containers` (None: everything available and not held). Returns the run id.
    pub(crate) async fn update(&self, containers: Option<Vec<String>>, trigger: &str, force: bool) -> Result<i64> {
        if !self.app.docker_ok().await {
            return Err(Error::conflict(DOCKER_OFF));
        }
        let containers = match containers {
            Some(c) => c,
            None => self.available_unheld().await,
        };
        if containers.is_empty() {
            return Err(Error::conflict("nothing to update (run a check first?)"));
        }
        let targets = containers.iter().map(|n| self.target(n)).collect::<Result<Vec<_>>>()?;
        let also = self.suites_to_back_up(&targets);
        let u = self.clone();
        let trigger = trigger.to_string();
        let names = containers.clone();
        self.app
            .submit(SUITE, "update", &containers.join(", "), also.into_iter().collect(), move |rl| async move {
                u.update_job(&rl, &names, &trigger, force).await
            })
            .await
    }

    /// Queue a rollback of `container` to the image before its last update.
    pub(crate) async fn rollback(&self, container: &str) -> Result<i64> {
        self.target(container)?;
        let Some(last) = self.rollback_candidate(container).await? else {
            return Err(Error::conflict(format!("{container}: no successful update to roll back")));
        };
        let u = self.clone();
        let name = container.to_string();
        self.app
            .submit(SUITE, "rollback", container, HashSet::new(), move |rl| async move {
                u.rollback_job(&rl, &name, &last).await
            })
            .await
    }

    /// The newest image change, unless a rollback already undid it or the
    /// container already runs the image it would go back to (an update cut off
    /// before it was recorded leaves the history one change behind).
    pub(crate) async fn rollback_candidate(&self, container: &str) -> Result<Option<UpdateRow>> {
        let Some(cand) = rollback_candidate_in(&self.app.state().list_updates(Some(container), 50).await?) else {
            return Ok(None);
        };
        let current = match self.docker() {
            Ok(d) => d.inspect(container).await.ok().and_then(|c| c["Image"].as_str().map(str::to_owned)),
            Err(_) => None,
        };
        Ok((current.is_none() || current != cand.old_image).then_some(cand))
    }

    fn suites_to_back_up(&self, targets: &[Target]) -> BTreeSet<String> {
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

    /// Back up affected suites; returns the containers whose app failed to back up.
    async fn pre_backup(&self, rl: &RunLog, targets: &[Target]) -> Result<HashSet<String>> {
        let mut bad = HashSet::new();
        for suite in self.suites_to_back_up(targets) {
            rl.append(format!("pre-update backup of suite {suite}")).await;
            let res = pre_update_backup(&self.app, rl, &suite).await?;
            let version = res.result["version"].as_str().unwrap_or("").to_string();
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
        Ok(bad)
    }

    async fn image_cfg(&self, image_id: &str) -> Value {
        match self.docker() {
            Ok(d) => d.image(image_id).await.map(|i| i["Config"].clone()).unwrap_or(Value::Null),
            Err(_) => Value::Null,
        }
    }

    async fn update_job(&self, rl: &RunLog, names: &[String], trigger: &str, force: bool) -> JobResult {
        let d = self.docker()?;
        let mut targets = names.iter().map(|n| self.target(n)).collect::<Result<Vec<_>>>()?;
        let order = |r: &str| match r {
            "vpn" => 0,
            "postgres" => 1,
            "extra" => 2,
            _ => 3,
        };
        targets.sort_by_key(|t| order(t.role));
        let list: Vec<&str> = targets.iter().map(|t| t.container.as_str()).collect();
        rl.append(format!("update ({trigger}): {}", list.join(", "))).await;
        let mut results = Results::default();
        let holds = self.holds().await;
        for t in &targets {
            if let Some(why) = holds.get(&t.container).filter(|_| trigger != "manual") {
                results.set(&t.container, "held");
                rl.append(format!("{}: held ({why}), skipping", t.container)).await;
            }
        }
        targets.retain(|t| !results.has(&t.container));

        // pull everything first so a registry problem doesn't leave the stack half-updated
        let mut pulled: HashMap<String, String> = HashMap::new();
        for t in &targets {
            let name = &t.container;
            let step = async {
                let c = d.inspect(name).await?;
                let reference = self
                    .update_reference(name, image_reference(&c))
                    .await
                    .map_err(|e| DockerError::Other(e.to_string()))?;
                if self.is_self(&c) {
                    return Ok::<_, DockerError>((c, reference, None));
                }
                d.pull(&reference).await?;
                let new_id = d.image(&reference).await?["Id"].as_str().unwrap_or("").to_string();
                Ok((c, reference, Some(new_id)))
            };
            let (c, reference, new_id) = match step.await {
                Ok(v) => v,
                Err(e) => {
                    results.set(name, "failed");
                    rl.append(format!("{name}: pull failed: {e}")).await;
                    self.record(
                        rl,
                        NewUpdate {
                            container: name.clone(),
                            action: "update".into(),
                            status: "failed".into(),
                            note: format!("pull: {e}"),
                            ..Default::default()
                        },
                    )
                    .await;
                    continue;
                }
            };
            let Some(new_id) = new_id else {
                results.set(name, "skipped");
                rl.append(format!("{name}: that's wranglarr itself; {}", self_update_hint(&reference))).await;
                continue;
            };
            let old_id = c["Image"].as_str().unwrap_or("");
            if new_id == old_id && reference == image_reference(&c) {
                results.set(name, "current");
                rl.append(format!("{name}: {reference} already up to date")).await;
                continue;
            }
            if t.role == "postgres" {
                let old_major = env_map(&self.image_cfg(old_id).await["Env"]).remove("PG_MAJOR").unwrap_or_default();
                let new_major = env_map(&self.image_cfg(&new_id).await["Env"]).remove("PG_MAJOR").unwrap_or_default();
                if !old_major.is_empty() && !new_major.is_empty() && old_major != new_major {
                    results.set(name, "refused");
                    let reason =
                        format!("Postgres {old_major} -> {new_major} is a major upgrade (dump/restore, not a swap)");
                    rl.append(format!("{name}: refusing: {reason}; see the wiki")).await;
                    self.block(name, &new_id, &reason).await?;
                    self.retag(rl, &reference, old_id).await;
                    continue;
                }
            }
            rl.append(format!("{name}: pulled {reference} ({} -> {})", short(old_id), short(&new_id))).await;
            pulled.insert(name.clone(), reference);
        }

        // back up only what is actually about to be replaced
        let swap: Vec<Target> = targets.iter().filter(|t| pulled.contains_key(&t.container)).cloned().collect();
        if self.settings().await?.backup_first && !swap.is_empty() {
            let bad = self.pre_backup(rl, &swap).await?;
            for t in &swap {
                if bad.contains(&t.container) {
                    if force {
                        rl.append(format!(
                            "{}: pre-update backup incomplete, continuing because force was set",
                            t.container
                        ))
                        .await;
                    } else {
                        results.set(&t.container, "skipped");
                        pulled.remove(&t.container);
                        rl.append(format!(
                            "{}: pre-update backup incomplete, not updating it (use force to override)",
                            t.container
                        ))
                        .await;
                    }
                }
            }
        }

        let mut failed_ns: HashSet<String> = HashSet::new();
        for t in &targets {
            let Some(reference) = pulled.get(&t.container) else { continue };
            if failed_ns.contains(&t.container) {
                // couldn't follow the vpn; its own update would fail the same way
                continue;
            }
            match self.recreate(rl, t, Some(reference), None, "update").await {
                Ok(info) => {
                    if let Err(e) = self.unblock(&t.container).await {
                        tracing::warn!("updates: unblock {}: {e}", t.container);
                    }
                    results.set(&t.container, "updated");
                    for dep in info.dependents {
                        results.set_default(&dep, "recreated");
                    }
                    for (dep, _) in info.dependents_failed {
                        results.set(&dep, "failed");
                        failed_ns.insert(dep);
                    }
                }
                Err(e) => {
                    results.set(&t.container, "failed");
                    rl.append(format!("{}: update FAILED: {e}", t.container)).await;
                    if t.role == "postgres" || t.role == "vpn" {
                        rl.append("stopping here: the rest of the stack depends on it").await;
                        break;
                    }
                }
            }
        }

        self.recheck().await?;
        let status = run_status(&results.0);
        let line: Vec<String> = results.0.iter().map(|(n, r)| format!("{n}={r}")).collect();
        rl.append(format!("update finished: {}", line.join(", "))).await;
        let map: Map<String, Value> = results.0.into_iter().map(|(n, r)| (n, json!(r))).collect();
        Ok(JobOutcome::new(status, json!({"results": map})))
    }

    async fn rollback_job(&self, rl: &RunLog, container: &str, last: &UpdateRow) -> JobResult {
        let d = self.docker()?;
        let t = self.target(container)?;
        let old_image = last.old_image.clone().unwrap_or_default();
        let old_version = last.old_version.clone().unwrap_or_default();
        match d.image(&old_image).await {
            Err(DockerError::NotFound(_)) => {
                return Err(Error::conflict(format!(
                    "previous image {} is gone from the host (pruned?); cannot roll back",
                    short(&old_image)
                )));
            }
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let reference = image_reference(&d.inspect(container).await?).to_string();
        let shown = if old_version.is_empty() { "unknown version" } else { old_version.as_str() };
        rl.append(format!("{container}: rolling back to {} ({shown})", short(&old_image))).await;
        d.tag(&old_image, &reference).await?;
        rl.append(format!("tagged {} as {reference}", short(&old_image))).await;
        let info = self.recreate(rl, &t, Some(&reference), None, "rollback").await?;
        let to = if old_version.is_empty() { short(&old_image) } else { old_version.clone() };
        self.set_hold(container, true, &format!("rolled back to {to}")).await?;
        rl.append(format!("{container}: held so automatic updates leave it alone; release the hold to update again"))
            .await;
        self.recheck().await?;
        let mut out = json!({"container": container, "image": old_image});
        if info.dependents_failed.is_empty() {
            return Ok(JobOutcome::ok(out));
        }
        let bad: Map<String, Value> = info.dependents_failed.into_iter().map(|(n, e)| (n, json!(e))).collect();
        out["dependents_failed"] = Value::Object(bad);
        Ok(JobOutcome::new(RunStatus::Partial, out))
    }
}

/// The suite backup, run inline on the update's own run: the single worker can't wait
/// for a run queued behind this one. Its result holds the `version` made.
async fn pre_update_backup(app: &App, rl: &RunLog, suite: &str) -> JobResult {
    app.backup_job(rl.clone(), suite, "pre-update").await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(action: &str, status: &str, old: &str, new: &str) -> UpdateRow {
        UpdateRow {
            id: 0,
            container: "radarr".into(),
            image: "x".into(),
            old_image: Some(old.into()),
            new_image: Some(new.into()),
            old_version: Some("1".into()),
            new_version: Some("2".into()),
            action: action.into(),
            status: status.into(),
            run_id: None,
            at: String::new(),
            note: None,
        }
    }

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

    #[test]
    fn rollback_candidates() {
        let h = [row("update", "failed", "a", "b"), row("update", "ok", "a", "b"), row("update", "ok", "z", "a")];
        assert_eq!(rollback_candidate_in(&h).map(|u| u.new_image), Some(Some("b".into())));
        let h = [row("rollback", "ok", "b", "a"), row("update", "ok", "a", "b")];
        assert!(rollback_candidate_in(&h).is_none(), "already rolled back");
        let h = [row("recreate", "ok", "a", "a"), row("update", "ok", "", "b")];
        assert!(rollback_candidate_in(&h).is_none(), "no image change");
        let h = [row("recreate", "ok", "a", "c")];
        assert!(rollback_candidate_in(&h).is_some());
    }

    #[test]
    fn statuses() {
        let r = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        assert_eq!(run_status(&r(&[("a", "updated"), ("b", "current")])), RunStatus::Ok);
        assert_eq!(run_status(&r(&[("a", "updated"), ("b", "refused")])), RunStatus::Partial);
        assert_eq!(run_status(&r(&[("a", "updated"), ("b", "failed")])), RunStatus::Partial);
        assert_eq!(run_status(&r(&[("a", "failed")])), RunStatus::Failed);
        assert_eq!(run_status(&r(&[("a", "skipped")])), RunStatus::Failed);
        assert_eq!(run_status(&r(&[("a", "held")])), RunStatus::Ok);
        assert_eq!(run_status(&[]), RunStatus::Ok);
    }

    #[test]
    fn results_keep_order() {
        let mut r = Results::default();
        r.set("vpn", "updated");
        r.set_default("qb", "recreated");
        r.set_default("qb", "x");
        r.set("vpn", "failed");
        assert_eq!(r.0, vec![("vpn".into(), "failed".into()), ("qb".into(), "recreated".into())]);
    }

    #[test]
    fn env_maps() {
        let m = env_map(&json!(["PG_MAJOR=17", "FLAG", "A=b=c"]));
        assert_eq!(m.get("PG_MAJOR").map(String::as_str), Some("17"));
        assert_eq!(m.get("FLAG").map(String::as_str), Some(""));
        assert_eq!(m.get("A").map(String::as_str), Some("b=c"));
    }
}
