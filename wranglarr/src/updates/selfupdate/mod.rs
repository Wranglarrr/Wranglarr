// SPDX-License-Identifier: GPL-3.0-or-later
//! wranglarr updating itself.
//!
//! A process can't replace the container it runs in, so wranglarr hands the
//! swap to a [helper](helper) started from the new image. Only two things start
//! it: someone signed in pressing **Update now**, or the opt-in schedule
//! (`[updates] self_update`); never *Update all* or the stack's auto-update.
//!
//! The run, on the job worker (suite `system`, kind `self-update`):
//!
//! 1. pull the image wranglarr runs; stop if it hasn't changed;
//! 2. copy the state database (`VACUUM INTO`) and `wranglarr.toml` into
//!    `<state folder>/self-update/<time>/`, the newest three kept;
//! 3. start the helper, then wait. The helper stops this wranglarr, which hands
//!    the run over ([`JobOutcome::handed_off`]) instead of finishing it.
//! 4. The wranglarr that starts next (the new one, or this one put back) keeps
//!    that run open at start-up, adopts it on its worker, waits for the helper's
//!    [report], records it (log lines, history rows, a rejected image) and
//!    finishes the run, which sends the usual update notification.
//!
//! A helper that fails before stopping wranglarr reports back to this
//! process; one that dies without a report, or a report that outlives its
//! deadline, fails the run instead of leaving it open.

mod helper;
mod report;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use tokio::time::Instant;

use helper::{Args, container_body, mounts_missing};
pub use helper::{health_main, helper_main, main};
use report::{Phase, Report};

use super::{DOCKER_OFF, SUITE, Updater, self_update_hint, short};
use crate::app::App;
use crate::docker::{DockerError, image_reference};
use crate::error::{Error, Result};
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::standalone::image_version;

/// The run kind.
pub(crate) const KIND: &str = "self-update";
/// Snapshots kept in `<state folder>/self-update/`.
const KEEP_SNAPSHOTS: usize = 3;
/// How long the old wranglarr waits on a helper that neither stops it nor reports.
const HELPER_GRACE: Duration = Duration::from_secs(600);

/// wranglarr's own container.
#[derive(Debug, Clone)]
pub(crate) struct Own {
    pub(crate) name: String,
    /// `docker inspect`
    pub(crate) info: Value,
}

impl Own {
    /// The image reference it was deployed with (see [`image_reference`]).
    pub(crate) fn reference(&self) -> String {
        image_reference(&self.info).to_string()
    }
}

/// Why an image reference can't be followed by pulling it again: built here,
/// or pinned to a digest. None: it can.
fn not_from_a_registry(reference: &str) -> Option<String> {
    if reference.contains('@') || reference.starts_with("sha256:") {
        return Some("its image is pinned to a digest".into());
    }
    if self_update_hint(reference).starts_with("rebuild") {
        return Some(format!("it runs {reference:?}, an image built on this machine"));
    }
    None
}

/// The run (if any) handed over to the process starting on the state database
/// `state_db`: it must stay open until that process has adopted it.
pub(crate) fn handed_over(state_db: &Path) -> Option<i64> {
    match Report::load(&report::path(state_db)) {
        Ok(Some(r)) if !r.stale(Utc::now()) => Some(r.run_id),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("self-update: {e}");
            None
        }
    }
}

/// Start-up: finish the run a self-update handed over, and clear a report
/// that is left over.
pub(crate) async fn resume(app: &App) {
    let path = report::path(&app.cfg().state_db);
    let r = match Report::load(&path) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("self-update: unreadable report removed: {e}");
            remove_report(&path);
            return;
        }
    };
    let open =
        matches!(app.state().get_run(r.run_id).await, Ok(Some(run)) if run.finished.is_none() && run.kind == KIND);
    if !open || r.stale(Utc::now()) {
        tracing::info!("self-update: removed the report of run {}, which is over", r.run_id);
        remove_report(&path);
        return;
    }
    let u = app.updates();
    let helper = r.helper.clone();
    match app.adopt(r.run_id, move |rl| async move { u.await_helper(&rl, &helper, None).await }).await {
        Ok(()) => tracing::info!("self-update: run {} handed over to this wranglarr", r.run_id),
        Err(e) => tracing::warn!("self-update: could not take over run {}: {e}", r.run_id),
    }
}

fn remove_report(path: &Path) {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            tracing::warn!("self-update: could not remove {}: {e}", path.display());
        }
        _ => {}
    }
}

