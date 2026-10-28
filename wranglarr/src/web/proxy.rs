// SPDX-License-Identifier: GPL-3.0-or-later
//! The reverse proxy page and its API.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use minijinja::value::{Enumerator, Object, ObjectRepr, Serde};
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::proxy::{EXPOSE, KINDS};
use crate::web::ApiResult;
use crate::web::{Body, Ctx, Page, shell, tpl};

pub fn routes() -> Router<App> {
    Router::new().route("/proxy", get(proxy_page)).route("/api/proxy", get(proxy).put(proxy_save))
}

/// The proxies as a template map that keeps their order.
#[derive(Debug)]
struct Kinds;

impl Object for Kinds {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }
    fn get_value(self: &Arc<Self>, key: &minijinja::Value) -> Option<minijinja::Value> {
        let k = key.as_str()?;
        KINDS.iter().find(|(n, _)| *n == k).map(|(_, v)| minijinja::Value::from(*v))
    }
    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Values(KINDS.iter().map(|(k, _)| minijinja::Value::from(*k)).collect())
    }
}

async fn proxy_page(State(app): State<App>, ctx: Ctx) -> Page {
    let base = shell(
        &app,
        &ctx,
        "proxy",
        json!({"expose": EXPOSE, "browser_host": ctx.host, "host_ip": app.deployment().proxy.host_ip}),
    )
    .await?;
    let v =
        minijinja::context! { kinds => minijinja::Value::from_object(Kinds), ..minijinja::Value::from(Serde(&base)) };
    tpl::render("proxy.html", v).map(Html).map_err(Into::into)
}

async fn proxy(State(app): State<App>, ctx: Ctx) -> ApiResult<axum::Json<Value>> {
    let s = app.proxy().settings().await;
    Ok(axum::Json(app.proxy().answer(&s, &ctx.host).await?))
}

async fn proxy_save(
    State(app): State<App>,
    ctx: Ctx,
    Body(body): Body<Map<String, Value>>,
) -> ApiResult<axum::Json<Value>> {
    let s = app.proxy().save(&body).await?;
    Ok(axum::Json(app.proxy().answer(&s, &ctx.host).await?))
}
