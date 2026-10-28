// SPDX-License-Identifier: GPL-3.0-or-later
//! The notifier: channels and settings in the state db, sending and the log,
//! events from the rest of the app, and the overview for the page.

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::task::JoinSet;

use crate::app::App;
use crate::arr_notifications;
use crate::config::AppConfig;
use crate::error::{Error, Result};
use crate::service::{AppConfigExt, Roles};
use crate::value::{text, truthy};

use super::arr::arr_body;
use super::catalog::{ARR_EVENTS, ARR_EVENTS_DEFAULT, event, events_json, kind, kinds_json};
use super::channel::{Channel, clean, public, scrub};
use super::delivery::{Endpoints, OutBody, build_request, check_response, send_email, send_error};
use super::runs::run_message;
use super::settings::Settings;
use super::{KEY_CHANNELS, KEY_LOG, KEY_SETTINGS, LOG_KEEP, MASK};
use crate::time::now_iso;

// ---- the service side ----------------------------------------------------------------------

/// How long shutdown waits for notifications still being sent.
const DRAIN: Duration = Duration::from_secs(5);

pub struct Notifier {
    /// Held while the watcher is restarted, so the last change's settings win.
    pub(super) rescheduling: tokio::sync::Mutex<()>,
    pub(super) checking: tokio::sync::Mutex<()>,
    pub(super) watch: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Notifications being sent in the background; shutdown waits for them.
    sending: Mutex<JoinSet<()>>,
    /// The runtime they run on, so code on a blocking thread can send too.
    runtime: OnceLock<tokio::runtime::Handle>,
    endpoints: Mutex<Endpoints>,
    http: reqwest::Client,
}

impl Default for Notifier {
    fn default() -> Self {
        Notifier::new()
    }
}

#[derive(Serialize)]
pub struct Overview {
    pub kinds: Map<String, Value>,
    pub events: Map<String, Value>,
    pub arr_events: Map<String, Value>,
    pub arr_events_default: &'static [&'static str],
    pub arrs: Vec<Value>,
    pub channels: Vec<Value>,
    pub settings: Settings,
    pub watching: Vec<Value>,
    pub log: Vec<Value>,
    pub mask: &'static str,
}

impl Notifier {
    pub fn new() -> Notifier {
        Notifier {
            rescheduling: tokio::sync::Mutex::new(()),
            checking: tokio::sync::Mutex::new(()),
            watch: Mutex::new(None),
            sending: Mutex::new(JoinSet::new()),
            runtime: OnceLock::new(),
            endpoints: Mutex::new(Endpoints::default()),
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default(),
        }
    }

    /// Send on `runtime` from now on, whichever thread an event comes from.
    pub fn attach(&self, runtime: tokio::runtime::Handle) {
        let _ = self.runtime.set(runtime);
    }

