// SPDX-License-Identifier: GPL-3.0-or-later
//! Replacing a container on a new image with the settings it had, waiting until
//! it is healthy, and putting the old one back when it isn't. Containers that
//! share its network namespace are recreated on the new one afterwards.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::time::Instant;

use super::{OLD_SUFFIX, Target, USER_ENV, Updater, short};
use crate::config::AppConfig;
use crate::docker::{Docker, DockerError, IMAGE_REFERENCE_LABEL, image_reference};
use crate::error::{Error, Result};
use crate::jobs::RunLog;
use crate::standalone::image_version;
use crate::state::NewUpdate;
use crate::value::head;
use crate::value::truthy;

/// A /containers/create body that reproduces `old` on a new image.
///
/// Values the old container only had because its *image* defined them (env,
/// labels, cmd, entrypoint, exposed ports …) are dropped so the new image's
/// defaults apply, the way `docker compose up` would. Returns (body,
/// extra_networks): the create call accepts one network, the rest are
/// connected afterwards.
pub(super) fn build_create_body(
    old: &Value,
    old_image: Option<&Value>,
    image_ref: &str,
    new_image_id: &str,
    network_mode: Option<&str>,
) -> (Value, Vec<(String, Value)>) {
    let mut cfg = old["Config"].as_object().cloned().unwrap_or_default();
    let mut hc = old["HostConfig"].as_object().cloned().unwrap_or_default();
    let empty = Map::new();
    let icfg = old_image.and_then(|i| i["Config"].as_object()).unwrap_or(&empty);
    let old_id = old["Id"].as_str().unwrap_or("");
    let old_short = old_id.get(..12).unwrap_or(old_id);
    let null = Value::Null;

    cfg.insert("Image".into(), json!(image_ref));
    // a value equal to the image's could be inherited or set on purpose; these are always yours
    let img_env: HashSet<&str> = icfg
        .get("Env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|e| !USER_ENV.contains(&e.split('=').next().unwrap_or("")))
        .collect();
    // Podman lists the HOSTNAME it set among the env: the new container gets its own
    let engine_hostname = cfg.get("Hostname").and_then(Value::as_str).map(|h| format!("HOSTNAME={h}"));
    let env: Vec<Value> = cfg
        .get("Env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| !e.as_str().is_some_and(|s| img_env.contains(s) || engine_hostname.as_deref() == Some(s)))
        .cloned()
        .collect();
    cfg.insert("Env".into(), Value::Array(env));
    let img_labels = icfg.get("Labels").and_then(Value::as_object).unwrap_or(&empty);
    let mut labels: Map<String, Value> = cfg
        .get("Labels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(k, v)| img_labels.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if labels.contains_key("com.docker.compose.image") {
        labels.insert("com.docker.compose.image".into(), json!(new_image_id)); // keeps compose from seeing drift
    }
    cfg.insert("Labels".into(), Value::Object(labels));
    for key in ["Cmd", "Entrypoint", "WorkingDir", "User", "Healthcheck", "StopSignal", "OnBuild", "Shell"] {
        if cfg.get(key).is_some_and(|v| v == icfg.get(key).unwrap_or(&null)) {
            cfg.remove(key);
        }
    }
    for key in ["ExposedPorts", "Volumes"] {
        let img_keys = icfg.get(key).and_then(Value::as_object).unwrap_or(&empty);
        let left: Map<String, Value> = cfg
            .get(key)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(k, _)| !img_keys.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if left.is_empty() {
            cfg.remove(key);
        } else {
            cfg.insert(key.into(), Value::Object(left));
        }
    }

    if let Some(m) = network_mode.filter(|m| !m.is_empty()) {
        hc.insert("NetworkMode".into(), json!(m));
    }
    let mode = hc.get("NetworkMode").and_then(Value::as_str).unwrap_or("").to_string();
    let shared_ns = mode.starts_with("container:");
    if shared_ns || cfg.get("Hostname").and_then(Value::as_str) == Some(old_short) {
        cfg.remove("Hostname");
        cfg.remove("Domainname");
    }
    if shared_ns {
        cfg.remove("ExposedPorts");
        cfg.remove("MacAddress");
        hc.remove("PortBindings");
    }

    // volumes the container had that aren't declared in HostConfig (anonymous
    // volumes from the image's VOLUME lines): reattach so data isn't orphaned
    let mut declared: HashSet<String> = hc
        .get("Binds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|b| b.split(':').nth(1).map(str::to_string))
        .collect();
    declared.extend(
        hc.get("Mounts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|m| m["Target"].as_str().map(str::to_string)),
    );
    let mut extra_mounts = vec![];
    for m in old["Mounts"].as_array().into_iter().flatten() {
        let dest = m["Destination"].as_str().unwrap_or("");
        if m["Type"] == "volume" && !declared.contains(dest) && truthy(&m["Name"]) {
            let rw = m["RW"].as_bool().unwrap_or(true);
            extra_mounts.push(json!({"Type": "volume", "Source": m["Name"], "Target": m["Destination"],
                                     "ReadOnly": !rw}));
        }
    }
    if !extra_mounts.is_empty() {
        let mounts = hc.entry("Mounts").or_insert_with(|| json!([]));
        if !mounts.is_array() {
            *mounts = json!([]);
        }
        if let Some(a) = mounts.as_array_mut() {
            a.extend(extra_mounts);
        }
    }

    let mut endpoints: Vec<(String, Value)> = vec![];
    if !shared_ns && mode != "host" && mode != "none" {
        for (net, ep) in old["NetworkSettings"]["Networks"].as_object().into_iter().flatten() {
            let aliases: Vec<Value> = ep["Aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| !a.as_str().is_some_and(|a| a == old_short || a == old_id))
                .cloned()
                .collect();
            let mut e = Map::new();
            e.insert("Aliases".into(), if aliases.is_empty() { Value::Null } else { Value::Array(aliases) });
            for k in ["IPAMConfig", "Links", "DriverOpts"] {
                if truthy(&ep[k]) {
                    e.insert(k.into(), ep[k].clone());
                }
            }
            endpoints.push((net.clone(), Value::Object(e)));
        }
    }
    let first = endpoints.iter().position(|(n, _)| *n == mode).or(if endpoints.is_empty() { None } else { Some(0) });
    let mut body = cfg;
    body.insert("HostConfig".into(), Value::Object(hc));
    if let Some(i) = first {
        let (net, ep) = endpoints.remove(i);
        body.insert("NetworkingConfig".into(), json!({"EndpointsConfig": {net: ep}}));
    }
    (Value::Object(body), endpoints)
}

/// What a recreate did to the containers sharing the recreated one's network.
#[derive(Debug, Default)]
pub(super) struct Recreated {
    pub(super) dependents: Vec<String>,
    /// dependent -> why it failed
    pub(super) dependents_failed: Vec<(String, String)>,
}

/// Where a swap's lines go: a run's log, or the self-update helper's report.
pub(super) trait Log: Sync {
    fn line(&self, msg: String) -> impl Future<Output = ()> + Send;
}

impl Log for RunLog {
    async fn line(&self, msg: String) {
        self.append(msg).await
    }
}

/// What [`swap`] needs from whoever runs it: when the new container counts as
/// up, and work at two points of the swap.
pub(super) trait Swapper: Log {
    /// Whether the started container `id` is up: healthy, its app answering.
    fn ready(&self, d: &Docker, id: &str) -> impl Future<Output = Result<()>> + Send;

    /// After the old container is stopped and parked, before the new one is
    /// created: a failure here puts the old one back.
    fn before_create(&self, _d: &Docker) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// After a failed swap has removed the new container, before the old one
    /// gets its name back and starts. Failure leaves the old container parked.
    fn before_put_back(&self, _d: &Docker) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }
}

/// The images and versions one swap went between: what the history row records.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct SwapFacts {
    pub(super) name: String,
    /// The reference the new container runs (its tag).
    pub(super) reference: String,
    pub(super) old_image: String,
    pub(super) new_image: String,
    pub(super) old_version: String,
    pub(super) new_version: String,
}

impl SwapFacts {
    /// The history row for this swap.
    pub(super) fn row(&self, action: &str, status: &str, note: String) -> NewUpdate {
        NewUpdate {
            container: self.name.clone(),
            image: self.reference.clone(),
            action: action.to_owned(),
            status: status.to_owned(),
            old_image: self.old_image.clone(),
            new_image: self.new_image.clone(),
            old_version: self.old_version.clone(),
            new_version: self.new_version.clone(),
            note,
            ..Default::default()
        }
    }

    /// The new image, for people: its version, else its short id.
    pub(super) fn new_shown(&self) -> String {
        if self.new_version.is_empty() { short(&self.new_image) } else { self.new_version.clone() }
    }
}

/// A container swapped for a new one: its new id, the containers that were in
/// the old one's network namespace, and what changed.
#[derive(Debug)]
pub(super) struct Swapped {
    pub(super) new_id: String,
    pub(super) dependents: Vec<Value>,
    pub(super) facts: SwapFacts,
}

/// A swap that didn't happen. `attempted` is set once the old container was
/// stopped and parked: the new one was tried and recovery was attempted.
#[derive(Debug)]
pub(super) struct SwapFailed {
    pub(super) error: Error,
    pub(super) attempted: Option<Box<SwapFacts>>,
}

impl SwapFailed {
    fn before(error: impl Into<Error>) -> SwapFailed {
        SwapFailed { error: error.into(), attempted: None }
    }
}

/// When a started container counts as up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Health {
    /// For the whole wait.
    pub(super) timeout: Duration,
    /// Between looks.
    pub(super) poll: Duration,
    /// How long one without a healthcheck must stay running.
    pub(super) stable: Duration,
}

/// Wait until the container `cid` is up: healthy by its own healthcheck, or
/// running for `h.stable` without one. True when its healthcheck said so.
pub(super) async fn wait_running(d: &Docker, cid: &str, h: &Health) -> Result<bool> {
    let deadline = Instant::now() + h.timeout;
    let mut up_since: Option<Instant> = None;
    loop {
        let st = d.inspect(cid).await?["State"].clone();
        let health = st["Health"]["Status"].as_str().map(str::to_string);
        let (running, restarting) = (truthy(&st["Running"]), truthy(&st["Restarting"]));
        if !running && !restarting {
            let code = st["ExitCode"].as_i64().map_or_else(|| "None".to_string(), |c| c.to_string());
            return Err(Error::conflict(format!("exited with code {code}")));
        }
        match health.as_deref() {
            Some("unhealthy") => return Err(Error::conflict("container reports unhealthy")),
            Some("healthy") => return Ok(true),
            None if running && !restarting => {
                let since = *up_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= h.stable {
                    return Ok(false);
                }
            }
            _ => up_since = None,
        }
        if Instant::now() > deadline {
            return Err(Error::conflict(format!("not healthy after {}s", h.timeout.as_secs())));
        }
        tokio::time::sleep(h.poll).await;
    }
}

/// Point `reference` back at `image_id`, the image still running, so a later
/// `docker compose up` doesn't silently switch to a rejected one. A digest or
/// image id reference has no tag to move.
pub(super) async fn retag(d: &Docker, log: &impl Log, reference: &str, image_id: &str) {
    if reference.starts_with("sha256:") || reference.contains('@') {
        return;
    }
    match d.tag(image_id, reference).await {
        Ok(()) => log.line(format!("tagged {reference} back to the running image {}", short(image_id))).await,
        Err(e) => log.line(format!("could not retag {reference}: {e}")).await,
    }
}

/// Replace the container `name` with one made from the same settings on `image`
/// (default: its current image reference): park the old one under
/// `<name>-wranglarr-old`, create and start the new one, and remove the parked
/// one once the new one is up. On a failure after parking, the old one is put
/// back. Containers in its network namespace are listed, not followed.
pub(super) async fn swap(
    d: &Docker,
    s: &impl Swapper,
    name: &str,
    image: Option<&str>,
    network_mode: Option<&str>,
) -> std::result::Result<Swapped, SwapFailed> {
    let old = d.inspect(name).await.map_err(SwapFailed::before)?;
    let old_id = old["Id"].as_str().unwrap_or("").to_string();
    let old_image_id = old["Image"].as_str().unwrap_or("").to_string();
    let reference = image.map_or_else(|| image_reference(&old).to_string(), str::to_string);
    // Following a namespace change is not an application update. A held
    // app must keep its exact image even if somebody pulled a newer tag.
    let namespace_only = image.is_none() && network_mode.is_some();
    let new_img = d.image(if namespace_only { &old_image_id } else { &reference }).await.map_err(SwapFailed::before)?;
    let new_image_id = new_img["Id"].as_str().unwrap_or("").to_string();
    let old_img = d.image(&old_image_id).await.ok();
    let (mut body, extra_nets) = build_create_body(&old, old_img.as_ref(), &reference, &new_image_id, network_mode);
    if namespace_only {
        // created from the image id: remember the reference it was deployed with
        body["Image"] = json!(old_image_id);
        body["Labels"][IMAGE_REFERENCE_LABEL] = json!(reference);
    } else if let Some(labels) = body["Labels"].as_object_mut() {
        labels.remove(IMAGE_REFERENCE_LABEL);
    }
    let was_running = truthy(&old["State"]["Running"]);
    let parked = format!("{name}{OLD_SUFFIX}");
    match d.inspect(&parked).await {
        Err(DockerError::NotFound(_)) => {}
        Ok(_) => {
            return Err(SwapFailed::before(Error::conflict(format!(
                "{parked} exists from an earlier interrupted update; inspect and remove it first"
            ))));
        }
        Err(e) => return Err(SwapFailed::before(e)),
    }
    let facts = SwapFacts {
        name: name.to_owned(),
        reference: reference.clone(),
        old_image: old_image_id.clone(),
        new_image: new_image_id.clone(),
        old_version: old_img.as_ref().map(image_version).unwrap_or_default(),
        new_version: image_version(&new_img),
    };

    let change = if facts.old_version != facts.new_version {
        let shown = |v: &str| if v.is_empty() { "?".to_string() } else { v.to_string() };
        format!(" ({} -> {})", shown(&facts.old_version), shown(&facts.new_version))
    } else {
        String::new()
    };
    s.line(format!("{name}: recreating on {}{change}", short(&new_image_id))).await;
    // containers living in this one's network namespace must follow it to the new id
    let modes = [format!("container:{old_id}"), format!("container:{name}")];
    let dependents: Vec<Value> = d
        .containers(true)
        .await
        .map_err(SwapFailed::before)?
        .into_iter()
        .filter(|c| c["HostConfig"]["NetworkMode"].as_str().is_some_and(|m| modes.iter().any(|x| x == m)))
        .collect();
    if was_running {
        let timeout = old["Config"]["StopTimeout"].as_u64().filter(|t| *t > 0).unwrap_or(30);
        if let Err(e) = d.stop(name, timeout).await {
            // not renamed yet: make sure it keeps running
            if let Err(e2) = d.start(name).await {
                s.line(format!("{name}: could not start it again after the failed stop: {e2}")).await;
            }
            return Err(SwapFailed::before(e));
        }
    }
    if let Err(e) = d.rename(name, &parked).await {
        if was_running && let Err(e2) = d.start(name).await {
            s.line(format!("{name}: could not start it again after the failed rename: {e2}")).await;
        }
        return Err(SwapFailed::before(e));
    }
    let mut new_id: Option<String> = None;
    let swapped: Result<()> = async {
        s.before_create(d).await?;
        let id = d.create(name, &body).await?;
        new_id = Some(id.clone());
        for (net, ep) in &extra_nets {
            d.connect(net, &id, ep).await?;
        }
        if was_running {
            d.start(&id).await?;
            s.ready(d, &id).await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = swapped {
        s.line(format!("{name}: new container did not come up ({e}); restoring the old one")).await;
        let error = match put_back(d, s, name, &parked, new_id.as_deref(), was_running).await {
            Ok(()) => e,
            Err(recovery) => Error::internal(format!("{e}; rollback failed: {recovery}")),
        };
        return Err(SwapFailed { error, attempted: Some(Box::new(facts)) });
    }
    let new_id = new_id.unwrap_or_default();
    if let Err(e) = d.remove(&parked, true).await {
        // the new one is up: the parked one is only left over
        s.line(format!("{name}: could not remove the old container {parked}: {e}")).await;
    }
    Ok(Swapped { new_id, dependents, facts })
}

/// After a failed swap: remove the new container (if it was made), give the
/// parked one its name back and start it again if it was running.
async fn put_back(
    d: &Docker,
    s: &impl Swapper,
    name: &str,
    parked: &str,
    new_id: Option<&str>,
    was_running: bool,
) -> Result<()> {
    let restored: Result<()> = async {
        if let Some(id) = new_id {
            d.remove(id, true).await?;
        }
        s.before_put_back(d).await?;
        d.rename(parked, name).await?;
        if was_running {
            d.start(name).await?;
        }
        Ok(())
    }
    .await;
    match &restored {
        Ok(()) => s.line(format!("{name}: old container restored")).await,
        Err(e) => {
            s.line(format!("{name}: could not restore the old container: {e}; inspect {parked} and recovery artifacts before starting it manually")).await;
        }
    }
    restored
}

type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A swap on the job worker: lines go to the run's log, and the new container
/// is up once its app's API (or Postgres) answers.
struct OnRun<'a> {
    updater: &'a Updater,
    rl: &'a RunLog,
    target: &'a Target,
}

impl Log for OnRun<'_> {
    fn line(&self, msg: String) -> impl Future<Output = ()> + Send {
        self.rl.line(msg)
    }
}

impl Swapper for OnRun<'_> {
    fn ready(&self, _d: &Docker, id: &str) -> impl Future<Output = Result<()>> + Send {
        self.updater.wait_healthy(self.rl, self.target, id)
    }
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
        Box::pin(async move {
            let swapped = self.replace(rl, t, image, network_mode, action).await?;
            Ok(self.follow(rl, &t.container, &swapped).await)
        })
    }

    /// [`swap`] on the job worker, with its history row. A failed update also
    /// remembers the image as rejected and moves the tag back.
    async fn replace(
        &self,
        rl: &RunLog,
        t: &Target,
        image: Option<&str>,
        network_mode: Option<&str>,
        action: &str,
    ) -> Result<Swapped> {
        let d = self.docker()?;
        let name = t.container.as_str();
        let on_run = OnRun { updater: self, rl, target: t };
        match swap(d, &on_run, name, image, network_mode).await {
            Ok(swapped) => {
                let f = &swapped.facts;
                self.record(rl, f.row(action, "ok", String::new())).await;
                let now = if f.new_version.is_empty() { String::new() } else { format!(", now {}", f.new_version) };
                rl.append(format!("{name}: {action} done{now}")).await;
                Ok(swapped)
            }
            Err(SwapFailed { error, attempted: None }) => Err(error),
            Err(SwapFailed { error, attempted: Some(f) }) => {
                self.record(rl, f.row(action, "failed", head(&error.to_string(), 300))).await;
                if action == "update" && f.new_image != f.old_image {
                    let reason = head(&format!("{} failed to start: {error}", f.new_shown()), 200);
                    if let Err(e3) = self.block(name, &f.new_image, &reason).await {
                        rl.append(format!("{name}: could not remember the rejected image: {e3}")).await;
                    }
                    self.retag(rl, &f.reference, &f.old_image).await;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn old_container() -> Value {
        json!({
            "Id": "abcdef0123456789ffff",
            "Image": "sha256:old",
            "Config": {"Image": "lscr.io/linuxserver/radarr:latest", "Hostname": "abcdef012345",
                       "Env": ["PATH=/usr/bin", "PUID=1000", "TZ=UTC", "MINE=1"],
                       "Labels": {"maintainer": "x", "com.docker.compose.image": "sha256:old",
                                  "com.docker.compose.service": "radarr"},
                       "Cmd": ["/init"], "ExposedPorts": {"7878/tcp": {}, "9999/tcp": {}},
                       "Volumes": {"/config": {}}, "Healthcheck": null},
            "HostConfig": {"NetworkMode": "arrs", "Binds": ["/srv/radarr:/config"],
                           "RestartPolicy": {"Name": "unless-stopped"}},
            "Mounts": [{"Type": "bind", "Source": "/srv/radarr", "Destination": "/config", "RW": true},
                       {"Type": "volume", "Name": "anon1", "Destination": "/cache", "RW": false}],
            "NetworkSettings": {"Networks": {
                "arrs": {"Aliases": ["radarr", "abcdef012345"], "IPAMConfig": null, "Links": null},
                "other": {"Aliases": ["abcdef0123456789ffff"], "IPAMConfig": {"IPv4Address": "10.0.0.5"}}}}
        })
    }

    fn old_image() -> Value {
        json!({"Config": {"Env": ["PATH=/usr/bin", "TZ=UTC"], "Labels": {"maintainer": "x"}, "Cmd": ["/init"],
                          "ExposedPorts": {"7878/tcp": {}}, "Volumes": {"/config": {}}}})
    }

    #[test]
    fn create_body_keeps_yours_drops_the_images() {
        let (body, extra) =
            build_create_body(&old_container(), Some(&old_image()), "lscr.io/x:latest", "sha256:new", None);
        assert_eq!(body["Image"], "lscr.io/x:latest");
        // image-only env dropped; TZ is a user var so kept even though it equals the image's
        assert_eq!(body["Env"], json!(["PUID=1000", "TZ=UTC", "MINE=1"]));
        assert_eq!(
            body["Labels"],
            json!({"com.docker.compose.image": "sha256:new", "com.docker.compose.service": "radarr"})
        );
        assert!(body.get("Cmd").is_none(), "Cmd equal to the image's is dropped");
        assert!(body.get("Healthcheck").is_none(), "null on both sides is dropped");
        assert_eq!(body["ExposedPorts"], json!({"9999/tcp": {}}));
        assert!(body.get("Volumes").is_none());
        assert!(body.get("Hostname").is_none(), "the generated hostname follows the new id");
        assert_eq!(body["HostConfig"]["RestartPolicy"], json!({"Name": "unless-stopped"}));
        assert_eq!(
            body["HostConfig"]["Mounts"],
            json!([{"Type": "volume", "Source": "anon1", "Target": "/cache", "ReadOnly": true}])
        );
        assert_eq!(body["NetworkingConfig"], json!({"EndpointsConfig": {"arrs": {"Aliases": ["radarr"]}}}));
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "other");
        assert_eq!(extra[0].1, json!({"Aliases": null, "IPAMConfig": {"IPv4Address": "10.0.0.5"}}));
    }

    #[test]
    fn create_body_in_a_shared_namespace() {
        let mut old = old_container();
        old["HostConfig"]["PortBindings"] = json!({"7878/tcp": [{"HostPort": "7878"}]});
        old["Config"]["Hostname"] = json!("custom");
        old["Config"]["MacAddress"] = json!("02:00:00:00:00:01");
        let (body, extra) = build_create_body(&old, Some(&old_image()), "x:1", "sha256:new", Some("container:vpnid"));
        assert_eq!(body["HostConfig"]["NetworkMode"], "container:vpnid");
        for k in ["Hostname", "ExposedPorts", "MacAddress", "NetworkingConfig"] {
            assert!(body.get(k).is_none(), "{k} must go in a shared namespace");
        }
        assert!(body["HostConfig"].get("PortBindings").is_none());
        assert!(extra.is_empty());
    }

    #[test]
    fn create_body_leaves_podmans_hostname_env_behind() {
        // Podman lists the HOSTNAME it set (the old id) among the env
        let mut old = old_container();
        old["Config"]["Env"] = json!(["PUID=1000", "HOSTNAME=abcdef012345", "MINE=1"]);
        let (body, _) = build_create_body(&old, Some(&old_image()), "x:1", "sha256:new", None);
        assert_eq!(body["Env"], json!(["PUID=1000", "MINE=1"]), "the new container gets its own");
        // a HOSTNAME of your own, unlike the container's, is yours
        old["Config"]["Env"] = json!(["HOSTNAME=media-box"]);
        let (body, _) = build_create_body(&old, Some(&old_image()), "x:1", "sha256:new", None);
        assert_eq!(body["Env"], json!(["HOSTNAME=media-box"]));
    }

    #[test]
    fn create_body_without_the_old_image() {
        let (body, _) = build_create_body(&old_container(), None, "x:1", "sha256:new", None);
        assert_eq!(body["Env"], json!(["PATH=/usr/bin", "PUID=1000", "TZ=UTC", "MINE=1"]));
        assert_eq!(body["Cmd"], json!(["/init"]));
        assert_eq!(body["Labels"]["maintainer"], "x");
    }
}
