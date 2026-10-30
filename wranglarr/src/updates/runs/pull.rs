// SPDX-License-Identifier: GPL-3.0-or-later
//! Pull candidate images before any swaps and refuse Postgres major upgrades.

use std::collections::HashMap;

use serde_json::Value;

use super::super::{Target, Updater, self_update_hint, short};
use super::Results;
use crate::docker::{DockerError, image_reference};
use crate::error::Result;
use crate::jobs::RunLog;
use crate::state::NewUpdate;

/// Keep the pulled digest with its reference even if another swap retags it.
pub(super) struct PulledImage {
    pub(super) reference: String,
    pub(super) id: String,
}

fn env_map(env: &Value) -> HashMap<String, String> {
    env.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|e| match e.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (e.to_string(), String::new()),
        })
        .collect()
}

impl Updater {
    async fn image_cfg(&self, image_id: &str) -> Value {
        match self.docker() {
            Ok(d) => d.image(image_id).await.map(|i| i["Config"].clone()).unwrap_or(Value::Null),
            Err(_) => Value::Null,
        }
    }

    pub(super) async fn pull_targets(
        &self,
        rl: &RunLog,
        targets: &[Target],
        results: &mut Results,
    ) -> Result<HashMap<String, PulledImage>> {
        let d = self.docker()?;
        let mut pulled = HashMap::new();
        for t in targets {
            let name = &t.container;
            let step = async {
                let c = d.inspect(name).await?;
                let reference = self
                    .update_reference(name, image_reference(&c))
                    .await
                    .map_err(|e| DockerError::Other(e.to_string()))?;
                if self.is_self(&c) {
                    return Ok::<_, DockerError>((c, reference, None));
                }
                d.pull(&reference).await?;
                let new_id = d.image(&reference).await?["Id"].as_str().unwrap_or("").to_string();
                Ok((c, reference, Some(new_id)))
            };
            let (c, reference, new_id) = match step.await {
                Ok(v) => v,
                Err(e) => {
                    results.set(name, "failed");
                    rl.append(format!("{name}: pull failed: {e}")).await;
                    self.record(
                        rl,
                        NewUpdate {
                            container: name.clone(),
                            action: "update".into(),
                            status: "failed".into(),
                            note: format!("pull: {e}"),
                            ..Default::default()
                        },
                    )
                    .await;
                    continue;
                }
            };
            let Some(new_id) = new_id else {
                results.set(name, "skipped");
                rl.append(format!("{name}: that's wranglarr itself; {}", self_update_hint(&reference))).await;
                continue;
            };
            let old_id = c["Image"].as_str().unwrap_or("");
            if new_id == old_id && reference == image_reference(&c) {
                results.set(name, "current");
                rl.append(format!("{name}: {reference} already up to date")).await;
                continue;
            }
            if t.role == "postgres" {
                let old_major = env_map(&self.image_cfg(old_id).await["Env"]).remove("PG_MAJOR").unwrap_or_default();
                let new_major = env_map(&self.image_cfg(&new_id).await["Env"]).remove("PG_MAJOR").unwrap_or_default();
                if !old_major.is_empty() && !new_major.is_empty() && old_major != new_major {
                    results.set(name, "refused");
                    let reason =
                        format!("Postgres {old_major} -> {new_major} is a major upgrade (dump/restore, not a swap)");
                    rl.append(format!("{name}: refusing: {reason}; see the wiki")).await;
                    self.block(name, &new_id, &reason).await?;
                    self.retag(rl, &reference, old_id).await;
                    continue;
                }
            }
            rl.append(format!("{name}: pulled {reference} ({} -> {})", short(old_id), short(&new_id))).await;
            pulled.insert(name.clone(), PulledImage { reference, id: new_id });
        }

        Ok(pulled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn env_maps() {
        let m = env_map(&json!(["PG_MAJOR=17", "FLAG", "A=b=c"]));
        assert_eq!(m.get("PG_MAJOR").map(String::as_str), Some("17"));
        assert_eq!(m.get("FLAG").map(String::as_str), Some(""));
        assert_eq!(m.get("A").map(String::as_str), Some("b=c"));
    }
}
