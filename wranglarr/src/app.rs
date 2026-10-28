// SPDX-License-Identifier: GPL-3.0-or-later
//! The service: one cloneable handle every module hangs its methods on.
//!
//! `App` holds the handles the subsystems share (configuration, the state
//! database, the backup store, Postgres, Docker, the scheduler, the VPN
//! check, auth and the job runner), each behind a read-only accessor, and
//! the subsystems' own state, each a type its module keeps private. Jobs run
//! one at a time on the job runner (`jobs::JobRunner`).

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::auth::Auth;
use crate::community::Community;
use crate::config::{Config, SuiteConfig};
use crate::connect::ConnectState;
use crate::deployment::Deployment;
use crate::docker::Docker;
use crate::downloads::DownloadsState;
use crate::error::{Error, Result};
use crate::events::EventsState;
use crate::feeds::FeedCache;
use crate::indexers::IndexersState;
use crate::jobs::{Finished, JobResult, JobRunner, RunLog};
use crate::keys::KeysState;
use crate::links::LinkCache;
use crate::mediaservers::MediaServers;
use crate::notify::Notifier;
use crate::overview::OverviewCache;
use crate::pg::Postgres;
use crate::restore_plan::{self, RestorePlan};
use crate::sched::Scheduler;
use crate::state::{State, SuiteSettings};
use crate::store::BackupStore;
use crate::vpn::Vpn;

struct Inner {
    cfg: Config,
    /// How this install is deployed (read where the command started).
    deployment: Deployment,
    state: State,
    store: BackupStore,
    pg: Postgres,
    docker: Option<Docker>,
    sched: Scheduler,
    vpn: Vpn,
    auth: Auth,
    jobs: JobRunner,
    /// Where the setup wizard leaves a restore plan.
    plan_path: std::path::PathBuf,
    /// Held through a settings change and the reschedule that follows it.
    settings_gate: tokio::sync::Mutex<()>,
    overview_cache: OverviewCache,
    notify: Notifier,
    media: MediaServers,
    connect: ConnectState,
    events: EventsState,
    keys: KeysState,
    indexers: IndexersState,
    downloads: DownloadsState,
    links: LinkCache,
    feeds: FeedCache,
    community: Community,
}

#[derive(Clone)]
pub struct App(Arc<Inner>);

impl App {
    /// The service on `cfg`, deployed as `deployment`: opens the state database
    /// and the backup store.
    pub async fn new(cfg: Config, deployment: Deployment) -> Result<App> {
        let (cfg, deployment, state, store, auth) = tokio::task::spawn_blocking(move || -> Result<_> {
            let state = State::open(&cfg.state_db)?;
            let store =
                BackupStore::new(&cfg.storage_root, &deployment.host_backups)?.with_owner(deployment.stack_owner);
            let auth = crate::auth::Auth::with_state(state.clone(), deployment.auth.clone())?;
            Ok((cfg, deployment, state, store, auth))
        })
        .await??;
        // a self-update's run stays open for this process to finish (see updates::start)
        let handed_over = crate::updates::handed_over(&cfg.state_db);
        state.abandon_stale_runs(handed_over).await?;
        let pg = Postgres::new(cfg.postgres.clone());
        let docker = (!cfg.docker_socket.is_empty()).then(|| Docker::new(&cfg.docker_socket));
        let plan_path =
            cfg.state_db.parent().map(|p| p.join(restore_plan::FILE)).unwrap_or_else(|| restore_plan::FILE.into());
        let jobs = JobRunner::new(state.clone());
        let vpn = Vpn::new(cfg.vpn.clone());
        Ok(App(Arc::new(Inner {
            cfg,
            deployment,
            state,
            store,
            pg,
            docker,
            sched: Scheduler::default(),
            vpn,
            auth,
            jobs,
            plan_path,
            settings_gate: tokio::sync::Mutex::new(()),
            overview_cache: OverviewCache::default(),
            notify: Notifier::new(),
            media: MediaServers::new(),
            connect: ConnectState::default(),
            events: EventsState::default(),
            keys: KeysState::default(),
            indexers: IndexersState::default(),
            downloads: DownloadsState::default(),
            links: LinkCache::default(),
            feeds: FeedCache::default(),
            community: Community::new(),
        })))
    }

    /// Start the job worker and the schedules. Call once.
    pub async fn start(&self) {
        self.notifier().attach(tokio::runtime::Handle::current());
        let app = self.clone();
        self.0.jobs.start(move |run| app.run_finished(run));
        for s in &self.cfg().suites {
            self.reschedule(&s.name).await;
        }
        crate::auth::start(self).await;
        crate::notify::start(self).await;
        crate::updates::start(self).await;
        crate::connect::start(self);
        crate::events::start(self);
        crate::keys::start(self);
        crate::indexers::start(self).await;
    }

