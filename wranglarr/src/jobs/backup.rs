// SPDX-License-Identifier: GPL-3.0-or-later
//! A suite backup: a new version in the store with each app's databases, its own
//! backup zip and a plain copy of its /config, the manifest that lists them, and
//! the standalone restore stack. Old versions are pruned afterwards.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{sha256_of, size_of};
use crate::app::App;
use crate::config::AppConfig;
use crate::docker::Docker;
use crate::error::{Error, Result};
use crate::files::{CopySpec, snapshot_config};
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::manifest::{AppBackup, ConfigArchive, DatabaseDump, Globals, Manifest};
use crate::service::{ARR_COPY, AnyClient, AppConfigExt};
use crate::standalone;
use crate::state::RunStatus;
use crate::time::now_iso_micros;
use crate::value::text;

/// What went wrong in one app's backup: `notes` go into its manifest entry's
/// `errors`; `failed` counts the steps that make the backup partial.
#[derive(Default)]
struct Problems {
    notes: Vec<String>,
    failed: usize,
}

impl Problems {
    /// Noted in the entry, without failing the backup.
    fn note(&mut self, msg: String) {
        self.notes.push(msg);
    }
    /// Noted, and the backup is partial.
    fn fail(&mut self, msg: String) {
        self.notes.push(msg);
        self.failed += 1;
    }
}

impl App {
    pub async fn backup_suite(&self, suite: &str, trigger: &str) -> Result<i64> {
        self.suite(suite)?;
        let (app, name, trigger) = (self.clone(), suite.to_string(), trigger.to_string());
        self.submit(
            suite,
            "backup",
            "",
            HashSet::new(),
            move |rl| async move { app.backup_job(rl, &name, &trigger).await },
        )
        .await
    }

    /// The backup itself, on the caller's run (an update's pre-update backups run inline).
    pub(crate) async fn backup_job(&self, rl: RunLog, suite_name: &str, trigger: &str) -> JobResult {
        let suite = self.suite(suite_name)?.clone();
        let suite_id = suite_name.to_string();
        let (version, vdir) = self.blocking(move |app| app.store().new_version(&suite_id)).await?;
        rl.append(format!("backup of suite {suite_name} -> version {version} ({trigger})")).await;
        let vpn = if self.cfg().vpn.is_some() { self.vpn().status(false).await } else { Value::Null };
        let mut manifest = Manifest::started(suite_name, &version, now_iso_micros(), trigger, crate::VERSION);
        manifest.vpn = Some(vpn);
        manifest = self.save_job_manifest(suite_name, &version, manifest).await?;
        let mut errors = 0;

        let dock = if self.docker_ok().await { self.docker() } else { None };
        if suite.apps.iter().any(|a| !a.databases.is_empty()) {
            manifest.postgres_version = Some(self.pg().server_version().await);
            manifest.postgres_container = Some(standalone::container_info(dock, &self.cfg().postgres.container).await);
            match self.pg().dump_globals(&vdir.join("globals.sql")).await {
                Ok(size) => {
                    manifest.globals = Some(Globals { file: "globals.sql".into(), size, ..Globals::default() });
                    rl.append(format!("dumped postgres globals ({size} B)")).await;
                }
                Err(e) => {
                    rl.append(format!("globals dump failed: {e}")).await;
                    errors += 1;
                }
            }
        }

        for app in &suite.apps {
            let (entry, errs) = self.backup_app(&rl, app, &vdir, dock).await;
            errors += errs;
            manifest.apps.insert(app.name.clone(), entry);
        }

        let sa = {
            let (vdir, m, cfg, s) = (vdir.clone(), manifest.clone(), self.cfg().clone(), suite.clone());
            let env = self.deployment().restore_stack.clone();
            tokio::task::spawn_blocking(move || standalone::write(&vdir, &m, &cfg, &s, &env)).await
        };
        match sa {
            Ok(Ok(written)) => {
                if !written.is_empty() {
                    rl.append(format!("standalone restore stack: {}", written.join(", "))).await;
                }
                manifest.standalone = Some(written);
            }
            // never lose a backup over the extras
            Ok(Err(e)) => {
                manifest.standalone = Some(vec![]);
                rl.append(format!("could not write the standalone restore stack: {e}")).await;
            }
            Err(e) => {
                manifest.standalone = Some(vec![]);
                rl.append(format!("could not write the standalone restore stack: {e}")).await;
            }
        }
        let ok_apps = manifest.apps.values().filter(|e| e.ok()).count();
        let status = RunStatus::of_steps(errors > 0, ok_apps > 0);
        manifest.status = status.into();
        manifest = self.save_job_manifest(suite_name, &version, manifest).await?;
        rl.append(format!("version {version} {status}: {} B", manifest.bytes())).await;

        let suite_id = suite_name.to_string();
        let pruned = self.prune_suite(&suite_id).await?;
        if !pruned.is_empty() {
            rl.append(format!("pruned {} old version(s): {}", pruned.len(), pruned.join(", "))).await;
        }
        Ok(JobOutcome::new(status, json!({"version": version, "pruned": pruned, "total_bytes": manifest.total_bytes})))
    }

