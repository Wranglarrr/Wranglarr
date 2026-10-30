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

/// The API restore's observed phase, separate from container/data recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApiRestoreOutcome {
    #[default]
    NotAttempted,
    UnavailableBeforeUpload,
    /// A request returned an HTTP error and the app still answers. The
    /// multi-request restore may have made changes; this is not a no-write claim.
    RequestFailed,
    UploadOutcomeUnknown,
    RestartNotReady,
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiAvailability {
    /// No request changed the app and no new availability failure was observed.
    Unchanged,
    Ready,
    Unavailable,
    Unknown,
}

#[derive(Debug)]
pub(crate) struct ApiRestoreFailed {
    pub(crate) error: Error,
    pub(crate) outcome: ApiRestoreOutcome,
    pub(crate) availability: ApiAvailability,
}

/// A restore that passed its checks ([`App::prepare_restore`]): what goes back
/// for one app, and from where.
pub(crate) struct Prepared {
    pub(crate) app: AppConfig,
    pub(crate) version: String,
    vdir: PathBuf,
    dumps: Vec<crate::manifest::DatabaseDump>,
    source: Option<ConfigSource>,
    file_app: bool,
    /// An SQLite arr's database, put back offline from the plain copy of its
    /// config folder (see [`App::prepare_offline`]): (that copy, the live folder).
    sqlite: Option<(PathBuf, PathBuf)>,
    /// Why its database can only go back through its running API, if so.
    offline_gap: Option<String>,
}

