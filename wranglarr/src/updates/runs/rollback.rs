// SPDX-License-Identifier: GPL-3.0-or-later
//! Manual rollback admission, history selection and execution.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::super::recreate::DataBack;
use super::super::{OLD_SUFFIX, SUITE, Target, Updater, short};
use crate::docker::{DockerError, image_reference};
use crate::error::{Error, Result};
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::manifest::{Manifest, PRE_UPDATE};
use crate::state::{RunStatus, UpdateRow};

/// The newest image change in `history` (newest first), unless a rollback already undid it.
fn rollback_candidate_in(history: &[UpdateRow]) -> Option<UpdateRow> {
    for u in history {
        if u.status != "ok" {
            continue;
        }
        if u.action == "rollback" {
            return None;
        }
        let old = u.old_image.as_deref().unwrap_or("");
        if (u.action == "update" || u.action == "recreate") && !old.is_empty() && u.old_image != u.new_image {
            return Some(u.clone());
        }
    }
    None
}

impl Updater {
    /// Queue a rollback of `container` to the image before its last update.
    /// With `restore_data`, its app's data goes back too, from the backup taken
    /// just before that update: the newer version may have migrated it.
    pub(crate) async fn rollback(&self, container: &str, restore_data: bool) -> Result<i64> {
        let t = self.target(container)?;
        let parked = format!("{container}{OLD_SUFFIX}");
        if self.parked().await.contains(&parked) {
            return Err(Error::conflict(format!(
                "{parked} is left from an earlier update or rollback: recover {container} first (see the Updates page)"
            )));
        }
        let Some(last) = self.rollback_candidate(container).await? else {
            return Err(Error::conflict(format!("{container}: no successful update to roll back")));
        };
        let pre = match restore_data {
            false => None,
            true => Some(self.pre_update_backup(&t, &last).await?.ok_or_else(|| {
                Error::conflict(format!(
                    "{container}: the backup from before that update isn't kept any more; roll back without \
                     restoring data, or restore a backup by hand"
                ))
            })?),
        };
        if let (Some(m), Some(a)) = (&pre, &t.app) {
            // checked now as well as in the run: a 409 is better than a stopped app
            let p = self.app.prepare_offline(&m.suite, &m.version, &a.name).await?;
            if let Some(gap) = p.offline_gap() {
                return Err(Error::conflict(format!(
                    "{gap}, so the old version would start on the newer one's database. Mount its config folder \
                     into wranglarr (read-write), or roll back without restoring data"
                )));
            }
        }
        let also: HashSet<String> = pre.as_ref().map(|m| m.suite.clone()).into_iter().collect();
        let u = self.clone();
        let name = container.to_string();
        self.app
            .submit(SUITE, "rollback", container, also, move |rl| async move {
                u.rollback_job(&rl, &name, &last, pre.as_ref()).await
            })
            .await
    }

    /// The backup made just before update `last` of `t` (in the same run, ahead
    /// of the swap), when it's still kept and holds `t`'s app intact.
    pub(crate) async fn pre_update_backup(&self, t: &Target, last: &UpdateRow) -> Result<Option<Manifest>> {
        let (Some(suite), Some(app), Some(run_id)) = (t.suite.clone(), t.app.as_ref(), last.run_id) else {
            return Ok(None);
        };
        let Some(run) = self.app.state().get_run(run_id).await? else { return Ok(None) };
        let at = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
        let (Some(from), Some(to)) = (at(&run.started), at(&last.at)) else { return Ok(None) };
        let name = app.name.clone();
        let versions = self.app.blocking(move |app| Ok(app.store().list_versions(&suite))).await?;
        Ok(versions.into_iter().find(|m| {
            m.trigger.as_deref() == Some(PRE_UPDATE)
                && m.created.as_deref().and_then(at).is_some_and(|c| from <= c && c <= to)
                && m.apps.get(&name).is_some_and(|a| a.ok() && a.restorable())
        }))
    }

