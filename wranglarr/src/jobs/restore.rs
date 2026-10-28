// SPDX-License-Identifier: GPL-3.0-or-later
//! Restoring an app from a backup version (its databases, its config, or both),
//! and running a restore plan: several such restores in one run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use super::{fail, sha256_of};
use crate::app::App;
use crate::config::AppConfig;
use crate::error::{Error, Result};
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::restore_plan::{LOCK, RestorePlan};
use crate::service::AppConfigExt;
use crate::state::RunStatus;

/// The config to put back, relative to the version folder: the app's own backup
/// zip, checked against its checksum, or the plain copy of a file app's config
/// folder (which carries a checksum file of its own).
struct ConfigSource {
    file: String,
    sha256: Option<String>,
}

impl App {
    pub async fn restore_app(
        &self,
        suite: &str,
        version: &str,
        app_name: &str,
        databases: bool,
        config: bool,
    ) -> Result<i64> {
        self.suite_app(suite, app_name)?;
        let (s, v) = (suite.to_string(), version.to_string());
        if !self.blocking(move |app| Ok(app.store().read_manifest(&s, &v).is_some())).await? {
            return Err(Error::not_found("no such version"));
        }
        let (app, s, v, a) = (self.clone(), suite.to_string(), version.to_string(), app_name.to_string());
        self.submit(suite, "restore", &format!("{app_name}@{version}"), HashSet::new(), move |rl| async move {
            app.do_restore(&rl, &s, &v, &a, databases, config).await
        })
        .await
    }

    /// The app in a suite (404 `no app 'x' in suite`).
    pub fn suite_app(&self, suite: &str, app: &str) -> Result<AppConfig> {
        self.suite(suite)?.app(app).cloned().ok_or_else(|| Error::not_found(format!("no app {app:?} in suite")))
    }

