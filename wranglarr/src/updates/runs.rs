// SPDX-License-Identifier: GPL-3.0-or-later
//! Queue and coordinate updates: pulls, backups, swaps and ordered run results.
//! Manual rollback, backup preparation and image checks live in child modules.

mod backup;
mod pull;
mod rollback;

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::recreate::DataBack;
use super::{DOCKER_OFF, SUITE, Target, Updater, short};
use crate::error::{Error, Result};
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::state::RunStatus;

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

        // Pull everything first so a registry problem doesn't leave the stack half-updated.
        let mut pulled = self.pull_targets(rl, &targets, &mut results).await?;

        // back up only what is actually about to be replaced
        let swap: Vec<Target> = targets.iter().filter(|t| pulled.contains_key(&t.container)).cloned().collect();
        let (mut bad, mut made) = (HashSet::new(), HashMap::new());
        if self.settings().await?.backup_first && !swap.is_empty() {
            (bad, made) = self.pre_backup(rl, &swap).await?;
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

        // each app's way back, checked before it's stopped: if the new version
        // fails after it ran, its data has to go back with it stopped
        for t in &swap {
            let (Some(suite), Some(app), "app") = (&t.suite, &t.app, t.role) else { continue };
            let Some(version) = made.get(suite).filter(|_| !bad.contains(&t.container)) else { continue };
            let why = match self.app.prepare_offline(suite, version, &app.name).await {
                Ok(p) => p.offline_gap().map(str::to_owned),
                Err(e) => Some(format!("its pre-update backup {version} can't be restored ({e})")),
            };
            let Some(why) = why else { continue };
            if force {
                rl.append(format!(
                    "{}: {why}; updating anyway because force was set. If the new version fails after it ran, \
                     the container is left stopped for you to restore by hand",
                    t.container
                ))
                .await;
            } else {
                results.set(&t.container, "skipped");
                pulled.remove(&t.container);
                rl.append(format!(
                    "{}: {why}, so a failed update couldn't be undone; not updating it (use force to override)",
                    t.container
                ))
                .await;
            }
        }

        let mut failed_ns: HashSet<String> = HashSet::new();
        // image id -> the container it just failed on
        let mut failed_images: HashMap<String, String> = HashMap::new();
        for t in &targets {
            let Some(image) = pulled.get(&t.container) else { continue };
            let reference = &image.reference;
            if failed_ns.contains(&t.container) {
                // couldn't follow the vpn; its own update would fail the same way
                continue;
            }
            let want = &image.id;
            let name = &t.container;
            if let Some(on) = failed_images.get(want) {
                // one outage for one bad image: the others keep running what they have
                results.set(name, "skipped");
                let reason = format!("the same image just failed on {on}");
                rl.append(format!(
                    "{name}: not updating it: {reason}. It's skipped here too; Try again to update it anyway"
                ))
                .await;
                if let Err(e) = self.block(name, want, &reason).await {
                    rl.append(format!("{name}: could not remember the rejected image: {e}")).await;
                }
                continue;
            }
            // a failure earlier in the run moves a shared tag back to the image still running
            let now_at = d.image(reference).await.ok().and_then(|i| i["Id"].as_str().map(str::to_owned));
            if now_at.as_deref() != Some(want.as_str()) {
                results.set(name, "skipped");
                rl.append(format!(
                    "{name}: {reference} no longer points at the image pulled for it ({}); not updating it",
                    short(want)
                ))
                .await;
                continue;
            }
            // if the new version fails after it ran, its data goes back from that backup
            let data = match (&t.suite, t.role) {
                (Some(suite), "app") if !bad.contains(&t.container) => made
                    .get(suite)
                    .map_or(DataBack::None, |v| DataBack::IfFailed { suite: suite.clone(), version: v.clone() }),
                _ => DataBack::None,
            };
            match self.recreate_with(rl, t, Some(reference), None, "update", &data).await {
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
                    // the image itself was tried and rejected, not a failure before the swap
                    let rejected =
                        self.blocked().await.get(&t.container).and_then(|b| b["image"].as_str().map(str::to_owned));
                    if rejected.as_deref() == Some(want.as_str()) {
                        failed_images.insert(want.clone(), t.container.clone());
                    }
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
        let mut out = json!({"results": map});
        let reports = self.run_reports(rl.run_id).await;
        if !reports.is_empty() {
            out["reports"] = Value::Object(reports);
        }
        Ok(JobOutcome::new(status, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
