// SPDX-License-Identifier: GPL-3.0-or-later
//! The watcher: on a timer it looks at the stack, turns what it finds into
//! conditions, and works out which ones are new or cleared since the last round.

use std::collections::HashSet;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::config::AppConfig;
use crate::service::{self, Roles};
use crate::value::{head, text, truthy};

use super::KEY_STATE;
use super::catalog::event;
use super::runs::human;
use super::service::Notifier;
use super::settings::{Settings, setting_int};
use crate::time::{now_iso, unix_iso, unix_now};

// ---- the watcher's logic ----------------------------------------------------------------

/// Something wrong right now.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Cond {
    pub event: &'static str,
    pub level: &'static str,
    pub title: String,
    pub message: String,
    /// Seen this many rounds in a row before it's sent.
    pub confirm: i64,
    /// And this many seconds after it was first seen.
    pub after: f64,
    /// (title, message) to send when it clears.
    pub clear: Option<(String, String)>,
    /// Sent together with the others of the group: one message.
    pub group: Option<&'static str>,
}

impl Cond {
    pub(super) fn new(event: &'static str, level: &'static str, title: String, message: String) -> Cond {
        Cond { event, level, title, message, confirm: 1, after: 0.0, clear: None, group: None }
    }
}

/// Conditions in the order found; the same key twice keeps its place.
#[derive(Debug, Default)]
pub(super) struct Conds(pub Vec<(String, Cond)>);

impl Conds {
    pub(super) fn put(&mut self, key: String, c: Cond) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = c,
            None => self.0.push((key, c)),
        }
    }
    fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(k, _)| k == key)
    }
}

/// What one app looks like right now.
#[derive(Debug, Clone, Default)]
pub(super) struct Probe {
    pub name: String,
    pub url: String,
    pub online: bool,
    pub error: Option<String>,
    pub health: Vec<Value>,
    pub queue: Vec<(String, String, String)>,
    pub roots: Vec<Value>,
}

/// Everything the watcher looked at in one round.
#[derive(Debug, Default)]
pub(super) struct Inputs {
    pub probes: Vec<Probe>,
    /// (path, free bytes) for wranglarr's own storage
    pub backups_disk: Option<(String, f64)>,
    /// gluetun's status, when a VPN is configured
    pub vpn: Option<Value>,
    /// downloads: {client, id, name, state, state_text, arr: {name} | null}
    pub downloads: Vec<Value>,
    /// update checks: container -> {status, held, remote_digest}
    pub updates: Vec<(String, Value)>,
}