    async fn do_restore(
        &self,
        rl: &RunLog,
        suite_name: &str,
        version: &str,
        app_name: &str,
        databases: bool,
        config: bool,
    ) -> JobResult {
        let app = self.suite_app(suite_name, app_name)?;
        let (suite_id, version_id) = (suite_name.to_string(), version.to_string());
        let manifest = self
            .blocking(move |app| {
                app.store()
                    .read_manifest(&suite_id, &version_id)
                    .ok_or_else(|| fail(format!("no such version {version_id}")))
            })
            .await?;
        let entry = manifest.apps.get(app_name).cloned().unwrap_or_default();
        let (suite_id, version_id) = (suite_name.to_string(), version.to_string());
        let vdir = self.blocking(move |app| app.store().version_dir(&suite_id, &version_id)).await?;
        rl.append(format!("restore {app_name} from {version} (databases={databases}, config={config})")).await;

        let file_app = app.is_config_dir();
        let dumps = if databases { entry.databases } else { vec![] };
        let source = if !config {
            None
        } else if file_app {
            // download clients: the plain config dir copy
            entry.config_snapshot.map(|s| ConfigSource { file: s.path, sha256: None })
        } else {
            entry.config_zip.map(|z| ConfigSource { file: z.file, sha256: Some(z.sha256) })
        };
        let has_zip = source.is_some();
        if config && !has_zip {
            return Err(fail("requested config restore is missing its backup; restore refused"));
        }
        if databases {
            let expected: HashSet<_> = app.databases.iter().map(String::as_str).collect();
            let recorded: HashSet<_> = dumps.iter().map(|d| d.name.as_str()).collect();
            if expected != recorded || recorded.len() != dumps.len() {
                return Err(fail(
                    "requested database restore does not exactly match configured databases; restore refused",
                ));
            }
        }
        if dumps.is_empty() && !has_zip {
            return Err(fail("nothing to restore: version has no matching databases/config for this app"));
        }
        // Validate every reference before stopping an app or touching a database.
        // A matching checksum does not make an absolute or escaping path safe.
        let paths: Vec<String> =
            dumps.iter().map(|d| d.file.clone()).chain(source.as_ref().map(|s| s.file.clone())).collect();
        let root = vdir.clone();
        self.blocking(move |_| {
            for path in paths {
                crate::store::checked_path(&root, &path)?;
            }
            Ok(())
        })
        .await?;
        let checks = dumps.iter().map(|d| (&d.file, Some(&d.sha256)));
        for (file, expected) in checks.chain(source.as_ref().map(|s| (&s.file, s.sha256.as_ref()))) {
            let Some(expected) = expected else { continue };
            let sum = sha256_of(vdir.join(file)).await.map_err(|e| fail(format!("{file}: {e}")))?;
            if &sum != expected {
                return Err(fail(format!("{file}: checksum mismatch, refusing to restore")));
            }
        }
        let zip_file = source.as_ref().map_or("", |s| s.file.as_str());
        if file_app && has_zip {
            let source = vdir.join(zip_file);
            self.blocking(move |_| crate::files::validate_restore_source(&source)).await?;
        }

        let c = crate::service::client(&app).await?; // before stopping: if this fails, the app is still running
        let need_stop = !dumps.is_empty() || (file_app && has_zip);
        let mut stopped = false;
        let mut restart_after = false;
        let docker = self.docker();
        if need_stop {
            let d = docker.filter(|_| !app.container.is_empty()).ok_or_else(|| fail(format!(
                "{app_name}: restoring databases or config files requires Docker control and a configured container; live restore is refused"
            )))?;
            let before = d.inspect(&app.container).await?;
            restart_after = before["State"]["Running"]
                .as_bool()
                .ok_or_else(|| fail("Docker returned no container running state"))?;
            if !restart_after && has_zip && !file_app {
                return Err(fail(format!("{app_name}: start the container before requesting an API config restore")));
            }
            if restart_after {
                d.stop(&app.container, 60)
                    .await
                    .map_err(|e| fail(format!("could not stop {}: {e}; restore refused", app.container)))?;
            }
            let after = d.inspect(&app.container).await?;
            if after["State"]["Running"].as_bool() != Some(false) {
                return Err(fail(format!("{} is not confirmed stopped; restore refused", app.container)));
            }
            stopped = true;
            rl.append(format!("confirmed container {} stopped", app.container)).await;
        }

        let restored: Result<()> = async {
            for d in &dumps {
                self.pg().restore(&d.name, &vdir.join(&d.file)).await?;
                rl.append(format!("restored database {} from {}", d.name, d.file)).await;
            }
            if file_app && has_zip {
                c.restore_config(&vdir.join(zip_file), stopped).await?;
                rl.append(format!("{app_name}: restored config files from {zip_file}")).await;
            }
            Ok(())
        }
        .await;
        if let Err(e) = restored {
            if stopped {
                rl.append(format!("restore failed: leaving {} stopped for recovery; review the error and recovery copies before starting it", app.container)).await;
            }
            return Err(e);
        }
        if stopped
            && restart_after
            && let Some(d) = docker
        {
            d.start(&app.container).await?;
            rl.append(format!("started container {}, waiting for it", app.container)).await;
            if !c.wait_ready(Duration::from_secs(300)).await {
                return Err(fail(format!("{app_name} did not come back after start")));
            }
        }

        if has_zip && !file_app {
            if !c.wait_ready(Duration::from_secs(120)).await {
                return Err(fail(format!("{app_name} API not reachable for config restore")));
            }
            let tmp = tempfile::tempdir().map_err(|e| Error::internal(format!("{app_name}: {e}")))?;
            let zip = keep_connection(&app, &vdir.join(zip_file), tmp.path(), rl).await?;
            c.restore_config(&zip, false).await?;
            rl.append(format!("{app_name}: restored config from {zip_file}; the app restarts to apply it")).await;
            tokio::time::sleep(Duration::from_secs(8)).await;
            let c2 = crate::service::client(&app).await?; // api key may have changed with the restored config
            if !c2.wait_ready(Duration::from_secs(300)).await {
                return Err(fail(format!(
                    "{app_name}: not reachable after config restore (check its API key); restore not verified"
                )));
            }
            rl.append(format!("{app_name}: back online")).await;
        }
        let names: Vec<&str> = dumps.iter().map(|d| d.name.as_str()).collect();
        Ok(JobOutcome::ok(json!({"version": version, "app": app_name, "databases": names, "config": has_zip})))
    }

    // ---- restore plan ------------------------------------------------------------
    pub async fn run_restore_plan(&self) -> Result<i64> {
        let plan = self.blocking(|app| Ok(app.restore_plan())).await?.unwrap_or_default();
        if plan.items.is_empty() {
            return Err(Error::conflict("no restore plan"));
        }
        if plan.items.iter().any(|i| i.started) {
            return Err(Error::conflict(
                "restore plan contains an interrupted item; inspect its run and application before discarding the plan and restoring remaining items individually",
            ));
        }
        let Some(first) = plan.available().next() else {
            return Err(Error::conflict("none of the planned versions exist in the store"));
        };
        let suite = first.suite.clone();
        let mut also: HashSet<String> = plan.items.iter().map(|i| i.suite.clone()).collect();
        also.insert(LOCK.into());
        let app = self.clone();
        let target = format!("{} items", plan.items.len());
        self.submit(&suite, "restore-plan", &target, also, move |rl| async move { app.plan_job(&rl, plan).await }).await
    }

    async fn save_plan_progress(&self, plan: &RestorePlan) -> Result<()> {
        let plan = plan.clone();
        self.blocking(move |app| app.write_restore_plan(&plan)).await
    }

