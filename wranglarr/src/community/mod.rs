// SPDX-License-Identifier: GPL-3.0-or-later
//! Community configs: task-sized bundles of arr settings that people share as JSON.
//!
//! A config holds, per app kind, the settings for one job ("x265 first, smallest size",
//! "anime, English dub"): custom formats, quality profiles and sizes, release and delay
//! profiles, auto tagging, tags, Lidarr's metadata profiles, file naming and a few media
//! management options. Everything in it is matched by name, so it works on any instance of
//! that app. Where the app already has something by that name, the importer picks: change
//! theirs, keep theirs, or add the config's as a copy under a prefix (and the rest of the
//! config then points at the copy). Applying saves what it replaced, so it can be undone.
//!
//! Errors: a config error is [`Error::Invalid`] (400), an unknown history entry or
//! example [`Error::NotFound`] (404 `not found`), an arr that fails [`Error::Upstream`] (502).

mod arr_view;
mod export;
mod history;
mod names;
mod parse;
mod plan;
mod timeline;
mod values;

use rust_embed::RustEmbed;
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::config::AppConfig;
use crate::error::{Error, Result as R};
use crate::service::AppConfigExt;
use crate::service::ServiceDef;
use crate::service::arr::ArrClient;
use crate::service::arr::{self, CommunityDef};
use crate::value::text;
use arr_view::{ArrView, CfIds, id_str, rows};
use history::Change;
use parse::{Config, cerr, choose, extras, parse};
use plan::{Work, plan_one};

const FORMAT: &str = "wranglarr-config/1";
/// Apps with custom formats and quality profiles (the registry's arrs that take
/// community configs), in order.
fn kinds() -> Vec<&'static str> {
    crate::service::ALL.iter().filter(|d| d.arr.is_some_and(|a| a.community.is_some())).map(|d| d.id).collect()
}
/// What community configs know about this kind of app (None: it takes none).
fn def(kind: &str) -> Option<&'static CommunityDef> {
    crate::service::get(kind)?.arr?.community.as_ref()
}
/// Every arr that takes configs, in the registry's order.
fn defs() -> impl Iterator<Item = &'static CommunityDef> {
    arr::all().filter_map(|a| a.community.as_ref())
}
/// The apps with metadata profiles, for messages ("Lidarr").
fn metadata_apps() -> String {
    let has = |d: &&ServiceDef| d.arr.and_then(|a| a.community.as_ref()).is_some_and(|c| c.metadata_profiles.is_some());
    crate::service::ALL.iter().copied().filter(has).map(|d| d.label).collect::<Vec<_>>().join(" / ")
}
/// A bare custom format: the user picks the app.
const ANY: &str = "*";
const MAX_TEXT: usize = 512 * 1024;
const KEY_HISTORY: &str = "community:history";
const HISTORY_KEEP: usize = 30;
const DEFAULT_PREFIX: &str = "v2 · ";
const CHOICES: [&str; 3] = ["change", "keep", "prefix"];
/// Media management options a config may carry, besides each arr's own
/// ([`CommunityDef::media_fields`]): none of them names a path, a permission or a script.
const MEDIA_FIELDS: [&str; 10] = [
    "downloadPropersAndRepacks",
    "deleteEmptyFolders",
    "autoRenameFolders",
    "fileDate",
    "rescanAfterRefresh",
    "importExtraFiles",
    "extraFileExtensions",
    "enableMediaInfo",
    "allowFingerprinting",
    "watchLibraryForChanges",
];

/// A media management option a config may carry (for any arr: a config section
/// is checked the same whichever arr it's for).
fn media_field(k: &str) -> bool {
    MEDIA_FIELDS.contains(&k) || defs().any(|c| c.media_fields.contains(&k))
}

/// The type of the auto tagging fields that hold tags, in any arr ("movieTag").
fn tag_field(t: &str) -> bool {
    defs().any(|c| c.tag_field == Some(t))
}

/// Where `parse` puts what it read from a release name, in the arrs' order
/// ("parsedMovieInfo", …).
fn parsed_info() -> impl Iterator<Item = &'static str> {
    defs().filter_map(|c| c.parsed_info)
}

