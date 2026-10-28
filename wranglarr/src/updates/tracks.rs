// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-container image-tag selection. The default follows the deployed image;
//! explicit tags stay within that image's repository and never change publisher.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use super::{SUITE, Target, Updater};
use crate::docker::{image_reference, split_ref, valid_tag};
use crate::error::{Error, Result};

const KEY: &str = "updates:tracks";
type Overrides = BTreeMap<String, Option<String>>;

#[derive(Debug, Serialize)]
pub(super) struct Selection {
    pub tag: Option<String>,
    pub source: &'static str,
}

/// Resolve a selected tag without ever permitting a repository change. No
/// selection preserves a digest pin or a locally built reference exactly.
pub(super) fn selected_reference(current: &str, tag: Option<&str>) -> Result<String> {
    let Some(tag) = tag else { return Ok(current.to_string()) };
    if !valid_tag(tag) {
        return Err(Error::invalid("update track must be a Docker image tag (1–128 ASCII letters, digits, _, . or -)"));
    }
    if current.is_empty() || current.starts_with("sha256:") {
        return Err(Error::invalid("this container has no image repository; recreate it with a named image first"));
    }
    let (repo, _, _) = split_ref(current);
    Ok(format!("{repo}:{tag}"))
}

impl Updater {
    pub(super) async fn track(&self, container: &str) -> Result<Selection> {
        let saved: Overrides = self.app.state().get(KEY).await?.unwrap_or_default();
        if let Some(tag) = saved.get(container) {
            return Ok(Selection { tag: tag.clone(), source: "ui" });
        }
        Ok(match self.app.cfg().updates.tracks.get(container) {
            Some(tag) => Selection { tag: Some(tag.clone()), source: "config" },
            None => Selection { tag: None, source: "container" },
        })
    }

    pub(super) async fn update_reference(&self, container: &str, current: &str) -> Result<String> {
        selected_reference(current, self.track(container).await?.tag.as_deref())
    }

    /// A saved preference changes future checks and updates, never the running
    /// container. Do not let a queued update observe a preference halfway through.
    pub(crate) async fn save_track(&self, container: &str, tag: Option<String>, reset: bool) -> Result<()> {
        // a managed container, or wranglarr's own (which Update now follows)
        if let Err(e) = self.target(container)
            && self.own().await.is_none_or(|own| own.name != container)
        {
            return Err(e);
        }
        let tag = tag.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        if let Some(tag) = &tag
            && !valid_tag(tag)
        {
            return Err(Error::invalid(
                "update track must be a Docker image tag (1–128 ASCII letters, digits, _, . or -)",
            ));
        }
        let container = container.to_owned();
        self.app
            .while_idle(SUITE, || async {
                self.app
                    .state()
                    .write(move |tx| {
                        let mut saved: Overrides = tx.get(KEY)?.unwrap_or_default();
                        if reset {
                            saved.remove(&container);
                        } else {
                            saved.insert(container.clone(), tag);
                        }
                        tx.put(KEY, &saved)?;
                        // A result checked against another track must not be actionable.
                        let mut checked: BTreeMap<String, Value> = tx.get("updates:last_check")?.unwrap_or_default();
                        checked.remove(&container);
                        tx.put("updates:last_check", &checked)
                    })
                    .await
            })
            .await
    }

    pub(super) async fn track_info(&self, t: &Target, row: &mut Value) -> Result<()> {
        let selection = self.track(&t.container).await?;
        // An unchecked row still needs the deployed repository to offer the
        // correct publisher's choices. Checks cache the other inspect fields.
        if row["image"].as_str().is_none()
            && let Some(d) = self.app.docker()
            && let Ok(c) = d.inspect(&t.container).await
        {
            row["image"] = json!(image_reference(&c));
            row["compose_service"] = c["Config"]["Labels"]["com.docker.compose.service"].clone();
        }
        let current = row["image"].as_str().unwrap_or("");
        let catalog = crate::service::image_tracks::catalog(current);
        let (selected, error) = if current.is_empty() {
            (None, None)
        } else {
            match selected_reference(current, selection.tag.as_deref()) {
                Ok(r) => (Some(r), None),
                Err(e) => (None, Some(e.to_string())),
            }
        };
        row["track"] = json!({"tag": selection.tag, "source": selection.source,
            "image": selected, "choices": catalog.choices, "docs": catalog.docs,
            "error": error,
            "changed": selected.as_deref().is_some_and(|r| r != current)});
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_stays_in_its_repository_and_requires_an_explicit_unpin() {
        assert_eq!(
            selected_reference("registry:5000/team/app:old", Some("develop")).expect("tag"),
            "registry:5000/team/app:develop"
        );
        let pinned = "registry:5000/team/app:1@sha256:abcd";
        assert_eq!(selected_reference(pinned, None).expect("pin"), pinned);
        assert_eq!(selected_reference(pinned, Some("edge")).expect("unpin"), "registry:5000/team/app:edge");
        for tag in ["../dev", "host/image:dev", "x@y", "-dev", "", "é", "a b"] {
            assert!(selected_reference("team/app:latest", Some(tag)).is_err(), "{tag}");
        }
        assert!(selected_reference("sha256:abcd", Some("edge")).is_err());
    }
}
