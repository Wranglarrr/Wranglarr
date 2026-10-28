// SPDX-License-Identifier: GPL-3.0-or-later
//! The job runner: jobs (backups, restores, syncs, updates) run one at a time
//! on a single worker.
//!
//! A suite counts as busy while it has a queued or running run, or while
//! another run holds it (a restore plan across suites, an update's pre-update
//! backups). Admission is one gate: a destructive operation that needs a suite
//! idle holds it, so no run can be admitted in between.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::error::{Error, Result};
use crate::state::{RunStatus, State};

/// How a job ended: the run's status and the result stored with it.
#[derive(Debug, Clone, PartialEq)]
pub struct JobOutcome {
    pub status: RunStatus,
    pub result: Value,
    /// The run carries on in another process, which finishes it.
    handed_off: bool,
}

impl JobOutcome {
    pub fn new(status: RunStatus, result: Value) -> JobOutcome {
        JobOutcome { status, result, handed_off: false }
    }

    /// Everything done.
    pub fn ok(result: Value) -> JobOutcome {
        JobOutcome::new(RunStatus::Ok, result)
    }

    /// A job that stopped with an error.
    fn failed(e: impl std::fmt::Display) -> JobOutcome {
        JobOutcome::new(RunStatus::Failed, json!({"error": e.to_string()}))
    }

    /// The run goes on in another process (wranglarr's own update, which
    /// replaces this one): it stays open here, and that process finishes it
    /// ([`JobRunner::adopt`]).
    pub fn handed_off() -> JobOutcome {
        JobOutcome { status: RunStatus::Running, result: Value::Null, handed_off: true }
    }

    pub fn is_handed_off(&self) -> bool {
        self.handed_off
    }
}

/// What a job returns. An error fails the run, with `{"error": …}` as its result.
pub type JobResult = Result<JobOutcome>;

type JobFuture = Pin<Box<dyn Future<Output = JobResult> + Send + 'static>>;
type Job = Box<dyn FnOnce(RunLog) -> JobFuture + Send + 'static>;

/// Appends to a run's log (and the process log).
#[derive(Clone)]
pub struct RunLog {
    state: State,
    pub run_id: i64,
    shutdown: watch::Receiver<bool>,
}

impl RunLog {
    /// Resolves once the service has begun shutting down. A job that waits
    /// on another process can stop waiting then.
    pub async fn stopping(&self) {
        let mut rx = self.shutdown.clone();
        // a closed channel means the runner is gone: shutting down too
        let _ = rx.wait_for(|stop| *stop).await;
    }

    /// Add a line to the run's log, in order, and to the process log.
    pub async fn append(&self, msg: impl AsRef<str>) {
        let msg = msg.as_ref();
        tracing::info!("run {}: {msg}", self.run_id);
        if let Err(e) = self.state.append_log(self.run_id, msg).await {
            tracing::warn!("run {}: could not save log line: {e}", self.run_id);
        }
    }
}

/// A run that ended, as the worker hands it to its owner: after the result
/// is saved and the run's holds are released.
#[derive(Debug, Clone)]
pub struct Finished {
    pub run_id: i64,
    pub suite: String,
    pub kind: String,
    pub target: String,
    /// The other suites the run held.
    pub also: HashSet<String>,
    pub status: RunStatus,
    pub result: Value,
}

struct Queued {
    run_id: i64,
    suite: String,
    kind: String,
    target: String,
    also: HashSet<String>,
    job: Job,
}

/// The queue, its single worker, admission and holds.
#[derive(Clone)]
pub struct JobRunner(Arc<Shared>);

struct Shared {
    state: State,
    queue: mpsc::UnboundedSender<Queued>,
    /// Taken by the worker when it starts.
    queue_rx: Mutex<Option<mpsc::UnboundedReceiver<Queued>>>,
    /// Held while a run is admitted, and through a deletion that needs its suite idle.
    admission: tokio::sync::Mutex<()>,
    stopping: AtomicBool,
    shutdown: watch::Sender<bool>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// run id -> other suites that run reads or writes
    held: Mutex<HashMap<i64, HashSet<String>>>,
}

impl JobRunner {
    /// A runner that records its runs in `state`. Nothing runs until [`start`](Self::start).
    pub fn new(state: State) -> JobRunner {
        let (queue, queue_rx) = mpsc::unbounded_channel();
        JobRunner(Arc::new(Shared {
            state,
            queue,
            queue_rx: Mutex::new(Some(queue_rx)),
            admission: tokio::sync::Mutex::new(()),
            stopping: AtomicBool::new(false),
            shutdown: watch::channel(false).0,
            worker: Mutex::new(None),
            held: Mutex::new(HashMap::new()),
        }))
    }