/// The parts of a config, in the order they're applied: tags first (others use
/// them), auto tagging after the profiles it can point at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Part {
    Tags,
    CustomFormats,
    QualityDefinitions,
    QualityProfiles,
    MetadataProfiles,
    ReleaseProfiles,
    DelayProfiles,
    AutoTags,
    Naming,
    MediaManagement,
}

impl Part {
    pub const ALL: [Part; 10] = [
        Part::Tags,
        Part::CustomFormats,
        Part::QualityDefinitions,
        Part::QualityProfiles,
        Part::MetadataProfiles,
        Part::ReleaseProfiles,
        Part::DelayProfiles,
        Part::AutoTags,
        Part::Naming,
        Part::MediaManagement,
    ];
    /// Matched by name; these can be optional extras.
    pub const NAMED: [Part; 5] =
        [Part::CustomFormats, Part::QualityProfiles, Part::MetadataProfiles, Part::ReleaseProfiles, Part::AutoTags];
    /// One per app: change or keep, no copies.
    pub const APP_WIDE: [Part; 3] = [Part::QualityDefinitions, Part::Naming, Part::MediaManagement];

    pub fn key(self) -> &'static str {
        match self {
            Part::Tags => "tags",
            Part::CustomFormats => "customFormats",
            Part::QualityDefinitions => "qualityDefinitions",
            Part::QualityProfiles => "qualityProfiles",
            Part::MetadataProfiles => "metadataProfiles",
            Part::ReleaseProfiles => "releaseProfiles",
            Part::DelayProfiles => "delayProfiles",
            Part::AutoTags => "autoTags",
            Part::Naming => "naming",
            Part::MediaManagement => "mediaManagement",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Part::Tags => "tag",
            Part::CustomFormats => "custom format",
            Part::QualityDefinitions => "quality size",
            Part::QualityProfiles => "quality profile",
            Part::MetadataProfiles => "metadata profile",
            Part::ReleaseProfiles => "release profile",
            Part::DelayProfiles => "delay profile",
            Part::AutoTags => "auto tagging",
            Part::Naming => "file naming",
            Part::MediaManagement => "media management",
        }
    }

    /// The row name of an app-wide part.
    pub fn whole(self) -> &'static str {
        match self {
            Part::QualityDefinitions => "Quality sizes",
            Part::Naming => "File naming",
            Part::MediaManagement => "Media management",
            _ => "",
        }
    }

    pub fn from_key(k: &str) -> Option<Part> {
        Part::ALL.into_iter().find(|p| p.key() == k)
    }
}

/// How many of a part a config may hold.
fn limit(part: Part) -> usize {
    match part {
        Part::Tags => 100,
        Part::CustomFormats => 300,
        Part::QualityProfiles => 30,
        Part::MetadataProfiles => 20,
        Part::QualityDefinitions => 100,
        Part::ReleaseProfiles => 50,
        Part::DelayProfiles => 30,
        Part::AutoTags => 50,
        Part::Naming | Part::MediaManagement => 40,
    }
}

/// What each app ships with: changing one of these changes the app's own defaults.
fn builtin(part: Part, kind: &str) -> &'static [&'static str] {
    let Some(c) = def(kind) else { return &[] };
    match part {
        Part::QualityProfiles => c.quality_profiles,
        Part::MetadataProfiles => c.metadata_profiles.unwrap_or(&[]),
        _ => &[],
    }
}

fn count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// The examples shipped with wranglarr.
#[derive(RustEmbed)]
#[folder = "configs/"]
struct Examples;

fn example_text(file: &str) -> Option<String> {
    Examples::get(file).and_then(|f| String::from_utf8(f.data.into_owned()).ok())
}

/// A check or apply request (`POST /api/configs/check` and `/apply`).
#[derive(Debug, Default, Clone)]
pub struct Request {
    text: Option<String>,
    targets: Option<Value>,
    extras: Option<Value>,
    resolve: Option<Value>,
    prefix: Option<Value>,
    /// The person OKed changing what the app ships with, app-wide parts, and what
    /// other installed configs set ([`timeline::protected`]).
    ack: bool,
}

