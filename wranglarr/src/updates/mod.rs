// SPDX-License-Identifier: GPL-3.0-or-later
//! Container image updates for the wrangled stack.
//!
//! - check: ask the registry (via the Docker daemon, no pull) whether a tag moved.
//! - update: back up the affected suite(s), pull, then recreate the container with
//!   exactly the settings it had (mounts, env, networks, labels, restart policy),
//!   wait until it is healthy, and put the old one back if not.
//! - rollback: retag the previous image and recreate on it; the container is then
//!   held so the next automatic update doesn't undo the rollback.
//!
//! Containers sharing another container's network (`network_mode: service:vpn`)
//! point at that container's id, so they are recreated too whenever it is.
//!
//! Settings, holds, blocks and the last check live in the state db's key/value
//! table, under keys and JSON that must not change (`updates:settings`,
//! `updates:holds`, `updates:blocked`, `updates:last_check`).
//!
//! - `settings`: the settings, holds and blocks, and the two schedules;
//! - `check`: whether each container's tag moved in its registry;
//! - `recreate`: replacing one container on a new image, then the containers
//!   sharing its network;
//! - `runs`: the update and rollback runs on the job worker;
//! - `selfupdate`: wranglarr replacing its own container, through a helper.

mod check;
mod recreate;
mod runs;
mod selfupdate;
mod settings;
mod tracks;

use std::collections::HashSet;
use std::time::Duration;

use serde_json::{Value, json};

use crate::app::App;
use crate::config::AppConfig;
use crate::docker::Docker;
use crate::error::{Error, Result};
use crate::value::head;

pub(crate) use selfupdate::handed_over;
pub use selfupdate::{health_main, helper_main as self_update_helper, main as self_update_main};
pub use settings::UpdateSettings;

/// Runs table bucket for update runs.
pub(crate) const SUITE: &str = "system";
const OLD_SUFFIX: &str = "-wranglarr-old";
/// Env values that are always the user's, even when equal to the image's.
const USER_ENV: [&str; 4] = ["TZ", "PUID", "PGID", "UMASK"];

const DOCKER_OFF: &str = "docker control is off: mount /var/run/docker.sock and set [docker] socket";

/// What the updater takes from the environment (part of
/// [`crate::deployment::Deployment`]).
#[derive(Debug, Clone, PartialEq)]
pub struct UpdatesEnv {
    /// WRANGLARR_UPDATE_POLL: time between health polls (2 s; the test suite shrinks it).
    pub poll: Duration,
    /// WRANGLARR_UPDATE_STABLE: how long a container without a healthcheck must
    /// stay running to count as up (15 s).
    pub stable: Duration,
    /// HOSTNAME: the start of wranglarr's own container id, which only its
    /// self-update helper replaces.
    pub hostname: String,
}

impl UpdatesEnv {
    /// The settings in the environment `env`. A duration that isn't a number of
    /// seconds (0 or more) takes its default.
    pub fn read(env: crate::config::Lookup) -> UpdatesEnv {
        let secs = |name: &str, default: f64| {
            let v = env(name).and_then(|s| s.trim().parse::<f64>().ok());
            Duration::from_secs_f64(v.filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(default))
        };
        UpdatesEnv {
            poll: secs("WRANGLARR_UPDATE_POLL", 2.0),
            stable: secs("WRANGLARR_UPDATE_STABLE", 15.0),
            hostname: env("HOSTNAME").unwrap_or_default().trim().to_string(),
        }
    }

    /// Whether the container `info` (`docker inspect`) is wranglarr's own.
    fn is_self(&self, info: &Value) -> bool {
        !self.hostname.is_empty() && info["Id"].as_str().unwrap_or("").starts_with(&self.hostname)
    }
}

/// The release notes on the docs site.
pub(crate) const RELEASE_NOTES: &str = "https://wranglarrr.github.io/Wranglarr/#/Release-Notes";

