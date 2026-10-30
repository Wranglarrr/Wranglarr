// SPDX-License-Identifier: GPL-3.0-or-later
//! Replacing a container on a new image with the settings it had, waiting until
//! it is healthy, and putting the old one back when it isn't. Containers that
//! share its network namespace are recreated on the new one afterwards.
//! Swap callbacks live in `on_run`; `failure` owns finalization once they return.

mod create;
mod failure;
mod on_run;
mod swap;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::time::Instant;

use super::{OLD_SUFFIX, Target, Updater};
use crate::config::AppConfig;
use crate::error::{Error, Result};
use crate::jobs::RunLog;
use crate::jobs::restore::Prepared;
use crate::state::NewUpdate;
use crate::value::head;
pub(in crate::updates) use create::build_create_body;
use on_run::OnRun;
pub(in crate::updates) use swap::{Health, Log, SwapFacts, SwapFailed, Swapped, Swapper, retag, swap, wait_running};

/// What a recreate did to the containers sharing the recreated one's network.
#[derive(Debug, Default)]
pub(super) struct Recreated {
    pub(super) dependents: Vec<String>,
    /// dependent -> why it failed
    pub(super) dependents_failed: Vec<(String, String)>,
}

type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The app data that goes back with a swap, from a pre-update backup version.
/// The new version of an app may migrate its database (or config files) when it
/// starts, and the old version can't read them after that.
pub(super) enum DataBack {
    /// Nothing: the swap only changes the program.
    None,
    /// An update: when the new container was started and fails, the app's data
    /// goes back from this version before the old container starts again.
    IfFailed { suite: String, version: String },
    /// A rollback: the data goes back before the container on the old image is
    /// created.
    First(Box<Prepared>),
}

impl Updater {
    /// Point the tag back at the image that is still running (see [`retag`]).
    pub(super) async fn retag(&self, rl: &RunLog, reference: &str, image_id: &str) {
        if let Ok(d) = self.docker() {
            retag(d, rl, reference, image_id).await;
        }
    }

    pub(super) async fn record(&self, rl: &RunLog, mut u: NewUpdate) {
        u.run_id = Some(rl.run_id);
        let what = format!("{} {}", u.action, u.container);
        if let Err(e) = self.app.state().add_update(u).await {
            tracing::error!("updates: could not record {what}: {e}");
        }
    }

    /// Replace `t`'s container with one made from the same settings on `image`
    /// (default: its current image reference), then the containers sharing its
    /// network. On any failure the old container is put back.
    pub(super) fn recreate<'a>(
        &'a self,
        rl: &'a RunLog,
        t: &'a Target,
        image: Option<&'a str>,
        network_mode: Option<&'a str>,
        action: &'a str,
    ) -> Fut<'a, Result<Recreated>> {
        self.recreate_with(rl, t, image, network_mode, action, &DataBack::None)
    }

    /// [`recreate`](Self::recreate), with the app's data put back as `data` says.
    pub(super) fn recreate_with<'a>(
        &'a self,
        rl: &'a RunLog,
        t: &'a Target,
        image: Option<&'a str>,
        network_mode: Option<&'a str>,
        action: &'a str,
        data: &'a DataBack,
    ) -> Fut<'a, Result<Recreated>> {
        Box::pin(async move {
            let swapped = self.replace(rl, t, image, network_mode, action, data).await?;
            Ok(self.follow(rl, &t.container, &swapped).await)
        })
    }

    /// [`swap`] on the job worker, recording success or finalizing recovery.
    async fn replace(
        &self,
        rl: &RunLog,
        t: &Target,
        image: Option<&str>,
        network_mode: Option<&str>,
        action: &str,
        data: &DataBack,
    ) -> Result<Swapped> {
        let d = self.docker()?;
        let name = t.container.as_str();
        let on_run = OnRun::new(self, rl, t, data);
        match swap(d, &on_run, name, image, network_mode).await {
            Ok(swapped) => {
                self.set_recovery(name, None).await;
                let f = &swapped.facts;
                self.record(rl, f.row(action, "ok", String::new())).await;
                let now = if f.new_version.is_empty() { String::new() } else { format!(", now {}", f.new_version) };
                rl.append(format!("{name}: {action} done{now}")).await;
                Ok(swapped)
            }
            Err(SwapFailed { error, attempted: None }) => Err(error),
            Err(SwapFailed { error, attempted: Some(attempt) }) => {
                self.finish_failed_swap(on_run, action, &error, *attempt).await;
                Err(error)
            }
        }
    }

    /// Recreate the containers that shared `name`'s network on its new container.
    /// A failure there doesn't undo `name`'s swap: it is reported per dependent.
    async fn follow(&self, rl: &RunLog, name: &str, swapped: &Swapped) -> Recreated {
        let mut out = Recreated::default();
        let mode = format!("container:{}", swapped.new_id);
        for dep in &swapped.dependents {
            let dep_name = dep["Names"][0]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("/{}", head(dep["Id"].as_str().unwrap_or(""), 12)));
            let dep_name = dep_name.trim_start_matches('/').to_string();
            if dep_name.ends_with(OLD_SUFFIX) {
                continue;
            }
            rl.append(format!("{dep_name}: shares {name}'s network, recreating it on the new container")).await;
            let dt = self
                .targets()
                .into_iter()
                .find(|x| x.container == dep_name)
                .unwrap_or_else(|| Target::new(&dep_name, "extra"));
            match self.recreate(rl, &dt, None, Some(&mode), "recreate").await {
                Ok(_) => out.dependents.push(dep_name),
                Err(e) => {
                    rl.append(format!("{dep_name}: recreate FAILED: {e}")).await;
                    rl.append(format!(
                        "{dep_name}: it has no network until it is recreated on the new {name}; \
                         run `docker compose up -d {dep_name}` in the stack folder"
                    ))
                    .await;
                    out.dependents_failed.push((dep_name, head(&e.to_string(), 300)));
                }
            }
        }
        out
    }

    /// How long a container gets to come up, and how often it is looked at.
    pub(super) fn health(&self) -> Health {
        Health {
            timeout: Duration::from_secs(self.app.cfg().updates.health_timeout.max(0) as u64),
            poll: self.env().poll,
            stable: self.env().stable,
        }
    }

    pub(super) async fn wait_healthy(&self, rl: &RunLog, t: &Target, cid: &str) -> Result<()> {
        let d = self.docker()?;
        let h = self.health();
        let deadline = Instant::now() + h.timeout;
        let healthy = wait_running(d, cid, &h).await?;
        if let Some(a) = &t.app {
            let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(30));
            if !app_ready(a, left).await? {
                return Err(Error::conflict(format!(
                    "{} API did not answer within {:.0}s",
                    a.name,
                    left.as_secs_f64()
                )));
            }
            rl.append(format!("{}: {} API is answering", t.container, a.name)).await;
        } else if t.role == "postgres" {
            while !self.app.pg().available().await {
                if Instant::now() > deadline {
                    return Err(Error::conflict("postgres not accepting connections"));
                }
                tokio::time::sleep(h.poll).await;
            }
            rl.append(format!("{}: postgres accepting connections", t.container)).await;
        } else {
            rl.append(format!("{}: running{}", t.container, if healthy { " and healthy" } else { "" })).await;
        }
        Ok(())
    }
}

/// Wait until the app's API answers.
async fn app_ready(a: &AppConfig, timeout: Duration) -> Result<bool> {
    Ok(crate::service::client(a).await?.wait_ready(timeout).await)
}
