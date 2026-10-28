// SPDX-License-Identifier: GPL-3.0-or-later
//! The apps' API keys: listed for a signed-in admin (setting up another app
//! needs them), and watched, so a key changed by hand in an app is put right
//! where the other apps keep a copy of it.
//!
//! wranglarr reads each key from the app's own config file every time, so it
//! follows a new key by itself. The copies are what breaks: Prowlarr, Seerr and
//! Bazarr keep the arrs' keys, the arrs keep SABnzbd's, and the indexers
//! Prowlarr pushes to the arrs carry Prowlarr's. Once a minute (and when an app
//! sends an event) wranglarr compares each key with a fingerprint of the last one
//! it saw. On a change it reconnects every app connected to that one, as
//! **Connect apps** would, and when one had to be reconnected says so with the
//! `api_key_changed` notification (info; a warning when one couldn't be).

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::app::App;
use crate::background::Background;
use crate::config::AppConfig;
use crate::error::{Error, Result};
use crate::service::{self, AppConfigExt, Credentials, Roles};

/// app -> fingerprint of the key last seen
const SEEN: &str = "keys:seen";
/// apps whose new key still has to reach the others (kept across restarts)
const REPAIR: &str = "keys:repair";
const EVERY: Duration = Duration::from_secs(60);
/// After a failed repair the next try waits this long, doubling up to [`MAX_WAIT`].
const FIRST_WAIT: Duration = Duration::from_secs(60);
const MAX_WAIT: Duration = Duration::from_secs(3600);

/// A repair that failed: when to try again, for which key.
struct Retry {
    generation: Value,
    next: Instant,
    wait: Duration,
}

#[derive(Default)]
pub struct KeysState {
    started: AtomicBool,
    tasks: Background,
    /// One check at a time: the minute's and an event's would both see the change.
    checking: tokio::sync::Mutex<()>,
    /// Repairs queued or waiting for their result in this process. The durable
    /// retry record remains until completion, including across restarts.
    repairing: tokio::sync::Mutex<BTreeSet<String>>,
    /// Failed repairs back off, and warn once per key: an app that stays down
    /// would otherwise get a reconnect run and a warning every minute.
    retries: std::sync::Mutex<HashMap<String, Retry>>,
}

impl KeysState {
    /// Whether the repair of `name`'s key `generation` may run now.
    fn due(&self, name: &str, generation: &Value) -> bool {
        let Ok(mut r) = self.retries.lock() else { return true };
        match r.get(name) {
            Some(x) if &x.generation == generation => Instant::now() >= x.next,
            Some(_) => {
                r.remove(name); // a newer key: start over
                true
            }
            None => true,
        }
    }

    /// A repair failed: wait longer before the next. True the first time for
    /// this key, which is when the user hears about it.
    fn failed(&self, name: &str, generation: &Value) -> bool {
        let Ok(mut r) = self.retries.lock() else { return true };
        let (wait, first) = match r.get(name) {
            Some(x) if &x.generation == generation => ((x.wait * 2).min(MAX_WAIT), false),
            _ => (FIRST_WAIT, true),
        };
        r.insert(name.to_owned(), Retry { generation: generation.clone(), next: Instant::now() + wait, wait });
        first
    }

    fn succeeded(&self, name: &str) {
        if let Ok(mut r) = self.retries.lock() {
            r.remove(name);
        }
    }
}

impl KeysState {
    pub(crate) async fn shutdown(&self) {
        self.tasks.shutdown().await;
    }
}

/// Enough to tell two keys apart, nothing to recover one from.
fn fingerprint(key: &str) -> String {
    hex::encode(&Sha256::digest(format!("wranglarr-key:{key}").as_bytes())[..12])
}

