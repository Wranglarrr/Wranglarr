// SPDX-License-Identifier: GPL-3.0-or-later
//! The Community configs page and its API. A config error answers 400, an
//! unknown history entry, timeline mark or example 404 `not found`, an arr that fails 502.

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use super::{Body, Ctx, Page, page};
use crate::app::App;
use crate::community::Request;
use crate::value::{text, truthy};
use crate::web::ApiResult;

type J = ApiResult<Json<Value>>;
type Obj = Map<String, Value>;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/configs", get(configs_page))
        .route("/api/configs", get(overview))
        .route("/api/configs/examples/{eid}", get(example))
        .route("/api/configs/check", post(check))
        .route("/api/configs/apply", post(apply))
        .route("/api/configs/history/{hid}/undo", post(undo))
        .route("/api/configs/history/{hid}", delete(forget))
        .route("/api/configs/inventory/{name}", get(inventory))
        .route("/api/configs/export", post(export))
        .route("/api/configs/try", post(try_title))
        .route("/api/configs/installed", get(installed))
        .route("/api/configs/installed/{name}/{id}", get(removal))
        .route("/api/configs/installed/{name}/{id}/remove", post(remove))
}

async fn configs_page(State(app): State<App>, ctx: Ctx) -> Page {
    page(&app, &ctx, "configs.html", "configs", json!({})).await.map_err(Into::into)
}

async fn overview(State(app): State<App>) -> J {
    Ok(Json(app.community().overview(&app).await?))
}

async fn example(State(app): State<App>, Path(eid): Path<String>) -> J {
    Ok(Json(json!({"text": app.community().example(&eid)?})))
}

async fn check(State(app): State<App>, Body(body): Body<Obj>) -> J {
    Ok(Json(app.community().check(&app, &Request::from_body(&body)).await?))
}

async fn apply(State(app): State<App>, Body(body): Body<Obj>) -> J {
    Ok(Json(app.community().apply(&app, &Request::from_body(&body)).await?))
}

async fn undo(State(app): State<App>, Path(hid): Path<String>) -> J {
    Ok(Json(app.community().undo(&app, &hid).await?))
}

async fn forget(State(app): State<App>, Path(hid): Path<String>) -> J {
    Ok(Json(json!({"history": app.community().forget(&app, &hid).await?})))
}

async fn installed(State(app): State<App>) -> J {
    Ok(Json(json!({"installed": app.community().installed(&app).await?})))
}

async fn removal(State(app): State<App>, Path((name, id)): Path<(String, String)>) -> J {
    Ok(Json(app.community().removal(&app, &name, &id).await?))
}

/// `{"throughout": true}` takes the install's changes out over later ones too.
async fn remove(State(app): State<App>, Path((name, id)): Path<(String, String)>, Body(body): Body<Option<Obj>>) -> J {
    let throughout = body.and_then(|b| b.get("throughout").cloned()).is_some_and(|v| truthy(&v));
    Ok(Json(app.community().remove(&app, &name, &id, throughout).await?))
}

async fn inventory(State(app): State<App>, ctx: Ctx, Path(name): Path<String>) -> J {
    let mut inv = app.community().inventory(&app, &name).await?;
    // where the browser opens the app, for the "open in the app" links
    inv["url"] = json!(app.links().resolve(&ctx.host).await.remove(&name).and_then(|l| l.url));
    Ok(Json(inv))
}

async fn export(State(app): State<App>, Body(body): Body<Obj>) -> J {
    Ok(Json(app.community().export(&app, &body).await?))
}

async fn try_title(State(app): State<App>, Body(body): Body<Obj>) -> J {
    let name = body.get("app").filter(|v| truthy(v)).map(text);
    Ok(Json(app.community().try_title(&app, &name.unwrap_or_default(), body.get("title")).await?))
}
