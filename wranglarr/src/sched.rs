// SPDX-License-Identifier: GPL-3.0-or-later
//! Cron schedules: five-field crontab expressions in the container's time
//! zone (TZ). Standard cron weekdays: 0 and 7 are Sunday.
//!
//! A missed run (the machine slept, wranglarr was busy) isn't made up for:
//! the next run is computed from the time the last one finished.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use croner::Cron;
use croner::parser::{CronParser, Seconds};

/// A boxed future that can move between threads: what a scheduled task (and a
/// queued job) runs.
pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub fn parse(expr: &str) -> Result<Cron, String> {
    CronParser::builder().seconds(Seconds::Disallowed).build().parse(expr.trim()).map_err(|e| e.to_string())
}

/// The next time `expr` fires after now (local time).
pub fn next_after(cron: &Cron, after: DateTime<Local>) -> Option<DateTime<Local>> {
    cron.find_next_occurrence(&after, false).ok()
}

type Task = Arc<dyn Fn() -> BoxFut<()> + Send + Sync>;

async fn run_task(task: Task) -> Result<(), tokio::task::JoinError> {
    tokio::spawn(async move { task().await }).await
}

struct Entry {
    handle: tokio::task::JoinHandle<()>,
    next: Arc<Mutex<Option<DateTime<Local>>>>,
}

#[derive(Default)]
pub struct Scheduler {
    jobs: Mutex<HashMap<String, Entry>>,
}

impl Scheduler {
    /// (Re)place job `id` on schedule `expr`. Each run waits for the one before.
    pub fn add<F>(&self, id: &str, expr: &str, f: F) -> Result<(), String>
    where
        F: Fn() -> BoxFut<()> + Send + Sync + 'static,
    {
        let cron = parse(expr)?;
        // Serialize replacement as one operation. remove() followed by insert()
        // can orphan a concurrently installed task whose handle is overwritten.
        let mut jobs = self.jobs.lock().map_err(|_| "scheduler lock poisoned".to_string())?;
        if let Some(old) = jobs.remove(id) {
            old.handle.abort();
        }
        let task: Task = Arc::new(f);
        let next = Arc::new(Mutex::new(next_after(&cron, Local::now())));
        let next2 = next.clone();
        let id2 = id.to_string();
        let handle = tokio::spawn(async move {
            let mut last: Option<DateTime<Local>> = None;
            loop {
                let cursor = last.map_or_else(Local::now, |at| at.max(Local::now()));
                let at = next_after(&cron, cursor);
                if let Ok(mut n) = next2.lock() {
                    *n = at;
                }
                let Some(at) = at else {
                    tracing::warn!("schedule {id2}: never fires again");
                    return;
                };
                // Tokio sleeps are monotonic; recheck wall time so clock
                // corrections do not leave a cron asleep for hours.
                loop {
                    let wait = (at - Local::now()).to_std().unwrap_or_default();
                    if wait.is_zero() {
                        break;
                    }
                    tokio::time::sleep(wait.min(std::time::Duration::from_secs(30))).await;
                }
                last = Some(at);
                // Replacing a schedule cancels its timer, not work already
                // started (which might be restoring data).
                let task = task.clone();
                if let Err(e) = run_task(task).await {
                    tracing::error!("schedule {id2}: task failed: {e}");
                }
            }
        });
        jobs.insert(id.to_string(), Entry { handle, next });
        Ok(())
    }

    pub fn remove(&self, id: &str) {
        if let Some(e) = self.jobs.lock().ok().and_then(|mut j| j.remove(id)) {
            e.handle.abort();
        }
    }