    // ---- the shared handles ----------------------------------------------------
    pub fn cfg(&self) -> &Config {
        &self.0.cfg
    }

    /// How this install is deployed.
    pub fn deployment(&self) -> &Deployment {
        &self.0.deployment
    }

    pub fn state(&self) -> &State {
        &self.0.state
    }

    pub fn store(&self) -> &BackupStore {
        &self.0.store
    }

    pub fn pg(&self) -> &Postgres {
        &self.0.pg
    }

    /// The Docker socket, when one is configured.
    pub fn docker(&self) -> Option<&Docker> {
        self.0.docker.as_ref()
    }

    pub fn sched(&self) -> &Scheduler {
        &self.0.sched
    }

    pub fn vpn(&self) -> &Vpn {
        &self.0.vpn
    }

    pub fn auth(&self) -> &Auth {
        &self.0.auth
    }

    // ---- the subsystems' own state (each type's fields are its module's) -------
    pub fn notifier(&self) -> &Notifier {
        &self.0.notify
    }

    pub fn media(&self) -> &MediaServers {
        &self.0.media
    }

    pub fn community(&self) -> &Community {
        &self.0.community
    }

    pub(crate) fn overview_cache(&self) -> &OverviewCache {
        &self.0.overview_cache
    }

    pub(crate) fn connect_state(&self) -> &ConnectState {
        &self.0.connect
    }

    pub(crate) fn events_state(&self) -> &EventsState {
        &self.0.events
    }

    pub(crate) fn keys_state(&self) -> &KeysState {
        &self.0.keys
    }

    pub(crate) fn indexers_state(&self) -> &IndexersState {
        &self.0.indexers
    }

    pub(crate) fn downloads_state(&self) -> &DownloadsState {
        &self.0.downloads
    }

    pub(crate) fn link_cache(&self) -> &LinkCache {
        &self.0.links
    }

    pub(crate) fn feed_cache(&self) -> &FeedCache {
        &self.0.feeds
    }

    /// Run filesystem or other synchronous work outside runtime threads (never
    /// the state database: await `state` instead). The owned handle also lets
    /// accepted work finish if a request is dropped.
    pub async fn blocking<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(App) -> Result<T> + Send + 'static,
    {
        let app = self.clone();
        tokio::task::spawn_blocking(move || f(app)).await?
    }

    /// Stop timers and admission immediately so shutdown can close HTTP too.
    pub async fn begin_shutdown(&self) {
        let _settings = self.0.settings_gate.lock().await;
        self.0.jobs.begin_shutdown();
        self.0.sched.clear();
        self.0.events.close();
    }

    /// Fail queued work and await the active job, then stop the notifier's
    /// background work. Cancelling an arbitrary restore could leave a
    /// container stopped or its config partly replaced.
    pub async fn shutdown(&self) -> Result<()> {
        self.begin_shutdown().await;
        // the start-up tasks submit runs; stop them before the worker drains
        self.0.connect.shutdown().await;
        self.0.events.shutdown().await;
        self.0.keys.shutdown().await;
        self.0.indexers.shutdown().await;
        let worker = self.0.jobs.join().await;
        self.notifier().shutdown().await;
        worker
    }

    /// Exclude admission throughout a destructive operation (see [`JobRunner::while_idle`]).
    pub(crate) async fn while_idle<T, Fut>(&self, suite: &str, f: impl FnOnce() -> Fut) -> Result<T>
    where
        Fut: Future<Output = Result<T>>,
    {
        self.0.jobs.while_idle(suite, f).await
    }

    pub fn suite(&self, name: &str) -> Result<&SuiteConfig> {
        self.0.cfg.suite(name).ok_or_else(|| Error::not_found(format!("no suite {name:?}")))
    }

    pub fn auth_enabled(&self) -> bool {
        self.0.auth.enabled()
    }

    /// Containers with an update waiting (0 until the updater has checked).
    pub async fn updates_available(&self) -> usize {
        self.updates().available_count().await
    }

    /// Each app's address as the browser should open it (links module).
    pub async fn links_for(&self, rp_id: &str) -> Value {
        serde_json::to_value(self.links().resolve(rp_id).await).unwrap_or_else(|_| json!({}))
    }

    pub async fn connect_status(&self) -> Value {
        self.connect().status(None).await.unwrap_or_else(|e| {
            tracing::warn!("connect status: {e}");
            json!({"auto": self.0.cfg.connect.auto, "links": [], "missing": []})
        })
    }

    pub async fn home_settings(&self) -> Value {
        serde_json::to_value(self.feeds().settings().await).unwrap_or_else(|_| json!({}))
    }

    pub async fn vpn_ok(&self) -> Value {
        self.0.vpn.status(false).await.get("ok").cloned().unwrap_or(Value::Null)
    }

