// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire the stack's apps together, based on what is enabled.
//!
//! - Every download client into every arr (Radarr, Sonarr, Lidarr, ...) and into
//!   Prowlarr, with the app's own name as category, so finished downloads land in
//!   `/data/torrents/<arr>` or `/data/usenet/complete/<arr>`. Prowlarr's are for grabs
//!   from its own search page (category prowlarr: no arr imports those). SABnzbd and
//!   NZBGet categories are created first (the arrs refuse a category that doesn't exist).
//! - Sonarr and Radarr into Bazarr.
//! - Every arr into Prowlarr (as an "application"), so the indexers added in
//!   Prowlarr are pushed to the arrs by Prowlarr itself.
//! - Radarr and Sonarr into Seerr, once Seerr's own setup (a media server
//!   sign-in, which only the user can do) is done. Until then these links wait,
//!   and a watcher retries them every few minutes.
//!
//! Hosts are the compose service names. Apps that share one network namespace
//! (the shared VPN tunnel) reach each other on localhost instead.
//!
//! Modes:
//! - `new`: only links never handled before; existing settings are left alone.
//!   Runs once at startup (when `[connect] auto = true`) so a fresh stack is
//!   wired without anyone touching it, and newly enabled apps get wired on the
//!   next start. A link removed by hand is not put back.
//! - `all`: create or update every link (the "Connect apps" button).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use reqwest::Method;
use serde::Serialize;
use serde_json::{Map, Value, json};
use url::Url;

use crate::app::App;
use crate::background::Background;
use crate::config::AppConfig;
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::links::{parse_url, url_host};
use crate::service::AnyClient;
use crate::service::AppConfigExt;
use crate::service::arr::ArrClient;
use crate::service::link::{LinkHost, LinkOutcome, LinkState, LinkTarget, WireMode};
use crate::service::{self, Credentials, Roles};
use crate::state::RunStatus;
use crate::value::{head, text, truthy};

use crate::error::{Error, Result};
use crate::vpn::in_tunnel;

pub const SUITE: &str = "system";
pub const DONE_KEY: &str = "connect:done";
/// The watcher for links waiting on their source's setup.
const SEERR_JOB: &str = "connect:seerr";
/// Download client kind -> (its implementation in the arrs, display name).
pub fn dl(kind: &str) -> Option<(&'static str, &'static str)> {
    let d = service::get(kind)?;
    Some((d.downloads?.arr_impl, d.label))
}

/// The arrs Prowlarr can push indexers to: kind -> its application implementation.
pub fn prowlarr_app(kind: &str) -> Option<&'static str> {
    service::get(kind)?.arr?.prowlarr_impl
}

// ---- reading arr resources ------------------------------------------------------

static NULL: Value = Value::Null;

/// The value of the named entry in an arr resource's `fields` (Null when missing).
pub fn field<'a>(res: &'a Value, name: &str) -> &'a Value {
    res["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["name"].as_str() == Some(name))
        .map_or(&NULL, |f| &f["value"])
}

/// The field that holds a download client's category (`category`, `movieCategory`, …).
pub fn category_field(fields: &Value) -> Option<String> {
    fields.as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).find_map(|n| {
        (n == "category" || (n.ends_with("Category") && !n.ends_with("ImportedCategory"))).then(|| n.to_string())
    })
}

/// The host of an address; None when it isn't a URL with one.
pub fn host_of(url: &str) -> Option<String> {
    parse_url(url).as_ref().and_then(url_host)
}

/// A JSON object from (key, value) pairs, in order.
fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// `body["fields"]` with the values in `ours` put in.
pub(crate) fn set_fields(body: &mut Value, ours: &Map<String, Value>) {
    if let Some(fields) = body["fields"].as_array_mut() {
        for f in fields {
            if let Some(v) = f["name"].as_str().and_then(|n| ours.get(n)).cloned()
                && let Some(o) = f.as_object_mut()
            {
                o.insert("value".into(), v);
            }
        }
    }
}

/// `body` is `cur` with nothing changed but the values the app hides (`********`
/// in place of a key or password), which can't be compared.
pub(crate) fn same_but_hidden(cur: &Value, body: &Value) -> bool {
    let (Some(a), Some(b)) = (cur.as_object(), body.as_object()) else { return cur == body };
    let hidden = |v: &Value| v.as_str().is_some_and(|s| !s.is_empty() && s.chars().all(|c| c == '*'));
    a.len() == b.len()
        && a.iter().all(|(k, v)| {
            if k != "fields" {
                return b.get(k) == Some(v);
            }
            let (Some(x), Some(y)) = (v.as_array(), b[k].as_array()) else { return b.get(k) == Some(v) };
            x.len() == y.len()
                && x.iter().zip(y).all(|(f, g)| f["name"] == g["name"] && (f == g || hidden(&f["value"])))
        })
}