impl Request {
    pub fn from_body(b: &Map<String, Value>) -> Request {
        let opt = |k: &str| b.get(k).filter(|v| !v.is_null()).cloned();
        Request {
            text: b.get("text").and_then(Value::as_str).map(str::to_string),
            targets: opt("targets"),
            extras: opt("extras"),
            resolve: opt("resolve"),
            prefix: opt("prefix"),
            ack: b.get("ack").is_some_and(crate::value::truthy),
        }
    }
}

/// The service side. Apply and undo take turns.
#[derive(Default)]
pub struct Community {
    lock: tokio::sync::Mutex<()>,
}

impl Community {
    pub fn new() -> Community {
        Community::default()
    }

    /// The apps that take configs.
    pub fn apps(app: &App) -> Vec<AppConfig> {
        app.cfg().all_apps().filter(|a| kinds().contains(&a.kind.as_str()) && a.enabled).cloned().collect()
    }

    fn app(app: &App, name: &str) -> R<AppConfig> {
        Self::apps(app)
            .into_iter()
            .find(|a| a.name == name)
            .ok_or_else(|| cerr(format!("no app {:?} that takes configs", name)))
    }

    /// An app's API client; an unreadable API key file and the like come out as an arr error.
    async fn client(a: &AppConfig) -> R<ArrClient> {
        let key = crate::service::api_key(a).await.map_err(|e| Error::upstream(e.message()))?;
        Ok(ArrClient::new(&a.name, &a.kind, &a.url, &key, a.snapshot_source())?)
    }

    pub async fn overview(&self, app: &App) -> R<Value> {
        let apps: Vec<Value> = Self::apps(app).iter().map(|a| json!({"name": a.name, "kind": a.kind})).collect();
        Ok(json!({"format": FORMAT, "apps": apps, "examples": examples(), "history": self.history(app).await?,
                  "prefix": DEFAULT_PREFIX}))
    }

    pub fn example(&self, eid: &str) -> R<String> {
        let ok = !eid.is_empty() && eid.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let text = if ok { example_text(&format!("{eid}.json")) } else { None };
        text.ok_or_else(|| Error::not_found("not found"))
    }

    /// targets: kind → [app names]; a missing kind gets the first app of that kind.
    fn targets(app: &App, cfg: &Config, targets: Option<&Value>) -> R<Vec<(String, AppConfig)>> {
        let apps = Self::apps(app);
        let targets = targets.and_then(Value::as_object);
        let mut out = vec![];
        for kind in cfg.apps.keys() {
            let fits: Vec<&AppConfig> = apps.iter().filter(|a| kind == ANY || &a.kind == kind).collect();
            let picked: Vec<&AppConfig> = match targets.and_then(|t| t.get(kind)) {
                Some(w) => {
                    let want = rows(w);
                    if let Some(bad) = want.iter().find(|n| !fits.iter().any(|a| n.as_str() == Some(&a.name))) {
                        return Err(cerr(format!("{bad} can't take the {kind} part")));
                    }
                    fits.into_iter().filter(|a| want.iter().any(|n| n.as_str() == Some(&a.name))).collect()
                }
                None => fits.into_iter().take(1).collect(),
            };
            out.extend(picked.into_iter().map(|a| (kind.clone(), a.clone())));
        }
        Ok(out)
    }