    /// Start the worker; `on_finish` hears about each run that ends. Once:
    /// later calls do nothing.
    pub fn start(&self, on_finish: impl Fn(Finished) + Send + 'static) {
        let rx = self.0.queue_rx.lock().ok().and_then(|mut g| g.take());
        if let Some(rx) = rx {
            let worker = tokio::spawn(self.clone().work(rx, on_finish));
            *self.0.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);
        }
    }

    /// Shutdown has begun: nothing new is admitted.
    pub fn is_stopping(&self) -> bool {
        self.0.stopping.load(Ordering::Acquire)
    }

    /// Stop admitting runs and tell the worker to fail what is queued. The
    /// active job carries on: cancelling an arbitrary restore could leave a
    /// container stopped or its config partly replaced.
    pub fn begin_shutdown(&self) {
        self.0.stopping.store(true, Ordering::Release);
        self.0.shutdown.send_replace(true);
    }

    /// Wait for the worker to end (after [`begin_shutdown`](Self::begin_shutdown)).
    pub async fn join(&self) -> Result<()> {
        let worker = self.0.worker.lock().map_err(|_| Error::internal("worker lock poisoned"))?.take();
        if let Some(worker) = worker {
            worker.await?;
        }
        Ok(())
    }

    /// A run of this suite is queued or running, or another run uses its
    /// versions. Deleting or pruning versions waits for it; new runs just
    /// queue, since jobs run one at a time.
    pub async fn busy(&self, suite: &str) -> Result<bool> {
        if self.0.state.active_run(suite).await?.is_some() {
            return Ok(true);
        }
        Ok(self.0.held.lock().map(|h| h.values().any(|s| s.contains(suite))).unwrap_or(false))
    }

    /// Exclude admission throughout a destructive operation. busy() alone is
    /// only an observation and cannot protect a subsequent deletion.
    pub async fn while_idle<T, Fut>(&self, suite: &str, f: impl FnOnce() -> Fut) -> Result<T>
    where
        Fut: Future<Output = Result<T>>,
    {
        let _gate = self.0.admission.lock().await;
        if self.busy(suite).await? {
            return Err(Error::conflict("a run is in progress"));
        }
        f().await
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
        let _gate = self.0.admission.lock().await;
        if self.is_stopping() {
            return Err(Error::internal("job worker is gone"));
        }
        let run_id = self.0.state.reserve_run(suite, kind, target).await?;
        let others: HashSet<String> = also.into_iter().filter(|s| s != suite).collect();
        if !others.is_empty()
            && let Ok(mut h) = self.0.held.lock()
        {
            h.insert(run_id, others.clone());
        }
        let job: Job = Box::new(move |rl| Box::pin(f(rl)));
        self.enqueue(Queued {
            run_id,
            suite: suite.into(),
            kind: kind.into(),
            target: target.into(),
            also: others,
            job,
        })
        .await?;
        Ok(run_id)
    }

    /// Queue a job for a run another process handed over
    /// ([`JobOutcome::handed_off`]): the run keeps its id, its log and its place
    /// as the running one, and this job finishes it.
    pub async fn adopt<F, Fut>(&self, run_id: i64, f: F) -> Result<()>
    where
        F: FnOnce(RunLog) -> Fut + Send + 'static,
        Fut: Future<Output = JobResult> + Send + 'static,
    {
        let _gate = self.0.admission.lock().await;
        if self.is_stopping() {
            return Err(Error::internal("job worker is gone"));
        }
        let run = self.0.state.get_run(run_id).await?.ok_or_else(|| Error::not_found(format!("no run {run_id}")))?;
        if run.finished.is_some() {
            return Err(Error::conflict(format!("run {run_id} has already finished")));
        }
        let job: Job = Box::new(move |rl| Box::pin(f(rl)));
        let target = run.target.unwrap_or_default();
        self.enqueue(Queued { run_id, suite: run.suite, kind: run.kind, target, also: HashSet::new(), job }).await
    }

    async fn enqueue(&self, q: Queued) -> Result<()> {
        let run_id = q.run_id;
        if self.0.queue.send(q).is_err() {
            self.0.held.lock().map_err(|_| Error::internal("job holds lock poisoned"))?.remove(&run_id);
            self.0.state.finish_run(run_id, RunStatus::Failed, json!({"error": "job worker is gone"})).await?;
            return Err(Error::internal("job worker is gone"));
        }
        Ok(())
    }

    async fn work(self, mut rx: mpsc::UnboundedReceiver<Queued>, on_finish: impl Fn(Finished)) {
        let mut shutdown = self.0.shutdown.subscribe();
        loop {
            let q = tokio::select! {
                biased;
                _ = async {
                    if !*shutdown.borrow() { let _ = shutdown.changed().await; }
                } => {
                    rx.close();
                    while let Some(q) = rx.recv().await { self.fail_queued(q).await; }
                    return;
                }
                q = rx.recv() => match q { Some(q) => q, None => return },
            };
            let rl = RunLog { state: self.0.state.clone(), run_id: q.run_id, shutdown: self.0.shutdown.subscribe() };
            let run_id = q.run_id;
            if let Err(e) = self.0.state.set_status(run_id, RunStatus::Running).await {
                tracing::error!("run {run_id}: could not start: {e}");
                self.fail_queued(q).await;
                continue;
            }
            // a panic in a job fails that run, not the worker
            let job_log = rl.clone();
            // Invoke the factory inside the task too: it can panic before
            // returning a future, and must not take down the worker.
            let mut job = tokio::spawn(async move { (q.job)(job_log).await });
            let outcome = tokio::select! {
                outcome = &mut job => outcome,
                _ = async {
                    if !*shutdown.borrow() { let _ = shutdown.changed().await; }
                } => {
                    rx.close();
                    while let Some(queued) = rx.recv().await { self.fail_queued(queued).await; }
                    job.await
                }
            };
            let JobOutcome { status, result, .. } = match outcome {
                Ok(Ok(done)) if done.is_handed_off() => {
                    // another process finishes it: leave it running, release its holds
                    if let Ok(mut h) = self.0.held.lock() {
                        h.remove(&run_id);
                    }
                    tracing::info!("run {run_id} ({} {} {}) handed over", q.kind, q.suite, q.target);
                    continue;
                }
                Ok(Ok(done)) => done,
                Ok(Err(e)) => {
                    rl.append(format!("FAILED: {e}")).await;
                    JobOutcome::failed(e)
                }
                Err(e) => {
                    rl.append(format!("FAILED: {e}")).await;
                    JobOutcome::failed(e)
                }
            };
            if let Err(e) = self.0.state.finish_run(run_id, status, result.clone()).await {
                tracing::error!("run {}: could not save the result: {e}", q.run_id);
            }
            if let Ok(mut h) = self.0.held.lock() {
                h.remove(&q.run_id);
            }
            tracing::debug!("run {run_id} ({} {} {}) finished: {status}", q.kind, q.suite, q.target);
            on_finish(Finished {
                run_id,
                suite: q.suite,
                kind: q.kind,
                target: q.target,
                also: q.also,
                status,
                result,
            });
        }
    }

    async fn fail_queued(&self, queued: Queued) {
        let id = queued.run_id;
        if let Err(e) =
            self.0.state.finish_run(id, RunStatus::Failed, json!({"error": "wranglarr shutting down"})).await
        {
            tracing::error!("run {id}: could not record shutdown: {e}");
        }
        if let Ok(mut held) = self.0.held.lock() {
            held.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn runner() -> JobRunner {
        JobRunner::new(State::memory().expect("state"))
    }

    fn ok() -> std::future::Ready<JobResult> {
        std::future::ready(Ok(JobOutcome::ok(json!({}))))
    }

    /// The run once it has finished (or panics after 5 s).
    async fn finished(runner: &JobRunner, run: i64) -> crate::state::Run {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let row = runner.0.state.get_run(run).await.expect("run").expect("row");
                if row.finished.is_some() {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("finished")
    }

    #[tokio::test]
    async fn outcome_is_stored_as_status_and_result() {
        let jobs = runner();
        jobs.start(|_| {});
        let partial = json!({"done": ["a"], "failed": ["b"]});
        let result = partial.clone();
        let run = jobs
            .submit("media", "test", "", HashSet::new(), move |_| async move {
                Ok(JobOutcome::new(RunStatus::Partial, result))
            })
            .await
            .expect("submit");
        let row = finished(&jobs, run).await;
        assert_eq!(row.status, "partial");
        assert_eq!(row.result, partial);
        let failed = jobs
            .submit("media", "test", "", HashSet::new(), |_| async { Err(Error::conflict("nope")) })
            .await
            .expect("submit");
        let row = finished(&jobs, failed).await;
        assert_eq!(row.status, "failed");
        assert_eq!(row.result, json!({"error": "nope"}));
    }

    #[tokio::test]
    async fn factory_panic_does_not_kill_worker() {
        let jobs = runner();
        jobs.start(|_| {});
        let first = jobs
            .submit("media", "test", "", HashSet::new(), |_| -> std::future::Ready<JobResult> {
                panic!("factory panic")
            })
            .await
            .expect("submit");
        assert_eq!(finished(&jobs, first).await.status, "failed", "worker survived");
        let second =
            jobs.submit("media", "test", "", HashSet::new(), |_| async { panic!("job panic") }).await.expect("submit");
        assert_eq!(finished(&jobs, second).await.status, "failed", "worker survived");
        let third = jobs.submit("media", "test", "", HashSet::new(), |_| ok()).await.expect("submit again");
        assert_eq!(finished(&jobs, third).await.status, "ok", "next job completes");
    }

    #[tokio::test]
    async fn closed_queue_releases_run_and_holds() {
        let jobs = runner();
        drop(jobs.0.queue_rx.lock().expect("lock").take());
        let result = jobs.submit("media", "test", "", HashSet::from(["other".into()]), |_| ok()).await;
        assert!(result.is_err());
        assert!(!jobs.busy("media").await.expect("busy"));
        assert!(!jobs.busy("other").await.expect("held"));
        let runs = jobs.0.state.list_runs(None, 1).await.expect("runs");
        assert_eq!(runs[0].status, "failed");
        assert!(runs[0].finished.is_some());
    }

    #[tokio::test]
    async fn jobs_run_one_at_a_time_across_suites() {
        let jobs = runner();
        jobs.start(|_| {});
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let first = jobs
            .submit("media", "test", "", HashSet::new(), move |_| async move {
                started_tx.send(()).expect("started");
                release_rx.await.expect("release");
                Ok(JobOutcome::ok(json!({})))
            })
            .await
            .expect("first");
        started_rx.await.expect("started");
        let second = jobs.submit("other", "test", "", HashSet::new(), |_| ok()).await.expect("second");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let waiting = jobs.0.state.get_run(second).await.expect("get").expect("row");
        assert_eq!(waiting.status, "queued", "another suite's run waits for the active one");
        assert!(jobs.busy("other").await.expect("busy"), "a queued run makes its suite busy");
        release_tx.send(()).expect("release");
        assert_eq!(finished(&jobs, first).await.status, "ok");
        assert_eq!(finished(&jobs, second).await.status, "ok");
    }

    #[tokio::test]
    async fn holds_last_until_the_run_ends_and_then_it_is_reported() {
        let jobs = runner();
        let (done_tx, mut done_rx) = mpsc::unbounded_channel();
        let watcher = jobs.clone();
        jobs.start(move |f: Finished| {
            // holds are released before the owner hears of the run
            let held = watcher.0.held.lock().expect("held").contains_key(&f.run_id);
            done_tx.send((f, held)).expect("report");
        });
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let run = jobs
            .submit("media", "restore", "v1", HashSet::from(["media".into(), "other".into()]), move |_| async move {
                started_tx.send(()).expect("started");
                release_rx.await.expect("release");
                Ok(JobOutcome::ok(json!({"n": 1})))
            })
            .await
            .expect("submit");
        started_rx.await.expect("started");
        assert!(jobs.busy("other").await.expect("busy"), "the held suite is busy while the run is");
        let deleted = jobs.while_idle("other", || async { Ok(()) }).await;
        assert!(matches!(deleted, Err(Error::Conflict(_))), "a held suite is not idle");
        release_tx.send(()).expect("release");
        let (f, still_held) =
            tokio::time::timeout(Duration::from_secs(5), done_rx.recv()).await.expect("reported").expect("finished");
        assert!(!still_held);
        assert_eq!((f.run_id, f.suite.as_str(), f.kind.as_str(), f.target.as_str()), (run, "media", "restore", "v1"));
        assert_eq!(f.also, HashSet::from(["other".to_string()]), "the run's own suite is not a hold");
        assert_eq!((f.status, f.result), (RunStatus::Ok, json!({"n": 1})));
        assert!(!jobs.busy("other").await.expect("busy"));
    }

    #[tokio::test]
    async fn shutdown_finishes_active_job_and_fails_queued_work() {
        let jobs = runner();
        jobs.start(|_| {});
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let first = jobs
            .submit("media", "test", "", HashSet::new(), move |_| async move {
                started_tx.send(()).expect("started");
                finish_rx.await.expect("finish");
                Ok(JobOutcome::ok(json!({})))
            })
            .await
            .expect("first");
        started_rx.await.expect("started");
        let queued = jobs
            .submit("other", "test", "", HashSet::from(["held".into()]), |_| async {
                panic!("queued job must not run")
            })
            .await
            .expect("queued");
        jobs.begin_shutdown();
        assert!(jobs.is_stopping());
        assert!(jobs.submit("third", "test", "", HashSet::new(), |_| ok()).await.is_err());
        finish_tx.send(()).expect("finish");
        tokio::time::timeout(Duration::from_secs(5), jobs.join()).await.expect("shutdown").expect("worker");
        assert_eq!(jobs.0.state.get_run(first).await.expect("get").expect("row").status, "ok");
        let failed = jobs.0.state.get_run(queued).await.expect("get").expect("row");
        assert_eq!((failed.status.as_str(), failed.result), ("failed", json!({"error": "wranglarr shutting down"})));
        assert!(!jobs.busy("held").await.expect("busy"));
        assert!(!jobs.busy("other").await.expect("busy"));
    }

    #[tokio::test]
    async fn deletion_and_admission_are_mutually_exclusive() {
        let jobs = runner();
        jobs.while_idle("media", || async {
            assert!(jobs.0.admission.try_lock().is_err());
            Ok(())
        })
        .await
        .expect("idle");
        jobs.submit("media", "test", "", HashSet::new(), |_| ok()).await.expect("run");
        assert!(matches!(jobs.while_idle("media", || async { Ok(()) }).await, Err(Error::Conflict(_))));
    }

    #[tokio::test]
    async fn a_handed_off_run_stays_open_until_another_runner_adopts_it() {
        let state = State::memory().expect("state");
        // the old process: its job waits for shutdown, then hands the run over
        let old = JobRunner::new(state.clone());
        let (heard_tx, mut heard_rx) = mpsc::unbounded_channel();
        old.start(move |f: Finished| heard_tx.send(f.run_id).expect("heard"));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let run = old
            .submit(
                "system",
                "self-update",
                "wranglarr",
                HashSet::from(["media".into()]),
                move |rl: RunLog| async move {
                    rl.append("helper started").await;
                    started_tx.send(()).expect("started");
                    rl.stopping().await;
                    Ok(JobOutcome::handed_off())
                },
            )
            .await
            .expect("submit");
        started_rx.await.expect("started");
        old.begin_shutdown();
        tokio::time::timeout(Duration::from_secs(5), old.join()).await.expect("shutdown").expect("worker");
        let row = state.get_run(run).await.expect("get").expect("row");
        assert_eq!((row.status.as_str(), row.finished.as_deref()), ("running", None), "left open");
        assert!(heard_rx.try_recv().is_err(), "nobody is told a handed-off run ended");
        assert!(old.0.held.lock().expect("held").is_empty(), "its holds are released");

        // the new process finishes it, log and all
        let new = JobRunner::new(state.clone());
        let (done_tx, mut done_rx) = mpsc::unbounded_channel();
        new.start(move |f: Finished| done_tx.send(f).expect("done"));
        new.adopt(run, |rl: RunLog| async move {
            rl.append("new wranglarr is up").await;
            Ok(JobOutcome::ok(json!({"results": {"wranglarr": "updated"}})))
        })
        .await
        .expect("adopt");
        let f = tokio::time::timeout(Duration::from_secs(5), done_rx.recv()).await.expect("in time").expect("done");
        assert_eq!(
            (f.run_id, f.kind.as_str(), f.target.as_str(), f.status),
            (run, "self-update", "wranglarr", RunStatus::Ok)
        );
        let row = finished(&new, run).await;
        assert_eq!(row.status, "ok");
        assert!(row.log.contains("helper started") && row.log.contains("new wranglarr is up"), "{}", row.log);
        assert!(matches!(new.adopt(run, |_| ok()).await, Err(Error::Conflict(_))), "only an open run");
        assert!(matches!(new.adopt(run + 99, |_| ok()).await, Err(Error::NotFound(_))));
    }
}