impl Updater {
    /// wranglarr's own container, found by HOSTNAME (its id); None outside a
    /// container or without Docker control.
    pub(crate) async fn own(&self) -> Option<Own> {
        let host = &self.env().hostname;
        let d = self.app.docker()?;
        if host.is_empty() {
            return None;
        }
        let info = d.inspect(host).await.ok()?;
        if !self.is_self(&info) {
            return None;
        }
        let name = info["Name"].as_str().unwrap_or("").trim_start_matches('/').to_string();
        (!name.is_empty()).then_some(Own { name, info })
    }

    /// Why wranglarr can't update itself from its container `own` (`docker
    /// inspect`); None when it can.
    pub(crate) fn self_update_blocker(&self, own: &Value) -> Option<String> {
        let cfg = self.app.cfg();
        if self.app.docker().is_none() {
            return Some(DOCKER_OFF.into());
        }
        not_from_a_registry(image_reference(own)).or_else(|| {
            let state_dir = cfg.state_db.parent().unwrap_or(Path::new("/"));
            mounts_missing(own, &cfg.docker_socket, state_dir)
        })
    }

    /// Queue wranglarr's own update (`trigger`: manual | scheduled). Returns the run id.
    pub(crate) async fn self_update(&self, trigger: &str) -> Result<i64> {
        if !self.app.docker_ok().await {
            return Err(Error::conflict(DOCKER_OFF));
        }
        let own = self.own().await.ok_or_else(|| {
            Error::conflict("wranglarr's own container wasn't found: it updates itself only when it runs in one")
        })?;
        if let Some(why) = self.self_update_blocker(&own.info) {
            return Err(Error::conflict(format!("wranglarr can't update itself here: {why}")));
        }
        let open = self.app.state().list_runs(Some(SUITE), 50).await?;
        if open.iter().any(|r| r.kind == KIND && r.finished.is_none()) {
            return Err(Error::conflict("an update of wranglarr is already under way"));
        }
        let u = self.clone();
        let trigger = trigger.to_string();
        let name = own.name.clone();
        self.app
            .submit(
                SUITE,
                KIND,
                &name,
                Default::default(),
                move |rl| async move { u.self_update_job(&rl, &trigger).await },
            )
            .await
    }