    fn prefix(p: Option<&Value>) -> String {
        match p.and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => s.chars().take(40).collect(),
            _ => DEFAULT_PREFIX.into(),
        }
    }

    /// A dry run: what applying would do, per app. Changes nothing.
    pub async fn check(&self, app: &App, req: &Request) -> R<Value> {
        let full = parse(req.text.as_deref())?;
        let cfg = choose(&full, req.extras.as_ref());
        let prefix = Self::prefix(req.prefix.as_ref());
        let timelines = timeline::load(app).await?;
        let mut plans = vec![];
        for (kind, a) in Self::targets(app, &cfg, req.targets.as_ref())? {
            let c = Self::client(&a).await?;
            let mut arr = ArrView::new(c);
            let (rows, error) = match plan_one(&cfg.apps[&kind], &mut arr, &a.name, req.resolve.as_ref(), &prefix).await
            {
                Ok((rows, _)) => (rows, Value::Null),
                Err(Error::Upstream(e)) => (vec![], json!(e.message())),
                Err(e) => return Err(e),
            };
            let n = |acts: &[&str]| rows.iter().filter(|r| acts.contains(&r["action"].as_str().unwrap_or(""))).count();
            let (changes, errors) = (n(&["create", "update"]), n(&["error"]));
            let warnings: usize = rows.iter().map(|r| arr_view::rows(&r["warnings"]).len()).sum();
            let protected = timeline::protected(&rows, &a.kind, &cfg.name, timelines.get(&a.name));
            plans.push(json!({"kind": kind, "app": a.name, "app_kind": a.kind, "rows": rows, "error": error,
                              "changes": changes, "errors": errors, "warnings": warnings, "protected": protected}));
        }
        let apps = Self::apps(app);
        let mut fits = Map::new();
        let mut chosen = Map::new();
        for k in cfg.apps.keys() {
            let f: Vec<&str> = apps.iter().filter(|a| k == ANY || &a.kind == k).map(|a| a.name.as_str()).collect();
            fits.insert(k.clone(), json!(f));
            let c: Vec<&Value> = plans.iter().filter(|p| p["kind"] == k.as_str()).map(|p| &p["app"]).collect();
            chosen.insert(k.clone(), json!(c));
        }
        let ack = plans.iter().any(|p| !arr_view::rows(&p["protected"]).is_empty());
        Ok(
            json!({"config": full, "extras": extras(&full, req.extras.as_ref()), "plans": plans, "fits": fits, "ack": ack,
                  "chosen": chosen, "prefix": prefix, "tag_prefix": names::slug(&prefix)}),
        )
    }

    /// Apply the config to the picked apps; what each replaced goes into the history.
    pub async fn apply(&self, app: &App, req: &Request) -> R<Value> {
        let cfg = choose(&parse(req.text.as_deref())?, req.extras.as_ref());
        let prefix = Self::prefix(req.prefix.as_ref());
        let pairs = Self::targets(app, &cfg, req.targets.as_ref())?;
        if pairs.is_empty() {
            return Err(cerr("pick at least one app"));
        }
        let mut results = vec![];
        let _turn = self.lock.lock().await;
        for (kind, a) in pairs {
            let c = Self::client(&a).await?;
            let mut done: Vec<Change> = vec![];
            let mut arr = ArrView::new(c);
            let mut was = None;
            let mut hid: Option<String> = None;
            let outcome: R<()> = async {
                // what changed outside wranglarr goes on the timeline first, so it isn't taken for the install
                was = Some(self.catch_up(app, &a).await?);
                let (rows, work) = plan_one(&cfg.apps[&kind], &mut arr, &a.name, req.resolve.as_ref(), &prefix).await?;
                if let Some(bad) = rows.iter().find(|r| r["action"] == "error") {
                    return Err(cerr(text(&bad["details"][0])));
                }
                let tl = timeline::load(app).await?;
                let protected = timeline::protected(&rows, &a.kind, &cfg.name, tl.get(&a.name));
                if !protected.is_empty() && !req.ack {
                    let what: Vec<String> = protected.iter().map(|p| text(&p["name"])).collect();
                    return Err(cerr(format!(
                        "this changes {} (built in, app-wide, or set by another config): check it, then \
                         apply with \"ack\": true",
                        what.join(", ")
                    )));
                }
                let id = self.remember(app, &cfg.name, &a.name, vec![], true).await?;
                hid = Some(id.clone());
                let journal = Journal { community: self, app, hid: &id };
                apply_one(&mut arr, work, &mut done, Some(&journal)).await
            }
            .await;
            let error = match outcome {
                Ok(()) => None,
                Err(e @ (Error::Upstream(_) | Error::Invalid(_))) => Some(e.to_string()),
                Err(e) => return Err(e),
            };
            let mut res = json!({"app": a.name, "kind": kind, "ok": error.is_none(), "error": error,
                                 "changed": done.len()});
            if let Some(hid) = hid {
                self.finish_journal(app, &hid, error.is_some()).await?;
                if done.is_empty() {
                    results.push(res);
                    continue;
                }
                res["history"] = json!(hid);
                if let Some(was) = was {
                    // the apply happened: a timeline that can't be written doesn't undo that
                    match self.record_install(app, &a, was, &cfg, Some(hid), error.is_some()).await {
                        Ok(mark) => res["installed"] = json!(mark),
                        Err(e) => {
                            tracing::warn!("community timeline: {}: {e}", a.name);
                            res["installed"] = Value::Null;
                        }
                    }
                }
            }
            results.push(res);
        }
        Ok(json!({"results": results, "history": self.history(app).await?}))
    }
}

