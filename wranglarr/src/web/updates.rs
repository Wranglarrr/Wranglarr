// SPDX-License-Identifier: GPL-3.0-or-later
//! The Updates page and `/api/updates*`: check, apply, roll back, hold, settings,
//! and wranglarr updating itself.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Body, Ctx, Page, page};
use crate::app::App;
use crate::error::Error;
use crate::updates::UpdateSettings;
use crate::web::ApiResult;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/updates", get(updates_page))
        .route("/api/updates", get(overview))
        .route("/api/updates/check", post(check))
        .route("/api/updates/apply", post(apply))
        .route("/api/updates/rollback", post(rollback))
        .route("/api/updates/hold", post(hold))
        .route("/api/updates/self", post(self_update))
        .route("/api/updates/settings", put(save_settings).delete(reset_settings))
        .route("/api/updates/tracks/{container}", put(save_track).delete(reset_track))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackIn {
    /// Null or blank follows the deployed image, including a digest pin.
    tag: Option<String>,
}

async fn save_track(
    State(app): State<App>,
    Path(container): Path<String>,
    Body(body): Body<TrackIn>,
) -> ApiResult<Json<Value>> {
    let u = app.updates();
    u.save_track(&container, body.tag, false).await?;
    Ok(Json(u.overview().await?))
}

async fn reset_track(State(app): State<App>, Path(container): Path<String>) -> ApiResult<Json<Value>> {
    let u = app.updates();
    u.save_track(&container, None, true).await?;
    Ok(Json(u.overview().await?))
}

/// `PUT /api/updates/settings`: only the keys sent change; a key left out is `None`.
#[derive(Deserialize, Default)]
pub struct UpdateSettingsIn {
    pub enabled: Option<bool>,
    pub check_schedule: Option<String>,
    pub auto_update: Option<bool>,
    pub auto_schedule: Option<String>,
    pub backup_first: Option<bool>,
    pub self_update: Option<bool>,
    pub self_schedule: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct UpdateIn {
    /// None = everything with an update that isn't held
    #[serde(default)]
    pub containers: Option<Vec<String>>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Deserialize)]
pub struct ContainerIn {
    pub container: String,
    #[serde(default = "yes")]
    pub hold: bool,
}

fn yes() -> bool {
    true
}

async fn updates_page(State(app): State<App>, ctx: Ctx) -> Page {
    let u = app.updates().overview().await?;
    page(&app, &ctx, "updates.html", "updates", json!({"u": u})).await.map_err(Into::into)
}

async fn overview(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(app.updates().overview().await?))
}

async fn check(State(app): State<App>) -> ApiResult<Json<Value>> {
    let u = app.updates();
    // pressed by hand: ask the registries afresh, not what they said minutes ago
    if let Some(d) = app.docker() {
        d.forget_digests();
    }
    u.check_all().await?;
    Ok(Json(u.overview().await?))
}

async fn apply(State(app): State<App>, Body(body): Body<Option<UpdateIn>>) -> ApiResult<(StatusCode, Json<Value>)> {
    let body = body.unwrap_or_default();
    let run = app.updates().update(body.containers, "manual", body.force).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"run": run}))))
}

async fn rollback(State(app): State<App>, Body(body): Body<ContainerIn>) -> ApiResult<(StatusCode, Json<Value>)> {
    let run = app.updates().rollback(&body.container).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"run": run}))))
}

async fn hold(State(app): State<App>, Body(body): Body<ContainerIn>) -> ApiResult<Json<Value>> {
    let u = app.updates();
    u.set_hold(&body.container, body.hold, "held from the UI").await?;
    Ok(Json(u.overview().await?))
}

/// `POST /api/updates/self`: replace wranglarr's own container with its newer
/// image. Someone signed in on its pages has to ask: an API key can't, since
/// the run stops the process serving it.
async fn self_update(State(app): State<App>, ctx: Ctx) -> ApiResult<(StatusCode, Json<Value>)> {
    if app.auth_enabled() && ctx.who.via != "session" {
        return Err(Error::Forbidden(
            "updating wranglarr itself needs someone signed in on its pages; an API key can't start it".into(),
        )
        .into());
    }
    let run = app.updates().self_update("manual").await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"run": run}))))
}

async fn save_settings(State(app): State<App>, Body(body): Body<UpdateSettingsIn>) -> ApiResult<Json<Value>> {
    let u = app.updates();
    let cur = u.settings().await?;
    let s = UpdateSettings {
        enabled: body.enabled.unwrap_or(cur.enabled),
        check_schedule: body.check_schedule.unwrap_or(cur.check_schedule),
        auto_update: body.auto_update.unwrap_or(cur.auto_update),
        auto_schedule: body.auto_schedule.unwrap_or(cur.auto_schedule),
        backup_first: body.backup_first.unwrap_or(cur.backup_first),
        self_update: body.self_update.unwrap_or(cur.self_update),
        self_schedule: body.self_schedule.unwrap_or(cur.self_schedule),
        source: "ui",
    };
    Ok(Json(serde_json::to_value(u.save_settings(&s).await?)?))
}

async fn reset_settings(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(app.updates().reset_settings().await?)?))
}