/// How to update wranglarr from the shell (when it can't update itself).
fn self_update_hint(reference: &str) -> String {
    let no_digest = reference.split('@').next().unwrap_or("");
    let name = no_digest.rsplit_once(':').map_or(no_digest, |(a, _)| a);
    if !name.contains('/') || name.starts_with("localhost/") {
        // wranglarr:local, an image built here
        return "rebuild it: `docker build -t wranglarr:local ./wranglarr && docker compose up -d wranglarr`".into();
    }
    "update it from the shell: `docker compose pull wranglarr && docker compose up -d wranglarr`".into()
}

/// A container wranglarr keeps up to date.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) container: String,
    /// app | postgres | vpn | extra | wranglarr
    pub(crate) role: &'static str,
    pub(crate) suite: Option<String>,
    pub(crate) app: Option<AppConfig>,
}

impl Target {
    fn new(container: &str, role: &'static str) -> Target {
        Target { container: container.into(), role, suite: None, app: None }
    }
    fn kind(&self) -> String {
        self.app.as_ref().map_or_else(|| self.role.to_string(), |a| a.kind.clone())
    }
}

/// `sha256:0123456789abcdef…` -> `0123456789ab`
fn short(image_id: &str) -> String {
    head(image_id.strip_prefix("sha256:").unwrap_or(image_id), 12)
}

/// Start-up: finish a self-update this process was handed, then the check and
/// auto-update schedules.
pub(crate) async fn start(app: &App) {
    selfupdate::resume(app).await;
    app.updates().reschedule().await;
}

impl App {
    /// The container updater.
    pub fn updates(&self) -> Updater {
        Updater { app: self.clone() }
    }
}

/// The container updater: a view on the service.
#[derive(Clone)]
pub struct Updater {
    app: App,
}

impl Updater {
    fn docker(&self) -> Result<&Docker> {
        self.app.docker().ok_or_else(|| Error::conflict(DOCKER_OFF))
    }

    // ---- targets -------------------------------------------------------------
    pub(crate) fn targets(&self) -> Vec<Target> {
        let c = &self.app.cfg();
        let u = &c.updates;
        let mut out: Vec<Target> = vec![];
        let mut add = |t: Target| {
            if !t.container.is_empty()
                && !out.iter().any(|o| o.container == t.container)
                && !u.exclude.contains(&t.container)
            {
                out.push(t);
            }
        };
        if u.include_vpn
            && let Some(v) = c.vpn.as_ref().filter(|v| !v.container.is_empty())
        {
            add(Target::new(&v.container, "vpn"));
        }
        if u.include_postgres && !c.postgres.container.is_empty() && c.all_apps().any(|a| !a.databases.is_empty()) {
            add(Target::new(&c.postgres.container, "postgres"));
        }
        for s in &c.suites {
            for a in s.apps.iter().filter(|a| !a.container.is_empty()) {
                add(Target {
                    container: a.container.clone(),
                    role: "app",
                    suite: Some(s.name.clone()),
                    app: Some(a.clone()),
                });
            }
        }
        for name in &u.extra {
            add(Target::new(name, "extra"));
        }
        out
    }

    /// The managed container called `container`; a 404 otherwise.
    pub(crate) fn target(&self, container: &str) -> Result<Target> {
        self.targets()
            .into_iter()
            .find(|t| t.container == container)
            .ok_or_else(|| Error::not_found(format!("not a managed container: '{container}'")))
    }

    fn env(&self) -> &UpdatesEnv {
        &self.app.deployment().updates
    }

    fn is_self(&self, info: &Value) -> bool {
        self.env().is_self(info)
    }

    /// The containers an interrupted update left parked (`<name>-wranglarr-old`).
    /// A swap refuses to run while its container has one.
    async fn parked(&self) -> HashSet<String> {
        let Ok(d) = self.docker() else { return HashSet::new() };
        d.containers(true)
            .await
            .unwrap_or_default()
            .iter()
            .flat_map(|c| c["Names"].as_array().cloned().unwrap_or_default())
            .filter_map(|n| n.as_str().map(|n| n.trim_start_matches('/').to_owned()))
            .filter(|n| n.ends_with(OLD_SUFFIX))
            .collect()
    }