    /// Stop the watcher and wait (a few seconds at most) for what is still
    /// being sent.
    pub async fn shutdown(&self) {
        if let Some(watcher) = self.watch.lock().ok().and_then(|mut w| w.take()) {
            watcher.abort();
        }
        let Some(mut sending) = self.sending.lock().ok().map(|mut s| std::mem::take(&mut *s)) else { return };
        let drained = tokio::time::timeout(DRAIN, async { while sending.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            tracing::warn!("notify: shutting down with {} notification(s) unsent", sending.len());
        }
    }

    /// Run `send` in the background, on the attached runtime (or the current one).
    fn spawn(&self, event: &str, send: impl Future<Output = ()> + Send + 'static) {
        let Some(runtime) = self.runtime.get().cloned().or_else(|| tokio::runtime::Handle::try_current().ok()) else {
            tracing::warn!("notify: {event}: no runtime to send from");
            return;
        };
        if let Ok(mut sending) = self.sending.lock() {
            while sending.try_join_next().is_some() {}
            sending.spawn_on(send, &runtime);
        }
    }

    /// Point Telegram / Pushover somewhere else (tests).
    pub fn set_endpoints(&self, ep: Endpoints) {
        if let Ok(mut e) = self.endpoints.lock() {
            *e = ep;
        }
    }

    fn endpoints(&self) -> Endpoints {
        self.endpoints.lock().map(|e| e.clone()).unwrap_or_default()
    }

    // ---- storage -------------------------------------------------------------------------
    pub async fn channels(&self, app: &App) -> Vec<Channel> {
        app.state().get::<Vec<Channel>>(KEY_CHANNELS).await.ok().flatten().unwrap_or_default()
    }

    pub async fn channel(&self, app: &App, cid: &str) -> Result<Channel> {
        self.channels(app).await.into_iter().find(|c| c.id == cid).ok_or_else(|| Error::not_found("no such channel"))
    }

    pub async fn settings(&self, app: &App) -> Settings {
        Settings::from_stored(app.state().get_value(KEY_SETTINGS).await.ok().flatten().as_ref())
    }

    pub async fn save_settings(&self, app: &App, body: &Map<String, Value>) -> Result<Settings> {
        let body = body.clone();
        let s = app
            .state()
            .write(move |tx| {
                let s = Settings::from_stored(tx.get_value(KEY_SETTINGS)?.as_ref()).merged(&body)?;
                tx.put(KEY_SETTINGS, &s)?;
                Ok(s)
            })
            .await?;
        self.reschedule(app).await;
        Ok(s)
    }

    /// The arrs a channel can be added to.
    pub(super) fn arr_apps(app: &App) -> Vec<AppConfig> {
        app.cfg().all_apps().filter(|a| a.has(Roles::ARR) && a.enabled).cloned().collect()
    }

    fn arr_names(app: &App) -> HashSet<String> {
        Self::arr_apps(app).into_iter().map(|a| a.name).collect()
    }

    /// Add (`cid` None) or change a channel; then put it into the picked arrs.
    pub async fn save_channel(
        &self,
        app: &App,
        body: &Map<String, Value>,
        cid: Option<&str>,
    ) -> Result<(Channel, Vec<Value>)> {
        let (body, cid, names) = (body.clone(), cid.map(str::to_owned), Self::arr_names(app));
        let (c, old) = app
            .state()
            .write(move |tx| {
                let chans: Vec<Channel> = tx.get(KEY_CHANNELS)?.unwrap_or_default();
                let old = cid.as_deref().and_then(|id| chans.iter().find(|c| c.id == id).cloned());
                if cid.is_some() && old.is_none() {
                    return Err(Error::not_found("no such channel"));
                }
                let c = clean(&body, old.as_ref(), &names)?;
                if chans.iter().any(|x| x.name.to_lowercase() == c.name.to_lowercase() && x.id != c.id) {
                    return Err(Error::invalid(format!("there's already a channel called {:?}", c.name)));
                }
                let mut all: Vec<Channel> = chans.into_iter().filter(|x| x.id != c.id).collect();
                all.push(c.clone());
                tx.put(KEY_CHANNELS, &all)?;
                Ok((c, old))
            })
            .await?;
        self.reschedule(app).await;
        let pushed = self.push_to_arrs(app, Some(&c), old.as_ref()).await;
        Ok((c, pushed))
    }

    pub async fn delete_channel(&self, app: &App, cid: &str) -> Result<Vec<Value>> {
        let cid = cid.to_owned();
        let old = app
            .state()
            .write(move |tx| {
                let chans: Vec<Channel> = tx.get(KEY_CHANNELS)?.unwrap_or_default();
                let old =
                    chans.iter().find(|c| c.id == cid).cloned().ok_or_else(|| Error::not_found("no such channel"))?;
                let rest: Vec<Channel> = chans.into_iter().filter(|c| c.id != cid).collect();
                tx.put(KEY_CHANNELS, &rest)?;
                Ok(old)
            })
            .await?;
        self.reschedule(app).await;
        Ok(self.push_to_arrs(app, None, Some(&old)).await)
    }

    // ---- sending ---------------------------------------------------------------------------
    /// Send one message; the error has any secret scrubbed out.
    pub(super) async fn send(&self, c: &Channel, title: &str, message: &str, level: &str, event: &str) -> Result<()> {
        let r = if c.kind == "email" {
            send_email(c, title, message).await
        } else {
            self.send_http(c, title, message, level, event).await
        };
        r.map_err(|e| {
            let s = scrub(c, &e);
            Error::invalid(if s.is_empty() { "error".to_string() } else { s })
        })
    }

    async fn send_http(
        &self,
        c: &Channel,
        title: &str,
        message: &str,
        level: &str,
        event: &str,
    ) -> std::result::Result<(), String> {
        let out = build_request(c, title, message, level, event, &now_iso(), &self.endpoints())?;
        let mut rb = self.http.post(&out.url);
        for (k, v) in &out.headers {
            let hv = reqwest::header::HeaderValue::from_bytes(v).map_err(|e| format!("{k} header: {e}"))?;
            rb = rb.header(k.as_str(), hv);
        }
        if let Some((u, p)) = &out.basic {
            rb = rb.basic_auth(u, Some(p));
        }
        rb = match &out.body {
            OutBody::Json(v) => rb.json(v),
            OutBody::Form(f) => rb.form(f),
            OutBody::Bytes(b) => rb.body(b.clone()),
        };
        let r = rb.send().await.map_err(|e| send_error(&e))?;
        let status = r.status().as_u16();
        let body = crate::http::response_text(r).await.unwrap_or_default();
        check_response(status, &body, out.what)
    }

    /// Send a test message with a channel from the page (or the saved one, by id).
    pub async fn test(&self, app: &App, body: &Map<String, Value>, cid: Option<&str>) -> Result<Value> {
        let has_config = body.get("config").is_some_and(truthy);
        let mut old = None;
        if let Some(id) = cid.filter(|c| !c.is_empty()) {
            match self.channel(app, id).await {
                Ok(c) => old = Some(c),
                Err(e) if !has_config => return Err(e), // nothing to test it with
                Err(_) => {}
            }
        }
        let c = match old {
            Some(o) if !has_config => o, // just the id: test the channel as saved
            old => {
                let mut b = body.clone();
                b.insert("events".into(), json!([]));
                clean(&b, old.as_ref(), &Self::arr_names(app))?
            }
        };
        let label = kind(&c.kind).map_or("", |k| k.label);
        let msg = format!("If you can read this, {label} works. wranglarr will send what you picked here.");
        Ok(match self.send(&c, "wranglarr: test", &msg, "ok", "test").await {
            Ok(()) => json!({"ok": true}),
            Err(e) => json!({"ok": false, "error": e.message()}),
        })
    }

    /// The log entry for an event, stamped now.
    fn entry(event: &str, title: &str, message: &str, level: Option<&str>) -> Value {
        let level = level
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self::event(event).map_or("info", |e| e.level).into());
        tracing::info!("notify: {event}: {title}");
        json!({"at": now_iso(), "event": event, "level": level, "title": title, "message": message, "sent": []})
    }