/// Start-up work: watch the keys.
pub fn start(app: &App) {
    let st = app.keys_state();
    if st.started.swap(true, Ordering::SeqCst) {
        return;
    }
    let me = app.clone();
    st.tasks.spawn("keys: watch", async move {
        loop {
            me.check_keys().await;
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// An app sent an event: a good moment to look at its key (a key reset in the
/// arr restarts nothing, and the next minute is a while to wait).
pub(crate) async fn event_seen(app: &App, _from: &AppConfig) {
    let me = app.clone();
    app.keys_state().tasks.spawn("keys: check", async move { me.check_keys().await });
}

impl App {
    /// The apps that use an API key.
    pub(crate) fn key_apps(&self) -> Vec<AppConfig> {
        self.cfg()
            .all_apps()
            .filter(|a| a.enabled && matches!(a.def().map(|d| &d.credentials), Some(Credentials::ApiKey { .. })))
            .cloned()
            .collect()
    }

    /// Every app's API key, for the API keys page.
    pub async fn app_keys(&self) -> Vec<Value> {
        let mut out = vec![];
        for a in self.key_apps() {
            let source = if a.api_key.is_empty() { a.api_key_file.clone() } else { "wranglarr.toml".into() };
            let mut row = json!({"name": a.name, "kind": a.kind, "url": a.url, "source": source});
            match service::apps::api_key(&a).await {
                Ok(k) if !k.is_empty() => row["key"] = json!(k),
                Ok(_) => row["error"] = json!("no API key in its config file yet"),
                Err(e) => row["error"] = json!(e.to_string()),
            }
            out.push(row);
        }
        out
    }

    /// Compare every key with the last one seen; put changed ones right.
    pub async fn check_keys(&self) {
        let _one = self.keys_state().checking.lock().await;
        let seen: Map<String, Value> = self.state().get(SEEN).await.ok().flatten().unwrap_or_default();
        let mut now = seen.clone();
        let mut changed = vec![];
        for a in self.key_apps() {
            let Ok(key) = service::apps::api_key(&a).await else { continue };
            if key.is_empty() {
                continue;
            }
            let fp = fingerprint(&key);
            match seen.get(&a.name).and_then(Value::as_str) {
                Some(old) if old == fp => {}
                Some(_) => {
                    tracing::info!("keys: {}'s API key changed", a.name);
                    changed.push(a.name.clone());
                    now.insert(a.name.clone(), json!(fp));
                }
                None => {
                    now.insert(a.name.clone(), json!(fp)); // first sight
                }
            }
        }
        let observed = now.clone();
        let wrote = self
            .state()
            .write(move |tx| {
                if now != seen {
                    tx.put(SEEN, &now)?;
                }
                let mut todo: BTreeSet<String> = tx.get(REPAIR)?.unwrap_or_default();
                if !changed.is_empty() {
                    todo.extend(changed);
                    tx.put(REPAIR, &todo)?;
                }
                Ok(todo)
            })
            .await;
        let todo = match wrote {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("keys: {e}");
                return;
            }
        };
        for name in todo {
            let generation = observed.get(&name).cloned().unwrap_or(Value::Null);
            if !self.keys_state().due(&name, &generation) {
                continue; // failed lately: backing off
            }
            if !self.keys_state().repairing.lock().await.insert(name.clone()) {
                continue;
            }
            if let Err(e) = self.repair_key(&name, generation.clone()).await {
                // another system run (an update) only means the next minute
                if !matches!(e, Error::Conflict(_)) {
                    tracing::warn!("keys: {name}: {e}");
                    self.keys_state().failed(&name, &generation);
                }
                // Admission failure is retryable too; nothing has completed.
                if let Err(e) = self.finish_key_repair(&name, generation, false).await {
                    tracing::warn!("keys: {name}: {e}");
                }
            }
        }
    }

    /// Clear only the generation actually repaired. A newer key observed while
    /// this run was queued must retain its own pending repair.
    async fn finish_key_repair(&self, name: &str, generation: Value, success: bool) -> Result<()> {
        let result = if success {
            let n = name.to_owned();
            self.state()
                .write(move |tx| {
                    let seen: Map<String, Value> = tx.get(SEEN)?.unwrap_or_default();
                    if seen.get(&n).unwrap_or(&Value::Null) == &generation {
                        let mut todo: BTreeSet<String> = tx.get(REPAIR)?.unwrap_or_default();
                        todo.remove(&n);
                        tx.put(REPAIR, &todo)?;
                    }
                    Ok(())
                })
                .await
        } else {
            Ok(())
        };
        self.keys_state().repairing.lock().await.remove(name);
        result
    }

    /// `name`'s key changed: reconnect every app connected to it (either way), as
    /// **Connect apps** would. The run itself finishes later.
    async fn repair_key(&self, name: &str, generation: Value) -> Result<()> {
        let Some(app) = self.cfg().app(name).cloned() else {
            return self.finish_key_repair(name, generation, true).await;
        };
        let links: Vec<String> = self
            .connect()
            .links()
            .into_iter()
            .filter(|l| l.dst.name == name || l.src.name == name)
            .map(|l| l.key())
            .collect();
        let run = if links.is_empty() { None } else { Some(self.connect().run("all", &links).await?) };
        let me = self.clone();
        self.keys_state().tasks.spawn("keys: repair", async move {
            let (success, notice) = me.after_repair(&app, run).await;
            // a failure is told once per key; the retries after it stay quiet
            let tell = if success {
                me.keys_state().succeeded(&app.name);
                true
            } else {
                me.keys_state().failed(&app.name, &generation)
            };
            if let Err(e) = me.finish_key_repair(&app.name, generation, success).await {
                tracing::warn!("keys: {}: {e}", app.name);
            }
            if let Some((message, level)) = notice.filter(|_| tell) {
                me.notifier().api_key_changed(&me, &format!("{}'s API key changed", app.name), &message, level);
            }
            me.publish(json!({"type": "keys", "app": app.name}));
        });
        Ok(())
    }

    /// Once the reconnect run is done: Prowlarr's indexers in the arrs carry its
    /// key, so it pushes them again. Returns what to tell the user, when an app
    /// had to be reconnected (or couldn't be).
    async fn after_repair(&self, app: &AppConfig, run: Option<i64>) -> (bool, Option<(String, &'static str)>) {
        let mut success = true;
        let mut lines = vec![];
        let mut redone = BTreeSet::new();
        let mut failed = BTreeSet::new();
        if let Some(id) = run {
            let results = loop {
                match self.state().get_run(id).await {
                    Ok(Some(r)) if matches!(r.status.as_str(), "queued" | "running") => {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    Ok(Some(r)) => {
                        success = r.status == "ok";
                        break r.result["results"].clone();
                    }
                    _ => {
                        success = false;
                        break Value::Null;
                    }
                }
            };
            for r in results.as_array().into_iter().flatten() {
                let other = if r["src"] == json!(app.name) { &r["dst"] } else { &r["src"] };
                let Some(other) = other.as_str() else { continue };
                match r["action"].as_str() {
                    // "kept": saved with the new key, nothing else to change
                    Some("created" | "updated" | "kept") => redone.insert(other.to_string()),
                    Some("failed") => failed.insert(other.to_string()),
                    _ => false,
                };
            }
        }
        if !success {
            let label = run.map_or_else(|| "Reconnect run".into(), |id| format!("Reconnect run #{id}"));
            lines.push(format!("{label} did not finish successfully; wranglarr will retry the key repair."));
        }
        if !redone.is_empty() {
            let list: Vec<&str> = redone.iter().map(String::as_str).collect();
            lines.push(format!("wranglarr reconnected {} with the new key.", list.join(", ")));
        }
        if !failed.is_empty() {
            let list: Vec<&str> = failed.iter().map(String::as_str).collect();
            let run = run.map(|id| format!(" (run #{id})")).unwrap_or_default();
            lines.push(format!(
                "Reconnecting {} failed{run}: press Connect apps on the System page once it's sorted.",
                list.join(", ")
            ));
        }
        if app.has(Roles::INDEXERS) && app.has(Roles::ARR) {
            // the indexers it pushed to the arrs carry its key
            let synced = async {
                let c = service::client(app).await?;
                let arr = c.arr().ok_or_else(|| Error::internal("not an arr"))?;
                arr.post("command", &json!({"name": "ApplicationIndexerSync", "forceSync": true})).await?;
                Ok::<_, Error>(())
            }
            .await;
            match synced {
                Ok(()) => lines.push(format!("{} is pushing its indexers to the apps again.", app.name)),
                Err(e) => {
                    failed.insert(app.name.clone());
                    lines.push(format!("Asking {} to push its indexers to the apps again failed: {e}", app.name));
                }
            }
        }
        success &= failed.is_empty();
        let notice = (!lines.is_empty()).then(|| (lines.join(" "), if success { "info" } else { "warning" }));
        (success, notice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repairs_clear_only_successful_matching_generations() {
        let (_dir, app) = crate::app::tests::app().await;
        app.state()
            .write(|tx| {
                tx.put(SEEN, &json!({"radarr":"new"}))?;
                tx.put(REPAIR, &BTreeSet::from(["radarr".to_string()]))
            })
            .await
            .expect("pending");
        app.finish_key_repair("radarr", json!("new"), false).await.expect("failed repair");
        assert_eq!(app.state().get_value(REPAIR).await.expect("state"), Some(json!(["radarr"])));
        app.finish_key_repair("radarr", json!("old"), true).await.expect("old repair");
        assert_eq!(app.state().get_value(REPAIR).await.expect("state"), Some(json!(["radarr"])));
        app.finish_key_repair("radarr", json!("new"), true).await.expect("current repair");
        assert_eq!(app.state().get_value(REPAIR).await.expect("state"), Some(json!([])));
    }

    #[test]
    fn failed_repairs_back_off_and_warn_once_per_key() {
        let st = KeysState::default();
        let (k1, k2) = (json!("fp1"), json!("fp2"));
        assert!(st.due("radarr", &k1));
        assert!(st.failed("radarr", &k1), "the first failure is told");
        assert!(!st.due("radarr", &k1), "then it waits");
        assert!(!st.failed("radarr", &k1), "a second failure stays quiet");
        assert_eq!(st.retries.lock().expect("retries")["radarr"].wait, FIRST_WAIT * 2);
        for _ in 0..10 {
            st.failed("radarr", &k1);
        }
        assert_eq!(st.retries.lock().expect("retries")["radarr"].wait, MAX_WAIT);
        assert!(st.due("radarr", &k2), "a newer key starts over");
        assert!(st.failed("radarr", &k2), "and is told again");
        st.succeeded("radarr");
        assert!(st.due("radarr", &k2));
    }

    #[test]
    fn fingerprints() {
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint("abc").len(), 24);
        assert!(!fingerprint("abc").contains("abc"));
    }
}