    async fn self_update_job(&self, rl: &RunLog, trigger: &str) -> JobResult {
        let d = self.docker()?;
        let own = self.own().await.ok_or_else(|| Error::conflict("wranglarr's own container wasn't found"))?;
        if let Some(why) = self.self_update_blocker(&own.info) {
            return Err(Error::conflict(format!("wranglarr can't update itself here: {why}")));
        }
        // the image of wranglarr's selected track (latest, edge, …), in the repository it runs
        let reference = self.update_reference(&own.name, &own.reference()).await?;
        rl.append(format!("update of wranglarr ({trigger}): {} on {reference}", own.name)).await;
        d.pull(&reference).await?;
        let old_id = own.info["Image"].as_str().unwrap_or("").to_string();
        let new_img = d.image(&reference).await?;
        let new_id = new_img["Id"].as_str().unwrap_or("").to_string();
        if new_id == old_id {
            rl.append(format!("{reference} is already the newest")).await;
            return Ok(JobOutcome::ok(json!({"results": {own.name.clone(): "current"}})));
        }
        let version = image_version(&new_img);
        let shown = if version.is_empty() { short(&new_id) } else { version };
        rl.append(format!("pulled {reference}: {} -> {shown}", short(&old_id))).await;

        let snapshot = self.snapshot(rl).await?;
        let path = report::path(&self.app.cfg().state_db);
        remove_report(&path);
        let helper = Args::helper_name(&own.name);
        match d.inspect(&helper).await {
            Ok(h) if h["State"]["Running"] == true => {
                return Err(Error::conflict(format!("{helper} is running: an update of wranglarr is under way")));
            }
            Ok(_) => d.remove(&helper, true).await?,
            Err(DockerError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
        let cfg = self.app.cfg();
        let args = Args {
            container: own.name.clone(),
            image: reference.clone(),
            image_id: new_id.clone(),
            run: rl.run_id,
            state_db: cfg.state_db.clone(),
            snapshot: Some(snapshot),
            health: health_addr(&cfg.bind, cfg.port),
            timeout: self.health().timeout.as_secs(),
            socket: cfg.docker_socket.clone(),
        };
        let body = container_body(&own.info, &args, &self.helper_env(&own.info))?;
        let id = d.create(&helper, &body).await?;
        if let Err(e) = d.start(&id).await {
            if let Err(e2) = d.remove(&id, true).await {
                rl.append(format!("could not remove {helper}: {e2}")).await;
            }
            return Err(e.into());
        }
        rl.append(format!(
            "started {helper} on the new image: it stops this wranglarr, starts the new one, \
             and puts this one back if the new one doesn't come up"
        ))
        .await;
        self.await_helper(rl, &helper, Some(Instant::now() + self.health().timeout + HELPER_GRACE)).await
    }

    /// The environment the helper runs with: wranglarr's time zone and the
    /// updater's timings.
    fn helper_env(&self, own: &Value) -> Vec<String> {
        let tz = own["Config"]["Env"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|e| e.starts_with("TZ="))
            .map(str::to_string);
        let env = self.env();
        let mut out = vec!["HOME=/tmp".to_string()];
        out.extend(tz);
        out.push(format!("WRANGLARR_UPDATE_POLL={}", env.poll.as_secs_f64()));
        out.push(format!("WRANGLARR_UPDATE_STABLE={}", env.stable.as_secs_f64()));
        out
    }

    /// Copy the state database and wranglarr.toml into
    /// `<state folder>/self-update/<time>/`; the database copy's path.
    async fn snapshot(&self, rl: &RunLog) -> Result<PathBuf> {
        let cfg = self.app.cfg();
        let base = cfg.state_db.parent().unwrap_or(Path::new(".")).join("self-update");
        let dir = base.join(Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string());
        let db = dir.join(cfg.state_db.file_name().unwrap_or("wranglarr.db".as_ref()));
        // said first: when an update is undone, this database copy is what comes back
        rl.append(format!("copying wranglarr's state database and config to {} first", dir.display())).await;
        let mk = dir.clone();
        tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&mk)
        })
        .await??;
        self.app.state().snapshot_to(&db).await?;
        let (toml, into, prune) = (self.app.deployment().config_path.clone(), dir.clone(), base.clone());
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if toml.is_file()
                && let Some(name) = toml.file_name()
            {
                std::fs::copy(&toml, into.join(name))?;
            }
            prune_snapshots(&prune, KEEP_SNAPSHOTS)
        })
        .await??;
        Ok(db)
    }

    /// Wait for the helper's report on run `rl` and finish the run from it. The
    /// helper container is `helper`; `give_up` (the old wranglarr's own wait)
    /// stops a helper that took too long. When this wranglarr is shutting down
    /// (the helper stopped it), the run is handed over.
    pub(crate) async fn await_helper(&self, rl: &RunLog, helper: &str, give_up: Option<Instant>) -> JobResult {
        let path = report::path(&self.app.cfg().state_db);
        let mut gone = 0;
        loop {
            tokio::select! {
                () = rl.stopping() => return Ok(JobOutcome::handed_off()),
                () = tokio::time::sleep(self.env().poll) => {}
            }
            match Report::load(&path) {
                Ok(Some(r)) if r.run_id == rl.run_id && r.phase == Phase::Done => {
                    return self.apply_report(rl, &path, r).await;
                }
                Ok(Some(r)) if r.run_id == rl.run_id && r.overdue(Utc::now()) => {
                    remove_report(&path);
                    return Err(Error::conflict("wranglarr's update helper stopped reporting before it finished"));
                }
                Ok(_) => {}
                Err(e) => return Err(Error::internal(e)),
            }
            // a helper that is gone with no final report died: say so rather than wait
            let alive = match self.app.docker() {
                Some(d) => match d.inspect(helper).await {
                    Ok(h) => h["State"]["Running"] == true,
                    Err(DockerError::NotFound(_)) => false,
                    Err(_) => true,
                },
                None => true,
            };
            gone = if alive { 0 } else { gone + 1 };
            if gone >= 3 {
                remove_report(&path);
                return Err(Error::conflict(format!("{helper} ended without reporting how the update went")));
            }
            if give_up.is_some_and(|t| Instant::now() > t) {
                if let Some(d) = self.app.docker()
                    && let Err(e) = d.remove(helper, true).await
                {
                    rl.append(format!("could not stop {helper}: {e}")).await;
                }
                return Err(Error::conflict(format!("{helper} took too long and was stopped")));
            }
        }
    }

    /// Record what the helper reported and finish the run with it.
    async fn apply_report(&self, rl: &RunLog, path: &Path, r: Report) -> JobResult {
        let state = self.app.state();
        state.append_log_lines(rl.run_id, r.lines.clone()).await?;
        for u in r.updates.iter().cloned() {
            if let Err(e) = state.add_update(u.into_row(rl.run_id)).await {
                tracing::error!("self-update: could not record the update: {e}");
            }
        }
        if let Some(rej) = &r.rejected {
            self.put_block(&r.container, &rej.image, rej.digests.clone(), &rej.reason).await?;
        }
        remove_report(path);
        if let Err(e) = self.recheck().await {
            rl.append(format!("update check after the update: {e}")).await;
        }
        if r.ok {
            let now = if r.version.is_empty() { String::new() } else { format!(", now {}", r.version) };
            rl.append(format!("wranglarr updated{now}")).await;
        } else {
            rl.append(format!("update of wranglarr FAILED: {}; see the recovery details above", r.error)).await;
        }
        Ok(JobOutcome::new(r.status(), r.run_result()))
    }

    /// wranglarr's row of the last check, when it is in it.
    pub(crate) async fn own_row(&self) -> Option<Value> {
        self.last_check().await.into_values().find(|r| r["status"] == "self")
    }

    /// The scheduled self-update: a check, then the update when a newer image
    /// is out, it can be applied here, and it wasn't rejected before.
    pub(super) async fn self_auto(&self) {
        if let Err(e) = self.check_all().await {
            tracing::warn!("scheduled update of wranglarr: check failed: {e}");
            return;
        }
        let Some(row) = self.own_row().await else { return };
        if row["self_update"] != true {
            tracing::info!("scheduled update of wranglarr: nothing to do");
            return;
        }
        let name = row["container"].as_str().unwrap_or("");
        let remote = row["remote_digest"].as_str().unwrap_or("");
        let blocked = self.blocked().await;
        if blocked
            .get(name)
            .and_then(|b| b["digests"].as_array())
            .is_some_and(|ds| ds.iter().any(|d| d.as_str() == Some(remote)))
        {
            tracing::info!("scheduled update of wranglarr: skipped, this image was rejected before");
            return;
        }
        if let Err(e) = self.self_update("scheduled").await {
            tracing::warn!("scheduled update of wranglarr skipped: {e}");
        }
    }
}

