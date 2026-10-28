// SPDX-License-Identifier: GPL-3.0-or-later
//! Trackers and aggregators, kept in wranglarr and pushed to the arrs you pick.
//!
//! A *source* is one of:
//! - `torznab`: a torrent tracker's Torznab feed -> one indexer per arr
//! - `newznab`: a usenet indexer's Newznab feed -> one indexer per arr
//! - `jackett`: a Jackett server: every configured tracker in it becomes a
//!   Torznab indexer (`<tracker> (Jackett)`) in each picked arr
//! - `nzbhydra`: NZBHydra2: one Newznab indexer (and a Torznab one if it also
//!   serves torrents) in each picked arr
//! - `prowlarr`: a Prowlarr outside this stack: each picked arr is registered
//!   in it as an application, and Prowlarr pushes its own indexers from there.
//!   (A Prowlarr in this stack is wired automatically, see connect.rs.)
//!
//! Sources live in the state db. wranglarr remembers what it created in each
//! app (kv "indexers:pushed"), so un-ticking an arr or deleting a source
//! removes those entries again and never touches indexers added by hand.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use indexmap::IndexMap;
use regex::Regex;
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::background::Background;
use crate::config::AppConfig;
use crate::http::{ArrError, ArrResult};
use crate::ids::token_hex;
use crate::jobs::{JobOutcome, JobResult, RunLog};
use crate::service::AppConfigExt;
use crate::service::arr::ArrClient;
use crate::service::{self, Roles};
use crate::state::{RunStatus, Tx};
use crate::value::{head, truthy};

use crate::connect::{field, from_schema, host_of, prowlarr_app, set_fields};
use crate::error::{Error, Result};
use crate::links::{parse_url, url_host};

pub const SUITE: &str = "system";
pub const SOURCES_KEY: &str = "indexers:sources";
pub const PUSHED_KEY: &str = "indexers:pushed";
/// The source type of a Prowlarr outside this stack (its own word, persisted in
/// the state db, not the kind of an app in wranglarr.toml).
pub const PROWLARR_SOURCE: &str = "prowlarr";
/// type -> label, in the order the form lists them.
pub const TYPES: &[(&str, &str)] = &[
    ("torznab", "Tracker (Torznab)"),
    ("newznab", "Usenet indexer (Newznab)"),
    ("jackett", "Jackett"),
    ("nzbhydra", "NZBHydra2"),
    (PROWLARR_SOURCE, "Prowlarr (outside this stack)"),
];

/// Newznab/Torznab categories each arr searches (the arrs' own defaults).
pub fn categories(kind: &str) -> &'static [i64] {
    crate::service::get(kind).and_then(|d| d.arr).map_or(&[], |a| a.categories)
}

fn anime(kind: &str) -> Option<&'static [i64]> {
    crate::service::get(kind)?.arr?.anime_categories
}

static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9 ._()\-+&]{1,60}$").expect("static regex"));

pub fn type_label(t: &str) -> Option<&'static str> {
    TYPES.iter().find(|(k, _)| *k == t).map(|(_, v)| *v)
}

/// A feed's base URL and API path: `https://x/api` is `https://x` and `/api`; anything
/// else is the base, with the API path `/api`.
pub fn split_feed(url: &str) -> (String, String) {
    let url = url.trim().trim_end_matches('/');
    if parse_url(url).is_some_and(|u| u.path().ends_with("/api")) {
        return (url[..url.len() - "/api".len()].to_string(), "/api".into());
    }
    (url.to_string(), "/api".into())
}

/// A source as stored: a JSON object (unknown keys kept).
pub type Source = Map<String, Value>;

fn s_str<'a>(s: &'a Source, k: &str) -> &'a str {
    s.get(k).and_then(Value::as_str).unwrap_or("")
}

fn s_enabled(s: &Source) -> bool {
    s.get("enabled").is_none_or(truthy)
}

/// What a source puts into an arr.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub name: String,
    /// "Torznab" or "Newznab"
    pub implementation: String,
    pub base_url: String,
    pub api_path: String,
    pub api_key: String,
}

/// A source as the form sends it (`SourceIn`).
#[derive(Debug, Clone, PartialEq)]
pub struct SourceIn {
    pub id: String,
    pub r#type: String,
    pub name: String,
    pub url: String,
    /// blank on edit = keep
    pub api_key: String,
    /// arr app names; [] = every arr
    pub apply_to: Vec<String>,
    pub enabled: bool,
    pub priority: i64,
    pub min_seeders: i64,
    /// nzbhydra: also add its Torznab feed
    pub torrents: bool,
}

impl Default for SourceIn {
    fn default() -> Self {
        SourceIn {
            id: String::new(),
            r#type: String::new(),
            name: String::new(),
            url: String::new(),
            api_key: String::new(),
            apply_to: vec![],
            enabled: true,
            priority: 25,
            min_seeders: 1,
            torrents: false,
        }
    }
}

