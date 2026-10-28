// SPDX-License-Identifier: GPL-3.0-or-later
//! Live status: the arrs and Prowlarr tell wranglarr when something changes.
//!
//! wranglarr keeps a `wranglarr · live status` webhook in each one's Settings →
//! Connect. The arr calls `POST /api/hooks/<app>` on a health issue, a health
//! issue clearing, an import, an update and so on, signed with that app's own
//! secret (HTTP basic auth: the app's name and a token derived from a secret in
//! the state db). wranglarr then refreshes the app's suite overview and tells the
//! signed-in pages (`GET /api/events/stream`, Server-Sent Events), which redraw
//! that suite. Without it a fixed problem shows when the five-minute cache runs out.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, watch};

use crate::app::App;
use crate::arr_notifications;
use crate::background::Background;
use crate::config::AppConfig;
use crate::error::{Error, Result};
use crate::service::{self, AppConfigExt, Roles};
use crate::time::now_iso;

/// The webhook's name in the arrs, after [`arr_notifications::PREFIX`].
pub const ENTRY: &str = "live status";
const SECRET: &str = "events:secret";
const WIRED: &str = "events:wired";
/// Events that arrive together (an import and its health check) make one refresh.
const SETTLE: Duration = Duration::from_secs(2);
/// How long the start-up wiring keeps trying apps that don't answer yet.
const WIRE_FOR: Duration = Duration::from_secs(900);

/// The subscribers, the refreshes waiting to run and the last event per app.
pub struct EventsState {
    tx: broadcast::Sender<Value>,
    /// suite -> the apps whose events it's waiting to show
    pending: Mutex<HashMap<String, BTreeSet<String>>>,
    /// app -> (event type, when)
    last: Mutex<HashMap<String, (String, String)>>,
    started: AtomicBool,
    tasks: Background,
    /// the secret the tokens come from, once read
    secret: tokio::sync::OnceCell<String>,
    /// true once wranglarr is stopping: the open streams end, so the server can stop
    closing: watch::Sender<bool>,
}

impl Default for EventsState {
    fn default() -> Self {
        EventsState {
            tx: broadcast::channel(64).0,
            pending: Mutex::default(),
            last: Mutex::default(),
            started: AtomicBool::new(false),
            tasks: Background::default(),
            secret: tokio::sync::OnceCell::new(),
            closing: watch::channel(false).0,
        }
    }
}

impl EventsState {
    /// End the open streams (the server waits for every connection to close).
    pub(crate) fn close(&self) {
        self.closing.send_replace(true);
    }

    pub(crate) async fn shutdown(&self) {
        self.tasks.shutdown().await;
    }
}

/// The token `app` signs its webhook calls with.
fn token(secret: &str, app: &str) -> String {
    let d = Sha256::digest(format!("wranglarr-events:{secret}:{app}").as_bytes());
    hex::encode(&d[..16])
}