    async fn plan_job(&self, rl: &RunLog, mut plan: RestorePlan) -> JobResult {
        let current = self.blocking(|app| Ok(app.restore_plan())).await?;
        if current.as_ref() != Some(&plan) {
            return Err(Error::conflict("restore plan changed while queued; review it before retrying"));
        }
        let (mut done, mut failed, mut unavailable) = (vec![], vec![], vec![]);
        let mut index = 0;
        while index < plan.items.len() {
            if plan.items[index].available != Some(true) {
                unavailable.push(plan.items[index].label());
                index += 1;
                continue;
            }
            // Write ahead: a crash after mutation but before completion must not
            // silently replay a destructive restore on the next run.
            plan.items[index].started = true;
            self.save_plan_progress(&plan).await?;
            let it = &plan.items[index];
            match self.do_restore(rl, &it.suite, &it.version, &it.app, it.databases, it.config).await {
                Ok(_) => {
                    done.push(it.label());
                    plan.items.remove(index);
                }
                Err(e) => {
                    rl.append(format!("{}: FAILED: {e}", it.app)).await;
                    failed.push(it.label());
                    // A failure can follow committed database/file writes. Keep
                    // the write-ahead marker and stop; do not replay uncertain
                    // work or restore dependent items automatically.
                    self.save_plan_progress(&plan).await?;
                    break;
                }
            }
            self.save_plan_progress(&plan).await?;
        }
        if plan.items.is_empty() {
            rl.append("restore plan complete; plan file removed").await;
        } else {
            rl.append("incomplete restore plan retained; completed items will not be repeated").await;
        }
        let status = RunStatus::of_steps(!failed.is_empty() || !unavailable.is_empty(), !done.is_empty());
        Ok(JobOutcome::new(status, json!({"done": done, "failed": failed, "unavailable": unavailable})))
    }
}

/// The arr backup zip to upload: a copy in `tmp` whose `config.xml` has this
/// stack's Postgres connection (read from the app's current `config.xml`), or
/// `zip` itself when that is already the same or there is no current config to
/// read it from.
async fn keep_connection(app: &AppConfig, zip: &Path, tmp: &Path, rl: &RunLog) -> Result<PathBuf> {
    let live = [Path::new(&app.config_dir).join("config.xml"), PathBuf::from(&app.api_key_file)]
        .into_iter()
        .find(|p| p.file_name().is_some_and(|f| f == "config.xml") && p.is_file());
    let Some(live) = live else {
        return Ok(zip.to_path_buf());
    };
    let text = match tokio::fs::read_to_string(&live).await {
        Ok(text) => text,
        Err(e) => {
            rl.append(format!(
                "{}: can't read {} ({e}); the restored config's database connection is used as is",
                app.name,
                live.display()
            ))
            .await;
            return Ok(zip.to_path_buf());
        }
    };
    let (name, src, out) = (app.name.clone(), zip.to_path_buf(), tmp.join("config.zip"));
    let changed = {
        let out = out.clone();
        tokio::task::spawn_blocking(move || crate::service::arr::connection::keep_connection(&name, &src, &text, &out))
            .await
            .map_err(|e| Error::internal(e.to_string()))??
    };
    if changed.is_empty() {
        return Ok(zip.to_path_buf());
    }
    rl.append(format!(
        "{}: the restored config keeps this stack's database connection ({})",
        app.name,
        changed.join(", ")
    ))
    .await;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use crate::store::sha256;

    #[tokio::test]
    async fn restore_rejects_manifest_paths_outside_version() {
        let (dir, app) = crate::app::tests::app().await;
        let outside = dir.path().join("outside.zip");
        std::fs::write(&outside, "not a backup").expect("write");
        let digest = sha256(&outside).expect("hash");
        for file in [outside.to_string_lossy().into_owned(), "../../outside.zip".into()] {
            let (version, _) = app.store().new_version("media").expect("version");
            let mut manifest: Manifest =
                serde_json::from_value(json!({"apps": {"radarr": {"config_zip": {"file": file, "sha256": digest}}}}))
                    .expect("manifest");
            app.store().write_manifest("media", &version, &mut manifest).expect("manifest");
            app.start().await;
            let run = app.restore_app("media", &version, "radarr", false, true).await.expect("submit");
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let row = app.state().get_run(run).await.expect("run").expect("row");
                    if row.finished.is_some() {
                        break row;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("complete");
            assert_eq!(result.status, "failed");
            assert_eq!(result.result["error"], "unsafe backup path");
        }
        app.shutdown().await.expect("shutdown");
        assert_eq!(std::fs::read_to_string(&outside).expect("read"), "not a backup");
    }
}