/// Jackett's trackers: (id, title) of every tracker configured in it.
pub fn parse_jackett(name: &str, xml: &str) -> ArrResult<Vec<(String, String)>> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| ArrError::new(format!("{name}: not a Jackett torznab answer ({e})")))?;
    let root = doc.root_element();
    if root.tag_name().name() == "error" {
        return Err(ArrError::new(format!("{name}: {}", root.attribute("description").unwrap_or("None"))));
    }
    Ok(root
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "indexer")
        .filter(|i| i.attribute("configured").unwrap_or("true") == "true")
        .map(|i| {
            let id = i.attribute("id").unwrap_or("None").to_string();
            let title = i
                .children()
                .find(|c| c.is_element() && c.tag_name().name() == "title")
                .and_then(|t| t.text())
                .filter(|t| !t.is_empty())
                .map_or_else(|| id.clone(), str::to_string);
            (id, title)
        })
        .collect())
}

/// What a source becomes in each arr. Jackett's trackers come from `trackers`
/// (looked up beforehand); the others need nothing but the source.
pub fn items_with(src: &Source, trackers: Option<&[(String, String)]>) -> Vec<Item> {
    let key = s_str(src, "api_key").to_string();
    let name = s_str(src, "name");
    let url = s_str(src, "url");
    let item = |name: String, imp: &str, base_url: String| Item {
        name,
        implementation: imp.into(),
        base_url,
        api_path: "/api".into(),
        api_key: key.clone(),
    };
    match s_str(src, "type") {
        t @ ("torznab" | "newznab") => {
            let (b, p) = split_feed(url);
            let imp = if t == "torznab" { "Torznab" } else { "Newznab" };
            vec![Item { api_path: p, ..item(name.into(), imp, b) }]
        }
        "nzbhydra" => {
            let base = url.trim_end_matches('/');
            let mut out = vec![item(name.into(), "Newznab", base.into())];
            if src.get("torrents").is_some_and(truthy) {
                out.push(item(format!("{name} (torrents)"), "Torznab", format!("{base}/torznab")));
            }
            out
        }
        "jackett" => {
            let base = url.trim_end_matches('/');
            trackers
                .unwrap_or_default()
                .iter()
                .map(|(tid, title)| {
                    item(
                        format!("{title} ({name})"),
                        "Torznab",
                        format!("{base}/api/v2.0/indexers/{tid}/results/torznab"),
                    )
                })
                .collect()
        }
        _ => vec![], // prowlarr: no indexers of its own here
    }
}

/// Does this source go into this arr?
pub fn applies(src: &Source, app: &AppConfig) -> bool {
    let to: Vec<&str> =
        src.get("apply_to").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    s_enabled(src) && (to.is_empty() || to.contains(&app.name.as_str()))
}

/// The source for the browser: no API key, whether it has one, the type's label.
pub fn public(src: &Source) -> Value {
    let mut out: Map<String, Value> =
        src.iter().filter(|(k, _)| *k != "api_key").map(|(k, v)| (k.clone(), v.clone())).collect();
    out.insert("has_key".into(), json!(src.get("api_key").is_some_and(truthy)));
    let t = s_str(src, "type");
    out.insert(
        "type_label".into(),
        type_label(t).map_or_else(|| src.get("type").cloned().unwrap_or_default(), |l| json!(l)),
    );
    Value::Object(out)
}

/// What was pushed: app name -> {indexer name -> source id}; for an external
/// Prowlarr, "prowlarr:<source id>" -> {application name -> app name}.
type Pushed = IndexMap<String, IndexMap<String, String>>;

/// How often a sync waiting on another system run tries again.
const LATER_EVERY: Duration = Duration::from_secs(5);

/// The start-up task, started once, and a sync waiting for another system run
/// to end; both stop at shutdown.
#[derive(Default)]
pub struct IndexersState {
    auto_started: AtomicBool,
    auto: Background,
    later_waiting: AtomicBool,
}

impl IndexersState {
    /// Stop the start-up task, if it is still waiting for the apps.
    pub(crate) async fn shutdown(&self) {
        self.auto.shutdown().await;
    }
}

/// The trackers and aggregators, as a handle on the service.
#[derive(Clone)]
pub struct Indexers {
    app: App,
}

impl App {
    pub fn indexers(&self) -> Indexers {
        Indexers { app: self.clone() }
    }
}

/// Start-up work: push sources to arrs that don't have them yet.
pub async fn start(app: &App) {
    app.indexers().start_auto(Duration::from_secs(600)).await;
}

fn http() -> reqwest::Client {
    crate::http::http(Duration::from_secs(20))
}