    // ---- views -------------------------------------------------------------------
    /// Everything the Updates page and `GET /api/updates` show.
    pub(crate) async fn overview(&self) -> Result<Value> {
        let checks = self.last_check().await;
        let blocked = self.blocked().await;
        let holds = self.holds().await;
        let parked = self.parked().await;
        let mut rows = vec![];
        for t in self.targets() {
            let mut row = checks.get(&t.container).cloned().unwrap_or_else(|| {
                json!({"container": t.container, "role": t.role, "suite": t.suite,
                       "app": t.app.as_ref().map(|a| &a.name), "kind": t.kind(), "status": "unchecked"})
            });
            let hist = self.app.state().list_updates(Some(&t.container), 5).await?;
            let cand = self.rollback_candidate(&t.container).await?;
            row["blocked"] = blocked.get(&t.container).cloned().unwrap_or(Value::Null);
            let left = format!("{}{OLD_SUFFIX}", t.container);
            row["interrupted"] = json!(parked.contains(&left).then_some(left));
            row["held"] = json!(holds.contains_key(&t.container));
            row["hold_reason"] = json!(holds.get(&t.container));
            row["last_update"] = json!(hist.first());
            row["can_rollback"] = json!(cand.is_some());
            row["rollback_to"] = json!(cand.map(|c| match c.old_version.filter(|v| !v.is_empty()) {
                Some(v) => v,
                None => short(c.old_image.as_deref().unwrap_or("")),
            }));
            self.track_info(&t, &mut row).await?;
            rows.push(row);
        }
        // wranglarr's own row, when it isn't a managed container too
        if let Some(own) = checks.values().find(|r| r["role"] == "self")
            && !rows.iter().any(|r| r["container"] == own["container"])
        {
            let mut own = own.clone();
            let name = own["container"].as_str().unwrap_or("").to_string();
            own["blocked"] = blocked.get(&name).cloned().unwrap_or(Value::Null);
            own["last_update"] = json!(self.app.state().list_updates(Some(&name), 1).await?.first());
            self.track_info(&Target::new(&name, "self"), &mut own).await?;
            rows.insert(0, own);
        }
        let available = rows.iter().filter(|r| r["status"] == "available").count();
        let own = rows.iter().find(|r| r["status"] == "self").cloned();
        Ok(json!({"settings": self.settings().await?, "next": self.next_runs(), "targets": rows,
                  "history": self.app.state().list_updates(None, 40).await?, "docker": self.app.docker_ok().await,
                  "available": available, "own": own}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standalone::image_version;

    #[test]
    fn hints() {
        assert!(self_update_hint("wranglarr:local").starts_with("rebuild it"));
        assert!(self_update_hint("localhost/wranglarr:dev").starts_with("rebuild it"));
        assert!(self_update_hint("ghcr.io/wranglarrr/wranglarr:latest").starts_with("update it from the shell"));
        assert!(self_update_hint("ghcr.io/wranglarrr/wranglarr@sha256:ab").starts_with("update it from the shell"));
        assert!(self_update_hint("localhost:5000/wranglarr:1").starts_with("update it from the shell"));
    }

    #[test]
    fn versions_and_short_ids() {
        let img = |labels: Value| json!({"Config": {"Labels": labels}});
        assert_eq!(image_version(&img(json!({"org.opencontainers.image.version": "5.14", "version": "x"}))), "5.14");
        assert_eq!(image_version(&img(json!({"org.opencontainers.image.version": "", "version": "x"}))), "x");
        assert_eq!(
            image_version(&img(json!({"build_version": "Linuxserver.io version:- 5.0.3-r0-ls360 Build-date:- 2026"}))),
            "5.0.3-r0-ls360"
        );
        assert_eq!(image_version(&img(json!({"build_version": "plain"}))), "plain");
        assert_eq!(image_version(&json!({})), "");
        assert_eq!(short("sha256:0123456789abcdef"), "0123456789ab");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
    }
}