    /// Cancel future firings while allowing work already started to finish.
    pub fn clear(&self) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        for (_, entry) in jobs.drain() {
            entry.handle.abort();
        }
    }

    pub fn has(&self, id: &str) -> bool {
        self.jobs.lock().is_ok_and(|j| j.contains_key(id))
    }

    pub fn next_run(&self, id: &str) -> Option<DateTime<Local>> {
        let jobs = self.jobs.lock().ok()?;

        *jobs.get(id)?.next.lock().ok()?
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // Dropping a JoinHandle detaches it; timers must not outlive their owner.
        let jobs = self.jobs.get_mut().unwrap_or_else(|e| e.into_inner());
        for (_, entry) in jobs.drain() {
            entry.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Timelike, Weekday};

    #[test]
    fn five_fields_only() {
        assert!(parse("0 3 * * *").is_ok());
        assert!(parse("0 0 3 * * *").is_err());
        assert!(parse("nonsense").is_err());
    }

    #[test]
    fn weekday_zero_is_sunday() {
        let c = parse("0 3 * * 0").expect("parses");
        let from = Local.with_ymd_and_hms(2026, 10, 26, 0, 0, 0).single().expect("time"); // a Monday
        let n = next_after(&c, from).expect("next");
        assert_eq!(n.weekday(), Weekday::Sun);
        assert_eq!(n.hour(), 3);
    }
    #[tokio::test]
    async fn replacement_and_drop_cancel_timers() {
        let scheduler = Scheduler::default();
        scheduler.add("job", "* * * * *", || Box::pin(async {})).expect("add");
        let old = scheduler.jobs.lock().expect("jobs")["job"].handle.abort_handle();
        scheduler.add("job", "* * * * *", || Box::pin(async {})).expect("replace");
        tokio::task::yield_now().await;
        assert!(old.is_finished());
        assert_eq!(scheduler.jobs.lock().expect("jobs").len(), 1);
        let current = scheduler.jobs.lock().expect("jobs")["job"].handle.abort_handle();
        drop(scheduler);
        tokio::task::yield_now().await;
        assert!(current.is_finished());
    }

    #[test]
    fn next_occurrence_is_strictly_after_fractional_second() {
        let cron = parse("* * * * *").expect("cron");
        let from = Local
            .with_ymd_and_hms(2026, 10, 26, 0, 1, 0)
            .single()
            .expect("time")
            .with_nanosecond(1)
            .expect("nanosecond");
        let next = next_after(&cron, from).expect("next");
        assert!(next > from);
        assert_eq!(next.minute(), 2);
    }
    #[tokio::test]
    async fn cancelling_timer_preserves_in_flight_work() {
        let started = Arc::new(tokio::sync::Notify::new());
        let finish = Arc::new(tokio::sync::Notify::new());
        let completed = Arc::new(tokio::sync::Notify::new());
        let task: Task = {
            let (started, finish, completed) = (started.clone(), finish.clone(), completed.clone());
            Arc::new(move || {
                let (started, finish, completed) = (started.clone(), finish.clone(), completed.clone());
                Box::pin(async move {
                    started.notify_one();
                    finish.notified().await;
                    completed.notify_one();
                })
            })
        };
        let timer = tokio::spawn(run_task(task));
        started.notified().await;
        timer.abort();
        finish.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), completed.notified()).await.expect("work survives");
    }
    #[test]
    fn dst_fixed_time_policy() {
        // TZ is process-global; isolate it instead of mutating the test runner's
        // environment while other tests are constructing local timestamps.
        if std::env::var("WRANGLARR_DST_TEST").as_deref() != Ok("1") {
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", "sched::tests::dst_fixed_time_policy", "--nocapture"])
                .env("TZ", "America/Toronto")
                .env("WRANGLARR_DST_TEST", "1")
                .output()
                .expect("child test");
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let from = Local.with_ymd_and_hms(2026, 3, 8, 1, 59, 0).single().expect("before spring forward");
        let next = next_after(&parse("30 2 * * *").expect("cron"), from).expect("next");
        // croner coalesces a fixed time inside the gap to its first valid instant.
        assert_eq!((next.month(), next.day(), next.hour(), next.minute()), (3, 8, 3, 0));
        let from = Local.with_ymd_and_hms(2026, 11, 1, 0, 59, 0).single().expect("before fall back");
        let cron = parse("30 1 * * *").expect("cron");
        let first = next_after(&cron, from).expect("first fold");
        let next = next_after(&cron, first).expect("next day");
        assert_eq!((first.day(), first.hour(), first.minute()), (1, 1, 30));
        assert_eq!((next.day(), next.hour(), next.minute()), (2, 1, 30));
    }
}