/// A Jackett lookup per source, once per status or sync.
type TrackerCache = HashMap<String, Vec<(String, String)>>;

impl Indexers {
    // ---- sources -------------------------------------------------------------------
    /// The arrs sources are pushed to (every arr but Prowlarr).
    pub fn targets(&self) -> Vec<AppConfig> {
        self.app.cfg().all_apps().filter(|a| a.has(Roles::LIBRARY)).cloned().collect()
    }

    pub async fn sources(&self) -> Result<Vec<Source>> {
        Ok(self.app.state().get::<Vec<Source>>(SOURCES_KEY).await?.unwrap_or_default())
    }

    /// Add or edit a source. A blank API key on edit keeps the stored one.
    pub async fn save_source(&self, data: &SourceIn) -> Result<Source> {
        let typ = data.r#type.as_str();
        if type_label(typ).is_none() {
            let all: Vec<&str> = TYPES.iter().map(|t| t.0).collect();
            return Err(Error::invalid(format!("type must be one of {}", all.join(", "))));
        }
        let name = data.name.trim();
        if !NAME.is_match(name) {
            return Err(Error::invalid("name: 1-60 letters, digits, spaces and . _ ( ) - + &"));
        }
        let url = data.url.trim();
        let host = parse_url(url).filter(|u| matches!(u.scheme(), "http" | "https")).as_ref().and_then(url_host);
        let Some(host) = host else {
            return Err(Error::invalid("url must be http(s)://host…"));
        };
        if typ == PROWLARR_SOURCE
            && self.app.cfg().all_apps().any(|a| a.has(Roles::INDEXERS) && host_of(&a.url).as_ref() == Some(&host))
        {
            return Err(Error::invalid(
                "that Prowlarr is part of this stack; wranglarr already registers the arrs in it",
            ));
        }
        let known: HashSet<String> = self.targets().into_iter().map(|a| a.name).collect();
        let apply_to: Vec<String> = data.apply_to.iter().filter(|x| known.contains(*x)).cloned().collect();
        let (data, typ, name, url) = (data.clone(), typ.to_owned(), name.to_owned(), url.to_owned());
        self.app.state().write(move |tx| merge_source(tx, &data, &typ, &name, &url, apply_to)).await
    }

    pub async fn delete_source(&self, sid: &str) -> Result<()> {
        let sid = sid.to_owned();
        self.app
            .state()
            .write(move |tx| {
                let srcs: Vec<Source> = tx.get(SOURCES_KEY)?.unwrap_or_default();
                if !srcs.iter().any(|s| s_str(s, "id") == sid) {
                    return Err(Error::not_found("no such source"));
                }
                let rest: Vec<Source> = srcs.into_iter().filter(|s| s_str(s, "id") != sid).collect();
                tx.put(SOURCES_KEY, &rest)
            })
            .await
    }
}

/// Put a checked source into the stored list, in the transaction that read it.
fn merge_source(tx: &Tx, data: &SourceIn, typ: &str, name: &str, url: &str, apply_to: Vec<String>) -> Result<Source> {
    let srcs: Vec<Source> = tx.get(SOURCES_KEY)?.unwrap_or_default();
    let cur = (!data.id.is_empty()).then(|| srcs.iter().position(|s| s_str(s, "id") == data.id)).flatten();
    if srcs.iter().enumerate().any(|(i, s)| s_str(s, "name").to_lowercase() == name.to_lowercase() && Some(i) != cur) {
        return Err(Error::invalid(format!("a source named {name:?} exists")));
    }
    let mut src = match cur {
        Some(i) => srcs[i].clone(),
        None => [("id".to_string(), json!(token_hex(4)))].into_iter().collect(),
    };
    for (k, v) in [
        ("type", json!(typ)),
        ("name", json!(name)),
        ("url", json!(url)),
        ("apply_to", json!(apply_to)),
        ("enabled", json!(data.enabled)),
        ("priority", json!(data.priority.clamp(1, 50))),
        ("min_seeders", json!(data.min_seeders.max(0))), // 0 is a real choice
        ("torrents", json!(data.torrents)),
    ] {
        src.insert(k.into(), v);
    }
    let key = data.api_key.trim();
    if !key.is_empty() || cur.is_none() {
        src.insert("api_key".into(), json!(key));
    }
    if !src.get("api_key").is_some_and(truthy) && typ != "torznab" {
        return Err(Error::invalid("api_key is required"));
    }
    let id = s_str(&src, "id").to_string();
    let mut all: Vec<Source> = srcs.into_iter().filter(|s| s_str(s, "id") != id).collect();
    all.push(src.clone());
    tx.put(SOURCES_KEY, &all)?;
    Ok(src)
}