    /// The newest image change, unless a rollback already undid it or the
    /// container already runs the image it would go back to (an update cut off
    /// before it was recorded leaves the history one change behind).
    pub(crate) async fn rollback_candidate(&self, container: &str) -> Result<Option<UpdateRow>> {
        let Some(cand) = rollback_candidate_in(&self.app.state().list_updates(Some(container), 50).await?) else {
            return Ok(None);
        };
        let current = match self.docker() {
            Ok(d) => d.inspect(container).await.ok().and_then(|c| c["Image"].as_str().map(str::to_owned)),
            Err(_) => None,
        };
        Ok((current.is_none() || current != cand.old_image).then_some(cand))
    }

    async fn rollback_job(&self, rl: &RunLog, container: &str, last: &UpdateRow, pre: Option<&Manifest>) -> JobResult {
        let d = self.docker()?;
        let t = self.target(container)?;
        // checked before anything stops: a backup that can't be restored refuses the rollback
        let data = match (pre, &t.app) {
            (Some(m), Some(a)) => {
                let p = self.app.prepare_offline(&m.suite, &m.version, &a.name).await?;
                if let Some(gap) = p.offline_gap() {
                    return Err(Error::conflict(format!("{gap}; roll back without restoring data instead")));
                }
                DataBack::First(Box::new(p))
            }
            _ => DataBack::None,
        };
        let old_image = last.old_image.clone().unwrap_or_default();
        let old_version = last.old_version.clone().unwrap_or_default();
        match d.image(&old_image).await {
            Err(DockerError::NotFound(_)) => {
                return Err(Error::conflict(format!(
                    "previous image {} is gone from the host (pruned?); cannot roll back",
                    short(&old_image)
                )));
            }
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let reference = image_reference(&d.inspect(container).await?).to_string();
        let shown = if old_version.is_empty() { "unknown version" } else { old_version.as_str() };
        rl.append(format!("{container}: rolling back to {} ({shown})", short(&old_image))).await;
        d.tag(&old_image, &reference).await?;
        rl.append(format!("tagged {} as {reference}", short(&old_image))).await;
        let info = self.recreate_with(rl, &t, Some(&reference), None, "rollback", &data).await?;
        let to = if old_version.is_empty() { short(&old_image) } else { old_version.clone() };
        self.set_hold(container, true, &format!("rolled back to {to}")).await?;
        rl.append(format!("{container}: held so automatic updates leave it alone; release the hold to update again"))
            .await;
        let mut out = json!({"container": container, "image": old_image});
        let mut partial = false;
        if let DataBack::First(p) = &data {
            out["data_from"] = json!(p.version);
            if p.api_config()
                && let Err(e) = self.app.restore_api_config(rl, p).await
            {
                rl.append(format!(
                    "{container}: could not put {}'s config back from {} ({e}); restore it by hand",
                    p.app.name, p.version
                ))
                .await;
                partial = true;
            }
        }
        self.recheck().await?;
        if info.dependents_failed.is_empty() && !partial {
            return Ok(JobOutcome::ok(out));
        }
        let bad: Map<String, Value> = info.dependents_failed.into_iter().map(|(n, e)| (n, json!(e))).collect();
        out["dependents_failed"] = Value::Object(bad);
        Ok(JobOutcome::new(RunStatus::Partial, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(action: &str, status: &str, old: &str, new: &str) -> UpdateRow {
        UpdateRow {
            id: 0,
            container: "radarr".into(),
            image: "x".into(),
            old_image: Some(old.into()),
            new_image: Some(new.into()),
            old_version: Some("1".into()),
            new_version: Some("2".into()),
            action: action.into(),
            status: status.into(),
            run_id: None,
            at: String::new(),
            note: None,
        }
    }

    #[test]
    fn rollback_candidates() {
        let h = [row("update", "failed", "a", "b"), row("update", "ok", "a", "b"), row("update", "ok", "z", "a")];
        assert_eq!(rollback_candidate_in(&h).map(|u| u.new_image), Some(Some("b".into())));
        let h = [row("rollback", "ok", "b", "a"), row("update", "ok", "a", "b")];
        assert!(rollback_candidate_in(&h).is_none(), "already rolled back");
        let h = [row("recreate", "ok", "a", "a"), row("update", "ok", "", "b")];
        assert!(rollback_candidate_in(&h).is_none(), "no image change");
        let h = [row("recreate", "ok", "a", "c")];
        assert!(rollback_candidate_in(&h).is_some());
    }
}
