// SPDX-License-Identifier: GPL-3.0-or-later
//! The connections API in-process: media servers into a mock arr, server tests and
//! token helpers against mock Plex / Jellyfin / Kodi / plex.tv, and Seerr's state.

mod common;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde_json::{Value, json};
use wranglarr::mediaservers::MASK;

use common::*;

fn hdr<'a>(h: &'a HeaderMap, k: &str) -> &'a str {
    h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// Plex: token "good"; Jellyfin: key "jkey"; Kodi: user kodi / pw "kpw".
async fn media_mock() -> String {
    let r = Router::new()
        .route(
            "/plex/library/sections",
            get(|h: HeaderMap| async move {
                if hdr(&h, "x-plex-token") != "good" {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                axum::Json(json!({"MediaContainer": {"Directory": [{"title": "Movies"}, {"title": "TV"}]}}))
                    .into_response()
            }),
        )
        .route("/plex/identity", get(|| async { axum::Json(json!({"MediaContainer": {"version": "1.40.0"}})) }))
        .route(
            "/jf/System/Info",
            get(|h: HeaderMap| async move {
                if hdr(&h, "x-emby-token") != "jkey" {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                axum::Json(json!({"Version": "10.9.0", "ServerName": "box", "ProductName": "Jellyfin Server"}))
                    .into_response()
            }),
        )
        .route("/jf/Library/VirtualFolders", get(|| async { axum::Json(json!([{"Name": "Films"}])) }))
        .route("/broken/System/Info", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route(
            "/jsonrpc",
            post(|h: HeaderMap| async move {
                if hdr(&h, "authorization") != "Basic a29kaTprcHc=" {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                axum::Json(json!({"result": {"version": {"major": 13, "minor": 2}}})).into_response()
            }),
        );
    serve(r).await
}

#[tokio::test]
async fn media_servers_crud_and_push() {
    let (arr, url) = MockArr::start().await;
    let t = app_with(
        &(arr_toml("radarr", "radarr", &url)
            + &arr_toml("prowlarr", "prowlarr", &closed_url())
            + &arr_toml("sonarr", "sonarr", &closed_url())),
    )
    .await;
    let app = &t.app;

    let (st, o) = call(app, "GET", "/api/connections", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(o["seerr"], Value::Null);
    assert_eq!(o["media"]["mask"], MASK);
    assert_eq!(o["media"]["servers"], json!([]));
    assert_eq!(o["media"]["arrs"], json!([{"name": "radarr", "kind": "radarr"}, {"name": "sonarr", "kind": "sonarr"}]));
    assert_eq!(o["media"]["kinds"]["plex"]["port"], 32400);
    let text = serde_json::to_string(&app.media().overview(app).await).expect("json");
    let pos = |k: &str| text.find(&format!("\"{k}\":{{")).expect(k);
    assert!(pos("plex") < pos("jellyfin") && pos("jellyfin") < pos("emby") && pos("emby") < pos("kodi"), "{text}");

    let bad = |b: Value| async move { call(app, "POST", "/api/connections/media", Some(b)).await };
    assert_eq!(bad(json!({"kind": "vlc"})).await, (StatusCode::BAD_REQUEST, json!({"error": r#"unknown kind "vlc""#})));
    assert_eq!(
        bad(json!({"kind": "plex", "url": "http://plex:32400"})).await,
        (StatusCode::BAD_REQUEST, json!({"error": "Plex: the Plex token is needed"}))
    );

    let body = json!({"kind": "plex", "url": "https://plex.lan:32443/", "token": "tok-123456", "arrs": ["radarr", "prowlarr"],
                      "map_from": "/data/media", "map_to": "/media"});
    let (st, r) = call(app, "POST", "/api/connections/media", Some(body.clone())).await;
    assert_eq!(st, StatusCode::CREATED, "{r}");
    assert_eq!(r["server"]["token"], MASK);
    assert_eq!(r["server"]["password"], "");
    assert_eq!(r["server"]["name"], "Plex");
    assert_eq!(r["server"]["url"], "https://plex.lan:32443");
    assert_eq!(r["server"]["arrs"], json!(["radarr"]), "not Prowlarr");
    assert_eq!(r["arrs"], json!([{"app": "radarr", "action": "created"}]));
    let sid = r["server"]["id"].as_str().expect("id").to_string();
    let n = arr.get("wranglarr · Plex").expect("in radarr");
    assert_eq!(n["implementation"], "PlexServer");
    assert_eq!(
        (field(&n, "host"), field(&n, "port"), field(&n, "useSsl")),
        (&json!("plex.lan"), &json!(32443), &json!(true))
    );
    assert_eq!(
        (field(&n, "authToken"), field(&n, "mapFrom"), field(&n, "mapTo")),
        (&json!("tok-123456"), &json!("/data/media"), &json!("/media"))
    );
    assert_eq!(
        (n["onGrab"].clone(), n["onDownload"].clone(), n["onRename"].clone()),
        (json!(false), json!(true), json!(true))
    );
    assert_eq!(n["onHealthIssue"], false);
    let stored = app.state().get_value("connections:servers").await.expect("db").expect("there");
    assert_eq!(stored[0]["token"], "tok-123456");
    assert_eq!(stored[0]["update_library"], true);

    // same name again
    let (st, r) = call(
        app,
        "POST",
        "/api/connections/media",
        Some(json!({"kind": "jellyfin", "name": "plex", "url": "http://j", "token": "k"})),
    )
    .await;
    assert_eq!((st, r), (StatusCode::BAD_REQUEST, json!({"error": r#"there's already one called "plex""#})));

    // edit with the masked token: kept; moved to sonarr (unreachable): removed from radarr, error for sonarr
    let mut b2 = body.clone();
    b2["token"] = json!(MASK);
    b2["arrs"] = json!(["sonarr"]);
    let (st, r) = call(app, "PUT", &format!("/api/connections/media/{sid}"), Some(b2)).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["arrs"][0], json!({"app": "radarr", "action": "removed"}));
    assert_eq!(r["arrs"][1]["app"], "sonarr");
    assert!(r["arrs"][1]["error"].as_str().is_some_and(|e| !e.contains("tok-123456")));
    assert!(arr.names().is_empty());
    assert_eq!(app.media().servers(app).await[0].token, "tok-123456");

    let (st, r) = call(app, "PUT", "/api/connections/media/nope", Some(body.clone())).await;
    assert_eq!((st, r), (StatusCode::NOT_FOUND, json!({"error": "no such media server"})));
    let (st, r) = call(app, "DELETE", "/api/connections/media/nope", None).await;
    assert_eq!((st, r), (StatusCode::NOT_FOUND, json!({"error": "no such media server"})));
    let (st, r) = call(app, "DELETE", &format!("/api/connections/media/{sid}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r["arrs"][0]["app"], "sonarr");
    assert!(app.media().servers(app).await.is_empty());
}

#[tokio::test]
async fn testing_servers() {
    let m = media_mock().await;
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let app = &t.app;
    let test = |srv: Value| async move {
        let (st, r) = call(app, "POST", "/api/connections/media/test", Some(json!({"server": srv}))).await;
        assert_eq!(st, StatusCode::OK);
        r
    };
    assert_eq!(
        test(json!({"kind": "plex", "url": format!("{m}/plex"), "token": "good"})).await,
        json!({"ok": true, "version": "1.40.0", "libraries": ["Movies", "TV"]})
    );
    assert_eq!(
        test(json!({"kind": "plex", "url": format!("{m}/plex"), "token": "bad"})).await,
        json!({"ok": false, "error": "Plex refused the token"})
    );
    assert_eq!(
        test(json!({"kind": "jellyfin", "url": format!("{m}/jf"), "token": "jkey"})).await,
        json!({"ok": true, "version": "10.9.0", "name": "box", "product": "Jellyfin Server", "libraries": ["Films"]})
    );
    assert_eq!(
        test(json!({"kind": "emby", "url": format!("{m}/jf"), "token": "nope"})).await,
        json!({"ok": false, "error": "the server refused the API key"})
    );
    let r = test(json!({"kind": "jellyfin", "url": format!("{m}/broken"), "token": "jkey"})).await;
    assert_eq!(r["error"], format!("500 Internal Server Error from {m}/broken/System/Info"));
    assert_eq!(
        test(json!({"kind": "kodi", "url": m.clone(), "username": "kodi", "password": "kpw"})).await,
        json!({"ok": true, "version": "JSON-RPC 13.2"})
    );
    assert_eq!(
        test(json!({"kind": "kodi", "url": m.clone(), "username": "kodi", "password": "x"})).await,
        json!({"ok": false, "error": "Kodi refused the user / password"})
    );
    // validation is an answer, not a 400
    assert_eq!(test(json!({"kind": "plex", "url": "plex"})).await["ok"], false);
    let r = test(json!({"kind": "plex", "url": closed_url(), "token": "secret-token"})).await;
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().is_some_and(|e| !e.is_empty() && !e.contains("secret-token")), "{r}");
}

#[derive(Clone, Default)]
struct PlexTv {
    approved: Arc<Mutex<bool>>,
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

async fn plex_tv() -> (PlexTv, String) {
    let s = PlexTv::default();
    let s1 = s.clone();
    let s2 = s.clone();
    let r = Router::new()
        .route(
            "/api/v2/pins",
            post(move |h: HeaderMap, Query(q): Query<std::collections::HashMap<String, String>>| {
                let s = s1.clone();
                async move {
                    s.seen.lock().expect("lock").push((hdr(&h, "x-plex-client-identifier").into(), q.get("strong").cloned().unwrap_or_default()));
                    axum::Json(json!({"id": 4242, "code": "ABCD1234"}))
                }
            }),
        )
        .route(
            "/api/v2/pins/{id}",
            get(move |Path(id): Path<i64>| {
                let s = s2.clone();
                async move {
                    let done = *s.approved.lock().expect("lock");
                    axum::Json(json!({"id": id, "authToken": if done { json!("acct-token") } else { Value::Null }}))
                }
            }),
        )
        .route(
            "/api/v2/resources",
            get(|h: HeaderMap| async move {
                assert_eq!(hdr(&h, "x-plex-token"), "acct-token");
                axum::Json(json!([
                    {"name": "Home", "provides": "server", "accessToken": "srv-token", "connections": [
                        {"uri": "https://relay:8443", "relay": true},
                        {"address": "192.168.1.5", "port": 32400, "uri": "https://192-168-1-5.plex.direct:32400", "local": true}]},
                    {"name": "TV", "provides": "player,client"}]))
            }),
        );
    let url = serve(r).await;
    (s, url)
}

#[tokio::test]
async fn plex_sign_in() {
    let (tv, url) = plex_tv().await;
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let app = &t.app;
    app.media().set_plex_tv(&url, "https://app.plex.tv");
    let (st, r) = call(app, "POST", "/api/connections/plex/pin", None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["id"], 4242);
    let cid = app.state().get::<String>("connections:plex_client_id").await.expect("db").expect("made one");
    assert!(cid.starts_with("wranglarr-") && cid.len() == "wranglarr-".len() + 16);
    assert_eq!(
        r["auth_url"],
        format!("https://app.plex.tv/auth#?clientID={cid}&code=ABCD1234&context%5Bdevice%5D%5Bproduct%5D=wranglarr")
    );
    assert_eq!(tv.seen.lock().expect("lock")[0], (cid.clone(), "true".into()));

    let (_, r) = call(app, "GET", "/api/connections/plex/pin/4242", None).await;
    assert_eq!(r, json!({"done": false}));
    *tv.approved.lock().expect("lock") = true;
    let (st, r) = call(app, "GET", "/api/connections/plex/pin/4242", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        r,
        json!({"done": true, "token": "acct-token", "servers": [{"name": "Home", "token": "srv-token",
               "addresses": ["http://192.168.1.5:32400", "https://192-168-1-5.plex.direct:32400"]}]})
    );
    let (st, _) = call(app, "GET", "/api/connections/plex/pin/abc", None).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // plex.tv out of reach: 502
    app.media().set_plex_tv(&closed_url(), "https://app.plex.tv");
    let (st, r) = call(app, "POST", "/api/connections/plex/pin", None).await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
    assert!(r["error"].as_str().is_some_and(|e| e.starts_with("plex.tv: ")), "{r}");
}

async fn jellyfin_admin() -> (Arc<Mutex<Vec<String>>>, String) {
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let c1 = calls.clone();
    let c2 = calls.clone();
    let c3 = calls.clone();
    let r = Router::new()
        .route(
            "/Users/AuthenticateByName",
            post(|axum::Json(b): axum::Json<Value>| async move {
                match (b["Username"].as_str(), b["Pw"].as_str()) {
                    (Some("admin"), Some("pw")) => {
                        axum::Json(json!({"AccessToken": "sess", "User": {"Policy": {"IsAdministrator": true}}}))
                            .into_response()
                    }
                    (Some("joe"), Some("pw")) => {
                        axum::Json(json!({"AccessToken": "sess2", "User": {"Policy": {"IsAdministrator": false}}}))
                            .into_response()
                    }
                    (Some("boom"), _) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    _ => StatusCode::UNAUTHORIZED.into_response(),
                }
            }),
        )
        .route(
            "/Auth/Keys",
            post(move |h: HeaderMap, Query(q): Query<std::collections::HashMap<String, String>>| {
                let c = c1.clone();
                async move {
                    c.lock().expect("lock").push(format!(
                        "create {} {}",
                        q.get("app").cloned().unwrap_or_default(),
                        hdr(&h, "x-emby-token")
                    ));
                    StatusCode::NO_CONTENT
                }
            })
            .get(move || {
                let c = c2.clone();
                async move {
                    c.lock().expect("lock").push("list".into());
                    axum::Json(json!({"Items": [{"AppName": "wranglarr-arrs", "AccessToken": "old"},
                                                {"AppName": "other", "AccessToken": "x"},
                                                {"AppName": "wranglarr-arrs", "AccessToken": "newkey"}]}))
                }
            }),
        )
        .route(
            "/Sessions/Logout",
            post(move |h: HeaderMap| {
                let c = c3.clone();
                async move {
                    c.lock()
                        .expect("lock")
                        .push(format!("logout {}", hdr(&h, "authorization").contains("Token=\"sess\"")));
                    StatusCode::NO_CONTENT
                }
            }),
        );
    let url = serve(r).await;
    (calls, url)
}

#[tokio::test]
async fn jellyfin_api_key() {
    let (calls, url) = jellyfin_admin().await;
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let app = &t.app;
    let key = |u: String, user: &str| {
        let body = json!({"url": u, "username": user, "password": "pw"});
        async move { call(app, "POST", "/api/connections/jellyfin/key", Some(body)).await }
    };
    assert_eq!(key(format!("{url}/"), "admin").await, (StatusCode::OK, json!({"token": "newkey"})));
    assert_eq!(*calls.lock().expect("lock"), vec!["create wranglarr-arrs sess", "list", "logout true"]);
    assert_eq!(key(url.clone(), "eve").await, (StatusCode::BAD_REQUEST, json!({"error": "wrong user or password"})));
    assert_eq!(
        key(url.clone(), "joe").await,
        (StatusCode::BAD_REQUEST, json!({"error": "that user isn't an administrator: API keys need one"}))
    );
    let (st, r) = key(url.clone(), "boom").await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
    assert!(r["error"].as_str().is_some_and(|e| e.starts_with("the server: 500 Internal Server Error from ")), "{r}");
    assert_eq!(
        key("jellyfin:8096".into(), "admin").await,
        (StatusCode::BAD_REQUEST, json!({"error": "the address starts with http:// or https://"}))
    );
    let down = closed_url();
    let (st, r) = key(down.clone(), "admin").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(r["error"].as_str().is_some_and(|e| e.starts_with(&format!("can't reach {down}: "))), "{r}");
}

#[tokio::test]
async fn seerr_on_the_page() {
    let seerr = serve(
        Router::new()
            .route("/api/v1/status", get(|| async { axum::Json(json!({"version": "2.7.0", "commitTag": "v2.7.0"})) }))
            .route("/api/v1/settings/public", get(|| async { axum::Json(json!({"initialized": true})) })),
    )
    .await;
    let t = app_with(&(arr_toml("radarr", "radarr", &closed_url()) + &arr_toml("seerr", "seerr", &seerr))).await;
    let (st, o) = call(&t.app, "GET", "/api/connections", None).await;
    assert_eq!(st, StatusCode::OK);
    let s = &o["seerr"];
    assert_eq!(
        (s["name"].clone(), s["online"].clone(), s["initialized"].clone()),
        (json!("seerr"), json!(true), json!(true))
    );
    assert_eq!(s["version"], "2.7.0");
    assert_eq!(s["links"][0]["key"], "radarr>seerr"); // from the Connect module

    let t = app_with(&(arr_toml("radarr", "radarr", &closed_url()) + &arr_toml("seerr", "seerr", &closed_url()))).await;
    let (_, o) = call(&t.app, "GET", "/api/connections", None).await;
    assert_eq!((o["seerr"]["online"].clone(), o["seerr"]["initialized"].clone()), (json!(false), json!(false)));

    let (st, page) = call(&t.app, "GET", "/connections", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(page.as_str().is_some_and(|h| h.contains("connections.js")));
}