impl Indexers {
    // ---- what each source becomes ----------------------------------------------------
    /// (id, title) of every tracker configured in a Jackett server.
    pub async fn jackett_trackers(&self, src: &Source) -> ArrResult<Vec<(String, String)>> {
        let base = s_str(src, "url").trim_end_matches('/');
        let name = s_str(src, "name");
        let r = http()
            .get(format!("{base}/api/v2.0/indexers/all/results/torznab/api"))
            .query(&[("apikey", s_str(src, "api_key")), ("t", "indexers"), ("configured", "true")])
            .send()
            .await
            .map_err(|e| ArrError::new(crate::http::reason(&e)))?;
        let status = r.status();
        let text = crate::http::response_text(r).await.map_err(|e| ArrError::new(e.describe(crate::http::reason)))?;
        if status.as_u16() >= 400 {
            return Err(ArrError::with_status(
                status,
                format!("{name}: Jackett answered {}: {}", status.as_u16(), head(&text, 120)),
            ));
        }
        parse_jackett(name, &text)
    }

    /// The indexers a source puts into an arr.
    pub async fn items(&self, src: &Source, cache: &mut TrackerCache) -> ArrResult<Vec<Item>> {
        if s_str(src, "type") == "jackett" {
            let id = s_str(src, "id").to_string();
            if !cache.contains_key(&id) {
                let t = self.jackett_trackers(src).await?;
                cache.insert(id.clone(), t);
            }
            return Ok(items_with(src, cache.get(&id).map(Vec::as_slice)));
        }
        Ok(items_with(src, None))
    }

    async fn arr(app: &AppConfig) -> Result<ArrClient> {
        service::client(app).await?.arr().cloned().ok_or_else(|| Error::invalid(format!("{}: not an arr", app.name)))
    }

    fn prowlarr_client(src: &Source) -> Result<ArrClient> {
        Ok(ArrClient::new(
            s_str(src, "name"),
            service::arr::prowlarr::DEF.id,
            s_str(src, "url"),
            s_str(src, "api_key"),
            None,
        )?)
    }

    // ---- status (read only) ------------------------------------------------------------
    pub async fn status(&self) -> Result<Value> {
        let (targets, srcs) = (self.targets(), self.sources().await?);
        let mut present: IndexMap<String, HashSet<String>> = IndexMap::new();
        let mut errors: Map<String, Value> = Map::new();
        for app in &targets {
            let names = async {
                let c = Self::arr(app).await?;
                let list = c.list("indexer").await?;
                Ok::<_, Error>(list.iter().filter_map(|r| r["name"].as_str().map(str::to_string)).collect())
            };
            match names.await {
                Ok(n) => {
                    present.insert(app.name.clone(), n);
                }
                Err(e) => {
                    errors.insert(app.name.clone(), json!(head(&e.to_string(), 160)));
                }
            }
        }
        let mut rows = vec![];
        let mut cache = TrackerCache::new();
        for s in &srcs {
            let mut row = match public(s) {
                Value::Object(o) => o,
                _ => Map::new(),
            };
            let its = match self.items(s, &mut cache).await {
                Ok(its) => {
                    row.insert("count".into(), json!(its.len()));
                    if s_str(s, "type") == "jackett" {
                        row.insert("detail".into(), json!(format!("{} tracker(s) configured in Jackett", its.len())));
                    }
                    its
                }
                Err(e) => {
                    row.insert("error".into(), json!(head(e.message(), 160)));
                    vec![]
                }
            };
            let mut cells = Map::new();
            for app in &targets {
                let cell = if !applies(s, app) {
                    "off".to_string()
                } else if errors.contains_key(&app.name) {
                    "error".into()
                } else if s_str(s, "type") == PROWLARR_SOURCE {
                    self.prowlarr_has(s, app).await.into()
                } else {
                    let have = present.get(&app.name);
                    let n = its.iter().filter(|i| have.is_some_and(|h| h.contains(&i.name))).count();
                    if !its.is_empty() && n == its.len() {
                        "ok".into()
                    } else if n > 0 {
                        "partial".into()
                    } else {
                        "missing".into()
                    }
                };
                cells.insert(app.name.clone(), json!(cell));
            }
            row.insert("cells".into(), Value::Object(cells));
            rows.push(Value::Object(row));
        }
        let types: Map<String, Value> = TYPES.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        let prowlarr: Vec<Value> = self
            .app
            .cfg()
            .all_apps()
            .filter(|a| a.has(Roles::INDEXERS))
            .map(|p| json!({"name": p.name, "url": p.url}))
            .collect();
        let counts: Map<String, Value> = present.iter().map(|(k, v)| (k.clone(), json!(v.len()))).collect();
        Ok(json!({
            "targets": targets.iter().map(|a| json!({"name": a.name, "kind": a.kind})).collect::<Vec<_>>(),
            "sources": rows, "errors": errors, "types": types, "stack_prowlarr": prowlarr,
            "indexer_counts": counts,
        }))
    }