/// The bundled examples, by file name.
fn examples() -> Vec<Value> {
    let mut files: Vec<String> = Examples::iter().map(|f| f.into_owned()).filter(|f| f.ends_with(".json")).collect();
    files.sort();
    let mut out = vec![];
    for f in files {
        let Some(text) = example_text(&f) else { continue };
        match parse(Some(&text)) {
            Ok(c) => {
                let id = f.strip_suffix(".json").unwrap_or(&f);
                out.push(json!({"id": id, "name": c.name, "description": c.description,
                                "apps": c.apps.keys().collect::<Vec<_>>()}));
            }
            Err(e) => tracing::warn!("community: example {f}: {e}"),
        }
    }
    out
}

struct Journal<'a> {
    community: &'a Community,
    app: &'a App,
    hid: &'a str,
}

async fn record(done: &mut Vec<Change>, journal: Option<&Journal<'_>>, item: Change) -> R<usize> {
    if let Some(j) = journal {
        j.community.journal_item(j.app, j.hid, item.clone()).await?;
    }
    let index = done.len();
    done.push(item);
    Ok(index)
}

async fn created_id(done: &mut [Change], journal: Option<&Journal<'_>>, index: usize, id: &Value) -> R<()> {
    if id.is_null() || !(id.is_number() || id.as_str().is_some_and(|s| !s.is_empty())) {
        return Err(Error::upstream("app returned a created resource without an id; check the app before undoing"));
    }
    if let Some(j) = journal {
        j.community.journal_id(j.app, j.hid, index, id.clone()).await?;
    }
    done[index].id = id.clone();
    Ok(())
}

/// Create or replace one thing, and note what it was before.
async fn save(
    arr: &ArrView,
    res: &str,
    cur: Option<&Value>,
    body: Value,
    name: &str,
    done: &mut Vec<Change>,
    journal: Option<&Journal<'_>>,
) -> R<Value> {
    if let Some(cur) = cur {
        let mut body = body;
        body["id"] = cur["id"].clone();
        record(done, journal, Change::replaced(res, cur, name)).await?;
        arr.c.put(&format!("{res}/{}", id_str(&cur["id"])), &body).await?;
        return Ok(cur.clone());
    }
    let index = record(done, journal, Change::created(res, &Value::Null, name)).await?;
    let made = arr.c.post(res, &body).await?;
    created_id(done, journal, index, &made["id"]).await?;
    Ok(made)
}

/// Tag labels → ids, making the ones the app doesn't have.
async fn tag_ids(
    arr: &mut ArrView,
    labels: &[String],
    done: &mut Vec<Change>,
    journal: Option<&Journal<'_>>,
) -> R<Vec<Value>> {
    let mut have: std::collections::HashMap<String, Value> = arr
        .tag_list()
        .await?
        .into_iter()
        .map(|t| (t["label"].as_str().unwrap_or("").to_lowercase(), t["id"].clone()))
        .collect();
    let mut out = vec![];
    for l in labels {
        if !have.contains_key(l) {
            let index = record(done, journal, Change::created("tag", &Value::Null, format!("tag {l}"))).await?;
            let made = arr.c.post("tag", &json!({"label": l})).await?;
            created_id(done, journal, index, &made["id"]).await?;
            have.insert(l.clone(), made["id"].clone());
            arr.push_tag(made);
        }
        out.push(have[l].clone());
    }
    Ok(out)
}

