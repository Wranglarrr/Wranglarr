// SPDX-License-Identifier: GPL-3.0-or-later
//! SQLite state: per-suite settings overrides, the run history, update history
//! and a key/value table the other modules keep their settings in.
//!
//! The schema is fixed: an existing wranglarr.db must keep working.
//!
//! One thread owns the connection. [`State`] is a cloneable handle that sends
//! it requests and gets the answers back, so SQLite (its file IO, and its
//! busy wait while another process holds the lock) never runs on an async
//! runtime thread. Async code awaits [`State::call`] or the typed methods;
//! code already on a blocking thread uses [`State::call_blocking`], which
//! refuses to run on a runtime thread. A read-modify-write is one request:
//! [`State::write`] runs it in a single `BEGIN IMMEDIATE` transaction.

mod db;

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::mpsc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::oneshot;

pub use db::{Db, Tx};

use crate::error::{Error, Result};

/// A run's status, as the `runs` table stores it and the API shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Queued,
    Running,
    /// Finished, everything done.
    Ok,
    /// Finished, some of it failed.
    Partial,
    /// Finished, nothing done (or the job stopped with an error).
    Failed,
}

impl RunStatus {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::Ok => "ok",
            RunStatus::Partial => "partial",
            RunStatus::Failed => "failed",
        }
    }

    /// The status of a job made of several steps: ok when none failed, partial
    /// when some failed and some worked, failed when none worked.
    pub fn of_steps(any_failed: bool, any_worked: bool) -> RunStatus {
        match (any_failed, any_worked) {
            (false, _) => RunStatus::Ok,
            (true, true) => RunStatus::Partial,
            (true, false) => RunStatus::Failed,
        }
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SuiteSettings {
    pub schedule: String,
    pub enabled: bool,
    pub keep_last: i64,
    pub keep_days: i64,
    /// "config" (wranglarr.toml) or "ui" (saved on the page).
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: i64,
    pub suite: String,
    pub kind: String,
    pub target: Option<String>,
    pub status: String,
    pub started: String,
    pub finished: Option<String>,
    pub log: String,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateRow {
    pub id: i64,
    pub container: String,
    pub image: String,
    pub old_image: Option<String>,
    pub new_image: Option<String>,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
    pub action: String,
    pub status: String,
    pub run_id: Option<i64>,
    pub at: String,
    pub note: Option<String>,
}

/// A row for the update history.
#[derive(Debug, Default, Clone)]
pub struct NewUpdate {
    pub container: String,
    pub image: String,
    pub action: String,
    pub status: String,
    pub run_id: Option<i64>,
    pub old_image: String,
    pub new_image: String,
    pub old_version: String,
    pub new_version: String,
    pub note: String,
}

/// What the worker runs: a request that answers on its own channel.
type Request = Box<dyn FnOnce(&Db) + Send>;

/// The handle to the state database. Clones share the one worker thread and
/// its connection; the thread ends when the last handle is dropped.
#[derive(Clone)]
pub struct State {
    requests: mpsc::Sender<Request>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State").finish_non_exhaustive()
    }
}

fn worker_gone() -> Error {
    Error::internal("the state database worker has stopped")
}

fn request_failed() -> Error {
    Error::internal("a state database request panicked")
}

impl State {
    /// The database at `path`, created (with its folder) when missing. Blocks
    /// until the schema is in place, which can wait on another process's
    /// lock: call it while starting up, off the runtime.
    pub fn open(path: &Path) -> Result<State> {
        let path = path.to_owned();
        State::start(move || Db::open(&path))
    }

    /// An in-memory database, for tests.
    pub fn memory() -> Result<State> {
        State::start(Db::memory)
    }

    fn start(open: impl FnOnce() -> Result<Db> + Send + 'static) -> Result<State> {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (ready, opened) = mpsc::sync_channel::<Result<()>>(1);
        std::thread::Builder::new().name("wranglarr-state".into()).spawn(move || {
            let db = match open() {
                Ok(db) => db,
                Err(e) => {
                    let _ = ready.send(Err(e));
                    return;
                }
            };
            let _ = ready.send(Ok(()));
            // every handle gone ends the loop, and dropping `db` closes the connection
            for request in inbox {
                // a panicking request loses its answer (the caller gets an
                // error) and rolls back its transaction; the worker carries on
                if std::panic::catch_unwind(AssertUnwindSafe(|| request(&db))).is_err() {
                    tracing::error!("state: a database request panicked");
                }
            }
        })?;
        opened.recv().map_err(|_| worker_gone())??;
        Ok(State { requests })
    }

    fn send<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Db) -> Result<T> + Send + 'static,
    ) -> Result<oneshot::Receiver<Result<T>>> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Box::new(move |db| {
                let _ = reply.send(f(db)); // the caller may have gone; the work is done either way
            }))
            .map_err(|_| worker_gone())?;
        Ok(answer)
    }

    /// Run `f` on the database thread and wait for its answer. Keep `f` to
    /// SQL: every other request waits behind it.
    pub async fn call<T: Send + 'static>(&self, f: impl FnOnce(&Db) -> Result<T> + Send + 'static) -> Result<T> {
        self.send(f)?.await.map_err(|_| request_failed())?
    }

    /// [`State::call`] for code on a blocking thread: inside `spawn_blocking`,
    /// on a thread of its own, or with no runtime at all (the CLI).
    ///
    /// # Panics
    ///
    /// On an async runtime thread, where waiting would stall every task on
    /// it: await [`State::call`] there instead.
    pub fn call_blocking<T: Send + 'static>(&self, f: impl FnOnce(&Db) -> Result<T> + Send + 'static) -> Result<T> {
        self.send(f)?.blocking_recv().map_err(|_| request_failed())?
    }

    /// A read-modify-write in one `BEGIN IMMEDIATE` transaction (see
    /// [`Db::write`]): commits when `f` returns Ok, rolls back otherwise.
    pub async fn write<T: Send + 'static>(&self, f: impl FnOnce(&Tx) -> Result<T> + Send + 'static) -> Result<T> {
        self.call(move |db| db.write(f)).await
    }

    // ---- settings ------------------------------------------------------------
    pub async fn settings(&self, name: &str, defaults: SuiteSettings) -> Result<SuiteSettings> {
        let name = name.to_owned();
        self.call(move |db| db.settings(&name, defaults)).await
    }

    pub async fn save_settings(&self, name: &str, s: SuiteSettings) -> Result<()> {
        let name = name.to_owned();
        self.call(move |db| db.save_settings(&name, &s)).await
    }

    pub async fn reset_settings(&self, name: &str) -> Result<()> {
        let name = name.to_owned();
        self.call(move |db| db.reset_settings(&name)).await
    }

    // ---- key/value -------------------------------------------------------------
    pub async fn get_value(&self, key: &str) -> Result<Option<Value>> {
        let key = key.to_owned();
        self.call(move |db| db.get_value(&key)).await
    }

    /// A typed value; a stored value of the wrong shape reads as missing.
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        Ok(self.get_value(key).await?.and_then(|v| serde_json::from_value(v).ok()))
    }

    pub async fn put<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        let (key, value) = (key.to_owned(), serde_json::to_value(value)?);
        self.call(move |db| db.put(&key, &value)).await
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        let key = key.to_owned();
        self.call(move |db| db.delete(&key)).await
    }

    // ---- update history ----------------------------------------------------------
    pub async fn add_update(&self, u: NewUpdate) -> Result<()> {
        self.call(move |db| db.add_update(&u)).await
    }

    pub async fn list_updates(&self, container: Option<&str>, limit: i64) -> Result<Vec<UpdateRow>> {
        let container = container.map(str::to_owned);
        self.call(move |db| db.list_updates(container.as_deref(), limit)).await
    }

    // ---- runs ----------------------------------------------------------------------
    /// A queued run of `suite`, unless it already has one queued or running
    /// (a conflict).
    pub async fn reserve_run(&self, suite: &str, kind: &str, target: &str) -> Result<i64> {
        let (suite, kind, target) = (suite.to_owned(), kind.to_owned(), target.to_owned());
        self.call(move |db| db.reserve_run(&suite, &kind, &target)).await
    }

    pub async fn set_status(&self, run_id: i64, status: RunStatus) -> Result<()> {
        self.call(move |db| db.set_status(run_id, status)).await
    }

    pub async fn append_log(&self, run_id: i64, line: &str) -> Result<()> {
        let line = line.to_owned();
        self.call(move |db| db.append_log(run_id, &line)).await
    }

    /// Lines written down elsewhere, each with its own time (the state db's format).
    pub async fn append_log_lines(&self, run_id: i64, lines: Vec<(String, String)>) -> Result<()> {
        self.call(move |db| lines.iter().try_for_each(|(at, line)| db.append_log_at(run_id, at, line))).await
    }

    pub async fn finish_run(&self, run_id: i64, status: RunStatus, result: Value) -> Result<()> {
        self.call(move |db| db.finish_run(run_id, status, &result)).await
    }

    pub async fn get_run(&self, run_id: i64) -> Result<Option<Run>> {
        self.call(move |db| db.get_run(run_id)).await
    }

    pub async fn list_runs(&self, suite: Option<&str>, limit: i64) -> Result<Vec<Run>> {
        let suite = suite.map(str::to_owned);
        self.call(move |db| db.list_runs(suite.as_deref(), limit)).await
    }

    pub async fn active_run(&self, suite: &str) -> Result<Option<Run>> {
        let suite = suite.to_owned();
        self.call(move |db| db.active_run(&suite)).await
    }

    /// The suites with a queued or running run.
    pub async fn active_suites(&self) -> Result<HashSet<String>> {
        self.call(|db| db.active_suites()).await
    }

    /// Anything still queued/running at startup died with the previous process,
    /// except `keep`: a run handed over to this one.
    pub async fn abandon_stale_runs(&self, keep: Option<i64>) -> Result<()> {
        self.call(move |db| db.abandon_stale_runs(keep)).await
    }

    /// A consistent copy of the database at `path`, taken while it is in use.
    pub async fn snapshot_to(&self, path: &std::path::Path) -> Result<()> {
        let path = path.to_owned();
        self.call(move |db| db.snapshot_to(&path)).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rusqlite::Connection;

    use super::*;

    #[test]
    fn run_status_of_steps() {
        assert_eq!(RunStatus::of_steps(false, false), RunStatus::Ok);
        assert_eq!(RunStatus::of_steps(false, true), RunStatus::Ok);
        assert_eq!(RunStatus::of_steps(true, true), RunStatus::Partial);
        assert_eq!(RunStatus::of_steps(true, false), RunStatus::Failed);
        let all = [RunStatus::Queued, RunStatus::Running, RunStatus::Ok, RunStatus::Partial, RunStatus::Failed];
        let stored: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        assert_eq!(stored, ["queued", "running", "ok", "partial", "failed"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_reservations_have_one_winner() {
        let s = State::memory().expect("db");
        let tries: Vec<_> = (0..16)
            .map(|_| {
                let s = s.clone();
                tokio::spawn(async move { s.reserve_run("media", "backup", "").await })
            })
            .collect();
        let mut winners = 0;
        for t in tries {
            winners += usize::from(t.await.expect("task").is_ok());
        }
        assert_eq!(winners, 1);
        assert_eq!(s.list_runs(None, 100).await.expect("runs").len(), 1);
    }

    #[tokio::test]
    async fn typed_values_round_trip() {
        let s = State::memory().expect("db");
        s.put("a", &serde_json::json!({"x": 1})).await.expect("put");
        assert_eq!(s.get_value("a").await.expect("get"), Some(serde_json::json!({"x": 1})));
        assert_eq!(s.get::<String>("a").await.expect("get"), None, "the wrong shape reads as missing");
        s.delete("a").await.expect("del");
        assert_eq!(s.get_value("a").await.expect("get"), None);
    }

    #[test]
    fn writes_from_several_connections_lose_nothing() {
        // two handles on one file, each with its own worker and connection,
        // like the service and wranglarr-auth in another process
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("state.db");
        let dbs = [State::open(&path).expect("db"), State::open(&path).expect("db")];
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let db = dbs[i % 2].clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        db.call_blocking(|db| {
                            db.write(|tx| {
                                let n = tx.get_value("n")?.and_then(|v| v.as_i64()).unwrap_or(0);
                                std::thread::yield_now(); // widen the window a plain read-then-put loses updates in
                                tx.put("n", &(n + 1))
                            })
                        })
                        .expect("write");
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("thread");
        }
        let n = dbs[0].call_blocking(|db| db.get_value("n")).expect("get");
        assert_eq!(n, Some(serde_json::json!(200)));
    }

    #[tokio::test]
    async fn a_panicking_request_fails_alone() {
        let s = State::memory().expect("db");
        s.put("a", &1).await.expect("put");
        let r: Result<()> = s
            .write(|tx| {
                tx.put("a", &2)?;
                panic!("in the middle of a write");
            })
            .await;
        assert!(r.is_err());
        assert_eq!(s.get_value("a").await.expect("the worker carries on"), Some(serde_json::json!(1)));
    }

    #[test]
    fn a_missing_folder_that_cannot_be_made_fails_open() {
        let dir = tempfile::tempdir().expect("tmp");
        let file = dir.path().join("file");
        std::fs::write(&file, "").expect("file");
        assert!(State::open(&file.join("wranglarr.db")).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[should_panic]
    async fn call_blocking_refuses_a_runtime_thread() {
        let s = State::memory().expect("db");
        let _ = s.call_blocking(|db| db.get_value("a"));
    }

    /// Another connection (`wranglarr-auth`, say) holds the write lock: the
    /// write waits for it on the database thread, the runtime keeps going, and
    /// the write lands once the lock is released.
    #[tokio::test(flavor = "current_thread")]
    async fn a_held_lock_stalls_only_the_database() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("state.db");
        let s = State::open(&path).expect("db");
        let other = Connection::open(&path).expect("other connection");
        other.execute_batch("BEGIN IMMEDIATE; INSERT INTO kv VALUES ('held', '1', '');").expect("lock");

        let write = tokio::spawn({
            let s = s.clone();
            async move { s.put("k", &"v").await }
        });
        // on this one runtime thread, a write waiting on the lock here would
        // stop the timer and the task below from running at all
        let started = std::time::Instant::now();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let unrelated = tokio::spawn(async { 1 + 1 }).await.expect("task");
        assert_eq!(unrelated, 2);
        assert!(started.elapsed() < Duration::from_secs(2), "the runtime was stalled: {:?}", started.elapsed());
        assert!(!write.is_finished(), "the write waits for the other connection's lock");

        other.execute_batch("COMMIT;").expect("release");
        tokio::time::timeout(Duration::from_secs(10), write)
            .await
            .expect("the write lands once the lock is released")
            .expect("task")
            .expect("write");
        assert_eq!(s.get::<String>("k").await.expect("get").as_deref(), Some("v"));
        assert_eq!(s.get_value("held").await.expect("get"), Some(serde_json::json!(1)));
    }
}