    async fn prowlarr_has(&self, src: &Source, app: &AppConfig) -> &'static str {
        let hosts = async {
            let c = Self::prowlarr_client(src)?;
            let apps = c.list("applications").await?;
            Ok::<_, Error>(apps.iter().map(|r| host_of(field(r, "baseUrl").as_str().unwrap_or(""))).collect::<Vec<_>>())
        };
        match hosts.await {
            Err(_) => "error",
            Ok(h) if h.contains(&host_of(&app.url)) => "ok",
            Ok(_) => "missing",
        }
    }

    // ---- syncing -----------------------------------------------------------------------
    /// Queue a sync of every source into the arrs.
    pub async fn sync(&self, trigger: &str) -> Result<i64> {
        let me = self.clone();
        self.app.submit(SUITE, "indexers", trigger, HashSet::new(), move |rl| me.job(rl)).await
    }

    /// Sync after a source changed: now, or once the system run that's busy
    /// ends (`None`). The sync reads the sources when it starts, so one waiting
    /// sync covers every change made meanwhile.
    pub async fn sync_or_later(&self, trigger: &str) -> Result<Option<i64>> {
        match self.sync(trigger).await {
            Ok(run) => Ok(Some(run)),
            Err(Error::Conflict(_)) => {
                let st = self.app.indexers_state();
                if !st.later_waiting.swap(true, Ordering::SeqCst) {
                    let (me, trigger) = (self.clone(), trigger.to_owned());
                    st.auto.spawn("indexers: sync after a change", async move { me.later(&trigger).await });
                }
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn later(&self, trigger: &str) {
        // a system run is minutes at most; give up after an hour
        for _ in 0..720 {
            tokio::time::sleep(LATER_EVERY).await;
            match self.sync(trigger).await {
                Err(Error::Conflict(_)) => continue,
                Err(e) => tracing::warn!("indexers: {e}"),
                Ok(_) => {}
            }
            break;
        }
        self.app.indexers_state().later_waiting.store(false, Ordering::SeqCst);
    }

    async fn job(self, rl: RunLog) -> JobResult {
        let (srcs, targets) = (self.sources().await?, self.targets());
        let pushed: Pushed = self.app.state().get(PUSHED_KEY).await?.unwrap_or_default();
        let mut cache = TrackerCache::new();
        let mut results: Vec<Value> = vec![];
        let mut failures = 0usize;
        // app -> indexer name -> (source index, item); None: keep as it is
        let mut wanted: IndexMap<String, IndexMap<String, Option<(usize, Item)>>> =
            targets.iter().map(|a| (a.name.clone(), IndexMap::new())).collect();
        for (si, s) in srcs.iter().enumerate() {
            if s_str(s, "type") == PROWLARR_SOURCE || !s_enabled(s) {
                continue;
            }
            let its = match self.items(s, &mut cache).await {
                Ok(its) => its,
                Err(e) => {
                    rl.append(format!("{}: {e}; its indexers are left as they are", s_str(s, "name"))).await;
                    failures += 1;
                    // keep what was pushed, don't delete on a failed lookup
                    for app in &targets {
                        for (n, sid) in pushed.get(&app.name).into_iter().flatten() {
                            if sid == s_str(s, "id")
                                && let Some(w) = wanted.get_mut(&app.name)
                            {
                                w.insert(n.clone(), None);
                            }
                        }
                    }
                    continue;
                }
            };
            if s_str(s, "type") == "jackett" {
                rl.append(format!("{}: {} tracker(s) in Jackett", s_str(s, "name"), its.len())).await;
            }
            for app in &targets {
                if applies(s, app)
                    && let Some(w) = wanted.get_mut(&app.name)
                {
                    for it in &its {
                        w.insert(it.name.clone(), Some((si, it.clone())));
                    }
                }
            }
        }
        let mut new_pushed: Pushed = IndexMap::new();
        for app in &targets {
            let mut mine = pushed.get(&app.name).cloned().unwrap_or_default();
            let c = match Self::arr(app).await {
                Ok(c) => c,
                Err(e) => {
                    rl.append(format!("{}: {e}", app.name)).await;
                    failures += 1;
                    new_pushed.insert(app.name.clone(), mine);
                    continue;
                }
            };
            let want = wanted.get(&app.name).cloned().unwrap_or_default();
            let synced: ArrResult<()> = async {
                let existing: IndexMap<String, Value> = c
                    .list("indexer")
                    .await?
                    .into_iter()
                    .filter_map(|r| r["name"].as_str().map(|n| (n.to_string(), r.clone())))
                    .collect();
                let mut schema: Option<HashMap<String, Value>> = None;
                for (name, pair) in &want {
                    let Some((si, it)) = pair else { continue };
                    let s = &srcs[*si];
                    if existing.contains_key(name) && !mine.contains_key(name) {
                        // same name as one added by hand: never take it over
                        failures += 1;
                        rl.append(format!(
                            "{}: '{name}' already exists and wasn't added by wranglarr; rename the source (or \
                             delete that indexer) to manage it here",
                            app.name
                        ))
                        .await;
                        results
                            .push(json!({"app": app.name, "indexer": name, "action": "failed", "error": "name taken"}));
                        continue;
                    }
                    let done: ArrResult<&str> = async {
                        if schema.is_none() {
                            let list = c.list("indexer/schema").await?;
                            schema = Some(
                                list.into_iter()
                                    .filter_map(|x| x["implementation"].as_str().map(|i| (i.to_string(), x.clone())))
                                    .collect(),
                            );
                        }
                        let sch = schema.as_ref().map_or_else(HashMap::new, Clone::clone);
                        upsert(&c, app, s, it, existing.get(name), &sch).await
                    }
                    .await;
                    match done {
                        Ok(action) => {
                            mine.insert(name.clone(), s_str(s, "id").to_string());
                            rl.append(format!("{}: {action} '{name}'", app.name)).await;
                            results.push(json!({"app": app.name, "indexer": name, "action": action}));
                        }
                        Err(e) => {
                            failures += 1;
                            rl.append(format!("{}: '{name}' FAILED: {e}", app.name)).await;
                            results.push(json!({"app": app.name, "indexer": name, "action": "failed",
                                                "error": e.to_string()}));
                        }
                    }
                }
                let gone: Vec<String> = mine.keys().filter(|n| !want.contains_key(*n)).cloned().collect();
                for name in gone {
                    if let Some(cur) = existing.get(&name) {
                        match c.delete(&format!("indexer/{}", crate::value::text(&cur["id"]))).await {
                            Ok(_) => {
                                rl.append(format!("{}: removed '{name}' (no longer selected)", app.name)).await;
                                results.push(json!({"app": app.name, "indexer": name, "action": "removed"}));
                            }
                            Err(e) => {
                                failures += 1;
                                rl.append(format!("{}: could not remove '{name}': {e}", app.name)).await;
                                continue;
                            }
                        }
                    }
                    mine.shift_remove(&name);
                }
                Ok(())
            }
            .await;
            if let Err(e) = synced {
                failures += 1;
                rl.append(format!("{}: {e}", app.name)).await;
            }
            new_pushed.insert(app.name.clone(), mine);
        }
        // external Prowlarr: register / unregister the picked arrs
        for s in srcs.iter().filter(|x| s_str(x, "type") == PROWLARR_SOURCE) {
            failures += self.sync_prowlarr(&rl, s, &targets, &pushed, &mut new_pushed, &mut results).await;
        }
        for k in pushed.keys().filter(|k| k.starts_with("prowlarr:")) {
            // deleted Prowlarr sources
            if !srcs
                .iter()
                .any(|x| s_str(x, "type") == PROWLARR_SOURCE && format!("{PROWLARR_SOURCE}:{}", s_str(x, "id")) == *k)
            {
                rl.append(format!("{k}: source deleted; its applications stay in that Prowlarr (remove them there)"))
                    .await;
            }
        }
        self.app.state().put(PUSHED_KEY, &new_pushed).await?;
        if results.is_empty() && failures == 0 {
            rl.append("nothing to change").await;
        }
        let status = RunStatus::of_steps(failures > 0, !results.is_empty());
        Ok(JobOutcome::new(status, json!({"results": results})))
    }

    async fn sync_prowlarr(
        &self,
        rl: &RunLog,
        s: &Source,
        targets: &[AppConfig],
        pushed: &Pushed,
        new_pushed: &mut Pushed,
        results: &mut Vec<Value>,
    ) -> usize {
        let key = format!("prowlarr:{}", s_str(s, "id"));
        let sname = s_str(s, "name");
        let mut mine = pushed.get(&key).cloned().unwrap_or_default();
        let c = match Self::prowlarr_client(s) {
            Ok(c) => c,
            Err(e) => {
                rl.append(format!("{sname}: {e}")).await;
                new_pushed.insert(key, mine);
                return 1;
            }
        };
        let mut fails = 0;
        let outer: ArrResult<()> = async {
            let apps: IndexMap<String, Value> = c
                .list("applications")
                .await?
                .into_iter()
                .filter_map(|r| r["name"].as_str().map(|n| (n.to_string(), r.clone())))
                .collect();
            let schema: HashMap<String, Value> = c
                .list("applications/schema")
                .await?
                .into_iter()
                .filter_map(|x| x["implementation"].as_str().map(|i| (i.to_string(), x.clone())))
                .collect();
            for app in targets {
                let want = s_enabled(s) && applies(s, app) && prowlarr_app(&app.kind).is_some();
                let name = if app.name == app.kind {
                    prowlarr_app(&app.kind).map_or_else(|| app.name.clone(), str::to_string)
                } else {
                    app.name.clone()
                };
                if want {
                    let done: Result<&str> = async {
                        let mut body = match apps.get(&name) {
                            Some(cur) => cur.clone(),
                            None => {
                                let imp = prowlarr_app(&app.kind).unwrap_or_default();
                                let sch = schema
                                    .get(imp)
                                    .ok_or_else(|| Error::invalid(format!("Prowlarr has no {imp:?} application")))?;
                                from_schema(sch)
                            }
                        };
                        if !body.is_object() || !body["fields"].is_array() {
                            return Err(Error::invalid("the Prowlarr application has no fields"));
                        }
                        let level = body.get("syncLevel").filter(|v| truthy(v)).cloned().unwrap_or(json!("fullSync"));
                        body["name"] = json!(name);
                        body["syncLevel"] = level;
                        let ours: Map<String, Value> = [
                            ("baseUrl".to_string(), json!(app.url.trim_end_matches('/'))),
                            ("prowlarrUrl".to_string(), json!(s_str(s, "url").trim_end_matches('/'))),
                            ("apiKey".to_string(), json!(crate::service::api_key(app).await?)),
                        ]
                        .into_iter()
                        .collect();
                        set_fields(&mut body, &ours);
                        c.call(Method::POST, "applications/test", Some(&body), Some(Duration::from_secs(120))).await?;
                        Ok(match apps.get(&name) {
                            Some(cur) => {
                                c.put(&format!("applications/{}", crate::value::text(&cur["id"])), &body).await?;
                                "updated"
                            }
                            None => {
                                c.post("applications", &body).await?;
                                "created"
                            }
                        })
                    }
                    .await;
                    match done {
                        Ok(action) => {
                            mine.insert(name.clone(), app.name.clone());
                            rl.append(format!("{sname}: {action} application '{name}' → {}", app.url)).await;
                            results.push(json!({"app": app.name, "indexer": sname, "action": action}));
                        }
                        Err(e) => {
                            fails += 1;
                            rl.append(format!("{sname}: application '{name}' FAILED: {e}")).await;
                        }
                    }
                } else if mine.contains_key(&name) {
                    if let Some(cur) = apps.get(&name) {
                        c.delete(&format!("applications/{}", crate::value::text(&cur["id"]))).await?;
                        rl.append(format!("{sname}: removed application '{name}'")).await;
                        results.push(json!({"app": app.name, "indexer": sname, "action": "removed"}));
                    }
                    mine.shift_remove(&name);
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = outer {
            fails += 1;
            rl.append(format!("{sname}: {e}")).await;
        }
        new_pushed.insert(key, mine);
        fails
    }

    // ---- startup -------------------------------------------------------------------------
    /// Background: push sources to arrs that don't have them yet (e.g. a newly enabled arr).
    pub async fn start_auto(&self, wait: Duration) {
        if self.sources().await.map(|s| s.is_empty()).unwrap_or(true)
            || self.app.indexers_state().auto_started.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let me = self.clone();
        self.app.indexers_state().auto.spawn("indexers: start-up", async move { me.auto(wait).await });
    }

    async fn auto(&self, wait: Duration) {
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            if let Ok(st) = self.status().await
                && st["errors"].as_object().is_none_or(Map::is_empty)
            {
                let todo = st["sources"].as_array().into_iter().flatten().any(|r| {
                    r["cells"].as_object().into_iter().flatten().any(|(_, v)| v == "missing" || v == "partial")
                });
                if todo {
                    for _ in 0..60 {
                        match self.sync("startup").await {
                            Err(Error::Conflict(_)) => tokio::time::sleep(Duration::from_secs(30)).await,
                            Err(e) => {
                                tracing::warn!("indexers: {e}");
                                return;
                            }
                            Ok(_) => return,
                        }
                    }
                }
                return;
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    }
}

/// Create or update one indexer in an arr: "created", "updated" or "unchanged".
async fn upsert(
    c: &ArrClient,
    app: &AppConfig,
    s: &Source,
    it: &Item,
    cur: Option<&Value>,
    schema: &HashMap<String, Value>,
) -> ArrResult<&'static str> {
    let Some(tmpl) = schema.get(&it.implementation) else {
        return Err(ArrError::new(format!("{} has no {} indexer", app.name, it.implementation)));
    };
    let mut ours: Map<String, Value> = [
        ("baseUrl", json!(it.base_url)),
        ("apiPath", json!(it.api_path)),
        ("apiKey", json!(it.api_key)),
        ("categories", json!(categories(&app.kind))),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    if let Some(a) = anime(&app.kind) {
        ours.insert("animeCategories".into(), json!(a));
    }
    if it.implementation == "Torznab" {
        ours.insert("minimumSeeders".into(), s.get("min_seeders").cloned().unwrap_or(json!(1)));
    }
    let mut body = match cur {
        Some(cur) => cur.clone(),
        None => from_schema(tmpl),
    };
    let priority = s.get("priority").cloned().unwrap_or(json!(25));
    if let Some(o) = body.as_object_mut() {
        o.insert("name".into(), json!(it.name));
        o.insert("enableRss".into(), json!(true));
        o.insert("enableAutomaticSearch".into(), json!(true));
        o.insert("enableInteractiveSearch".into(), json!(true));
        o.insert("priority".into(), priority.clone());
    }
    set_fields(&mut body, &ours);
    if let Some(cur) = cur {
        let same = ours.iter().filter(|(k, _)| *k != "apiKey").all(|(k, v)| field(cur, k) == v)
            && cur["priority"] == priority
            && (field(cur, "apiKey") == &json!(it.api_key) || field(cur, "apiKey") == "********");
        if same {
            return Ok("unchanged");
        }
        // the arr tests the feed before saving
        c.put(&format!("indexer/{}", crate::value::text(&cur["id"])), &body).await?;
        return Ok("updated");
    }
    c.post("indexer", &body).await?;
    Ok("created")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(v: Value) -> Source {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn feeds() {
        assert_eq!(split_feed(" https://x/api/ "), ("https://x".into(), "/api".into()));
        assert_eq!(split_feed("https://x/torznab"), ("https://x/torznab".into(), "/api".into()));
        assert_eq!(split_feed("https://x"), ("https://x".into(), "/api".into()));
    }

    #[test]
    fn what_sources_become() {
        let t = src(json!({"id": "1", "type": "torznab", "name": "T", "url": "http://t/api", "api_key": "k"}));
        assert_eq!(
            items_with(&t, None),
            vec![Item {
                name: "T".into(),
                implementation: "Torznab".into(),
                base_url: "http://t".into(),
                api_path: "/api".into(),
                api_key: "k".into()
            }]
        );
        let h = src(json!({"id": "2", "type": "nzbhydra", "name": "H", "url": "http://h:5076/", "torrents": true}));
        let its = items_with(&h, None);
        assert_eq!(its.len(), 2);
        assert_eq!((its[0].implementation.as_str(), its[0].base_url.as_str()), ("Newznab", "http://h:5076"));
        assert_eq!((its[1].name.as_str(), its[1].base_url.as_str()), ("H (torrents)", "http://h:5076/torznab"));
        let j = src(json!({"id": "3", "type": "jackett", "name": "J", "url": "http://j:9117"}));
        let its = items_with(&j, Some(&[("1337x".into(), "1337x".into())]));
        assert_eq!(its[0].name, "1337x (J)");
        assert_eq!(its[0].base_url, "http://j:9117/api/v2.0/indexers/1337x/results/torznab");
        assert!(items_with(&src(json!({"type": "prowlarr"})), None).is_empty());
    }

    #[test]
    fn jackett_answers() {
        let xml = r#"<?xml version="1.0"?><indexers>
            <indexer id="a" configured="true"><title>Alpha</title></indexer>
            <indexer id="b" configured="false"><title>Beta</title></indexer>
            <indexer id="c"><title></title></indexer></indexers>"#;
        assert_eq!(
            parse_jackett("J", xml).expect("parses"),
            vec![("a".into(), "Alpha".into()), ("c".into(), "c".into())]
        );
        let e = parse_jackett("J", r#"<error code="100" description="Invalid API Key"/>"#).expect_err("error");
        assert_eq!(e.message(), "J: Invalid API Key");
        assert!(
            parse_jackett("J", "nope").expect_err("not xml").message().starts_with("J: not a Jackett torznab answer (")
        );
    }

    #[test]
    fn public_view_and_applies() {
        let s = src(json!({"id": "1", "type": "jackett", "name": "J", "api_key": "secret", "apply_to": ["radarr"]}));
        let p = public(&s);
        assert!(p.get("api_key").is_none());
        assert_eq!((p["has_key"].clone(), p["type_label"].clone()), (json!(true), json!("Jackett")));
        let app = |n: &str| AppConfig { name: n.into(), kind: "radarr".into(), ..Default::default() };
        assert!(applies(&s, &app("radarr")) && !applies(&s, &app("radarr4k")));
        let mut off = s.clone();
        off.insert("enabled".into(), json!(false));
        assert!(!applies(&off, &app("radarr")));
        assert_eq!(public(&src(json!({"type": "odd"})))["type_label"], "odd");
    }

    #[test]
    fn ids() {
        let (a, b) = (token_hex(4), token_hex(4));
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