/// `conditions()`: everything wrong right now, keyed so the same problem keeps the same key.
pub(super) fn build_conditions(inp: &Inputs, st: &Settings) -> Conds {
    let mut out = Conds::default();
    let mut disks: Vec<(String, f64)> = vec![];
    let mut put_disk = |path: String, free: f64| match disks.iter_mut().find(|(p, _)| *p == path) {
        Some(d) => d.1 = free,
        None => disks.push((path, free)),
    };
    for p in &inp.probes {
        if !p.online {
            let mut c = Cond::new(
                "app_down",
                "error",
                format!("{} is offline", p.name),
                p.error
                    .clone()
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| format!("wranglarr can't reach {}.", p.url)),
            );
            c.confirm = 2;
            c.clear = Some((format!("{} is back", p.name), format!("{} answers again.", p.name)));
            out.put(format!("down:{}", p.name), c);
            continue;
        }
        for hc in &p.health {
            let msg = text(&hc["message"]);
            let src = match hc.get("source").filter(|s| truthy(s)) {
                Some(s) => text(s),
                None => head(&msg, 40),
            };
            let typ = hc.get("type").map_or_else(|| "warning".to_string(), text);
            out.put(
                format!("health:{}:{src}", p.name),
                Cond::new("app_health", "warning", format!("{}: health {typ}", p.name), msg),
            );
        }
        for (id, title, message) in &p.queue {
            out.put(
                format!("queue:{}:{id}", p.name),
                Cond::new(
                    "import_blocked",
                    "warning",
                    head(&format!("{} needs you: {title}", p.name), 200),
                    message.clone(),
                ),
            );
        }
        for r in &p.roots {
            if let Some(free) = r.get("freeSpace").filter(|v| !v.is_null())
                && truthy(&r["path"])
            {
                put_disk(text(&r["path"]), free.as_f64().unwrap_or(0.0));
            }
        }
    }
    if let Some((path, free)) = &inp.backups_disk {
        put_disk(format!("{path} (backups)"), *free);
    }
    let gb = st.disk_free_gb;
    let limit = gb * 1024f64.powi(3);
    if limit != 0.0 {
        for (path, free) in &disks {
            if *free < limit {
                out.put(
                    format!("disk:{path}"),
                    Cond::new(
                        "disk_low",
                        "warning",
                        format!("Low disk space: {path}"),
                        format!("{} free (warning below {} GB).", human(&json!(free)), gb),
                    ),
                );
            }
        }
    }
    if let Some(v) = &inp.vpn
        && !truthy(&v["ok"])
    {
        let status = if truthy(&v["status"]) { text(&v["status"]) } else { "no status".into() };
        let msg = if truthy(&v["error"]) {
            text(&v["error"])
        } else {
            format!("gluetun reports {status}; the download clients behind it are cut off.")
        };
        let ip = if truthy(&v["public_ip"]) { text(&v["public_ip"]) } else { "no IP yet".into() };
        let mut c = Cond::new("vpn_down", "error", "The VPN is down".into(), msg);
        c.confirm = 2;
        c.clear = Some(("The VPN is back up".into(), format!("gluetun is running again ({ip}).")));
        out.put("vpn".into(), c);
    }
    let stall = st.stall_minutes;
    for d in &inp.downloads {
        let who = match d.get("arr").filter(|a| truthy(a)) {
            Some(a) => format!(" ({})", text(&a["name"])),
            None => String::new(),
        };
        let (client, id, name) = (text(&d["client"]), text(&d["id"]), text(&d["name"]));
        let state = text(&d["state"]);
        let state_text = text(&d["state_text"]);
        if state == "error" {
            out.put(
                format!("dl:{client}:{id}"),
                Cond::new(
                    "download_problem",
                    "warning",
                    format!("Download error in {client}{who}"),
                    format!("{name}: {}", if state_text.is_empty() { "error" } else { &state_text }),
                ),
            );
        } else if (state == "stalled" || state == "metadata") && stall != 0 {
            let mut c = Cond::new(
                "download_problem",
                "warning",
                format!("Download stuck in {client}{who}"),
                format!("{name}: {} for over {stall} min.", if state_text.is_empty() { &state } else { &state_text }),
            );
            c.after = (stall * 60) as f64;
            out.put(format!("dl:{client}:{id}"), c);
        }
    }
    let mut avail: Vec<&(String, Value)> =
        inp.updates.iter().filter(|(_, s)| s["status"] == "available" && !truthy(&s["held"])).collect();
    avail.sort_by(|a, b| a.0.cmp(&b.0));
    for (n, s) in avail {
        let digest = s.get("remote_digest").map(text).unwrap_or_default();
        let mut c = Cond::new("update_available", "info", "Container updates available".into(), n.clone());
        c.group = Some("updates");
        out.put(format!("upd:{n}:{digest}"), c);
    }
    // a newer wranglarr: once per new image, with how to update it
    for (n, s) in inp.updates.iter().filter(|(_, s)| s["status"] == "self" && truthy(&s["newer"])) {
        let digest = s.get("remote_digest").map(text).unwrap_or_default();
        let hint = s.get("update_hint").map(text).unwrap_or_default();
        let how: String = if s["self_update"] == true {
            format!("Press Update now on the dashboard or the Updates page, or {hint}")
        } else {
            let mut hint = hint.chars();
            hint.next().map(|c| c.to_uppercase().chain(hint).collect()).unwrap_or_default()
        };
        let message = format!("{how}. Release notes: {}", crate::updates::RELEASE_NOTES);
        out.put(
            format!("self:{n}:{digest}"),
            Cond::new("self_update_available", "info", "A newer wranglarr is out".into(), message),
        );
    }
    out
}