    /// Send `entry` to the channels that want it; log it with the results.
    async fn send_entry(&self, app: &App, entry: Value) -> Vec<Value> {
        let link = self.settings(app).await.link;
        let message = text(&entry["message"]);
        let body = if link.is_empty() { message } else { format!("{message}\n{link}") };
        let event = text(&entry["event"]);
        let chans: Vec<Channel> =
            self.channels(app).await.into_iter().filter(|c| c.enabled && c.events.contains(&event)).collect();
        self.deliver(app, chans, entry, body).await
    }

    async fn deliver(&self, app: &App, chans: Vec<Channel>, mut entry: Value, body: String) -> Vec<Value> {
        let (event, title, level) = (text(&entry["event"]), text(&entry["title"]), text(&entry["level"]));
        let mut sent = vec![];
        for c in &chans {
            match self.send(c, &title, &body, &level, &event).await {
                Ok(()) => sent.push(json!({"channel": c.name, "ok": true})),
                Err(e) => {
                    tracing::warn!("notify: {}: {e}", c.name);
                    sent.push(json!({"channel": c.name, "ok": false, "error": e.message()}));
                }
            }
        }
        entry["sent"] = json!(sent);
        self.log(app, entry).await;
        sent
    }

    /// Send to every enabled channel that wants this event, in the background.
    /// `level` None: the event's own.
    pub(super) fn emit(&self, app: &App, event: &str, title: &str, message: &str, level: Option<&str>) {
        let entry = Self::entry(event, title, message, level);
        let me = app.clone();
        self.spawn(event, async move {
            me.notifier().send_entry(&me, entry).await;
        });
    }

