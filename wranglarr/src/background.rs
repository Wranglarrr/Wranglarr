// SPDX-License-Identifier: GPL-3.0-or-later
//! Background work a subsystem starts and the service stops: the connect and
//! indexers start-up tasks. Each owner keeps its tasks in a [`Background`];
//! `App::shutdown` stops them, and a panic is logged with the task's name
//! rather than lost with a detached handle.

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;

use futures::FutureExt;
use tokio::task::JoinSet;

/// The tasks one subsystem runs in the background.
#[derive(Default)]
pub struct Background {
    tasks: Mutex<JoinSet<()>>,
}

impl Background {
    /// Run `work` on the current runtime as `name`. Finished tasks are reaped
    /// here; a panic is logged.
    pub fn spawn(&self, name: &'static str, work: impl Future<Output = ()> + Send + 'static) {
        let Ok(mut tasks) = self.tasks.lock() else { return };
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            if let Err(panic) = AssertUnwindSafe(work).catch_unwind().await {
                tracing::error!("{name}: stopped by a panic: {}", panic_text(panic.as_ref()));
            }
        });
    }

    /// How many are still running.
    #[cfg(test)]
    pub fn running(&self) -> usize {
        self.tasks.lock().map_or(0, |mut t| {
            while t.try_join_next().is_some() {}
            t.len()
        })
    }

    /// Stop every task and wait until they have ended.
    pub async fn shutdown(&self) {
        let Some(mut tasks) = self.tasks.lock().ok().map(|mut t| std::mem::take(&mut *t)) else { return };
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
}

fn panic_text(panic: &(dyn Any + Send)) -> &str {
    panic.downcast_ref::<&str>().copied().or_else(|| panic.downcast_ref::<String>().map(String::as_str)).unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn shutdown_stops_what_is_running() {
        let bg = Background::default();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        bg.spawn("forever", async move {
            let _keep = tx;
            std::future::pending::<()>().await;
        });
        assert_eq!(bg.running(), 1);
        tokio::time::timeout(Duration::from_secs(1), bg.shutdown()).await.expect("stopped");
        assert!(rx.await.is_err(), "the task was dropped");
        assert_eq!(bg.running(), 0);
    }

    #[tokio::test]
    async fn a_panic_ends_only_its_task() {
        let bg = Background::default();
        bg.spawn("boom", async { panic!("boom") });
        let (tx, rx) = tokio::sync::oneshot::channel();
        bg.spawn("fine", async move {
            let _ = tx.send(());
        });
        tokio::time::timeout(Duration::from_secs(1), rx).await.expect("ran").expect("sent");
        tokio::time::timeout(Duration::from_secs(1), async {
            while bg.running() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both ended");
    }
}