/// A schema entry as a new resource: without the keys the app fills in.
pub(crate) fn from_schema(schema: &Value) -> Value {
    let mut body = schema.clone();
    if let Some(o) = body.as_object_mut() {
        for k in ["id", "presets", "infoLink", "message"] {
            o.shift_remove(k);
        }
    }
    body
}

// ---- links -------------------------------------------------------------------------

/// One connection: `src` is the app being configured, `dst` the app it is pointed at.
#[derive(Debug, Clone)]
pub struct Link {
    pub src: AppConfig,
    pub dst: AppConfig,
}

impl Link {
    pub fn key(&self) -> String {
        format!("{}>{}", self.dst.name, self.src.name)
    }

    pub fn what(&self) -> String {
        if self.dst.has(Roles::DOWNLOADS) {
            return "download client".into();
        }
        if self.resource() == "applications" { "application".into() } else { self.dst.kind.clone() }
    }

    pub fn resource(&self) -> &'static str {
        if self.src.has(Roles::INDEXERS) && prowlarr_app(&self.dst.kind).is_some() {
            "applications"
        } else {
            "downloadclient"
        }
    }

    pub fn implementation(&self) -> &'static str {
        if self.resource() == "applications" {
            prowlarr_app(&self.dst.kind).unwrap_or_default()
        } else {
            dl(&self.dst.kind).map(|d| d.0).unwrap_or_default()
        }
    }

    pub fn display(&self) -> String {
        if self.dst.name != self.dst.kind {
            return self.dst.name.clone();
        }
        if let Some((_, d)) = dl(&self.dst.kind) {
            return d.into();
        }
        prowlarr_app(&self.dst.kind).map_or_else(|| self.dst.name.clone(), str::to_string)
    }
}

/// Every link between the given apps, in a fixed order.
pub fn links_between(apps: &[&AppConfig]) -> Vec<Link> {
    let link = |s: &AppConfig, d: &AppConfig| Link { src: s.clone(), dst: d.clone() };
    let mut out = vec![];
    for a in apps.iter().filter(|a| a.has(Roles::ARR)) {
        for d in apps.iter().filter(|d| dl(&d.kind).is_some()) {
            out.push(link(a, d));
        }
    }
    for p in apps.iter().filter(|a| a.has(Roles::INDEXERS)) {
        for a in apps.iter().filter(|a| prowlarr_app(&a.kind).is_some()) {
            out.push(link(p, a));
        }
    }
    // the apps that link themselves to arrs (Seerr, Bazarr), in the registry's order
    for def in service::ALL {
        let Some(takes) = def.takes else { continue };
        for s in apps.iter().filter(|a| a.kind == def.id) {
            if takes.one_each {
                for kind in takes.kinds {
                    if let Some(src) = apps.iter().find(|a| a.kind == *kind) {
                        out.push(link(s, src));
                    }
                }
            } else {
                for a in apps.iter().filter(|a| takes.kinds.contains(&a.kind.as_str())) {
                    out.push(link(s, a));
                }
            }
        }
    }
    out
}

/// The start-up task: started once, stopped at shutdown.
#[derive(Default)]
pub struct ConnectState {
    auto_started: AtomicBool,
    auto: Background,
}

impl ConnectState {
    /// Stop the start-up task, if it is still waiting for the apps.
    pub(crate) async fn shutdown(&self) {
        self.auto.shutdown().await;
    }
}

/// The app connections, as a handle on the service.
#[derive(Clone)]
pub struct Connector {
    app: App,
}

impl App {
    pub fn connect(&self) -> Connector {
        Connector { app: self.clone() }
    }
}

/// Start-up work: `[connect] auto`.
pub fn start(app: &App) {
    app.connect().start_auto(Duration::from_secs(900));
}

/// The client for a link's app, kept for the rest of a status or run.
async fn cached(clients: &mut HashMap<String, AnyClient>, app: &AppConfig) -> Result<AnyClient> {
    if let Some(c) = clients.get(&app.name) {
        return Ok(c.clone());
    }
    let c = service::client(app).await?;
    clients.insert(app.name.clone(), c.clone());
    Ok(c)
}