/// A message the watcher sends.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Send {
    pub event: String,
    pub title: String,
    pub message: String,
    pub level: String,
}

/// One round of the watcher, on what it remembers (`prev`) and what is wrong now:
/// what to remember next, what to send, and how many problems were sent.
pub(super) fn step(
    prev: &Map<String, Value>,
    cur: &Conds,
    now: f64,
    listening: bool,
    offline: &HashSet<String>,
) -> (Map<String, Value>, Vec<Send>, usize) {
    let mut seen = Map::new();
    let mut new: Vec<&Cond> = vec![];
    let mut grouped: Vec<(&str, Vec<&Cond>)> = vec![];
    for (key, c) in &cur.0 {
        let p = prev.get(key).filter(|p| truthy(p));
        let first = p.and_then(|p| p["first"].as_f64()).unwrap_or(now);
        let count = p.and_then(|p| setting_int(&p["count"])).unwrap_or(0) + 1;
        let mut sent = p.is_some_and(|p| truthy(&p["sent"]));
        let due = count >= c.confirm && now - first >= c.after;
        if due && !sent && listening {
            sent = true;
            match c.group {
                Some(g) => match grouped.iter_mut().find(|(n, _)| *n == g) {
                    Some(slot) => slot.1.push(c),
                    None => grouped.push((g, vec![c])),
                },
                None => new.push(c),
            }
        }
        let clear = c.clear.as_ref().map_or(Value::Null, |(a, b)| json!([a, b]));
        seen.insert(
            key.clone(),
            json!({"first": first, "count": count, "sent": sent, "event": c.event, "clear": clear,
                   "title": c.title, "message": head(&c.message, 200)}),
        );
    }
    let mut by_event: Vec<(&str, Vec<&Cond>)> = vec![];
    for c in new {
        match by_event.iter_mut().find(|(e, _)| *e == c.event) {
            Some(slot) => slot.1.push(c),
            None => by_event.push((c.event, vec![c])),
        }
    }
    let mut sends = vec![];
    // several of a kind in one round: one message
    for (ev, items) in &by_event {
        if let [c] = items.as_slice() {
            sends.push(Send {
                event: ev.to_string(),
                title: c.title.clone(),
                message: c.message.clone(),
                level: c.level.into(),
            });
        } else {
            let lvl = if items.iter().any(|i| i.level == "error") { "error" } else { items[0].level };
            let plural = event(ev).and_then(|e| e.plural).unwrap_or(ev);
            sends.push(Send {
                event: ev.to_string(),
                title: format!("{} {plural}", items.len()),
                message: items.iter().map(|i| format!("• {}: {}", i.title, i.message)).collect::<Vec<_>>().join("\n"),
                level: lvl.into(),
            });
        }
    }
    for (_, items) in &grouped {
        let c = items[0];
        let mut msgs: Vec<&str> = items.iter().map(|i| i.message.as_str()).collect();
        msgs.sort_unstable();
        sends.push(Send {
            event: c.event.into(),
            title: c.title.clone(),
            message: msgs.join(", "),
            level: c.level.into(),
        });
    }
    for (key, p) in prev {
        if cur.has(key) {
            continue;
        }
        let parts: Vec<&str> = key.split(':').collect();
        if parts.len() > 1 && (parts[0] == "health" || parts[0] == "queue") && offline.contains(parts[1]) {
            seen.insert(key.clone(), p.clone()); // can't see it while the app is down: keep it
            continue;
        }
        if truthy(&p["sent"])
            && truthy(&p["clear"])
            && let Some([a, b]) = p["clear"].as_array().map(Vec::as_slice)
        {
            sends.push(Send { event: text(&p["event"]), title: text(a), message: text(b), level: "ok".into() });
        }
    }
    (seen, sends, by_event.len() + grouped.len())
}