async fn apply_one(arr: &mut ArrView, work: Work, done: &mut Vec<Change>, journal: Option<&Journal<'_>>) -> R<()> {
    if !work.tags.is_empty() {
        tag_ids(arr, &work.tags, done, journal).await?;
    }
    let mut ids = CfIds::default();
    if !work.cfs.is_empty() || !work.profiles.is_empty() {
        for cf in arr.cfs().await? {
            ids.add(cf["name"].as_str().unwrap_or(""), cf["id"].clone());
        }
    }
    for (body, cur) in work.cfs {
        let name = text(&body["name"]);
        let made = save(arr, "customformat", cur.as_ref(), body, &name, done, journal).await?;
        ids.add(&name, made["id"].clone());
    }
    if !work.defs.is_empty() {
        let at = record(
            done,
            journal,
            Change {
                res: "qualitydefinition".into(),
                id: Value::Null,
                name: "quality sizes".into(),
                before: json!([]),
                deleted: false,
            },
        )
        .await?;
        for (new, old) in work.defs {
            let mut item = done[at].clone();
            item.before.as_array_mut().expect("quality sizes list").push(old);
            item.name = format!("{} quality sizes", rows(&item.before).len());
            if let Some(j) = journal {
                j.community.journal_replace(j.app, j.hid, at, item.clone()).await?;
            }
            done[at] = item;
            arr.c.put(&format!("qualitydefinition/{}", id_str(&new["id"])), &new).await?;
        }
    }
    if !work.profiles.is_empty() {
        arr.drop_cache("qualityprofile");
        let fresh = ArrView::by_name(&arr.list("qualityprofile").await?);
        for (p, cur) in work.profiles {
            let cur = if cur.is_some() { fresh.get(&p.name.to_lowercase()) } else { None };
            let base = if cur.is_none() && p.orig != p.name { fresh.get(&p.orig.to_lowercase()) } else { None };
            let body = arr.profile_body(&p, cur, &ids, base).await?;
            save(arr, "qualityprofile", cur, body, &p.name, done, journal).await?;
        }
        arr.drop_cache("qualityprofile");
    }
    let had_mps = !work.mps.is_empty();
    for (m, cur) in work.mps {
        let base = if cur.is_none() && m.orig != m.name {
            ArrView::by_name(&arr.list("metadataprofile").await?).get(&m.orig.to_lowercase()).cloned()
        } else {
            None
        };
        let body = arr.mp_body(&m, cur.as_ref(), base.as_ref()).await?;
        save(arr, "metadataprofile", cur.as_ref(), body, &m.name, done, journal).await?;
    }
    if had_mps {
        arr.drop_cache("metadataprofile");
    }
    for (r, cur) in work.rps {
        let tags = tag_ids(arr, &r.tags, done, journal).await?;
        let body = json!({"name": r.name, "enabled": r.enabled, "required": r.required, "ignored": r.ignored,
                          "tags": tags, "indexerId": 0});
        let body = match &cur {
            Some(c) => {
                let mut m = c.as_object().cloned().unwrap_or_default();
                for (k, v) in body.as_object().into_iter().flatten() {
                    m.insert(k.clone(), v.clone());
                }
                Value::Object(m)
            }
            None => body,
        };
        save(arr, "releaseprofile", cur.as_ref(), body, &r.name, done, journal).await?;
    }
    for (d, cur) in work.dps {
        let tags = tag_ids(arr, &d.tags, done, journal).await?;
        let body = arr.dp_body(&d, cur.as_ref(), tags).await?;
        save(arr, "delayprofile", cur.as_ref(), body, &d.label(), done, journal).await?;
    }
    for (a, cur) in work.ats {
        let tags = tag_ids(arr, &a.tags, done, journal).await?;
        let by_label: std::collections::HashMap<String, Value> = arr
            .tag_list()
            .await?
            .iter()
            .map(|t| (t["label"].as_str().unwrap_or("").to_lowercase(), t["id"].clone()))
            .collect();
        let mut body = arr.at_body(&a, Some(&by_label), &[], &[]).await?;
        body["tags"] = Value::Array(tags);
        save(arr, "autotagging", cur.as_ref(), body, &a.name, done, journal).await?;
    }
    for (w, res, name) in
        [(work.naming, "config/naming", "file naming"), (work.mm, "config/mediamanagement", "media management")]
    {
        if let Some((new, cur)) = w {
            record(done, journal, Change::replaced(res, &cur, name)).await?;
            arr.c.put(&format!("{res}/{}", id_str(&cur["id"])), &new).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_arrs_ship_with() {
        let std = ["any", "sd", "hd-720p", "hd-1080p", "ultra-hd", "hd - 720p/1080p"];
        for kind in ["radarr", "sonarr", "whisparr"] {
            assert_eq!(builtin(Part::QualityProfiles, kind), std, "{kind}");
            assert!(builtin(Part::MetadataProfiles, kind).is_empty(), "{kind}");
        }
        assert_eq!(builtin(Part::QualityProfiles, "lidarr"), ["any", "lossless", "standard"]);
        assert_eq!(builtin(Part::MetadataProfiles, "lidarr"), ["standard", "none"]);
        for kind in ["readarr", "prowlarr", "bazarr", "nope", ANY] {
            assert!(builtin(Part::QualityProfiles, kind).is_empty() && def(kind).is_none(), "{kind}");
        }
        assert_eq!(kinds(), ["radarr", "sonarr", "lidarr", "whisparr"]);
        assert_eq!(metadata_apps(), "Lidarr");
    }

    #[test]
    fn the_arrs_vocabulary_is_merged() {
        let old = [
            "downloadPropersAndRepacks",
            "autoUnmonitorPreviouslyDownloadedMovies",
            "autoUnmonitorPreviouslyDownloadedEpisodes",
            "autoUnmonitorPreviouslyDownloadedTracks",
            "createEmptyMovieFolders",
            "createEmptySeriesFolders",
            "createEmptyArtistFolders",
            "deleteEmptyFolders",
            "autoRenameFolders",
            "episodeTitleRequired",
            "fileDate",
            "rescanAfterRefresh",
            "importExtraFiles",
            "extraFileExtensions",
            "enableMediaInfo",
            "allowFingerprinting",
            "watchLibraryForChanges",
        ];
        assert!(old.iter().all(|k| media_field(k)));
        let all = MEDIA_FIELDS.len() + defs().map(|c| c.media_fields.len()).sum::<usize>();
        assert_eq!(all, old.len(), "no more, no fewer");
        assert!(!media_field("recycleBin") && !media_field("id"));
        assert!(["movieTag", "seriesTag", "artistTag"].iter().all(|t| tag_field(t)) && !tag_field("tag"));
        assert_eq!(parsed_info().collect::<Vec<_>>(), ["parsedMovieInfo", "parsedEpisodeInfo", "parsedAlbumInfo"]);
    }

    #[test]
    fn bundled_examples() {
        let ex = examples();
        let ids: Vec<&str> = ex.iter().filter_map(|e| e["id"].as_str()).collect();
        assert_eq!(ids, ["anime-english-dub", "x265-small"]);
        assert_eq!(ex[0]["apps"], json!(["sonarr"]));
        assert!(Community::new().example("x265-small").is_ok());
        for bad in ["", "../x", "X265-small", "nope"] {
            assert!(matches!(Community::new().example(bad), Err(Error::NotFound(_))), "{bad}");
        }
    }

    #[test]
    fn prefixes() {
        assert_eq!(Community::prefix(None), DEFAULT_PREFIX);
        assert_eq!(Community::prefix(Some(&json!("  "))), DEFAULT_PREFIX);
        assert_eq!(Community::prefix(Some(&json!("x".repeat(50)))).len(), 40);
    }
}