/// `user:password` from an `Authorization: Basic …` header.
fn basic_auth(header: &str) -> Option<(String, String)> {
    use data_encoding::BASE64;
    let b64 = header.strip_prefix("Basic ").or_else(|| header.strip_prefix("basic "))?;
    let raw = BASE64.decode(b64.trim().as_bytes()).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let (u, p) = text.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

/// Start-up work: keep the webhook in every arr (or take it out with `[events] enabled = false`).
pub fn start(app: &App) {
    let st = app.events_state();
    if st.started.swap(true, Ordering::SeqCst) {
        return;
    }
    let me = app.clone();
    st.tasks.spawn("events: start-up", async move { me.wire_events_at_start().await });
}

impl App {
    /// The apps that can send events: the arrs and Prowlarr.
    pub(crate) fn event_apps(&self) -> Vec<AppConfig> {
        self.cfg().all_apps().filter(|a| a.has(Roles::ARR) && a.enabled).cloned().collect()
    }

    /// Where the apps send their events.
    pub(crate) fn hook_url(&self, app: &str) -> String {
        let base = match self.cfg().events.url.as_str() {
            "" => format!("http://wranglarr:{}", self.cfg().port),
            u => u.to_string(),
        };
        format!("{base}/api/hooks/{app}")
    }

    async fn events_secret(&self) -> Result<String> {
        let s = self
            .events_state()
            .secret
            .get_or_try_init(|| async {
                self.state()
                    .write(|tx| {
                        if let Some(s) = tx.get::<String>(SECRET)?.filter(|s| !s.is_empty()) {
                            return Ok(s);
                        }
                        let s = crate::ids::token_hex(32);
                        tx.put(SECRET, &s)?;
                        Ok(s)
                    })
                    .await
            })
            .await?;
        Ok(s.clone())
    }

    /// Put (or update) the webhook into `apps`, or remove it when events are off.
    /// One result per app, which is also kept for the System page.
    pub async fn wire_events(&self, apps: &[AppConfig]) -> Result<Vec<Value>> {
        let secret = self.events_secret().await?;
        let on = self.cfg().events.enabled;
        let mut out = vec![];
        for a in apps {
            let tok = token(&secret, &a.name);
            let mut values = Map::new();
            values.insert("url".into(), json!(self.hook_url(&a.name)));
            values.insert("method".into(), json!(1)); // POST
            values.insert("username".into(), json!(a.name));
            values.insert("password".into(), json!(tok));
            let mut extra = Map::new();
            extra.insert("includeHealthWarnings".into(), json!(true));
            let names: HashSet<String> = [a.name.clone()].into();
            let none = HashSet::new();
            let (want, had) = if on { (&names, &none) } else { (&none, &names) };
            let body_for = |schema: &[Value]| {
                arr_notifications::entry_body(schema, "Webhook", ENTRY, &values, |_| true, Some(&extra))
            };
            let scrub = |e: &str| e.replace(&tok, "…");
            let mut res =
                arr_notifications::sync(std::slice::from_ref(a), Some(ENTRY), Some(ENTRY), want, had, body_for, scrub)
                    .await
                    .pop()
                    .unwrap_or_else(|| json!({"app": a.name, "action": "none"}));
            res["at"] = json!(now_iso());
            out.push(res);
        }
        let rows = out.clone();
        self.state()
            .write(move |tx| {
                let mut all: Map<String, Value> = tx.get(WIRED)?.unwrap_or_default();
                for r in rows {
                    if let Some(name) = r["app"].as_str() {
                        all.insert(name.to_string(), r.clone());
                    }
                }
                tx.put(WIRED, &all)
            })
            .await?;
        Ok(out)
    }

    /// At start-up: once the apps answer (and with them wranglarr, whose address
    /// they test before saving), wire each one; retry the ones that failed.
    async fn wire_events_at_start(&self) {
        let deadline = tokio::time::Instant::now() + WIRE_FOR;
        let mut todo = self.event_apps();
        tokio::time::sleep(Duration::from_secs(5)).await; // the listener is bound right after start
        while !todo.is_empty() && tokio::time::Instant::now() < deadline {
            let mut up = vec![];
            for a in &todo {
                if let Ok(c) = service::client(a).await
                    && c.ping().await
                {
                    up.push(a.clone());
                }
            }
            if !up.is_empty() {
                match self.wire_events(&up).await {
                    Ok(res) => {
                        let done: HashSet<&str> =
                            res.iter().filter(|r| r.get("error").is_none()).filter_map(|r| r["app"].as_str()).collect();
                        for r in res.iter().filter(|r| r.get("error").is_some()) {
                            tracing::warn!("events: {}: {}", r["app"].as_str().unwrap_or("?"), r["error"]);
                        }
                        todo.retain(|a| !done.contains(a.name.as_str()));
                    }
                    Err(e) => tracing::warn!("events: {e}"),
                }
            }
            if !todo.is_empty() {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
        if !todo.is_empty() {
            let names: Vec<&str> = todo.iter().map(|a| a.name.as_str()).collect();
            tracing::warn!("events: no live status from {} (see System → Live status)", names.join(", "));
        }
    }

    /// An event from `app`: checked against its token, then its suite is refreshed.
    pub async fn receive_event(&self, app: &str, authorization: &str, body: &Value) -> Result<()> {
        let denied = || Error::Unauthorized("unknown app or wrong credentials".into());
        let Some(cfg) = self.event_apps().into_iter().find(|a| a.name == app) else {
            return Err(denied());
        };
        let (user, pass) = basic_auth(authorization).ok_or_else(denied)?;
        let want = token(&self.events_secret().await?, &cfg.name);
        if user != cfg.name || !bool::from(pass.as_bytes().ct_eq(want.as_bytes())) {
            return Err(denied());
        }
        let event = body["eventType"].as_str().unwrap_or("unknown").to_string();
        if let Ok(mut last) = self.events_state().last.lock() {
            last.insert(cfg.name.clone(), (event.clone(), now_iso()));
        }
        if event == "Test" {
            return Ok(()); // the arr checking the address before it saves the entry
        }
        crate::keys::event_seen(self, &cfg).await;
        if let Some(suite) = self.cfg().suites.iter().find(|s| s.apps.iter().any(|a| a.name == cfg.name)) {
            self.refresh_soon(&suite.name, &cfg.name, &event);
        }
        Ok(())
    }

    /// Refresh `suite`'s overview shortly (events in a burst make one refresh),
    /// then tell the pages.
    fn refresh_soon(&self, suite: &str, app: &str, event: &str) {
        let st = self.events_state();
        let first = match st.pending.lock() {
            Ok(mut p) => {
                let apps = p.entry(suite.to_string()).or_default();
                let first = apps.is_empty();
                apps.insert(app.to_string());
                first
            }
            Err(_) => return,
        };
        if !first {
            return;
        }
        let (me, suite, event) = (self.clone(), suite.to_string(), event.to_string());
        st.tasks.spawn("events: refresh", async move {
            tokio::time::sleep(SETTLE).await;
            let apps: Vec<String> = me
                .events_state()
                .pending
                .lock()
                .ok()
                .and_then(|mut p| p.remove(&suite))
                .map(|s| s.into_iter().collect())
                .unwrap_or_default();
            if let Err(e) = me.overview(&suite, true).await {
                tracing::debug!("events: {suite}: {e}");
            }
            me.publish(json!({"type": "overview", "suite": suite, "apps": apps, "event": event}));
        });
    }

    /// Tell every open page.
    pub(crate) fn publish(&self, msg: Value) {
        let _ = self.events_state().tx.send(msg); // no page open: nobody to tell
    }

    /// The events for one page, and the flag that ends its stream.
    pub(crate) fn subscribe_events(&self) -> (broadcast::Receiver<Value>, watch::Receiver<bool>) {
        let st = self.events_state();
        (st.tx.subscribe(), st.closing.subscribe())
    }

    /// For the System page: per app, whether the webhook is in place and the last event.
    pub async fn events_status(&self) -> Result<Value> {
        let wired: Map<String, Value> = self.state().get(WIRED).await?.unwrap_or_default();
        let last = self.events_state().last.lock().map(|l| l.clone()).unwrap_or_default();
        let apps: Vec<Value> = self
            .event_apps()
            .iter()
            .map(|a| {
                let w = wired.get(&a.name);
                json!({
                    "name": a.name, "kind": a.kind,
                    "action": w.and_then(|w| w.get("action")).cloned().unwrap_or(Value::Null),
                    "error": w.and_then(|w| w.get("error")).cloned().unwrap_or(Value::Null),
                    "wired_at": w.and_then(|w| w.get("at")).cloned().unwrap_or(Value::Null),
                    "last_event": last.get(&a.name).map(|(e, at)| json!({"event": e, "at": at})),
                })
            })
            .collect();
        Ok(json!({"enabled": self.cfg().events.enabled, "url": self.hook_url("<app>"), "apps": apps}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_differ_per_app_and_secret() {
        assert_eq!(token("s", "radarr"), token("s", "radarr"));
        assert_ne!(token("s", "radarr"), token("s", "sonarr"));
        assert_ne!(token("s", "radarr"), token("t", "radarr"));
        assert_eq!(token("s", "radarr").len(), 32);
    }

    #[test]
    fn reads_basic_auth() {
        assert_eq!(basic_auth("Basic cmFkYXJyOnNlY3JldA=="), Some(("radarr".into(), "secret".into())));
        assert_eq!(basic_auth("Bearer x"), None);
        assert_eq!(basic_auth("Basic !!!"), None);
    }
}