/// Queue items an arr has flagged: import blocked, failed, needs a manual import.
/// (id, title, message)
pub(super) fn queue_problems(q: &Value) -> Vec<(String, String, String)> {
    let records = match q {
        Value::Object(o) => o.get("records").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    let mut out = vec![];
    for r in records.as_array().into_iter().flatten() {
        let status = text(&r["trackedDownloadStatus"]);
        let state_raw = text(&r["trackedDownloadState"]);
        if !matches!(status.as_str(), "warning" | "error")
            && !matches!(state_raw.as_str(), "importBlocked" | "failedPending")
        {
            continue;
        }
        let mut msgs: Vec<String> = vec![];
        for s in r["statusMessages"].as_array().into_iter().flatten() {
            let list = match s.get("messages").filter(|m| truthy(m)) {
                Some(Value::Array(a)) => a.clone(),
                _ => vec![s.get("title").cloned().unwrap_or(Value::Null)],
            };
            for m in list.iter().filter(|m| truthy(m)) {
                let m = text(m);
                if !msgs.contains(&m) {
                    msgs.push(m);
                }
            }
        }
        let state = match state_raw.as_str() {
            "importBlocked" => "import blocked".to_string(),
            "importPending" => "waiting to import".to_string(),
            "failedPending" => "failed".to_string(),
            s => s.to_string(),
        };
        let id = if truthy(&r["downloadId"]) { text(&r["downloadId"]) } else { text(&r["id"]) };
        let title = if truthy(&r["title"]) { text(&r["title"]) } else { "?".into() };
        let detail = if !msgs.is_empty() {
            msgs.join("; ")
        } else if truthy(&r["errorMessage"]) {
            text(&r["errorMessage"])
        } else {
            "check the app's Activity → Queue".into()
        };
        let message = if state.is_empty() { detail } else { format!("{state}: {detail}") };
        out.push((id, title, message));
    }
    out
}

impl Notifier {
    // ---- the watcher -------------------------------------------------------------------------
    /// (Re)start the watcher: every `interval` minutes from now, while a channel is on.
    pub async fn reschedule(&self, app: &App) {
        let _gate = self.rescheduling.lock().await;
        let on = self.channels(app).await.iter().any(|c| c.enabled);
        let minutes = self.settings(app).await.interval;
        let Ok(mut w) = self.watch.lock() else { return };
        if let Some(h) = w.take() {
            h.abort();
        }
        if !on {
            return;
        }
        let app = app.clone();
        *w = Some(tokio::spawn(async move {
            loop {
                app.notifier().check(&app).await;
                tokio::time::sleep(Duration::from_secs(minutes.max(1) as u64 * 60)).await;
            }
        }));
    }

    /// Whether the watcher is on.
    pub fn watching(&self) -> bool {
        self.watch.lock().is_ok_and(|w| w.as_ref().is_some_and(|h| !h.is_finished()))
    }

    /// Look at the stack: the watcher's inputs, and the apps that are offline.
    pub(super) async fn gather(&self, app: &App) -> (Inputs, HashSet<String>) {
        let apps: Vec<AppConfig> = app.cfg().all_apps().filter(|a| a.enabled).cloned().collect();
        let probes: Vec<Probe> = futures::stream::iter(apps.iter().map(probe)).buffered(8).collect().await;
        let offline: HashSet<String> = probes.iter().filter(|p| !p.online).map(|p| p.name.clone()).collect();
        let root = app.cfg().storage_root.clone();
        let backups_disk = tokio::task::spawn_blocking(move || {
            rustix::fs::statvfs(&root)
                .ok()
                .map(|s| (root.display().to_string(), (s.f_bavail as f64) * (s.f_frsize as f64)))
        })
        .await
        .ok()
        .flatten();
        let vpn = if app.cfg().vpn.is_some() { Some(app.vpn().status(true).await) } else { None };
        let downloads = download_items(app).await;
        let updates = update_checks(app).await;
        (Inputs { probes, backups_disk, vpn, downloads, updates }, offline)
    }

    /// One round of the watcher: send what's new, and the recoveries.
    pub async fn check(&self, app: &App) -> Value {
        let Ok(_g) = self.checking.try_lock() else {
            return json!({"skipped": "a check is already running"});
        };
        let (inputs, offline) = self.gather(app).await;
        let cur = build_conditions(&inputs, &self.settings(app).await);
        let prev = match app.state().get_value(KEY_STATE).await {
            Ok(Some(Value::Object(m))) => m,
            _ => Map::new(),
        };
        // no channel: track, but don't use them up
        let listening = self.channels(app).await.iter().any(|c| c.enabled);
        let (seen, sends, sent) = step(&prev, &cur, unix_now(), listening, &offline);
        for s in sends {
            self.emit(app, &s.event, &s.title, &s.message, Some(&s.level));
        }
        if let Err(e) = app.state().put(KEY_STATE, &seen).await {
            tracing::warn!("notify: could not save the watcher's state: {e}");
        }
        json!({"at": now_iso(), "problems": cur.0.len(), "sent": sent})
    }

    /// What the watcher is tracking now, for the page.
    pub(super) async fn current(&self, app: &App) -> Vec<Value> {
        let st = match app.state().get_value(KEY_STATE).await {
            Ok(Some(Value::Object(m))) => m,
            _ => Map::new(),
        };
        let mut out: Vec<Value> = st
            .iter()
            .map(|(key, p)| {
                let title = if truthy(&p["title"]) { text(&p["title"]) } else { key.clone() };
                json!({"key": key, "event": p.get("event"), "title": title, "message": text(&p["message"]),
                       "since": unix_iso(p["first"].as_f64().unwrap_or(0.0)), "sent": p.get("sent")})
            })
            .collect();
        out.sort_by(|a, b| text(&a["since"]).cmp(&text(&b["since"])));
        out
    }
}

/// One app's state for the watcher.
async fn probe(a: &AppConfig) -> Probe {
    let mut res = Probe { name: a.name.clone(), url: a.url.clone(), ..Default::default() };
    let c = match service::client(a).await {
        Ok(c) => c,
        Err(e) => {
            res.error = Some(e.to_string());
            return res;
        }
    };
    res.online = c.ping().await;
    if !res.online {
        return res;
    }
    res.health = c
        .health()
        .await
        .into_iter()
        .filter(|x| matches!(x.get("type").and_then(Value::as_str), Some("warning" | "error")))
        .collect();
    if let Some(arr) = c.arr()
        && arr.def().has(Roles::LIBRARY)
    {
        let unknown: String = arr.arr_def().queue_unknown.iter().map(|p| format!("&{p}=true")).collect();
        if let Ok(q) = arr.get(&format!("queue?pageSize=200{unknown}")).await {
            res.queue = queue_problems(&q);
        }
        if let Ok(r) = arr.list("rootfolder").await {
            res.roots = r;
        }
    }
    res
}

/// The current downloads across the download clients, tagged with the arr
/// that sent each one.
async fn download_items(app: &App) -> Vec<Value> {
    app.downloads().list(true).await["items"].as_array().cloned().unwrap_or_default()
}

/// The updater's last check: container -> {status, held, remote_digest, …}.
async fn update_checks(app: &App) -> Vec<(String, Value)> {
    app.updates().last_check().await.into_iter().collect()
}