    pub async fn docker_ok(&self) -> bool {
        match self.docker() {
            Some(d) => d.available().await,
            None => false,
        }
    }

    // ---- settings ------------------------------------------------------------
    pub async fn settings(&self, suite: &str) -> Result<SuiteSettings> {
        let s = self.suite(suite)?;
        let config = SuiteSettings {
            schedule: s.schedule.clone(),
            enabled: s.enabled,
            keep_last: s.keep_last,
            keep_days: s.keep_days,
            source: "config",
            updated: None,
        };
        self.0.state.settings(suite, config).await
    }

    pub async fn save_settings(
        &self,
        suite: &str,
        schedule: &str,
        enabled: bool,
        keep_last: i64,
        keep_days: i64,
    ) -> Result<()> {
        self.modify_settings(suite, |mut current| {
            current.schedule = schedule.to_string();
            current.enabled = enabled;
            current.keep_last = keep_last;
            current.keep_days = keep_days;
            Ok(current)
        })
        .await?;
        Ok(())
    }

    /// Read, modify, persist, and reschedule under one gate. Partial HTTP
    /// updates must not overwrite a concurrent change to a different field.
    pub(crate) async fn modify_settings(
        &self,
        suite: &str,
        f: impl FnOnce(SuiteSettings) -> Result<SuiteSettings> + Send,
    ) -> Result<SuiteSettings> {
        let _gate = self.0.settings_gate.lock().await;
        let mut next = f(self.settings(suite).await?)?;
        next.schedule = next.schedule.trim().to_string();
        if !next.schedule.is_empty() {
            crate::sched::parse(&next.schedule).map_err(|e| Error::invalid(format!("invalid schedule: {e}")))?;
        }
        next.keep_last = next.keep_last.max(0);
        next.keep_days = next.keep_days.max(0);
        self.0.state.save_settings(suite, next).await?;
        self.reschedule_locked(suite).await;
        self.settings(suite).await
    }

    pub async fn reset_settings(&self, suite: &str) -> Result<()> {
        let _gate = self.0.settings_gate.lock().await;
        self.0.state.reset_settings(suite).await?;
        self.reschedule_locked(suite).await;
        Ok(())
    }

    pub async fn reschedule(&self, suite: &str) {
        let _gate = self.0.settings_gate.lock().await;
        self.reschedule_locked(suite).await;
    }

    async fn reschedule_locked(&self, suite: &str) {
        let id = format!("backup:{suite}");
        self.0.sched.remove(&id);
        let Ok(st) = self.settings(suite).await else { return };
        if st.enabled && !st.schedule.is_empty() && !self.0.jobs.is_stopping() {
            let app = self.clone();
            let name = suite.to_string();
            let added = self.0.sched.add(&id, &st.schedule, move || {
                let app = app.clone();
                let name = name.clone();
                Box::pin(async move {
                    if let Err(e) = app.backup_suite(&name, "scheduled").await {
                        tracing::warn!("scheduled backup: {e}");
                    }
                })
            });
            match added {
                Ok(()) => tracing::info!("suite {suite} scheduled: {}", st.schedule),
                Err(e) => tracing::error!("suite {suite}: bad schedule {:?}: {e}", st.schedule),
            }
        }
    }

    pub fn next_run(&self, suite: &str) -> Option<chrono::DateTime<chrono::Local>> {
        self.0.sched.next_run(&format!("backup:{suite}"))
    }

    // ---- job plumbing --------------------------------------------------------
    /// Whether a suite is busy (see `JobRunner::busy`).
    pub async fn busy(&self, suite: &str) -> Result<bool> {
        self.0.jobs.busy(suite).await
    }

    /// Queue a job. `also`: other suites the job reads or writes.
    pub async fn submit<F, Fut>(
        &self,
        suite: &str,
        kind: &str,
        target: &str,
        also: HashSet<String>,
        f: F,
    ) -> Result<i64>
    where
        F: FnOnce(RunLog) -> Fut + Send + 'static,
        Fut: Future<Output = JobResult> + Send + 'static,
    {
        self.0.jobs.submit(suite, kind, target, also, f).await
    }

    /// Take over the open run `run_id`, queued again with `f`: how a new process
    /// finishes a run the one before it handed over.
    pub(crate) async fn adopt<F, Fut>(&self, run_id: i64, f: F) -> Result<()>
    where
        F: FnOnce(RunLog) -> Fut + Send + 'static,
        Fut: Future<Output = JobResult> + Send + 'static,
    {
        self.0.jobs.adopt(run_id, f).await
    }

    /// A run ended: its suites' overviews are stale, and the notifier may tell someone.
    fn run_finished(&self, run: Finished) {
        self.overview_cache().invalidate(std::iter::once(&run.suite).chain(&run.also));
        self.notifier().run_finished(
            self,
            &run.suite,
            &run.kind,
            &run.target,
            run.status.as_str(),
            &run.result,
            run.run_id,
        );
    }
}