impl Prepared {
    /// Its databases or config files go back with the container stopped.
    pub(crate) fn needs_stop(&self) -> bool {
        !self.dumps.is_empty() || (self.file_app && self.source.is_some()) || self.sqlite.is_some()
    }
    /// Its config goes back through its API, with the app running.
    pub(crate) fn api_config(&self) -> bool {
        self.source.is_some() && !self.file_app && self.sqlite.is_none()
    }
    /// Why its data can't all go back with the app stopped: an SQLite arr's
    /// database comes back only through its API, and the old version may not
    /// start on a database the new one migrated. None when it can.
    pub(crate) fn offline_gap(&self) -> Option<&str> {
        self.offline_gap.as_deref()
    }
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
        let p = self.prepare_restore(suite_name, version, app_name, databases, config).await?;
        rl.append(format!("restore {app_name} from {version} (databases={databases}, config={config})")).await;
        let app = &p.app;
        let c = crate::service::client(app).await?; // before stopping: if this fails, the app is still running
        let mut stopped = false;
        let mut restart_after = false;
        let docker = self.docker();
        if p.needs_stop() {
            let d = docker.filter(|_| !app.container.is_empty()).ok_or_else(|| fail(format!(
                "{app_name}: restoring databases or config files requires Docker control and a configured container; live restore is refused"
            )))?;
            let before = d.inspect(&app.container).await?;
            restart_after = before["State"]["Running"]
                .as_bool()
                .ok_or_else(|| fail("Docker returned no container running state"))?;
            if !restart_after && p.api_config() {
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

        if let Err(e) = self.restore_stopped(rl, &p, &c, &mut vec![]).await {
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

        if p.api_config() {
            self.restore_api_config(rl, &p).await?;
        }
        let names: Vec<&str> = p.dumps.iter().map(|d| d.name.as_str()).collect();
        Ok(JobOutcome::ok(
            json!({"version": version, "app": app_name, "databases": names, "config": p.source.is_some()}),
        ))
    }

    /// Check a restore before anything is stopped: the version and the app's part
    /// in it, that its databases are exactly the configured ones, every path inside
    /// the version, every checksum.
    pub(crate) async fn prepare_restore(
        &self,
        suite_name: &str,
        version: &str,
        app_name: &str,
        databases: bool,
        config: bool,
    ) -> Result<Prepared> {
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
        if config && source.is_none() {
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
        if dumps.is_empty() && source.is_none() {
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
        if file_app && let Some(src) = &source {
            let source = vdir.join(&src.file);
            self.blocking(move |_| crate::files::validate_restore_source(&source)).await?;
        }
        Ok(Prepared {
            app,
            version: version.to_string(),
            vdir,
            dumps,
            source,
            file_app,
            sqlite: None,
            offline_gap: None,
        })
    }

    /// [`prepare_restore`](Self::prepare_restore) of everything, for putting an
    /// app's data back around an update or a rollback, where its old version
    /// starts on what's restored. An SQLite arr's database goes back offline,
    /// from the plain copy of its config folder (which holds the consistent copy
    /// from its own backup), rather than through its API after it starts.
    pub(crate) async fn prepare_offline(&self, suite: &str, version: &str, app_name: &str) -> Result<Prepared> {
        let mut p = self.prepare_restore(suite, version, app_name, true, true).await?;
        if p.file_app || !p.app.databases.is_empty() || p.source.is_none() {
            return Ok(p); // Postgres dumps and config folders go back offline already
        }
        let (suite_id, version_id) = (suite.to_string(), version.to_string());
        let manifest = self.blocking(move |app| Ok(app.store().read_manifest(&suite_id, &version_id))).await?;
        let snap = manifest.and_then(|m| m.apps.get(app_name).and_then(|e| e.config_snapshot.clone()));
        let live = p.app.snapshot_source();
        let gap = match (&snap, &live) {
            (None, _) => Some("its backup has no plain copy of its config folder".to_string()),
            (_, None) => Some("its config folder isn't mounted into wranglarr".to_string()),
            (Some(s), Some(live)) => {
                let src = crate::store::checked_path(&p.vdir, &s.path)?;
                let (src2, live2) = (src.clone(), live.clone());
                let blocker = self
                    .blocking(move |_| {
                        crate::files::validate_restore_source(&src2)?;
                        Ok(crate::files::sqlite_restore_blocker(&src2, &live2))
                    })
                    .await?;
                if blocker.is_none() {
                    p.sqlite = Some((src, live.clone()));
                }
                blocker
            }
        };
        p.offline_gap = gap.map(|g| format!("{}'s SQLite database can't be put back with it stopped: {g}", p.app.name));
        Ok(p)
    }

    /// The part of a restore that needs the app's container stopped: its
    /// databases, and the copy of a file app's config folder. `c` is its client,
    /// made before it was stopped. What went back is added to `done` as it
    /// goes, so after an error it says what's restored and what isn't.
    pub(crate) async fn restore_stopped(
        &self,
        rl: &RunLog,
        p: &Prepared,
        c: &crate::service::AnyClient,
        done: &mut Vec<String>,
    ) -> Result<()> {
        for d in &p.dumps {
            self.pg().restore(&d.name, &p.vdir.join(&d.file)).await?;
            done.push(format!("database {}", d.name));
            rl.append(format!("restored database {} from {}", d.name, d.file)).await;
        }
        if p.file_app
            && let Some(src) = &p.source
        {
            c.restore_config(&p.vdir.join(&src.file), true).await?;
            done.push("config files".into());
            rl.append(format!("{}: restored config files from {}", p.app.name, src.file)).await;
        }
        if let Some((snap, live)) = &p.sqlite {
            let (name, snap, live) = (p.app.name.clone(), snap.clone(), live.clone());
            let (r, dbs) = tokio::task::spawn_blocking(move || {
                let mut dbs = vec![];
                let r = crate::files::restore_sqlite(&name, &snap, &live, &mut dbs);
                (r, dbs)
            })
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
            done.extend(dbs.iter().map(|d| format!("SQLite database {d}")));
            let keep = r?;
            rl.append(format!(
                "{}: restored {} offline (the replaced files are in {})",
                p.app.name,
                dbs.join(", "),
                keep.display()
            ))
            .await;
        }
        Ok(())
    }

    /// The part of a restore that needs the app running: its own backup zip,
    /// through its API. The app restarts to apply it.
    pub(crate) async fn restore_api_config(&self, rl: &RunLog, p: &Prepared) -> Result<()> {
        self.restore_api_config_within(rl, p, Duration::from_secs(300)).await.map(|_| ()).map_err(|f| f.error)
    }

    /// Restore and verify the app's own backup, preserving the phase on failure.
    pub(crate) async fn restore_api_config_within(
        &self,
        rl: &RunLog,
        p: &Prepared,
        back_within: Duration,
    ) -> std::result::Result<ApiRestoreOutcome, ApiRestoreFailed> {
        use ApiRestoreOutcome::*;
        let failure = |outcome| {
            move |error| ApiRestoreFailed {
                error,
                outcome,
                availability: match outcome {
                    NotAttempted => ApiAvailability::Unchanged,
                    UnavailableBeforeUpload | RestartNotReady => ApiAvailability::Unavailable,
                    RequestFailed => ApiAvailability::Ready,
                    _ => ApiAvailability::Unknown,
                },
            }
        };
        let (app, app_name) = (&p.app, p.app.name.as_str());
        let Some(src) = p.source.as_ref().filter(|_| !p.file_app) else { return Ok(NotAttempted) };
        let c = crate::service::client(app).await.map_err(failure(NotAttempted))?;
        if !c.wait_ready(back_within.min(Duration::from_secs(120))).await {
            return Err(failure(UnavailableBeforeUpload)(fail(format!(
                "{app_name} API not reachable for config restore; upload was not attempted"
            ))));
        }
        let tmp =
            tempfile::tempdir().map_err(|e| failure(NotAttempted)(Error::internal(format!("{app_name}: {e}"))))?;
        let zip = keep_connection(app, &p.vdir.join(&src.file), tmp.path(), rl).await.map_err(failure(NotAttempted))?;
        if let Err(e) = c.restore_config(&zip, false).await {
            // A transport failure may hide an accepted upload or a changed API key.
            let availability = match crate::service::client(app).await {
                Ok(client) if client.wait_ready(back_within).await => ApiAvailability::Ready,
                Ok(_) => ApiAvailability::Unavailable,
                Err(_) => ApiAvailability::Unknown,
            };
            let outcome = if availability == ApiAvailability::Ready && e.status().is_some() {
                RequestFailed
            } else {
                UploadOutcomeUnknown
            };
            return Err(ApiRestoreFailed { error: e.into(), outcome, availability });
        }
        rl.append(format!("{app_name}: restored config from {}; the app restarts to apply it", src.file)).await;
        tokio::time::sleep(Duration::from_secs(8)).await;
        let c2 = crate::service::client(app).await.map_err(failure(RestartNotReady))?;
        if !c2.wait_ready(back_within).await {
            return Err(failure(RestartNotReady)(fail(format!(
                "{app_name}: not reachable after config restore (check its API key); restore not verified"
            ))));
        }
        rl.append(format!("{app_name}: back online")).await;
        Ok(Verified)
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
    async fn local_failure_before_upload_preserves_prior_availability() {
        let (_dir, app) = crate::app::tests::app().await;
        let jobs = crate::jobs::JobRunner::new(app.state().clone());
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        jobs.start(|_| {});
        let worker_app = app.clone();
        jobs.submit("media", "test", "radarr", HashSet::new(), move |rl| async move {
            let mut config = worker_app.suite_app("media", "radarr")?;
            config.kind = "unsupported-kind".into();
            let prepared = Prepared {
                app: config,
                version: "test".into(),
                vdir: PathBuf::new(),
                dumps: vec![],
                source: Some(ConfigSource { file: "config.zip".into(), sha256: None }),
                file_app: false,
                sqlite: None,
                offline_gap: None,
            };
            let failure = worker_app
                .restore_api_config_within(&rl, &prepared, Duration::ZERO)
                .await
                .expect_err("local client construction fails before upload");
            let _ = result_tx.send(failure);
            Ok(JobOutcome::ok(json!({})))
        })
        .await
        .expect("submit");
        let failure = tokio::time::timeout(Duration::from_secs(5), result_rx).await.expect("complete").expect("result");
        assert_eq!(failure.outcome, ApiRestoreOutcome::NotAttempted);
        assert_eq!(failure.availability, ApiAvailability::Unchanged);
        jobs.begin_shutdown();
        jobs.join().await.expect("worker shutdown");
        app.shutdown().await.expect("shutdown");
    }

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