/// Where wranglarr answers inside its own container: its `bind` address, or
/// loopback when it listens on every address.
fn health_addr(bind: &str, port: u16) -> String {
    let host = match bind.trim() {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        b => b,
    };
    if host.contains(':') && !host.starts_with('[') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

/// Keep the newest `keep` snapshot folders in `base` (their names sort by time).
fn prune_snapshots(base: &Path, keep: usize) -> std::io::Result<()> {
    let mut dirs: Vec<PathBuf> =
        std::fs::read_dir(base)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    let drop = dirs.len().saturating_sub(keep);
    for d in &dirs[..drop] {
        std::fs::remove_dir_all(d)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_registry_images_can_be_pulled_again() {
        assert_eq!(not_from_a_registry("ghcr.io/wranglarrr/wranglarr:latest"), None);
        assert_eq!(not_from_a_registry("localhost:5000/wranglarr:1"), None);
        assert!(not_from_a_registry("wranglarr:local").is_some_and(|w| w.contains("built on this machine")));
        assert!(not_from_a_registry("localhost/wranglarr:dev").is_some());
        assert!(not_from_a_registry("ghcr.io/wranglarrr/wranglarr@sha256:ab").is_some_and(|w| w.contains("digest")));
    }

    #[test]
    fn only_a_fresh_report_keeps_its_run_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("wranglarr.db");
        assert_eq!(handed_over(&db), None, "no report");
        let now = Utc::now();
        let fresh =
            Report::begin(7, "wranglarr", "wranglarr-wranglarr-updater", now, now + chrono::Duration::minutes(10));
        fresh.save(&report::path(&db)).expect("save");
        assert_eq!(handed_over(&db), Some(7));
        let old = now - chrono::Duration::hours(25);
        let stale =
            Report::begin(8, "wranglarr", "wranglarr-wranglarr-updater", old, old + chrono::Duration::minutes(10));
        stale.save(&report::path(&db)).expect("save");
        assert_eq!(handed_over(&db), None, "a day-old report is ignored");
        std::fs::write(report::path(&db), "{not json").expect("write");
        assert_eq!(handed_over(&db), None, "an unreadable report is ignored");
    }

    #[test]
    fn health_addresses() {
        assert_eq!(health_addr("0.0.0.0", 8787), "127.0.0.1:8787");
        assert_eq!(health_addr("", 9000), "127.0.0.1:9000");
        assert_eq!(health_addr("::", 8787), "127.0.0.1:8787");
        assert_eq!(health_addr("10.0.0.2", 8787), "10.0.0.2:8787");
        assert_eq!(health_addr("fd00::2", 8787), "[fd00::2]:8787");
    }

    #[test]
    fn the_newest_snapshots_are_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        for n in ["20261001T000000.000Z", "20261003T000000.000Z", "20261002T000000.000Z", "20261004T000000.000Z"] {
            std::fs::create_dir(dir.path().join(n)).expect("mkdir");
        }
        std::fs::write(dir.path().join("note.txt"), "x").expect("file");
        prune_snapshots(dir.path(), 3).expect("prune");
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .expect("ls")
            .map(|e| e.expect("e").file_name().into_string().expect("utf8"))
            .collect();
        left.sort();
        assert_eq!(left, ["20261002T000000.000Z", "20261003T000000.000Z", "20261004T000000.000Z", "note.txt"]);
    }
}
