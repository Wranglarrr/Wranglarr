// SPDX-License-Identifier: GPL-3.0-or-later
//! The System page: how wranglarr runs and what it can reach.

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use serde_json::{Value, json};

use super::{Ctx, Page, page};
use crate::app::App;
use crate::vpn::in_tunnel;

pub fn routes() -> Router<App> {
    Router::new().route("/system", get(system_page))
}

/// The apps in and out of the shared tunnel, when wranglarr knows.
fn vpn_split(app: &App) -> Value {
    let Some(v) = app.cfg().vpn.as_ref().filter(|v| v.apps.is_some()) else { return Value::Null };
    let (mut seen, mut inside, mut outside) = (std::collections::HashSet::new(), vec![], vec![]);
    for a in app.cfg().suites.iter().flat_map(|s| s.apps.iter()) {
        if seen.insert(a.name.clone()) {
            if in_tunnel(Some(v), a) { inside.push(&a.name) } else { outside.push(&a.name) }
        }
    }
    json!({"inside": inside, "outside": outside})
}

async fn system_page(State(app): State<App>, ctx: Ctx) -> Page {
    // no secrets in the page context: only what the page shows
    let suites: Vec<Value> = app
        .cfg()
        .suites
        .iter()
        .map(|s| {
            let apps: Vec<Value> = s
                .apps
                .iter()
                .map(|a| {
                    json!({"name": a.name, "kind": a.kind, "url": a.url, "databases": a.databases,
                                "container": a.container, "config_dir": a.config_dir})
                })
                .collect();
            json!({"name": s.name, "apps": apps})
        })
        .collect();
    let shared: Vec<Value> = app
        .cfg()
        .shared
        .iter()
        .map(|r| json!({"name": r.name, "label": r.label(), "base": r.base, "apply_to": r.apply_to}))
        .collect();
    let cfg = json!({"docker_socket": app.cfg().docker_socket,
                     "postgres": {"host": app.cfg().postgres.host, "port": app.cfg().postgres.port},
                     "vpn": app.cfg().vpn.as_ref().map(|v| json!({"control_url": v.control_url}))});
    let extra = json!({
        "vpn": app.vpn().status(false).await,
        "vpn_split": vpn_split(&app),
        "postgres": app.pg().available().await,
        "postgres_version": app.pg().server_version().await,
        "docker": app.docker_ok().await,
        "store_root": app.store().root.display().to_string(),
        "total_bytes": app.blocking(|app| Ok(app.store().total_usage())).await?,
        "suites": suites,
        "shared": shared,
        "cfg": cfg,
        "connect": app.connect_status().await,
        "events": app.events_status().await.unwrap_or(Value::Null),
        "process": crate::owner::process(),
        "home": app.home_settings().await,
    });
    page(&app, &ctx, "system.html", "system", extra).await.map_err(Into::into)
}