    /// Like `emit`, but waits: one result per channel.
    pub async fn emit_wait(
        &self,
        app: &App,
        event: &str,
        title: &str,
        message: &str,
        level: Option<&str>,
    ) -> Vec<Value> {
        self.send_entry(app, Self::entry(event, title, message, level)).await
    }

    async fn log(&self, app: &App, entry: Value) {
        let saved = app
            .state()
            .write(move |tx| {
                let mut lg: Vec<Value> = tx.get(KEY_LOG)?.unwrap_or_default();
                lg.insert(0, entry);
                lg.truncate(LOG_KEEP);
                tx.put(KEY_LOG, &lg)
            })
            .await;
        if let Err(e) = saved {
            tracing::warn!("notify: could not save the log: {e}");
        }
    }

    pub async fn history(&self, app: &App) -> Vec<Value> {
        app.state().get::<Vec<Value>>(KEY_LOG).await.ok().flatten().unwrap_or_default()
    }

    // ---- events from the rest of the service ------------------------------------------------
    /// A job ended (called from the job worker).
    #[allow(clippy::too_many_arguments)]
    pub fn run_finished(
        &self,
        app: &App,
        suite: &str,
        kind: &str,
        target: &str,
        status: &str,
        result: &Value,
        run_id: i64,
    ) {
        if let Some((ev, title, msg, level)) = run_message(suite, kind, target, status, result, run_id) {
            self.emit(app, ev, &title, &msg, Some(level));
        }
    }

    /// Someone was locked out of the sign-in (called by auth).
    pub fn lockout(&self, app: &App, ip: &str, minutes: i64) {
        self.emit(
            app,
            "sign_in_lockout",
            "Failed sign-ins to wranglarr",
            &format!("{ip} was locked out for {minutes} min after repeated wrong passwords or codes."),
            None,
        );
    }

    /// An app's API key changed; `message` says which apps wranglarr reconnected.
    pub fn api_key_changed(&self, app: &App, title: &str, message: &str, level: &str) {
        self.emit(app, "api_key_changed", title, message, Some(level));
    }

    pub async fn overview(&self, app: &App) -> Overview {
        Overview {
            kinds: kinds_json(),
            events: events_json(),
            arr_events: ARR_EVENTS.iter().map(|(id, label, _)| (id.to_string(), json!(label))).collect(),
            arr_events_default: ARR_EVENTS_DEFAULT,
            arrs: Self::arr_apps(app).iter().map(|a| json!({"name": a.name, "kind": a.kind})).collect(),
            channels: self.channels(app).await.iter().map(public).collect(),
            settings: self.settings(app).await,
            watching: self.current(app).await,
            log: self.history(app).await.into_iter().take(40).collect(),
            mask: MASK,
        }
    }

    // ---- the arrs' own Connect settings --------------------------------------------------------
    /// Add / update / remove this channel in each arr's Connect settings.
    pub(super) async fn push_to_arrs(&self, app: &App, new: Option<&Channel>, old: Option<&Channel>) -> Vec<Value> {
        let want: HashSet<String> =
            new.filter(|n| n.enabled).map(|n| n.arr.apps.iter().cloned().collect()).unwrap_or_default();
        let had: HashSet<String> = old.map(|o| o.arr.apps.iter().cloned().collect()).unwrap_or_default();
        let Some(who) = new.or(old) else { return vec![] };
        arr_notifications::sync(
            &Self::arr_apps(app),
            new.map(|n| n.name.as_str()),
            old.map(|o| o.name.as_str()),
            &want,
            &had,
            |schema| match new {
                Some(n) => arr_body(schema, n),
                None => Err("nothing to add".into()),
            },
            |t| scrub(who, t),
        )
        .await
    }
}