    async fn save_job_manifest(&self, suite: &str, version: &str, mut manifest: Manifest) -> Result<Manifest> {
        let (suite, version) = (suite.to_string(), version.to_string());
        self.blocking(move |app| {
            app.store().write_manifest(&suite, &version, &mut manifest)?;
            Ok(manifest)
        })
        .await
    }

    /// One app's part of a backup: its manifest entry and how many steps failed.
    async fn backup_app(&self, rl: &RunLog, app: &AppConfig, vdir: &Path, dock: Option<&Docker>) -> (AppBackup, usize) {
        let mut p = Problems::default();
        let container = standalone::container_info(dock, &app.container).await;
        let mut entry = AppBackup::new(&app.name, &app.kind, container);
        if let Some(msg) = app.backup_limitation() {
            p.fail(msg.into());
            rl.append(format!("{}: {msg}", app.name)).await;
        }
        let client = self.app_status(rl, app, &mut entry, &mut p).await;
        entry.databases = self.dump_databases(rl, app, vdir, &mut p).await;
        if let Some(c) = client {
            let zip =
                if app.is_config_dir() { None } else { self.capture_zip(rl, app, &c, vdir, &mut entry, &mut p).await };
            self.copy_config(rl, app, vdir, zip, &mut entry, &mut p).await;
        }
        entry.errors = p.notes;
        (entry, p.failed)
    }

    /// The app's status, collection and scope into `entry`. The client, unless
    /// the app didn't answer (then nothing else is asked of it).
    async fn app_status(
        &self,
        rl: &RunLog,
        app: &AppConfig,
        entry: &mut AppBackup,
        p: &mut Problems,
    ) -> Option<AnyClient> {
        let name = &app.name;
        let mut c: Option<AnyClient> = None;
        let status = match crate::service::client(app).await {
            Ok(cl) => {
                let st = cl.status().await.map_err(|e| e.to_string());
                c = Some(cl);
                st
            }
            Err(e) => Err(e.to_string()),
        };
        match status {
            Ok(st) => {
                let field = |f: &str| Some(st.get(f).cloned().unwrap_or(Value::Null));
                entry.version = field("version");
                entry.database_type = field("databaseType");
                entry.database_version = field("databaseVersion");
                entry.migration_version = field("migrationVersion");
                entry.instance_name = field("instanceName");
                entry.branch = field("branch");
                entry.os = field("osName");
                rl.append(format!("{name}: {} {} ({})", app.kind, known(&st["version"]), known(&st["databaseType"])))
                    .await;
                if let Some(cl) = &c {
                    match cl.collection().await {
                        Ok(col) => {
                            entry.collection = col;
                            entry.scope = cl.scope().await;
                            rl.append(format!("{name}: collection {}", counts(&entry.collection))).await;
                        }
                        Err(e) => {
                            p.note(format!("collection: {e}"));
                            rl.append(format!("{name}: collection failed: {e}")).await;
                        }
                    }
                }
                c
            }
            Err(e) => {
                p.fail(format!("status: {e}"));
                rl.append(format!("{name}: unreachable: {e}")).await;
                None
            }
        }
    }

    /// pg_dump of each of the app's databases: their manifest rows.
    async fn dump_databases(&self, rl: &RunLog, app: &AppConfig, vdir: &Path, p: &mut Problems) -> Vec<DatabaseDump> {
        let name = &app.name;
        let mut dbs = vec![];
        for db in &app.databases {
            let dest = vdir.join(name).join(format!("{db}.dump"));
            let done = async {
                let size = self.pg().dump(db, &dest).await.map_err(|e| e.to_string())?;
                let sum = sha256_of(dest.clone()).await.map_err(|e| e.to_string())?;
                Ok::<_, String>((size, sum))
            };
            match done.await {
                Ok((size, sum)) => {
                    dbs.push(DatabaseDump {
                        name: db.clone(),
                        file: format!("{name}/{db}.dump"),
                        size,
                        sha256: sum,
                        db_size: self.pg().database_size(db).await,
                        ..DatabaseDump::default()
                    });
                    rl.append(format!("{name}: dumped {db} ({size} B)")).await;
                }
                Err(e) => {
                    p.fail(format!("pg_dump {db}: {e}"));
                    rl.append(format!("{name}: pg_dump {db} failed: {e}")).await;
                }
            }
        }
        dbs
    }