fn as_arr(c: &AnyClient) -> Result<&ArrClient> {
    c.arr().ok_or_else(|| Error::invalid(format!("{}: not an arr", c.name())))
}
/// (host, port, base path, ssl) of the address `src` reaches `dst` at.
type Endpoint = (String, u16, String, bool);

impl Connector {
    // ---- what to wire ------------------------------------------------------------
    pub fn links(&self) -> Vec<Link> {
        let apps: Vec<&AppConfig> = self.app.cfg().all_apps().collect();
        links_between(&apps)
    }

    /// The links handled before (kept in the state db).
    pub async fn done(&self) -> Result<HashSet<String>> {
        Ok(self.app.state().get::<Vec<String>>(DONE_KEY).await?.unwrap_or_default().into_iter().collect())
    }

    async fn mark(&self, keys: &HashSet<String>) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let keys = keys.clone();
        self.app
            .state()
            .write(move |tx| {
                let done: Vec<String> = tx.get(DONE_KEY)?.unwrap_or_default();
                let all: BTreeSet<String> = done.into_iter().chain(keys).collect();
                tx.put(DONE_KEY, &all.into_iter().collect::<Vec<_>>())
            })
            .await
    }

    // ---- addressing ------------------------------------------------------------------
    async fn netns(&self, app: &AppConfig) -> Option<String> {
        if app.container.is_empty() || !self.app.docker_ok().await {
            return None;
        }
        let d = self.app.docker()?;
        let info = d.inspect(&app.container).await.ok()?;
        let mode = info["HostConfig"]["NetworkMode"].as_str().unwrap_or("");
        let id = match mode.strip_prefix("container:") {
            Some(other) => d.inspect(other).await.ok()?["Id"].clone(),
            None => info["Id"].clone(),
        };
        id.as_str().filter(|s| !s.is_empty()).map(str::to_string)
    }

    pub async fn shared_namespace(&self, a: &AppConfig, b: &AppConfig) -> bool {
        let (na, nb) = (self.netns(a).await, self.netns(b).await);
        if let (Some(na), Some(nb)) = (&na, &nb) {
            return na == nb;
        }
        // no socket: go by what the compose files say is in the tunnel
        match &self.app.cfg().vpn {
            None => false,
            Some(v) if v.apps.is_none() => true,
            Some(v) => in_tunnel(Some(v), a) && in_tunnel(Some(v), b),
        }
    }

    /// Where `src` reaches `dst`: localhost when they share a network namespace.
    pub async fn endpoint(&self, src: &AppConfig, dst: &AppConfig) -> Endpoint {
        let u = parse_url(&dst.url);
        let ssl = u.as_ref().is_some_and(|u| u.scheme() == "https");
        let port = u.as_ref().and_then(Url::port_or_known_default).unwrap_or(80);
        let host = if self.shared_namespace(src, dst).await {
            "localhost".to_string()
        } else {
            u.as_ref().and_then(url_host).unwrap_or_else(|| dst.name.clone())
        };
        let base = u.as_ref().map(|u| u.path().trim_end_matches('/').to_string()).unwrap_or_default();
        (host, port, base, ssl)
    }

    // ---- status (read only) ------------------------------------------------------------
    /// Every link and its state in the apps; `src` narrows to the links stored by
    /// the apps with those roles.
    pub async fn status(&self, src: Option<Roles>) -> Result<Value> {
        let done = self.done().await?;
        let mut rows: Vec<LinkRow> = vec![];
        let mut clients: HashMap<String, AnyClient> = HashMap::new();
        for ln in self.links() {
            if src.is_some_and(|r| !ln.src.has(r)) {
                continue;
            }
            let mut target = None;
            let checked: Result<LinkOutcome> = async {
                let c = cached(&mut clients, &ln.src).await?;
                let to = self.target(&ln).await;
                target = Some(format!("{}:{}", to.host, to.port));
                match c.link_source() {
                    Some(src) => src.link_state(&to).await,
                    None => self.arr_state(&c, &ln, &to.host, to.port).await,
                }
            }
            .await;
            let out = checked.unwrap_or_else(|e| LinkOutcome::new(LinkState::Error, head(&e.to_string(), 160)));
            let key = ln.key();
            rows.push(LinkRow {
                handled: done.contains(&key),
                key,
                src: ln.src.name.clone(),
                src_kind: ln.src.kind.clone(),
                dst: ln.dst.name.clone(),
                dst_kind: ln.dst.kind.clone(),
                what: ln.what(),
                both: ln.resource() == "applications",
                target,
                state: out.state,
                detail: out.detail,
            });
        }
        let missing = rows.iter().filter(|r| r.state == LinkState::Missing).count();
        Ok(json!({"links": rows, "auto": self.app.cfg().connect.auto, "missing": missing}))
    }

    /// The entry for this link in the app: same implementation, and same name or host.
    async fn find(&self, c: &ArrClient, ln: &Link, host: &str) -> Result<Option<Value>> {
        let hosts = [Some(host.to_string()), host_of(&ln.dst.url), Some("localhost".to_string())];
        for r in c.list(ln.resource()).await? {
            if r["implementation"].as_str() != Some(ln.implementation()) {
                continue;
            }
            let target = if ln.resource() == "applications" {
                host_of(field(&r, "baseUrl").as_str().unwrap_or(""))
            } else {
                match field(&r, "host") {
                    Value::Null => None,
                    v => Some(text(v)),
                }
            };
            if r["name"].as_str() == Some(ln.display().as_str()) || (target.is_some() && hosts.contains(&target)) {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Fields this link sets, and a short description.
    async fn ours(&self, ln: &Link) -> Result<(Map<String, Value>, String)> {
        let (host, port, base, ssl) = self.endpoint(&ln.src, &ln.dst).await;
        let url = |h: &str, p: u16, b: &str, s: bool| format!("{}://{h}:{p}{b}", if s { "https" } else { "http" });
        if ln.resource() == "applications" {
            // Prowlarr -> arr; the arr calls back on prowlarrUrl
            let (ph, pp, pb, pssl) = self.endpoint(&ln.dst, &ln.src).await;
            let ours = obj([
                ("baseUrl", json!(url(&host, port, &base, ssl))),
                ("prowlarrUrl", json!(url(&ph, pp, &pb, pssl))),
                ("apiKey", json!(crate::service::api_key(&ln.dst).await?)),
            ]);
            return Ok((ours, format!("{host}:{port}")));
        }
        let mut ours = obj([
            ("host", json!(host)),
            ("port", json!(port)),
            ("useSsl", json!(ssl)),
            ("urlBase", if base.is_empty() { Value::Null } else { json!(base) }),
        ]);
        if ln.dst.def().is_some_and(|d| matches!(d.credentials, Credentials::ApiKey { .. })) {
            ours.insert("apiKey".into(), json!(crate::service::api_key(&ln.dst).await?));
        } else {
            let (u, p) = crate::service::credentials(&ln.dst).await;
            ours.insert("username".into(), json!(u));
            ours.insert("password".into(), json!(p));
        }
        Ok((ours, format!("{host}:{port}")))
    }

    async fn arr_state(&self, c: &AnyClient, ln: &Link, host: &str, port: u16) -> Result<LinkOutcome> {
        let c = as_arr(c)?;
        let Some(cur) = self.find(c, ln, host).await? else { return Ok(LinkOutcome::new(LinkState::Missing, "")) };
        let cat_f = category_field(&cur["fields"]);
        let mut diffs = vec![];
        if ln.resource() == "applications" {
            let want = self.ours(ln).await?.0.remove("baseUrl").unwrap_or_default();
            if field(&cur, "baseUrl") != &want {
                diffs.push(format!("points at {}", text(field(&cur, "baseUrl"))));
            }
        } else if field(&cur, "host") != &json!(host) || field(&cur, "port") != &json!(port) {
            diffs.push(format!("points at {}:{}", text(field(&cur, "host")), text(field(&cur, "port"))));
        }
        if !cur.get("enable").is_none_or(truthy) {
            diffs.push("disabled".into());
        }
        if !diffs.is_empty() {
            return Ok(LinkOutcome::new(LinkState::Differs, diffs.join("; ")));
        }
        let name = text(&cur["name"]);
        let cat = cat_f.as_deref().map(|f| field(&cur, f)).filter(|v| truthy(v));
        Ok(LinkOutcome::new(
            LinkState::Connected,
            match cat {
                Some(cat) => format!("'{name}', category {}", text(cat)),
                None => format!("'{name}'"),
            },
        ))
    }

    // ---- running ---------------------------------------------------------------------
    /// Queue a connect run: `mode` "all" or "new", `keys` the links (empty: every link).
    pub async fn run(&self, mode: &str, keys: &[String]) -> Result<i64> {
        if mode != "all" && mode != "new" {
            return Err(Error::invalid("mode is 'all' or 'new'"));
        }
        let links: Vec<Link> =
            self.links().into_iter().filter(|l| keys.is_empty() || keys.contains(&l.key())).collect();
        if !keys.is_empty() && links.is_empty() {
            return Err(Error::not_found(format!("no such link: {:?}", keys.join(", "))));
        }
        let target = if keys.is_empty() { mode.to_string() } else { keys.join(",") };
        let me = self.clone();
        let mode = mode.to_string();
        self.app.submit(SUITE, "connect", &target, HashSet::new(), move |rl| me.job(rl, mode, links)).await
    }

    async fn job(self, rl: RunLog, mode: String, mut links: Vec<Link>) -> JobResult {
        let done = self.done().await?;
        if mode == "new" {
            links.retain(|l| !done.contains(&l.key()));
        }
        if links.is_empty() {
            rl.append("nothing to connect").await;
            return Ok(JobOutcome::ok(json!({"results": []})));
        }
        let wire_mode = if mode == "new" { WireMode::New } else { WireMode::All };
        let mut clients: HashMap<String, AnyClient> = HashMap::new();
        let mut results: Vec<Wired> = vec![];
        let mut handled = HashSet::new();
        // each download client once, in the order of the links
        let mut dsts: Vec<&AppConfig> = vec![];
        for ln in &links {
            if dl(&ln.dst.kind).is_some() && !dsts.iter().any(|d| d.name == ln.dst.name) {
                dsts.push(&ln.dst);
            }
        }
        let cats_for = |dst: &AppConfig| -> Vec<String> {
            let s: BTreeSet<String> =
                links.iter().filter(|l| l.dst.name == dst.name).map(|l| l.src.name.clone()).collect();
            s.into_iter().collect()
        };
        // 0. category folders (<client download root>/<arr>): the arrs warn about a
        //    client folder they can't see until the first download creates it
        for dst in &dsts {
            let msg = match cached(&mut clients, dst).await {
                Ok(c) => self.mkdirs(&c, dst, &cats_for(dst)).await,
                Err(e) => format!("download folder unknown ({e})"),
            };
            if !msg.is_empty() {
                rl.append(format!("{}: {msg}", dst.name)).await;
            }
        }
        // 1. categories in the usenet clients, for the arrs that will use them
        for dst in dsts.iter().filter(|d| d.def().and_then(|x| x.downloads).is_some_and(|x| x.categories)) {
            let cats = cats_for(dst);
            let added = match cached(&mut clients, dst).await {
                Ok(c) => c.ensure_categories(&cats).await.map_err(Error::from),
                Err(e) => Err(e),
            };
            match added {
                Ok(added) => {
                    rl.append(format!(
                        "{}: categories {}{}",
                        dst.name,
                        cats.join(", "),
                        if added.is_empty() {
                            " exist".to_string()
                        } else {
                            format!(" (created {})", added.join(", "))
                        }
                    ))
                    .await
                }
                Err(e) => rl.append(format!("{}: could not create categories: {e}", dst.name)).await,
            }
        }
        // 2. each link
        for ln in &links {
            let wired: Result<LinkOutcome> = async {
                let c = cached(&mut clients, &ln.src).await?;
                match c.link_source() {
                    Some(src) => src.wire(&self.target(ln).await, wire_mode, &self).await,
                    None => self.wire_arr(&c, ln, &mode).await,
                }
            }
            .await;
            let out = wired.unwrap_or_else(|e| LinkOutcome::new(LinkState::Failed, e.to_string()));
            rl.append(format!(
                "{} ← {}: {}{}",
                ln.src.name,
                ln.dst.name,
                out.state,
                if out.detail.is_empty() { String::new() } else { format!(": {}", out.detail) }
            ))
            .await;
            if !matches!(out.state, LinkState::Failed | LinkState::Waiting) {
                handled.insert(ln.key());
            }
            results.push(Wired {
                link: ln.key(),
                src: ln.src.name.clone(),
                dst: ln.dst.name.clone(),
                action: out.state,
                detail: out.detail,
            });
        }
        self.mark(&handled).await?;
        let failed = results.iter().filter(|r| r.action == LinkState::Failed).count();
        if results.iter().any(|r| r.action == LinkState::Waiting) {
            self.watch_waiting();
        }
        let status = RunStatus::of_steps(failed > 0, failed < results.len());
        Ok(JobOutcome::new(status, json!({"results": results})))
    }

    async fn mkdirs(&self, c: &AnyClient, dst: &AppConfig, cats: &[String]) -> String {
        let root = match c.download_root().await {
            Ok(r) => r.trim_end_matches('/').to_string(),
            Err(e) => return format!("download folder unknown ({e})"),
        };
        if !root.starts_with('/') {
            return String::new(); // relative to /config: not on the shared mount
        }
        let dirs: Vec<String> = cats.iter().map(|x| format!("{root}/{x}")).collect();
        let list = dirs.join(", ");
        let docker = match self.app.docker() {
            Some(d) if !dst.container.is_empty() && self.app.docker_ok().await => d,
            _ => return format!("create {list} (owned by PUID:PGID) if the arrs warn about them"),
        };
        let info = match docker.inspect(&dst.container).await {
            Ok(i) => i,
            Err(e) => return format!("could not create {list}: {e}"),
        };
        let env: HashMap<&str, &str> = info["Config"]["Env"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|e| e.split_once('='))
            .collect();
        let user = match env.get("PUID").filter(|u| !u.is_empty()) {
            Some(u) => format!("{u}:{}", env.get("PGID").unwrap_or(u)),
            None => String::new(),
        };
        let mut cmd = vec!["mkdir", "-p"];
        cmd.extend(dirs.iter().map(String::as_str));
        match docker.exec(&dst.container, &cmd, &user, Duration::from_secs(30)).await {
            Ok(0) => format!("folders {list}"),
            Ok(_) => format!(
                "could not create {list}: the download folder isn't writable by {} (fix its ownership; \
                 downloads would fail too)",
                if user.is_empty() { "the container user" } else { &user }
            ),
            Err(e) => format!("could not create {list}: {e}"),
        }
    }

    async fn wire_arr(&self, c: &AnyClient, ln: &Link, mode: &str) -> Result<LinkOutcome> {
        let c = as_arr(c)?;
        let (imp, res) = (ln.implementation(), ln.resource());
        let host = self.endpoint(&ln.src, &ln.dst).await.0;
        let cur = self.find(c, ln, &host).await?;
        if let Some(cur) = &cur
            && mode == "new"
        {
            return Ok(LinkOutcome::new(LinkState::Kept, format!("already has '{}'", text(&cur["name"]))));
        }
        let (mut ours, place) = self.ours(ln).await?;
        let mut body = match &cur {
            Some(cur) => cur.clone(),
            None => {
                let schema = c.list(&format!("{res}/schema")).await?;
                let Some(schema) = schema.iter().find(|s| s["implementation"].as_str() == Some(imp)) else {
                    return Ok(LinkOutcome::new(
                        LinkState::Failed,
                        format!("{} has no {imp} {}", ln.src.name, ln.what()),
                    ));
                };
                let mut body = from_schema(schema);
                body["name"] = json!(ln.display());
                if res == "downloadclient" {
                    body["enable"] = json!(true);
                    body["priority"] = json!(1);
                } else {
                    body["syncLevel"] = json!("fullSync");
                }
                body
            }
        };
        let cat_f = if res == "downloadclient" { category_field(&body["fields"]) } else { None };
        if let Some(f) = &cat_f {
            ours.insert(f.clone(), json!(ln.src.name));
        }
        if res == "downloadclient" {
            body["enable"] = json!(true);
        }
        set_fields(&mut body, &ours);
        // refuse to save something that can't connect
        c.call(Method::POST, &format!("{res}/test"), Some(&body), Some(Duration::from_secs(120))).await?;
        let extra = cat_f.map(|f| format!(", category {}", text(&ours[&f]))).unwrap_or_default();
        if let Some(cur) = &cur {
            // saved either way: the app hides keys and passwords, so a changed one
            // can't be seen, and this is what puts a new key in
            c.put(&format!("{res}/{}", text(&cur["id"])), &body).await?;
            let detail = format!("'{}' → {place}{extra}", text(&body["name"]));
            if same_but_hidden(cur, &body) {
                return Ok(LinkOutcome::new(LinkState::Kept, format!("{detail}, as it was")));
            }
            return Ok(LinkOutcome::new(LinkState::Updated, detail));
        }
        let saved = c.post(res, &body).await?;
        Ok(LinkOutcome::new(LinkState::Created, format!("'{}' → {place}{extra}", text(&saved["name"]))))
    }

    /// Where a link source reaches the linked app.
    async fn target<'l>(&self, ln: &'l Link) -> LinkTarget<'l> {
        let (host, port, base, ssl) = self.endpoint(&ln.src, &ln.dst).await;
        LinkTarget { app: &ln.dst, host, port, base, ssl, url_host: host_of(&ln.dst.url) }
    }

    /// The address a link source shows for an app: its link, unless that's only a
    /// port guess with no host set (the app can't know the browser's host).
    async fn external(&self, app: &AppConfig) -> String {
        let links = self.app.links();
        let hit = links.resolve("").await.remove(&app.name);
        match hit {
            Some(l) if l.how == "port" && links.settings().await.host.is_empty() => String::new(),
            Some(l) => l.url.unwrap_or_default(),
            None => String::new(),
        }
    }

    /// Retry the links that wait for their source's own setup (Seerr) every 5
    /// minutes, until it's done.
    fn watch_waiting(&self) {
        if self.app.sched().has(SEERR_JOB) {
            return;
        }
        let me = self.clone();
        let added = self.app.sched().add(SEERR_JOB, "*/5 * * * *", move || {
            let me = me.clone();
            Box::pin(async move { me.waiting_tick().await })
        });
        if let Err(e) = added {
            tracing::warn!("connect: could not schedule the watcher for waiting links: {e}");
        }
    }

    async fn waiting_tick(&self) {
        let done = self.done().await.unwrap_or_default();
        let after_setup = |a: &AppConfig| a.def().and_then(|d| d.takes).is_some_and(|t| t.after_setup);
        let waiting: Vec<Link> =
            self.links().into_iter().filter(|l| after_setup(&l.src) && !done.contains(&l.key())).collect();
        let Some(first) = waiting.first() else {
            self.app.sched().remove(SEERR_JOB);
            return;
        };
        let ready = match service::client(&first.src).await {
            Ok(c) => c.ping().await && matches!(c.link_source(), Some(s) if s.accepts_links().await),
            Err(e) => {
                tracing::warn!("connect: {e}");
                false
            }
        };
        if ready {
            let keys: Vec<String> = waiting.iter().map(Link::key).collect();
            match self.run("new", &keys).await {
                Ok(_) => self.app.sched().remove(SEERR_JOB),
                Err(Error::Conflict(_)) => {} // another system run: next tick
                Err(e) => tracing::warn!("connect: {e}"),
            }
        }
    }

    // ---- startup -----------------------------------------------------------------------
    /// Background: once the apps answer, wire the links not handled yet.
    pub fn start_auto(&self, wait: Duration) {
        if !self.app.cfg().connect.auto || self.app.connect_state().auto_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        self.app.connect_state().auto.spawn("connect: start-up", async move { me.auto(wait).await });
    }

    async fn auto(&self, wait: Duration) {
        let deadline = tokio::time::Instant::now() + wait;
        let done = self.done().await.unwrap_or_default();
        let pending: Vec<Link> = self.links().into_iter().filter(|l| !done.contains(&l.key())).collect();
        if pending.is_empty() {
            return;
        }
        let mut names: Vec<&AppConfig> = vec![];
        for a in pending.iter().flat_map(|l| [&l.src, &l.dst]) {
            if !names.iter().any(|n| n.name == a.name) {
                names.push(a);
            }
        }
        let mut up: HashSet<String> = HashSet::new();
        while tokio::time::Instant::now() < deadline {
            up.clear();
            for app in &names {
                // a missing API key file: not written yet
                if let Ok(c) = service::client(app).await
                    && c.ping().await
                {
                    up.insert(app.name.clone());
                }
            }
            if up.len() == names.len() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        let ready: Vec<String> =
            pending.iter().filter(|l| up.contains(&l.src.name) && up.contains(&l.dst.name)).map(Link::key).collect();
        if ready.is_empty() {
            tracing::warn!("connect: no apps answered within {}s; use Connect apps on the System page", wait.as_secs());
            return;
        }
        for _ in 0..60 {
            match self.run("new", &ready).await {
                Err(Error::Conflict(_)) => tokio::time::sleep(Duration::from_secs(30)).await, // an update is running
                Err(e) => {
                    tracing::warn!("connect: {e}");
                    return;
                }
                Ok(_) => return,
            }
        }
    }
}

#[async_trait::async_trait]
impl LinkHost for Connector {
    async fn public_url(&self, app: &AppConfig) -> String {
        self.external(app).await
    }
}

/// One link on the Connections page: the link and its state in the app storing it.
#[derive(Debug, Serialize)]
struct LinkRow {
    key: String,
    src: String,
    src_kind: String,
    dst: String,
    dst_kind: String,
    what: String,
    /// Prowlarr pushes indexers into the arr and the arr searches through Prowlarr.
    both: bool,
    handled: bool,
    /// Where the source reaches the app; absent when its client couldn't be made.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    state: LinkState,
    detail: String,
}

/// One link in a connect run's result.
#[derive(Debug, Serialize)]
struct Wired {
    link: String,
    src: String,
    dst: String,
    action: LinkState,
    detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_values_dont_count_as_changes() {
        let cur = json!({"id": 1, "enable": true, "fields": [
            {"name": "baseUrl", "value": "http://radarr:7878"}, {"name": "apiKey", "value": "********"}]});
        let mut body = cur.clone();
        set_fields(&mut body, &obj([("apiKey", json!("k1"))]));
        assert!(same_but_hidden(&cur, &body), "only the hidden key was sent");
        set_fields(&mut body, &obj([("baseUrl", json!("http://radarr4k:7878"))]));
        assert!(!same_but_hidden(&cur, &body), "the address changed");
        let mut off = cur.clone();
        off["enable"] = json!(false);
        assert!(!same_but_hidden(&off, &cur), "enabled again");
        let shown = json!({"fields": [{"name": "apiKey", "value": "k0"}]});
        assert!(!same_but_hidden(&shown, &json!({"fields": [{"name": "apiKey", "value": "k1"}]})), "a key it shows");
    }

    fn app(name: &str, kind: &str) -> AppConfig {
        AppConfig {
            name: name.into(),
            kind: kind.into(),
            url: format!("http://{name}:1234"),
            api_key: "k".into(),
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn discovers_links() {
        let apps = [
            app("radarr", "radarr"),
            app("sonarr", "sonarr"),
            app("sonarr4k", "sonarr"),
            app("prowlarr", "prowlarr"),
            app("qbittorrent", "qbittorrent"),
            app("sab", "sabnzbd"),
            app("bazarr", "bazarr"),
            app("seerr", "seerr"),
        ];
        let refs: Vec<&AppConfig> = apps.iter().collect();
        let keys: Vec<String> = links_between(&refs).iter().map(Link::key).collect();
        assert_eq!(
            keys,
            [
                "qbittorrent>radarr",
                "sab>radarr",
                "qbittorrent>sonarr",
                "sab>sonarr",
                "qbittorrent>sonarr4k",
                "sab>sonarr4k",
                "qbittorrent>prowlarr",
                "sab>prowlarr",
                "radarr>prowlarr",
                "sonarr>prowlarr",
                "sonarr4k>prowlarr",
                "sonarr>bazarr",
                "radarr>bazarr",
                "radarr>seerr",
                "sonarr>seerr",
                "sonarr4k>seerr",
            ]
        );
        assert!(links_between(&[&apps[0]]).is_empty(), "an arr alone has nothing to connect");
    }

    #[test]
    fn link_parts() {
        let l = Link { src: app("radarr", "radarr"), dst: app("qbittorrent", "qbittorrent") };
        assert_eq!(
            (l.resource(), l.implementation(), l.what()),
            ("downloadclient", "QBittorrent", "download client".into())
        );
        assert_eq!(l.display(), "qBittorrent");
        let l = Link { src: app("prowlarr", "prowlarr"), dst: app("radarr4k", "radarr") };
        assert_eq!((l.resource(), l.implementation(), l.what()), ("applications", "Radarr", "application".into()));
        assert_eq!(l.display(), "radarr4k");
        let l = Link { src: app("prowlarr", "prowlarr"), dst: app("sab", "sabnzbd") };
        assert_eq!((l.resource(), l.implementation(), l.display()), ("downloadclient", "Sabnzbd", "sab".into()));
        let l = Link { src: app("bazarr", "bazarr"), dst: app("sonarr", "sonarr") };
        assert_eq!(l.what(), "sonarr");
        assert_eq!(l.display(), "Sonarr");
    }

    #[test]
    fn fields_and_urls() {
        let r = json!({"fields": [{"name": "host", "value": "qb"}, {"name": "tvImportedCategory"},
                                  {"name": "tvCategory", "value": "sonarr"}]});
        assert_eq!(field(&r, "host"), "qb");
        assert!(field(&r, "nope").is_null());
        assert_eq!(category_field(&r["fields"]).as_deref(), Some("tvCategory"));
        assert_eq!(category_field(&json!([{"name": "category"}])).as_deref(), Some("category"));
        assert_eq!(category_field(&json!([])), None);
        assert_eq!(host_of("https://Radarr.Example:8443/base/").as_deref(), Some("radarr.example"));
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn schema_to_body() {
        let mut b = from_schema(&json!({"id": 0, "name": "", "presets": [], "infoLink": "x", "message": {},
                                        "fields": [{"name": "host", "value": "localhost"}, {"name": "port"}]}));
        set_fields(&mut b, &obj([("host", json!("qb")), ("port", json!(8080))]));
        assert_eq!(
            b,
            json!({"name": "", "fields": [{"name": "host", "value": "qb"}, {"name": "port", "value": 8080}]})
        );
    }
}
