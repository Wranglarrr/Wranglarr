// SPDX-License-Identifier: GPL-3.0-or-later
//! The API keys page: every app's key in one place, for setting up other apps.
//! Someone signed in on wranglarr's pages has to ask: an API key for wranglarr
//! (a dashboard widget, a script) doesn't get the keys of every app with it.

use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use super::{Ctx, page};
use crate::app::App;
use crate::error::{Error, Result};
use crate::web::ApiResult;

pub fn routes() -> Router<App> {
    Router::new().route("/keys", get(keys_page)).route("/api/keys", get(api_keys))
}

fn signed_in(app: &App, ctx: &Ctx) -> Result<()> {
    if app.auth_enabled() && ctx.who.via != "session" {
        return Err(Error::Forbidden("the apps' API keys are shown to someone signed in on wranglarr's pages".into()));
    }
    Ok(())
}

/// Never kept by the browser or a proxy.
fn no_store(mut r: Response) -> Response {
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn keys_page(State(app): State<App>, ctx: Ctx) -> ApiResult<Response> {
    signed_in(&app, &ctx)?;
    // the keys come from /api/keys, so they aren't in the page itself
    let apps: Vec<_> = app.key_apps().iter().map(|a| json!({"name": a.name, "kind": a.kind, "url": a.url})).collect();
    let extra = json!({"apps": apps, "own_url": own_url(&app)});
    Ok(no_store(page(&app, &ctx, "keys.html", "keys", extra).await?.into_response()))
}

/// Where the other apps reach wranglarr: its service name in the stack.
fn own_url(app: &App) -> String {
    format!("http://wranglarr:{}", app.cfg().port)
}

/// `GET /api/keys`: `{"apps": [{name, kind, url, source, key | error}],
/// "wranglarr": {url, auth, keys: [{id, name, created, last_used}]}}`. wranglarr's
/// own keys are kept as hashes, so only their names: a new one is shown once.
async fn api_keys(State(app): State<App>, ctx: Ctx) -> ApiResult<Response> {
    signed_in(&app, &ctx)?;
    let auth = app.auth_enabled();
    let own: Vec<serde_json::Value> = if auth { app.blocking(|a| a.auth().api_keys()).await? } else { vec![] };
    let body = json!({"apps": app.app_keys().await, "wranglarr": {"url": own_url(&app), "auth": auth, "keys": own}});
    Ok(no_store(Json(body).into_response()))
}
