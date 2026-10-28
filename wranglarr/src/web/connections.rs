// SPDX-License-Identifier: GPL-3.0-or-later
//! The Connections page: media servers for the arrs, and Seerr.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use super::{Body, Ctx, Page, page, path_int};
use crate::app::App;
use crate::mediaservers::public;
use crate::service;
use crate::service::AppConfigExt;
use crate::service::Roles;
use crate::value::{head, text, truthy};
use crate::web::ApiResult;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/connections", get(connections_page))
        .route("/api/connections", get(overview))
        .route("/api/connections/media", post(add))
        .route("/api/connections/media/{sid}", put(save).delete(delete))
        .route("/api/connections/media/test", post(test))
        .route("/api/connections/plex/pin", post(plex_pin))
        .route("/api/connections/plex/pin/{pin_id}", get(plex_check))
        .route("/api/connections/jellyfin/key", post(jellyfin_key))
}

async fn connections_page(State(app): State<App>, ctx: Ctx) -> Page {
    page(&app, &ctx, "connections.html", "connections", json!({})).await.map_err(Into::into)
}

/// The requests app's state for the page (the `seerr` key): up, set up (it takes
/// links), its version, and its links to the arrs.
async fn seerr_info(app: &App, ctx: &Ctx) -> Value {
    let Some(a) = app.cfg().all_apps().find(|a| a.has(Roles::REQUESTS)) else { return Value::Null };
    let url = app.links().resolve(&ctx.host).await.remove(&a.name).and_then(|l| l.url);
    let mut info = json!({"name": a.name, "online": false, "initialized": false, "url": url});
    match service::client(a).await {
        Ok(c) => {
            let online = c.ping().await;
            info["online"] = json!(online);
            if online {
                // a requests app without links of its own has no setup to wait for
                let ready = match c.link_source() {
                    Some(links) => links.accepts_links().await,
                    None => true,
                };
                info["initialized"] = json!(ready);
                match c.status().await {
                    Ok(st) => info["version"] = st.get("version").cloned().unwrap_or(Value::Null),
                    Err(e) => info["error"] = json!(head(&e.to_string(), 200)),
                }
            }
        }
        Err(e) => info["error"] = json!(head(&e.to_string(), 200)),
    }
    info["links"] = match app.connect().status(Some(Roles::REQUESTS)).await {
        Ok(st) => st["links"].clone(),
        Err(_) => json!([]),
    };
    info
}

async fn overview(State(app): State<App>, ctx: Ctx) -> Json<Value> {
    let media = serde_json::to_value(app.media().overview(&app).await).unwrap_or(Value::Null);
    let seerr = seerr_info(&app, &ctx).await;
    Json(json!({"media": media, "seerr": seerr}))
}

async fn add(State(app): State<App>, Body(body): Body<Map<String, Value>>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (m, pushed) = app.media().save(&app, &body, None).await?;
    Ok((StatusCode::CREATED, Json(json!({"server": public(&m), "arrs": pushed}))))
}

async fn save(
    State(app): State<App>,
    Path(sid): Path<String>,
    Body(body): Body<Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    let (m, pushed) = app.media().save(&app, &body, Some(&sid)).await?;
    Ok(Json(json!({"server": public(&m), "arrs": pushed})))
}

async fn delete(State(app): State<App>, Path(sid): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"arrs": app.media().delete(&app, &sid).await?})))
}

async fn test(State(app): State<App>, Body(body): Body<Map<String, Value>>) -> Json<Value> {
    let server = match body.get("server") {
        Some(Value::Object(o)) => o.clone(),
        _ => Map::new(),
    };
    let id = body.get("id").filter(|v| truthy(v)).map(text);
    Json(app.media().test(&app, &server, id.as_deref()).await)
}

async fn plex_pin(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(app.media().plex_pin(&app).await?))
}

async fn plex_check(State(app): State<App>, Path(pin_id): Path<String>) -> ApiResult<Json<Value>> {
    let id = path_int(&pin_id, "pin_id")?;
    Ok(Json(app.media().plex_check(&app, id).await?))
}

async fn jellyfin_key(State(app): State<App>, Body(body): Body<Map<String, Value>>) -> ApiResult<Json<Value>> {
    let s = |k: &str| body.get(k).filter(|v| truthy(v)).map(text).unwrap_or_default();
    Ok(Json(app.media().jellyfin_key(&s("url"), &s("username"), &s("password")).await?))
}
