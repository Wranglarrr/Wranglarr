// SPDX-License-Identifier: GPL-3.0-or-later
//! The swap itself: park the old container, create and start the new one, and
//! put the old one back when the new one doesn't come up.

use std::future::Future;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::Instant;

use super::super::recovery::{ContainerRecovery, Placement, RecoveryPhase};
use super::super::{OLD_SUFFIX, short};
use super::create::build_create_body;
use crate::docker::{Docker, DockerError, IMAGE_REFERENCE_LABEL, image_reference};
use crate::error::{Error, Result};
use crate::jobs::RunLog;
use crate::standalone::image_version;
use crate::state::NewUpdate;
use crate::value::truthy;

/// Where a swap's lines go: a run's log, or the self-update helper's report.
pub(in crate::updates) trait Log: Sync {
    fn line(&self, msg: String) -> impl Future<Output = ()> + Send;
}

impl Log for RunLog {
    async fn line(&self, msg: String) {
        self.append(msg).await
    }
}

/// What [`swap`] needs from whoever runs it: when the new container counts as
/// up, and work at two points of the swap.
pub(in crate::updates) trait Swapper: Log {
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

    /// Just before the new container is started. From here on it may have run,
    /// even when Docker's answer to the start never arrives.
    fn starting(&self) {}

    /// A failed swap made the new container `id`: a last look at it before
    /// it's removed.
    fn on_failure(&self, _d: &Docker, _id: &str) -> impl Future<Output = ()> + Send {
        async {}
    }
}

/// The images and versions one swap went between: what the history row records.
#[derive(Debug, Clone, Default, PartialEq)]
pub(in crate::updates) struct SwapFacts {
    pub(in crate::updates) name: String,
    /// The reference the new container runs (its tag).
    pub(in crate::updates) reference: String,
    pub(in crate::updates) old_image: String,
    pub(in crate::updates) new_image: String,
    pub(in crate::updates) old_version: String,
    pub(in crate::updates) new_version: String,
}

impl SwapFacts {
    /// The history row for this swap.
    pub(in crate::updates) fn row(&self, action: &str, status: &str, note: String) -> NewUpdate {
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
    pub(in crate::updates) fn new_shown(&self) -> String {
        if self.new_version.is_empty() { short(&self.new_image) } else { self.new_version.clone() }
    }
}

/// A container swapped for a new one: its new id, the containers that were in
/// the old one's network namespace, and what changed.
#[derive(Debug)]
pub(in crate::updates) struct Swapped {
    pub(in crate::updates) new_id: String,
    pub(in crate::updates) dependents: Vec<Value>,
    pub(in crate::updates) facts: SwapFacts,
}

/// A swap that didn't happen. `attempted` is set once the old container was
/// stopped and parked: the new one was tried and recovery was attempted.
#[derive(Debug)]
pub(in crate::updates) struct SwapFailed {
    pub(in crate::updates) error: Error,
    pub(in crate::updates) attempted: Option<Box<AttemptedSwap>>,
}

/// A parked swap always has both its image facts and a recovery result.
#[derive(Debug)]
pub(in crate::updates) struct AttemptedSwap {
    pub(in crate::updates) facts: SwapFacts,
    pub(super) recovery: ContainerRecovery,
    pub(super) was_running: bool,
}

impl SwapFailed {
    fn before(error: impl Into<Error>) -> SwapFailed {
        SwapFailed { error: error.into(), attempted: None }
    }
}

async fn left_after(d: &Docker, name: &str, new_id: Option<&str>) -> Placement {
    let running = |c: &Value| c["State"]["Running"].as_bool();
    if let Some(id) = new_id {
        match d.inspect(id).await {
            Ok(c) => return running(&c).map_or(Placement::Unknown, |running| Placement::Rejected { running }),
            Err(DockerError::NotFound(_)) => {}
            Err(_) => return Placement::Unknown,
        }
    }
    let parked = format!("{name}{OLD_SUFFIX}");
    match (d.inspect(&parked).await, d.inspect(name).await) {
        (Ok(p), Err(DockerError::NotFound(_))) if running(&p) == Some(false) => Placement::Parked,
        (Err(DockerError::NotFound(_)), Ok(c)) => {
            running(&c).map_or(Placement::Unknown, |running| Placement::Back { running })
        }
        _ => Placement::Unknown,
    }
}

/// When a started container counts as up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::updates) struct Health {
    /// For the whole wait.
    pub(in crate::updates) timeout: Duration,
    /// Between looks.
    pub(in crate::updates) poll: Duration,
    /// How long one without a healthcheck must stay running.
    pub(in crate::updates) stable: Duration,
}

/// Wait until the container `cid` is up: healthy by its own healthcheck, or
/// running for `h.stable` without one. True when its healthcheck said so.
pub(in crate::updates) async fn wait_running(d: &Docker, cid: &str, h: &Health) -> Result<bool> {
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
pub(in crate::updates) async fn retag(d: &Docker, log: &impl Log, reference: &str, image_id: &str) {
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
pub(in crate::updates) async fn swap(
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
            s.starting();
            d.start(&id).await?;
            s.ready(d, &id).await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = swapped {
        s.line(format!("{name}: new container did not come up ({e}); restoring the old one")).await;
        if let Some(id) = &new_id {
            s.on_failure(d, id).await;
        }
        let recovery = put_back(d, s, name, &parked, new_id.as_deref(), was_running).await;
        let error = match &recovery {
            ContainerRecovery::Restored { .. } => e,
            ContainerRecovery::Incomplete { phase, error, .. } => {
                Error::internal(format!("{e}; rollback failed while {phase}: {error}"))
            }
        };
        return Err(SwapFailed { error, attempted: Some(Box::new(AttemptedSwap { facts, recovery, was_running })) });
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
) -> ContainerRecovery {
    let mut phase = RecoveryPhase::RemoveNew;
    let restored: Result<()> = async {
        if let Some(id) = new_id
            && let Err(e) = d.remove(id, true).await
        {
            // it may be running on the data still: stopped, at least, it changes nothing
            match d.stop(id, 30).await {
                Ok(()) => s.line(format!("{name}: the new container couldn't be removed ({e}); it's stopped")).await,
                Err(e2) => {
                    s.line(format!(
                        "{name}: the new container couldn't be removed ({e}) or stopped ({e2}): it may still be running"
                    ))
                    .await;
                }
            }
            return Err(e.into());
        }
        phase = RecoveryPhase::RestoreData;
        s.before_put_back(d).await?;
        phase = RecoveryPhase::RenameOld;
        d.rename(parked, name).await?;
        if was_running {
            phase = RecoveryPhase::StartOld;
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
    match restored {
        Ok(()) => ContainerRecovery::Restored { running: was_running },
        Err(error) => ContainerRecovery::Incomplete { phase, placement: left_after(d, name, new_id).await, error },
    }
}
