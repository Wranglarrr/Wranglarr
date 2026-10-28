// SPDX-License-Identifier: GPL-3.0-or-later
//! The Notifications page and its API.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use super::{Body, Ctx, Page, page};
use crate::app::App;
use crate::notify::{Overview, public};
use crate::value::{text, truthy};
use crate::web::ApiResult;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/notifications", get(notifications_page))
        .route("/api/notify", get(overview))
        .route("/api/notify/channels", post(add))
        .route("/api/notify/channels/{cid}", put(save).delete(delete))
        .route("/api/notify/test", post(test))
        .route("/api/notify/settings", put(settings))
        .route("/api/notify/check", post(check))
}

async fn notifications_page(State(app): State<App>, ctx: Ctx) -> Page {
    page(&app, &ctx, "notifications.html", "notifications", json!({})).await.map_err(Into::into)
}

async fn overview(State(app): State<App>) -> Json<Overview> {
    Json(app.notifier().overview(&app).await)
}

async fn add(State(app): State<App>, Body(body): Body<Map<String, Value>>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (c, pushed) = app.notifier().save_channel(&app, &body, None).await?;
    Ok((StatusCode::CREATED, Json(json!({"channel": public(&c), "arrs": pushed}))))
}

async fn save(
    State(app): State<App>,
    Path(cid): Path<String>,
    Body(body): Body<Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    let (c, pushed) = app.notifier().save_channel(&app, &body, Some(&cid)).await?;
    Ok(Json(json!({"channel": public(&c), "arrs": pushed})))
}

async fn delete(State(app): State<App>, Path(cid): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"arrs": app.notifier().delete_channel(&app, &cid).await?})))
}

async fn test(State(app): State<App>, Body(body): Body<Map<String, Value>>) -> ApiResult<Json<Value>> {
    let channel = match body.get("channel") {
        Some(Value::Object(o)) => o.clone(),
        _ => Map::new(),
    };
    let id = body.get("id").filter(|v| truthy(v)).map(text);
    Ok(Json(app.notifier().test(&app, &channel, id.as_deref()).await?))
}

async fn settings(
    State(app): State<App>,
    Body(body): Body<Map<String, Value>>,
) -> ApiResult<Json<crate::notify::Settings>> {
    Ok(Json(app.notifier().save_settings(&app, &body).await?))
}

async fn check(State(app): State<App>) -> Json<Value> {
    Json(app.notifier().check(&app).await)
}
