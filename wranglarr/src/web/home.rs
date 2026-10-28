// SPDX-License-Identifier: GPL-3.0-or-later
//! The home page and what it reads: the apps' links, the lists under each app
//! (feeds), the home settings, the apps' logins and their logos.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::applogins::{mode_text, rerun};
use crate::error::Error;
use crate::links::LinkSettings;
use crate::service::HomeGroup;
use crate::web::ApiResult;
use crate::web::{Body, Ctx, Page, Q, page, tpl};

/// Which group each kind goes in (an unknown one: media).
pub fn home_role(kind: &str) -> &'static str {
    crate::service::get(kind).map_or(HomeGroup::Media, |d| d.home_group).key()
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/", get(home))
        .route("/api/links", get(links).put(links_save).delete(links_reset))
        .route("/api/links/configuration", axum::routing::put(links_configuration_save))
        .route("/api/home/settings", get(home_settings).put(home_settings_save).delete(home_settings_reset))
        .route("/api/home/feeds", get(home_feeds))
        .route("/api/app-logins", get(app_logins))
        .route("/api/apps/{name}/logo", get(app_logo))
}

/// Each suite and its apps, with where each opens.
pub async fn poster_suites(app: &App, browser_host: &str) -> Vec<Value> {
    let links = app.links().resolve(browser_host).await;
    app.cfg()
        .suites
        .iter()
        .map(|s| {
            let apps: Vec<Value> = s
                .apps
                .iter()
                .map(|a| {
                    let link = links.get(&a.name).map_or(json!({}), |l| json!(l));
                    json!({"name": a.name, "kind": a.kind, "link": link})
                })
                .collect();
            json!({"name": s.name, "apps": apps})
        })
        .collect()
}

/// The home page groups apps by what they do, not by backup suite: media first, Prowlarr/Bazarr last.
pub fn home_groups(suites: &[Value]) -> Vec<Value> {
    let mut groups: Vec<(&str, &str, Vec<Value>)> =
        HomeGroup::ALL.iter().map(|g| (g.key(), g.title(), vec![])).collect();
    for s in suites {
        for a in s["apps"].as_array().into_iter().flatten() {
            let role = home_role(a["kind"].as_str().unwrap_or(""));
            if let Some(g) = groups.iter_mut().find(|g| g.0 == role) {
                let mut a = a.clone();
                a["suite"] = s["name"].clone();
                g.2.push(a);
            }
        }
    }
    groups
        .into_iter()
        .filter(|g| !g.2.is_empty())
        .map(|(key, title, apps)| {
            let first = &apps[0]["suite"];
            let suite = if apps.iter().all(|a| &a["suite"] == first) { first.clone() } else { Value::Null };
            json!({"key": key, "title": title, "apps": apps, "suite": suite})
        })
        .collect()
}

async fn home(State(app): State<App>, ctx: Ctx) -> Page {
    let suites = poster_suites(&app, &ctx.host).await;
    let mode = app.app_logins().mode();
    page(
        &app,
        &ctx,
        "home.html",
        "home",
        json!({
            "groups": home_groups(&suites), "links": app.links().settings().await, "suites": suites,
            "panel": app.links().panel(&ctx.host).await,
            "home_settings": app.feeds().settings().await, "apps_auth": mode, "apps_auth_text": mode_text(&mode),
            "rerun": rerun(&app.deployment().image), "browser_host": ctx.host,
            "own": app.updates().own_row().await,
        }),
    )
    .await
    .map_err(Into::into)
}

// ---- links ------------------------------------------------------------------------------

async fn links_answer(app: &App, ctx: &Ctx) -> axum::Json<Value> {
    let resolved = app.links().resolve(&ctx.host).await;
    axum::Json(
        json!({"settings": app.links().settings().await, "links": resolved, "panel": app.links().panel(&ctx.host).await}),
    )
}

async fn links(State(app): State<App>, ctx: Ctx) -> axum::Json<Value> {
    links_answer(&app, &ctx).await
}

async fn links_save(State(app): State<App>, ctx: Ctx, Body(body): Body<LinkSettings>) -> ApiResult<axum::Json<Value>> {
    app.links().save(&body).await?;
    app.links().published(true).await;
    Ok(links_answer(&app, &ctx).await)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LinksConfiguration {
    proxy: Map<String, Value>,
    links: LinkSettings,
}

async fn links_configuration_save(
    State(app): State<App>,
    ctx: Ctx,
    Body(body): Body<LinksConfiguration>,
) -> ApiResult<axum::Json<Value>> {
    app.proxy().save_with_links(&body.proxy, &body.links).await?;
    app.links().published(true).await;
    Ok(links_answer(&app, &ctx).await)
}

async fn links_reset(State(app): State<App>, ctx: Ctx) -> ApiResult<axum::Json<Value>> {
    app.links().reset().await?;
    Ok(links_answer(&app, &ctx).await)
}

// ---- home settings and lists -----------------------------------------------------------------

async fn home_settings(State(app): State<App>) -> axum::Json<Value> {
    axum::Json(json!(app.feeds().settings().await))
}

async fn home_settings_save(
    State(app): State<App>,
    Body(body): Body<Map<String, Value>>,
) -> ApiResult<axum::Json<Value>> {
    Ok(axum::Json(json!(app.feeds().save_settings(&body).await?)))
}

async fn home_settings_reset(State(app): State<App>) -> ApiResult<axum::Json<Value>> {
    Ok(axum::Json(json!(app.feeds().reset().await?)))
}

async fn home_feeds(State(app): State<App>, q: Q) -> ApiResult<axum::Json<Value>> {
    Ok(axum::Json(app.feeds().all(q.flag("refresh")?).await))
}

async fn app_logins(State(app): State<App>) -> axum::Json<Value> {
    axum::Json(app.app_logins().status().await)
}

// ---- logos ------------------------------------------------------------------------------------

/// The app's own logo, or its kind's shipped one when the app doesn't hand one out.
async fn app_logo(State(app): State<App>, Path(name): Path<String>) -> ApiResult<Response> {
    let (body, ctype) = match app.links().logo(&name).await? {
        Some(hit) => hit,
        None => {
            // down, or no logo: the kind's own from static/icons
            let kind = &app.links().app(&name)?.kind;
            let file = tpl::kind_icon(kind).and_then(|_| tpl::Static::get(&format!("icons/{kind}.svg")));
            let Some(f) = file else {
                return Err(Error::not_found("this app doesn't serve a logo wranglarr knows").into());
            };
            (bytes::Bytes::from(f.data.into_owned()), "image/svg+xml".to_string())
        }
    };
    let mut r = body.into_response();
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&ctype) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // the app's own file: never let an SVG run script on wranglarr's origin
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; img-src data:; sandbox"),
    );
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_role() {
        let suites = vec![
            json!({"name": "media", "apps": [{"name": "radarr", "kind": "radarr"}, {"name": "qb", "kind": "qbittorrent"},
                                             {"name": "prowlarr", "kind": "prowlarr"}]}),
            json!({"name": "anime", "apps": [{"name": "sonarr2", "kind": "sonarr"}]}),
        ];
        let g = home_groups(&suites);
        let keys: Vec<&str> = g.iter().map(|g| g["key"].as_str().unwrap_or("")).collect();
        assert_eq!(keys, ["media", "downloads", "metadata"]);
        assert_eq!(g[0]["suite"], Value::Null, "media spans two suites");
        assert_eq!(g[0]["apps"][1], json!({"name": "sonarr2", "kind": "sonarr", "suite": "anime"}));
        assert_eq!(g[1]["suite"], "media");
        assert_eq!(g[1]["title"], "Downloads");
    }
}
