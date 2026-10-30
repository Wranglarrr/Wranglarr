// SPDX-License-Identifier: GPL-3.0-or-later
//! The job worker's side of a swap: health by the app's API, and the app's data
//! put back from the pre-update backup when the new container fails.

use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::super::recovery::{DataPhase, DataRecovery};
use super::super::report::Captured;
use super::super::{OLD_SUFFIX, Target, Updater};
use super::DataBack;
use super::swap::{Log, Swapper};
use crate::docker::Docker;
use crate::error::{Error, Result};
use crate::jobs::RunLog;
use crate::jobs::restore::Prepared;

/// A swap on the job worker: lines go to the run's log, and the new container
/// is up once its app's API (or Postgres) answers.
pub(super) struct OnRun<'a> {
    pub(super) updater: &'a Updater,
    pub(super) rl: &'a RunLog,
    pub(super) target: &'a Target,
    pub(super) data: &'a DataBack,
    /// The new container was (or may have been) started: it may have changed the data.
    started: AtomicBool,
    /// Data progress is one coherent state, independent of container placement.
    progress: Mutex<DataProgress>,
    /// The failed container's log and state, for the report.
    captured: Mutex<Option<Captured>>,
}

#[derive(Default)]
pub(super) struct DataProgress {
    pub(super) recovery: DataRecovery,
    pub(super) note: String,
    pub(super) prepared: Option<Prepared>,
}

impl<'a> OnRun<'a> {
    pub(super) fn new(updater: &'a Updater, rl: &'a RunLog, target: &'a Target, data: &'a DataBack) -> Self {
        Self {
            updater,
            rl,
            target,
            data,
            started: AtomicBool::new(false),
            progress: Mutex::new(DataProgress::default()),
            captured: Mutex::new(None),
        }
    }

    /// Consume callback state after swap returns. Recovery now owns its data
    /// progress exclusively, including the prepared API restore.
    pub(super) fn finish(self) -> (DataProgress, Option<Captured>) {
        let mut progress = self.progress.into_inner().expect("data progress");
        if progress.note.is_empty()
            && matches!(self.data, DataBack::IfFailed { .. })
            && self.started.load(Ordering::Acquire)
        {
            progress.note = "The data wasn't put back from the pre-update backup.".into();
        }
        (progress, self.captured.into_inner().ok().flatten())
    }

    fn note(&self, s: impl Into<String>) {
        self.progress.lock().expect("data progress").note = s.into();
    }

    fn data_recovery(&self) -> DataRecovery {
        self.progress.lock().expect("data progress").recovery.clone()
    }
}

impl Log for OnRun<'_> {
    fn line(&self, msg: String) -> impl Future<Output = ()> + Send {
        self.rl.line(msg)
    }
}

impl OnRun<'_> {
    fn parked(&self) -> String {
        format!("{}{OLD_SUFFIX}", self.target.container)
    }

    fn done(&self) -> String {
        let parts = self.data_recovery().completed;
        if parts.is_empty() { "nothing had finished".into() } else { format!("done: {}", parts.join(", ")) }
    }

    /// Put `p` back with the container stopped, keeping track of what went back.
    async fn restore(&self, p: &Prepared) -> Result<()> {
        self.progress.lock().expect("data progress").recovery.state = DataPhase::Restoring;
        let mut done = vec![];
        let r = async {
            let c = crate::service::client(&p.app).await?;
            self.updater.app.restore_stopped(self.rl, p, &c, &mut done).await
        }
        .await;
        let mut progress = self.progress.lock().expect("data progress");
        progress.recovery.completed.extend(done);
        progress.recovery.state = if r.is_ok() { DataPhase::OfflineRestored } else { DataPhase::NeedsRecovery };
        r
    }

    /// Stop here: the old container stays parked and stopped, for recovery by hand.
    fn keep_stopped(&self, why: String) -> Error {
        let msg = format!(
            "{why}. {} stays stopped as {} so nothing runs on that data: restore the backup by hand, then rename it back and start it",
            self.target.container,
            self.parked()
        );
        self.note(msg.clone());
        self.progress.lock().expect("data progress").recovery.state = DataPhase::NeedsRecovery;
        Error::conflict(msg)
    }
}

impl Swapper for OnRun<'_> {
    fn ready(&self, _d: &Docker, id: &str) -> impl Future<Output = Result<()>> + Send {
        self.updater.wait_healthy(self.rl, self.target, id)
    }

    fn starting(&self) {
        self.started.store(true, Ordering::Release);
    }

    async fn before_create(&self, _d: &Docker) -> Result<()> {
        let DataBack::First(p) = self.data else { return Ok(()) };
        let name = &self.target.container;
        self.rl
            .append(format!("{name}: putting {}'s data back from the pre-update backup {}", p.app.name, p.version))
            .await;
        self.restore(p).await
    }

    async fn on_failure(&self, d: &Docker, id: &str) {
        let log = d.logs(id, 300).await.unwrap_or_default();
        let state = d.inspect(id).await.map(|c| c["State"].clone()).unwrap_or(Value::Null);
        if let Ok(mut c) = self.captured.lock() {
            *c = Some(Captured { log, state });
        }
    }

    async fn before_put_back(&self, _d: &Docker) -> Result<()> {
        let Some(app) = &self.target.app else { return Ok(()) };
        let name = &self.target.container;
        match self.data {
            DataBack::None => Ok(()),
            DataBack::First(p) => {
                if self.data_recovery().state == DataPhase::Untouched {
                    return Ok(()); // failed before any data changed
                }
                // the data is (partly) the old version's now: the newer one mustn't start on it
                let why = format!(
                    "the rollback had begun putting {}'s data back from {} ({}), so the newer version isn't started on it again",
                    app.name,
                    p.version,
                    self.done()
                );
                self.rl.append(format!("{name}: {why}")).await;
                Err(self.keep_stopped(why))
            }
            DataBack::IfFailed { suite, version } => {
                if !self.started.load(Ordering::Acquire) {
                    self.note("The new version never started, so the data was left as it was.");
                    return Ok(()); // the new one never ran: the data is as it was
                }
                let a = &self.updater.app;
                // checked again: files can go missing or change after the update began
                let p = match a.prepare_offline(suite, version, &app.name).await {
                    Ok(p) => p,
                    Err(e) => {
                        let why = format!(
                            "the new version ran, and {}'s data can't be put back from {version} ({e}); the old version isn't started on what the new one may have migrated",
                            app.name
                        );
                        self.rl.append(format!("{name}: {why}")).await;
                        return Err(self.keep_stopped(why));
                    }
                };
                if let Some(gap) = p.offline_gap() {
                    let why = format!("the new version ran, and {gap}");
                    self.rl.append(format!("{name}: {why}")).await;
                    return Err(self.keep_stopped(why));
                }
                if p.needs_stop() {
                    self.rl
                        .append(format!(
                            "{name}: the new version may have migrated {}'s data: putting it back from {version}",
                            app.name
                        ))
                        .await;
                    if let Err(e) = self.restore(&p).await {
                        let why =
                            format!("putting {}'s data back from {version} failed ({e}; {})", app.name, self.done());
                        self.rl.append(format!("{name}: {why}")).await;
                        return Err(self.keep_stopped(why));
                    }
                    self.note("The data was put back from the pre-update backup.");
                }
                self.progress.lock().expect("data progress").prepared = Some(p);
                Ok(())
            }
        }
    }
}