    /// The app's own backup zip (download clients have none) into `entry`; its path.
    async fn capture_zip(
        &self,
        rl: &RunLog,
        app: &AppConfig,
        c: &AnyClient,
        vdir: &Path,
        entry: &mut AppBackup,
        p: &mut Problems,
    ) -> Option<PathBuf> {
        let name = &app.name;
        match c.capture_config(&vdir.join(name)).await {
            Ok(Some(f)) => {
                let fname = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let size = size_of(&f).await;
                match sha256_of(f.clone()).await {
                    Ok(sum) => {
                        entry.config_zip = Some(ConfigArchive {
                            file: format!("{name}/{fname}"),
                            size,
                            sha256: sum,
                            format: "zip".into(),
                            ..ConfigArchive::default()
                        });
                        rl.append(format!("{name}: captured {fname} ({size} B)")).await;
                        Some(f)
                    }
                    Err(e) => {
                        p.fail(format!("config backup: {e}"));
                        rl.append(format!("{name}: config backup failed: {e}")).await;
                        None
                    }
                }
            }
            Ok(None) => None,
            Err(e) => {
                p.fail(format!("config backup: {e}"));
                rl.append(format!("{name}: config backup failed: {e}")).await;
                None
            }
        }
    }

    /// The natural-format copy of /config, usable without wranglarr, into `entry`.
    async fn copy_config(
        &self,
        rl: &RunLog,
        app: &AppConfig,
        vdir: &Path,
        zip: Option<PathBuf>,
        entry: &mut AppBackup,
        p: &mut Problems,
    ) {
        let name = &app.name;
        let Some(src) = app.snapshot_source() else {
            if app.is_config_dir() {
                p.fail("config: config_dir is required".into());
                rl.append(format!("{name}: config_dir is required to back up this app")).await;
            } else {
                rl.append(format!("{name}: no config dir mounted, skipping the plain copy of /config")).await;
            }
            return;
        };
        // SQLite-mode apps: the backup zip has the consistent database copy
        let overlay = zip.filter(|_| app.databases.is_empty());
        let spec = app.def().map_or(CopySpec { rules: ARR_COPY, payload: None }, |d| d.copy_spec());
        let (n, d) = (name.clone(), vdir.join(name));
        let snap = tokio::task::spawn_blocking(move || snapshot_config(&n, &spec, &src, &d, overlay.as_deref()))
            .await
            .map_err(|e| Error::internal(e.to_string()))
            .and_then(|r| r);
        match snap {
            Ok(s) => {
                rl.append(format!(
                    "{name}: copied /config ({} files, {} B; {} log/cache entries left out)",
                    s.files, s.bytes, s.skipped
                ))
                .await;
                if !s.unreadable.is_empty() {
                    let bad = &s.unreadable;
                    let more = if bad.len() > 5 { " …" } else { "" };
                    p.fail(format!(
                        "config copy: {} file(s) or folder(s) couldn't be read (permissions?): {}{more}",
                        bad.len(),
                        bad.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                    ));
                    rl.append(format!(
                        "{name}: couldn't read {} item(s) in /config: {}",
                        bad.len(),
                        bad.iter().take(10).cloned().collect::<Vec<_>>().join(", ")
                    ))
                    .await;
                }
                entry.config_snapshot = Some(s);
            }
            Err(e) => {
                p.fail(format!("config copy: {e}"));
                rl.append(format!("{name}: config copy failed: {e}")).await;
            }
        }
    }
}

/// A value for a log line, "unknown" when the app didn't say.
fn known(v: &Value) -> String {
    if v.is_null() { "unknown".into() } else { text(v) }
}

/// An object of counts for a log line: `movies 3, missing 1`.
fn counts(v: &Value) -> String {
    match v {
        Value::Object(o) if !o.is_empty() => {
            o.iter().map(|(k, v)| format!("{k} {}", text(v))).collect::<Vec<_>>().join(", ")
        }
        Value::Object(_) => "empty".into(),
        other => text(other),
    }
}
