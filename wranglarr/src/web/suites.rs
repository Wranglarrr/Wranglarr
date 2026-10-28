// SPDX-License-Identifier: GPL-3.0-or-later
//! Suites, versions, runs and restore plans: the backup side of the UI and API.

use std::collections::HashMap;

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::tpl::{COLLECTION_LABELS, lookup, primary_count};
use super::{Body, Ctx, Page, Q, page, path_int, unprocessable};
use crate::app::App;
use crate::config::SuiteConfig;
use crate::error::{Error, Result};
use crate::manifest::{AppBackup, Manifest};
use crate::restore_plan::RestorePlan;
use crate::service::AppConfigExt;
use crate::service::Roles;
use crate::value::truthy;
use crate::web::ApiResult;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/suites", get(suites_page))
        .route("/suites/{suite}", get(suite_page))
        .route("/suites/{suite}/versions/{version}", get(version_page))
        .route("/suites/{suite}/settings", post(settings_form))
        .route("/activity", get(activity))
        .route("/runs/{run_id}", get(run_page))
        .route("/api/suites", get(api_suites))
        .route("/api/suites/{suite}", get(api_suite))
        .route("/api/suites/{suite}/overview", get(api_overview))
        .route("/api/suites/{suite}/settings", axum::routing::put(api_settings).delete(api_settings_reset))
        .route("/api/suites/{suite}/backup", post(api_backup))
        .route("/api/suites/{suite}/versions", get(api_versions))
        .route("/api/suites/{suite}/versions/{version}", get(api_version).delete(api_version_delete))
        .route("/api/suites/{suite}/versions/{version}/restore", post(api_restore))
        .route("/api/suites/{suite}/prune", post(api_prune))
        .route("/api/suites/{suite}/sync", post(api_sync))
        .route("/api/runs", get(api_runs))
        .route("/api/runs/{run_id}", get(api_run))
        .route("/api/vpn", get(api_vpn))
        .route("/api/restore-plan", get(api_plan).delete(api_plan_discard))
        .route("/api/restore-plan/run", post(api_plan_run))
}

fn accepted(v: Value) -> Response {
    (StatusCode::ACCEPTED, Json(v)).into_response()
}

// ---- view helpers -----------------------------------------------------------------

pub async fn suite_summary(app: &App, s: &SuiteConfig) -> Result<Value> {
    let settings = app.settings(&s.name).await?;
    let active_run = app.state().active_run(&s.name).await?;
    let name = s.name.clone();
    let usage = app.blocking(move |app| Ok(app.store().usage(&name))).await?;
    let shared: Vec<Value> = app
        .cfg()
        .shared
        .iter()
        .map(|r| {
            let apps: Vec<&str> =
                s.apps.iter().filter(|a| r.applies_to(&a.name, &a.kind)).map(|a| a.name.as_str()).collect();
            json!({"resource": r.resource, "label": r.label(), "name": r.name, "apps": apps})
        })
        .collect();
    let apps: Vec<Value> =
        s.apps.iter().map(|a| json!({"name": a.name, "kind": a.kind, "databases": a.databases})).collect();
    Ok(json!({"name": s.name, "apps": apps, "settings": settings, "usage": usage,
              "next_run": app.next_run(&s.name).map(|t| crate::time::iso(&t)),
              "active_run": active_run, "shared": shared}))
}

fn num_sub(a: &Value, b: &Value) -> Option<Value> {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return Some(json!(x - y));
    }
    Some(json!(a.as_f64()? - b.as_f64()?))
}

/// What an app reported about itself, null when it didn't.
fn reported(v: &Option<Value>) -> &Value {
    v.as_ref().unwrap_or(&Value::Null)
}

