// SPDX-License-Identifier: GPL-3.0-or-later
//! The update check: whether each managed container's tag points at a newer
//! image in its registry (asked through the Docker daemon, nothing pulled).

use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value, json};

use super::{DOCKER_OFF, Target, Updater, self_update_hint};
use crate::docker::{DockerError, image_reference, split_ref};
use crate::error::{Error, Result};
use crate::standalone::image_version;
use crate::time::now_iso_micros;
use crate::value::truthy;

/// The digests in an image's RepoDigests (`repo@sha256:…` -> `sha256:…`).
pub(super) fn repo_digests(img: &Value) -> BTreeSet<String> {
    img["RepoDigests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|d| d.split_once('@').map(|(_, h)| h.to_string()))
        .collect()
}

impl Updater {
    /// The last check's rows for the containers still managed (and wranglarr's
    /// own), with their holds.
    pub(crate) async fn last_check(&self) -> Map<String, Value> {
        let data: Map<String, Value> =
            self.app.state().get("updates:last_check").await.ok().flatten().unwrap_or_default();
        let holds = self.holds().await;
        let names: HashSet<String> = self.targets().into_iter().map(|t| t.container).collect();
        let mut out = Map::new();
        for (n, mut v) in data.into_iter().filter(|(n, v)| names.contains(n) || v["role"] == "self") {
            // A preference may change during an in-flight registry lookup.
            // Never expose that old lookup as an actionable update.
            let Ok(track) = self.track(&n).await else { continue };
            if v["checked_track"] != json!(track.tag) {
                continue;
            }
            if let Some(o) = v.as_object_mut() {
                o.insert("held".into(), json!(holds.contains_key(&n)));
                o.insert("hold_reason".into(), json!(holds.get(&n)));
            }
            out.insert(n, v);
        }
        out
    }

    pub(crate) async fn available_count(&self) -> usize {
        self.last_check().await.values().filter(|s| s["status"] == "available").count()
    }

    pub(crate) async fn check_one(&self, t: &Target) -> Result<Value> {
        let d = self.docker()?;
        let mut info = json!({"container": t.container, "role": t.role, "suite": t.suite,
                              "app": t.app.as_ref().map(|a| &a.name), "kind": t.kind(),
                              "checked": now_iso_micros(), "error": null});
        let track = self.track(&t.container).await?;
        info["checked_track"] = json!(track.tag);
        let done = |mut info: Value, status: &str, error: Option<String>| {
            info["status"] = json!(status);
            if let Some(e) = error {
                info["error"] = json!(e);
            }
            info
        };
        let c = match d.inspect(&t.container).await {
            Ok(c) => c,
            Err(DockerError::NotFound(_)) => {
                return Ok(done(info, "missing", Some("no such container (not deployed, or a different name)".into())));
            }
            Err(e) => return Ok(done(info, "error", Some(e.to_string()))),
        };
        let current = image_reference(&c).to_string();
        let reference = match super::tracks::selected_reference(&current, track.tag.as_deref()) {
            Ok(r) => r,
            Err(e) => return Ok(done(info, "error", Some(e.to_string()))),
        };
        let image_id = c["Image"].as_str().unwrap_or("").to_string();
        info["image"] = json!(current);
        info["target_image"] = json!(reference);
        info["compose_service"] = c["Config"]["Labels"]["com.docker.compose.service"].clone();
        info["image_id"] = json!(image_id);
        info["running"] = json!(truthy(&c["State"]["Running"]));
        info["health"] = c["State"]["Health"]["Status"].clone();
        let img = d.image(&image_id).await.unwrap_or_else(|_| json!({}));
        info["version"] = json!(image_version(&img));
        info["image_created"] = img["Created"].clone();
        let (_, _, digest) = split_ref(&reference);
        let local = repo_digests(&img);
        if self.is_self(&c) {
            // its own container: a helper replaces it, on request (see selfupdate)
            let blocker = self.self_update_blocker(&c);
            info["self_update"] = json!(false);
            info["self_update_blocker"] = json!(blocker);
            if !local.is_empty()
                && digest.is_empty()
                && let Ok(remote) = d.remote_digest(&reference).await
                && (!local.contains(&remote) || current != reference)
            {
                info["newer"] = json!(true);
                info["remote_digest"] = json!(remote);
                info["self_update"] = json!(blocker.is_none());
                let hint = self_update_hint(&reference);
                let msg = match &blocker {
                    None => "a newer wranglarr is out: press Update now".to_string(),
                    Some(why) => format!("a newer wranglarr is out; it can't update itself here ({why}), so {hint}"),
                };
                info["update_hint"] = json!(hint);
                return Ok(done(info, "self", Some(msg)));
            }
            return Ok(done(info, "self", None));
        }
        if !digest.is_empty() || reference.starts_with("sha256:") {
            return Ok(done(info, "pinned", None));
        }
        if local.is_empty() && track.tag.is_none() {
            return Ok(done(info, "local", Some("image was built locally, not pulled from a registry".into())));
        }
        let remote = match d.remote_digest(&reference).await {
            Ok(r) => r,
            Err(e) => return Ok(done(info, "error", Some(e.to_string()))),
        };
        info["remote_digest"] = json!(remote);
        info["local_digest"] = json!(local.iter().next());
        if local.contains(&remote) && current == reference {
            return Ok(done(info, "current", None));
        }
        if let Some(blk) = self.blocked().await.get(&t.container)
            && blk["digests"].as_array().is_some_and(|a| a.iter().any(|d| d.as_str() == Some(remote.as_str())))
        {
            let reason = blk["reason"].as_str().unwrap_or("").to_string();
            return Ok(done(info, "blocked", Some(reason)));
        }
        Ok(done(info, "available", None))
    }

    pub(crate) async fn check_all(&self) -> Result<Map<String, Value>> {
        if !self.app.docker_ok().await {
            return Err(Error::conflict(DOCKER_OFF));
        }
        let targets = self.targets();
        let mut results = Map::new();
        for t in &targets {
            results.insert(t.container.clone(), self.check_one(t).await?);
        }
        // wranglarr itself, also when it isn't one of the managed containers
        if let Some(own) = self.own().await
            && !results.contains_key(&own.name)
        {
            results.insert(own.name.clone(), self.check_one(&Target::new(&own.name, "self")).await?);
        }
        self.app.state().put("updates:last_check", &results).await?;
        let n = results.values().filter(|r| r["status"] == "available").count();
        tracing::info!("update check: {n} of {} container(s) have a newer image", results.len());
        Ok(self.last_check().await)
    }

    /// A check after a job; only "docker is off" is ignored.
    pub(super) async fn recheck(&self) -> Result<()> {
        match self.check_all().await {
            Ok(_) | Err(Error::Conflict(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }
}