impl App {
    // ---- restore plan (written by the setup wizard) ---------------------------
    /// The plan, with each item's `available` set; None when there is none.
    pub fn restore_plan(&self) -> Option<RestorePlan> {
        let text = std::fs::read_to_string(&self.0.plan_path).ok()?;
        let Ok(mut plan) = serde_json::from_str::<RestorePlan>(&text) else {
            return Some(RestorePlan::unreadable());
        };
        for it in &mut plan.items {
            let configured = self.0.cfg.suite(&it.suite).and_then(|s| s.app(&it.app)).is_some();
            it.available = Some(configured && self.0.store.read_manifest(&it.suite, &it.version).is_some());
        }
        Some(plan)
    }

    pub(crate) fn write_restore_plan(&self, plan: &RestorePlan) -> Result<()> {
        if plan.items.is_empty() {
            return self.discard_restore_plan();
        }
        crate::files::write_atomic(&self.0.plan_path, &serde_json::to_vec_pretty(plan)?, None)?;
        Ok(())
    }

    pub fn discard_restore_plan(&self) -> Result<()> {
        match std::fs::remove_file(&self.0.plan_path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => {
                if let Some(parent) = self.0.plan_path.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
                Ok(())
            }
        }
    }

    pub async fn prune_suite(&self, suite: &str) -> Result<Vec<String>> {
        let st = self.settings(suite).await?;
        let suite = suite.to_owned();
        self.blocking(move |app| app.store().prune(&suite, st.keep_last, st.keep_days)).await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::jobs::JobOutcome;

    pub(crate) async fn app() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().expect("temp");
        let mut cfg = crate::service::apps::parse(
            r#"
[[suite]]
name = "media"
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "http://127.0.0.1:1"
api_key = "test"
"#,
            |_| None,
        )
        .expect("config");
        cfg.storage_root = dir.path().join("backups");
        cfg.state_db = dir.path().join("state.db");
        (dir, App::new(cfg, Deployment::default()).await.expect("app"))
    }

    #[tokio::test]
    async fn shutdown_finishes_active_job_and_fails_queued_work() {
        let (_dir, app) = app().await;
        app.start().await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let first = app
            .submit("media", "test", "", HashSet::new(), move |_| async move {
                started_tx.send(()).expect("started");
                finish_rx.await.expect("finish");
                Ok(JobOutcome::ok(json!({})))
            })
            .await
            .expect("first");
        started_rx.await.expect("started");
        let queued = app
            .submit("other", "test", "", HashSet::from(["held".into()]), |_| async {
                panic!("queued job must not run")
            })
            .await
            .expect("queued");
        app.begin_shutdown().await;
        let third = app.submit("third", "test", "", HashSet::new(), |_| async { Ok(JobOutcome::ok(json!({}))) }).await;
        assert!(third.is_err());
        finish_tx.send(()).expect("finish");
        tokio::time::timeout(std::time::Duration::from_secs(5), app.shutdown())
            .await
            .expect("shutdown")
            .expect("worker");
        assert_eq!(app.state().get_run(first).await.expect("get").expect("row").status, "ok");
        assert_eq!(app.state().get_run(queued).await.expect("get").expect("row").status, "failed");
        assert!(!app.busy("held").await.expect("busy"));
        assert!(!app.busy("other").await.expect("busy"));
    }

    #[tokio::test]
    async fn a_finished_run_drops_its_suites_overviews() {
        let (_dir, app) = app().await;
        app.start().await;
        for suite in ["media", "other", "kept"] {
            app.overview_cache().put(suite, json!({"suite": suite}));
        }
        app.submit("media", "test", "", HashSet::from(["other".into()]), |_| async { Ok(JobOutcome::ok(json!({}))) })
            .await
            .expect("submit");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while app.overview_cache().fresh("media").is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the run finished");
        assert!(app.overview_cache().fresh("other").is_none(), "a held suite's overview goes too");
        assert!(app.overview_cache().fresh("kept").is_some());
        app.shutdown().await.expect("shutdown");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_work_does_not_stall_runtime() {
        let (_dir, app) = app().await;
        let runtime_thread = std::thread::current().id();
        app.blocking(move |_| {
            assert_ne!(std::thread::current().id(), runtime_thread);
            Ok(())
        })
        .await
        .expect("blocking");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_settings_updates_preserve_each_change() {
        let (_dir, app) = app().await;
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let app = app.clone();
                tokio::spawn(async move {
                    app.modify_settings("media", |mut current| {
                        current.keep_last += 1;
                        Ok(current)
                    })
                    .await
                    .expect("settings")
                })
            })
            .collect();
        for task in tasks {
            task.await.expect("task");
        }
        assert_eq!(app.settings("media").await.expect("settings").keep_last, 30);
    }
}