/// A count of bytes as a signed number, for differences.
fn signed(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// A backup version as the pages show it: its manifest and what changed since
/// the version before.
#[derive(Debug, Serialize)]
pub struct VersionView {
    #[serde(flatten)]
    pub manifest: Manifest,
    /// Per app, in the manifest's order.
    pub deltas: IndexMap<String, AppDelta>,
    /// What to call out: apps added, upgraded, removed, counts changed, errors.
    pub changes: Vec<Value>,
    /// Bytes more (or fewer) than the version before; None for the oldest.
    pub size_delta: Option<i64>,
}

/// How one app changed since the version before.
#[derive(Debug, Serialize)]
pub struct AppDelta {
    pub version_changed: bool,
    pub prev_version: Value,
    /// The collection count that is compared ("" when the kind has none).
    pub count_key: &'static str,
    pub count_delta: Value,
}

/// What changed since the backup before (versions are newest first).
pub fn with_deltas(versions: Vec<Manifest>) -> Vec<VersionView> {
    let deltas: Vec<_> = (0..versions.len()).map(|i| deltas(&versions[i], versions.get(i + 1))).collect();
    versions
        .into_iter()
        .zip(deltas)
        .map(|(manifest, (deltas, changes, size_delta))| VersionView { manifest, deltas, changes, size_delta })
        .collect()
}

fn deltas(m: &Manifest, prev: Option<&Manifest>) -> (IndexMap<String, AppDelta>, Vec<Value>, Option<i64>) {
    let mut deltas = IndexMap::new();
    let mut changes = vec![];
    for (name, a) in &m.apps {
        let p = prev.and_then(|pv| pv.apps.get(name));
        let kind = &a.kind;
        let key = primary_count(kind).unwrap_or("");
        let cur_n = &a.collection[key];
        let prev_n = p.map_or(&Value::Null, |p| &p.collection[key]);
        let prev_version = p.map_or(&Value::Null, |p| reported(&p.version));
        let version_changed = p.is_some() && reported(&a.version) != prev_version;
        let count_delta = if !cur_n.is_null() && !prev_n.is_null() { num_sub(cur_n, prev_n) } else { None };
        let count_delta = count_delta.unwrap_or(Value::Null);
        if prev.is_some() && p.is_none() {
            changes.push(json!({"app": name, "kind": kind, "what": "added"}));
        }
        if version_changed {
            changes.push(
                json!({"app": name, "kind": kind, "what": "upgraded", "from": prev_version, "to": reported(&a.version)}),
            );
        }
        if truthy(&count_delta) {
            changes.push(json!({"app": name, "kind": kind, "what": "count", "n": count_delta,
                                "label": lookup(COLLECTION_LABELS, key).unwrap_or(key)}));
        }
        if !a.ok() {
            changes.push(json!({"app": name, "kind": kind, "what": "error", "text": a.errors.join("; ")}));
        }
        deltas.insert(
            name.clone(),
            AppDelta { version_changed, prev_version: prev_version.clone(), count_key: key, count_delta },
        );
    }
    if let Some(p) = prev {
        for (name, pa) in &p.apps {
            if !m.apps.contains_key(name) {
                changes.push(json!({"app": name, "kind": pa.kind, "what": "removed"}));
            }
        }
    }
    let size_delta = prev.map(|p| signed(m.bytes()) - signed(p.bytes()));
    (deltas, changes, size_delta)
}

/// The last n backups, oldest first, for the bar strip.
pub fn history(versions: &[VersionView], n: usize) -> Vec<Value> {
    let recent: Vec<&Manifest> = versions.iter().take(n).rev().map(|v| &v.manifest).collect();
    let top = recent.iter().map(|m| m.bytes()).max().filter(|t| *t > 0).unwrap_or(1) as f64;
    recent
        .iter()
        .map(|m| {
            let h = ((100.0 * m.bytes() as f64 / top).round_ties_even() as i64).max(8);
            json!({"version": m.version, "status": m.status, "bytes": m.bytes(), "trigger": m.trigger, "h": h})
        })
        .collect()
}

/// Each app in the suite and what its newest backup holds.
fn suite_apps(s: &SuiteConfig, newest: Option<&Manifest>) -> Vec<Value> {
    s.apps
        .iter()
        .map(|a| {
            let m = newest.and_then(|n| n.apps.get(&a.name));
            let snap = m.and_then(|m| m.config_snapshot.as_ref());
            json!({"name": a.name, "kind": a.kind, "version": m.map(|m| &m.version), "in_backup": m.is_some(),
                   "database_type": m.map(|m| &m.database_type), "databases": m.map_or(&[][..], |m| &m.databases),
                   "db_bytes": m.map_or(0, AppBackup::database_bytes),
                   "zip_bytes": m.and_then(|m| m.config_zip.as_ref()).map(|z| z.size),
                   "files": snap.map(|s| s.files), "files_bytes": snap.map(|s| s.bytes),
                   "errors": m.map_or(&[][..], |m| &m.errors)})
        })
        .collect()
}

async fn runs(app: &App, suite: Option<&str>, limit: i64) -> Result<Value> {
    Ok(serde_json::to_value(app.state().list_runs(suite, limit).await?)?)
}

/// A suite's versions, newest first, with what changed from the one before.
async fn version_list(app: &App, suite: &str) -> Result<Vec<VersionView>> {
    let suite = suite.to_owned();
    app.blocking(move |app| Ok(with_deltas(app.store().list_versions(&suite)))).await
}

// ---- pages ----------------------------------------------------------------------------

async fn suites_page(State(app): State<App>, ctx: Ctx) -> Page {
    let mut suites = vec![];
    for cs in &app.cfg().suites {
        let mut s = suite_summary(&app, cs).await?;
        let versions = version_list(&app, &cs.name).await?;
        s["history"] = json!(history(&versions, 30));
        s["last"] = json!(versions.first());
        s["app_list"] = json!(suite_apps(cs, versions.first().map(|v| &v.manifest)));
        suites.push(s);
    }
    let newest = suites.iter().filter_map(|s| s["last"]["version"].as_str()).max().map(str::to_string);
    let problems: Vec<&Value> =
        suites.iter().filter(|s| truthy(&s["last"]) && s["last"]["status"] != "ok").map(|s| &s["name"]).collect();
    let never: Vec<&Value> = suites.iter().filter(|s| !truthy(&s["last"])).map(|s| &s["name"]).collect();
    let total_versions: i64 = suites.iter().filter_map(|s| s["usage"]["versions"].as_i64()).sum();
    let total_bytes = app.blocking(|app| Ok(app.store().total_usage())).await?;
    let extra = json!({"suites": suites, "runs": runs(&app, None, 6).await?, "newest": newest, "problems": problems,
                   "never": never, "total_versions": total_versions, "total_bytes": total_bytes});
    page(&app, &ctx, "suites.html", "suites", extra).await.map_err(Into::into)
}

async fn suite_page(State(app): State<App>, ctx: Ctx, Path(suite): Path<String>) -> Page {
    let links = app.links_for(&ctx.host).await;
    let s = app.suite(&suite)?.clone();
    let summary = suite_summary(&app, &s).await?;
    // the backups themselves are the version list; show the other runs (restores, syncs, failures)
    let other: Vec<Value> = app
        .state()
        .list_runs(Some(&suite), 40)
        .await?
        .into_iter()
        .filter(|r| !(r.kind == "backup" && r.status == "ok"))
        .take(10)
        .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
        .collect();
    let versions = version_list(&app, &suite).await?;
    // download clients: what the newest backup actually holds (torrents / NZBs, not data)
    let mut dl = vec![];
    for a in s.apps.iter().filter(|a| a.has(Roles::DOWNLOADS)) {
        let newest = versions.iter().find_map(|v| {
            let snap = v.manifest.apps.get(&a.name)?.config_snapshot.as_ref()?;
            Some((&v.manifest.version, snap))
        });
        let snap = newest.map(|(_, s)| s);
        dl.push(json!({"name": a.name, "kind": a.kind, "version": newest.map(|(v, _)| v),
                   "payload": snap.and_then(|s| s.payload.as_ref()), "files": snap.map(|s| s.files),
                   "bytes": snap.map(|s| s.bytes)}));
    }
    let extra = json!({"suite": summary, "history": history(&versions, 30),
                   "app_list": suite_apps(&s, versions.first().map(|v| &v.manifest)), "runs": other, "downloads": dl,
                   "links": links, "versions": versions});
    page(&app, &ctx, "suite.html", &format!("suite:{suite}"), extra).await.map_err(Into::into)
}

async fn version_page(State(app): State<App>, ctx: Ctx, Path((suite, version)): Path<(String, String)>) -> Page {
    let docker = app.docker_ok().await;
    let s = app.suite(&suite)?.clone();
    let summary = suite_summary(&app, &s).await?;
    let (name, v) = (suite.clone(), version.clone());
    let m = app
        .blocking(move |app| {
            let m = app.store().read_manifest(&name, &v).ok_or_else(|| Error::not_found("no such version"))?;
            let versions = with_deltas(app.store().list_versions(&name));
            Ok(match versions.into_iter().find(|view| view.manifest.version == v) {
                Some(view) => json!(view),
                None => json!(m),
            })
        })
        .await?;
    let root = if app.store().host_root.is_empty() {
        app.store().root.display().to_string()
    } else {
        app.store().host_root.clone()
    };
    let extra = json!({"suite": summary, "m": m, "docker": docker, "store_root": root});
    page(&app, &ctx, "version.html", &format!("suite:{suite}"), extra).await.map_err(Into::into)
}

async fn activity(State(app): State<App>, ctx: Ctx) -> Page {
    let extra = json!({"runs": runs(&app, None, 100).await?});
    page(&app, &ctx, "activity.html", "activity", extra).await.map_err(Into::into)
}

async fn run_page(State(app): State<App>, ctx: Ctx, Path(run_id): Path<String>) -> Page {
    let id = path_int(&run_id, "run_id")?;
    let run = app.state().get_run(id).await?.ok_or_else(|| Error::not_found("Not Found"))?;
    page(&app, &ctx, "run.html", "activity", json!({"run": run})).await.map_err(Into::into)
}

fn form_int(f: &HashMap<String, String>, k: &str, default: i64) -> Result<i64> {
    match f.get(k) {
        None => Ok(default),
        Some(v) => v.trim().parse().map_err(|_| super::not_whole(k, v)),
    }
}

async fn settings_form(
    State(app): State<App>,
    Path(suite): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> ApiResult<Redirect> {
    app.suite(&suite)?;
    let schedule = f.get("schedule").cloned().unwrap_or_default();
    let enabled = f.get("enabled").map(String::as_str).unwrap_or("off") == "on";
    let (keep_last, keep_days) = (form_int(&f, "keep_last", 14)?, form_int(&f, "keep_days", 0)?);
    app.save_settings(&suite, &schedule, enabled, keep_last, keep_days).await?;
    Ok(Redirect::to(&crate::url_base::prefix(&app.deployment().proxy.url_base, &format!("/suites/{suite}"))))
}

// ---- JSON API -------------------------------------------------------------------------

async fn api_suites(State(app): State<App>) -> ApiResult<Json<Value>> {
    let mut out = vec![];
    for s in &app.cfg().suites {
        out.push(suite_summary(&app, s).await?);
    }
    Ok(Json(json!(out)))
}

async fn api_suite(State(app): State<App>, Path(suite): Path<String>) -> ApiResult<Json<Value>> {
    let s = app.suite(&suite)?;
    Ok(Json(suite_summary(&app, s).await?))
}

async fn api_overview(State(app): State<App>, Path(suite): Path<String>, q: Q) -> ApiResult<Json<Value>> {
    app.suite(&suite)?;
    Ok(Json(app.overview(&suite, q.flag("refresh")?).await?))
}

fn opt<T: serde::de::DeserializeOwned>(body: &Value, k: &str, cur: T) -> Result<T> {
    match body.get(k) {
        None => Ok(cur),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| unprocessable(&format!("{k}: {e}"))),
    }
}

async fn api_settings(
    State(app): State<App>,
    Path(suite): Path<String>,
    Body(body): Body<Value>,
) -> ApiResult<Json<Value>> {
    app.suite(&suite)?;
    if !body.is_object() {
        return Err(unprocessable(super::NOT_AN_OBJECT).into());
    }
    // what isn't sent keeps its current value
    let settings = app
        .modify_settings(&suite, |mut current| {
            current.schedule = opt(&body, "schedule", current.schedule)?;
            current.enabled = opt(&body, "enabled", current.enabled)?;
            current.keep_last = opt(&body, "keep_last", current.keep_last)?;
            current.keep_days = opt(&body, "keep_days", current.keep_days)?;
            Ok(current)
        })
        .await?;
    Ok(Json(serde_json::to_value(settings)?))
}

async fn api_settings_reset(State(app): State<App>, Path(suite): Path<String>) -> ApiResult<Json<Value>> {
    app.suite(&suite)?;
    app.reset_settings(&suite).await?;
    Ok(Json(serde_json::to_value(app.settings(&suite).await?)?))
}

async fn api_backup(State(app): State<App>, Path(suite): Path<String>) -> ApiResult<Response> {
    Ok(accepted(json!({"run": app.backup_suite(&suite, "manual").await?})))
}

async fn api_versions(State(app): State<App>, Path(suite): Path<String>) -> ApiResult<Json<Value>> {
    app.blocking(move |app| {
        app.suite(&suite)?;
        Ok(Json(json!(with_deltas(app.store().list_versions(&suite)))))
    })
    .await
    .map_err(Into::into)
}

async fn api_version(
    State(app): State<App>,
    Path((suite, version)): Path<(String, String)>,
) -> ApiResult<Json<Manifest>> {
    app.blocking(move |app| {
        app.suite(&suite)?;
        app.store().read_manifest(&suite, &version).map(Json).ok_or_else(|| Error::not_found("Not Found"))
    })
    .await
    .map_err(Into::into)
}

async fn api_version_delete(
    State(app): State<App>,
    Path((suite, version)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    app.suite(&suite)?;
    let (s, v) = (suite.clone(), version.clone());
    app.while_idle(&suite, || app.blocking(move |app| app.store().delete_version(&s, &v))).await?;
    Ok(Json(json!({"deleted": version})))
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct RestoreIn {
    app: String,
    #[serde(default = "yes")]
    databases: bool,
    #[serde(default = "yes")]
    config: bool,
}

async fn api_restore(
    State(app): State<App>,
    Path((suite, version)): Path<(String, String)>,
    Body(b): Body<RestoreIn>,
) -> ApiResult<Response> {
    app.suite_app(&suite, &b.app)?;
    Ok(accepted(json!({"run": app.restore_app(&suite, &version, &b.app, b.databases, b.config).await?})))
}

async fn api_prune(State(app): State<App>, Path(suite): Path<String>) -> ApiResult<Json<Value>> {
    app.suite(&suite)?;
    let pruned = app.while_idle(&suite, || app.prune_suite(&suite)).await?;
    Ok(Json(json!({"pruned": pruned})))
}

#[derive(Deserialize, Default)]
struct SyncIn {
    #[serde(default)]
    app: Option<String>,
}

async fn api_sync(
    State(app): State<App>,
    Path(suite): Path<String>,
    Body(b): Body<Option<SyncIn>>,
) -> ApiResult<Response> {
    app.suite(&suite)?;
    let b = b.unwrap_or_default();
    Ok(accepted(json!({"run": app.sync_shared(&suite, b.app.as_deref()).await?})))
}

async fn api_runs(State(app): State<App>, q: Q) -> ApiResult<Json<Value>> {
    let suite = q.get("suite").filter(|s| !s.is_empty());
    Ok(Json(runs(&app, suite, q.int("limit", 20)?).await?))
}

async fn api_run(State(app): State<App>, Path(run_id): Path<String>) -> ApiResult<Json<Value>> {
    let id = path_int(&run_id, "run_id")?;
    let run = app.state().get_run(id).await?.ok_or_else(|| Error::not_found("Not Found"))?;
    Ok(Json(serde_json::to_value(run)?))
}

async fn api_vpn(State(app): State<App>, q: Q) -> ApiResult<Json<Value>> {
    Ok(Json(app.vpn().status(q.flag("refresh")?).await))
}

async fn api_plan(State(app): State<App>) -> ApiResult<Json<RestorePlan>> {
    app.blocking(|app| Ok(Json(app.restore_plan().unwrap_or_default()))).await.map_err(Into::into)
}

async fn api_plan_run(State(app): State<App>) -> ApiResult<Response> {
    Ok(accepted(json!({"run": app.run_restore_plan().await?})))
}

async fn api_plan_discard(State(app): State<App>) -> ApiResult<Json<Value>> {
    app.while_idle(crate::restore_plan::LOCK, || async {
        app.blocking(move |app| {
            app.discard_restore_plan()?;
            Ok(Json(json!({"discarded": true})))
        })
        .await
    })
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_and_history() {
        let v = vec![
            json!({"version": "20260103T000000Z", "status": "ok", "total_bytes": 300,
                   "apps": {"radarr": {"kind": "radarr", "version": "5.1", "collection": {"movies": 12}, "errors": []},
                            "qb": {"kind": "qbittorrent", "errors": ["status: down"]}}}),
            json!({"version": "20260102T000000Z", "status": "ok", "total_bytes": 100,
                   "apps": {"radarr": {"kind": "radarr", "version": "5.0", "collection": {"movies": 10}, "errors": []},
                            "gone": {"kind": "sonarr"}}}),
        ];
        let v = v.into_iter().map(|m| serde_json::from_value::<Manifest>(m).expect("manifest")).collect();
        let views = with_deltas(v);
        let d: Vec<Value> = views.iter().map(|v| json!(v)).collect();
        let what: Vec<&str> =
            d[0]["changes"].as_array().into_iter().flatten().filter_map(|c| c["what"].as_str()).collect();
        assert_eq!(what, ["upgraded", "count", "added", "error", "removed"]); // in app order
        assert_eq!(d[0]["deltas"]["radarr"]["count_delta"], 2);
        assert_eq!(d[0]["size_delta"], 200);
        assert_eq!(d[1]["size_delta"], Value::Null);
        let h = history(&views, 30);
        assert_eq!(h[0]["version"], "20260102T000000Z");
        assert_eq!(h[0]["h"], 33);
        assert_eq!(h[1]["h"], 100);
    }
}
