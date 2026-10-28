// SPDX-License-Identifier: GPL-3.0-or-later
//! Live status (the arrs' webhook, the pages' event stream), the API keys page,
//! and a key changed in an app, against small in-process mocks.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use data_encoding::BASE64;
use futures::StreamExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wranglarr::app::App;

use common::{Receiver, call, schema, serve};

const ENTRY: &str = "wranglarr · live status";

// ---- a mock arr: any resource list, create, update, delete ---------------------------------

#[derive(Default)]
struct Arr {
    resources: std::collections::HashMap<String, Vec<Value>>,
    schemas: std::collections::HashMap<String, Value>,
    calls: Vec<String>,
    next_id: i64,
}

type ArrState = Arc<Mutex<Arr>>;

fn lock(s: &ArrState) -> std::sync::MutexGuard<'_, Arr> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

async fn arr_api(
    State(s): State<ArrState>,
    method: Method,
    Path((_v, rest)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut a = lock(&s);
    a.calls.push(format!("{method} {rest} {body}"));
    let parts: Vec<&str> = rest.split('/').collect();
    match (method.as_str(), parts.as_slice()) {
        ("GET", [res, "schema"]) => axum::Json(a.schemas.get(*res).cloned().unwrap_or(json!([]))).into_response(),
        ("POST", [_, "test"]) => axum::Json(json!({})).into_response(),
        ("GET", ["system", "status"]) => axum::Json(json!({"version": "5.0"})).into_response(),
        ("GET", [res]) => axum::Json(json!(a.resources.get(*res).cloned().unwrap_or_default())).into_response(),
        ("POST", [res]) => {
            a.next_id += 1;
            let mut b = body;
            b["id"] = json!(a.next_id);
            a.resources.entry(res.to_string()).or_default().push(b.clone());
            (StatusCode::CREATED, axum::Json(b)).into_response()
        }
        ("PUT", [res, id]) => {
            let id: i64 = id.parse().unwrap_or(-1);
            let mut b = body;
            b["id"] = json!(id);
            if let Some(r) = a.resources.get_mut(*res).and_then(|l| l.iter_mut().find(|r| r["id"] == id)) {
                *r = b.clone();
            }
            axum::Json(b).into_response()
        }
        ("DELETE", [res, id]) => {
            let id: i64 = id.parse().unwrap_or(-1);
            if let Some(l) = a.resources.get_mut(*res) {
                l.retain(|r| r["id"] != id);
            }
            axum::Json(json!({})).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn mock_arr(schemas: &[(&str, Value)]) -> (String, ArrState) {
    let st: ArrState = Arc::default();
    for (res, list) in schemas {
        lock(&st).schemas.insert(res.to_string(), list.clone());
    }
    let r = Router::new()
        .route("/ping", get(|| async { "OK" }))
        .route("/api/{v}/{*rest}", any(arr_api))
        .with_state(st.clone());
    (serve(r).await, st)
}

fn field<'a>(r: &'a Value, name: &str) -> &'a Value {
    r["fields"].as_array().and_then(|f| f.iter().find(|f| f["name"] == name)).map_or(&Value::Null, |f| &f["value"])
}

fn app_schema(imp: &str) -> Value {
    let fields: Vec<Value> = ["prowlarrUrl", "baseUrl", "apiKey", "syncCategories"]
        .iter()
        .map(|n| json!({"name": n, "value": null}))
        .collect();
    json!({"implementation": imp, "name": "", "fields": fields})
}

struct T {
    app: App,
    dir: tempfile::TempDir,
}

/// `extra` goes before the suite: `[events]` and the like.
async fn svc(extra: &str, apps: &[(&str, &str, &str, &str)]) -> T {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path().display();
    let mut toml = format!(
        "[storage]\nroot = \"{d}/backups\"\nstate_db = \"{d}/state.db\"\n[connect]\nauto = false\n{extra}\n[[suite]]\nname = \"media\"\n"
    );
    for (name, kind, url, more) in apps {
        toml += &format!("[[suite.app]]\nname = \"{name}\"\nkind = \"{kind}\"\nurl = \"{url}\"\n{more}\n");
    }
    let cfg = wranglarr_services::apps::parse(&toml, |_| None).expect("config");
    let login_off = wranglarr::auth::Options { enabled: false, ..Default::default() };
    let app =
        App::new(cfg, wranglarr::deployment::Deployment { auth: login_off, ..Default::default() }).await.expect("app");
    T { app, dir }
}

async fn hook(app: &App, name: &str, auth: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut rb = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/api/hooks/{name}"))
        .header("host", "wranglarr:8787")
        .header("content-type", "application/json");
    if let Some(a) = auth {
        rb = rb.header("authorization", a);
    }
    let r = wranglarr::web::router(app.clone())
        .oneshot(rb.body(Body::from(body.to_string())).expect("req"))
        .await
        .expect("resp");
    let st = r.status();
    let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.expect("body");
    (st, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn basic(user: &str, pass: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{user}:{pass}").as_bytes()))
}

#[tokio::test]
async fn the_webhook_is_put_in_and_checks_its_token() {
    let (radarr, rs) = mock_arr(&[("notification", schema())]).await;
    let t = svc("", &[("radarr", "radarr", &radarr, "api_key = \"rk\"")]).await;

    let (st, r) = call(&t.app, "POST", "/api/events/wire", None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["results"][0]["action"], "created", "{r}");
    let entry = lock(&rs).resources["notification"].iter().find(|n| n["name"] == ENTRY).cloned().expect("the webhook");
    assert_eq!(entry["implementation"], "Webhook");
    assert_eq!(field(&entry, "url"), "http://wranglarr:8787/api/hooks/radarr");
    assert_eq!(field(&entry, "method"), 1);
    assert_eq!(field(&entry, "username"), "radarr");
    for k in ["onHealthIssue", "onHealthRestored", "onDownload", "onGrab", "includeHealthWarnings"] {
        assert_eq!(entry[k], true, "{k}: {entry}");
    }
    let token = field(&entry, "password").as_str().expect("token").to_string();
    assert_eq!(token.len(), 32);

    // the arr's own test before it saves, then a real event
    let ok = basic("radarr", &token);
    assert_eq!(
        hook(&t.app, "radarr", Some(&ok), json!({"eventType": "Test"})).await,
        (StatusCode::OK, json!({"ok": true}))
    );
    let denied = (StatusCode::UNAUTHORIZED, json!({"error": "unknown app or wrong credentials"}));
    assert_eq!(hook(&t.app, "radarr", Some(&basic("radarr", "nope")), json!({})).await, denied);
    assert_eq!(hook(&t.app, "radarr", Some(&basic("sonarr", &token)), json!({})).await, denied);
    assert_eq!(hook(&t.app, "radarr", None, json!({})).await, denied);
    assert_eq!(hook(&t.app, "nope", Some(&ok), json!({})).await, denied);

    let (_, s) = call(&t.app, "GET", "/api/events", None).await;
    assert_eq!(s["enabled"], true);
    assert_eq!(s["url"], "http://wranglarr:8787/api/hooks/<app>");
    assert_eq!(s["apps"][0]["name"], "radarr");
    assert_eq!(s["apps"][0]["action"], "created");
    assert_eq!(s["apps"][0]["last_event"]["event"], "Test");

    // wired again: the same entry, updated in place
    let (_, r) = call(&t.app, "POST", "/api/events/wire", None).await;
    assert_ne!(r["results"][0]["action"], "created", "{r}");
    assert_eq!(lock(&rs).resources["notification"].iter().filter(|n| n["name"] == ENTRY).count(), 1);
}

#[tokio::test]
async fn an_event_reaches_the_open_pages() {
    let (radarr, rs) = mock_arr(&[("notification", schema())]).await;
    let t =
        svc("[events]\nurl = \"http://10.0.0.5:8787/\"", &[("radarr", "radarr", &radarr, "api_key = \"rk\"")]).await;
    call(&t.app, "POST", "/api/events/wire", None).await;
    let entry = lock(&rs).resources["notification"][0].clone();
    assert_eq!(field(&entry, "url"), "http://10.0.0.5:8787/api/hooks/radarr", "[events] url, without the slash");
    let ok = basic("radarr", field(&entry, "password").as_str().expect("token"));

    let req =
        axum::http::Request::builder().uri("/api/events/stream").header("host", "w").body(Body::empty()).expect("req");
    let r = wranglarr::web::router(t.app.clone()).oneshot(req).await.expect("resp");
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    assert_eq!(r.headers()["x-accel-buffering"], "no");
    let mut body = r.into_body().into_data_stream();
    let mut next = async || {
        let chunk = tokio::time::timeout(Duration::from_secs(10), body.next()).await.expect("an event in time");
        String::from_utf8(chunk.expect("open").expect("chunk").to_vec()).expect("text")
    };
    let first = next().await;
    assert!(first.contains(r#"data: {"type":"hello"}"#) && first.contains("retry: 5000"), "{first}");

    assert_eq!(hook(&t.app, "radarr", Some(&ok), json!({"eventType": "HealthRestored"})).await.0, StatusCode::OK);
    let ev = next().await;
    let data: Value = serde_json::from_str(ev.trim().strip_prefix("data: ").expect("data")).expect("json");
    assert_eq!(data, json!({"type": "overview", "suite": "media", "apps": ["radarr"], "event": "HealthRestored"}));
}

#[tokio::test]
async fn events_off_takes_the_webhook_out() {
    let (radarr, rs) = mock_arr(&[("notification", schema())]).await;
    lock(&rs)
        .resources
        .insert("notification".into(), vec![json!({"id": 7, "name": ENTRY, "implementation": "Webhook"})]);
    let t = svc("[events]\nenabled = false", &[("radarr", "radarr", &radarr, "api_key = \"rk\"")]).await;
    let (_, r) = call(&t.app, "POST", "/api/events/wire", None).await;
    assert_eq!(r["enabled"], false);
    assert_eq!(r["results"][0]["action"], "removed", "{r}");
    assert!(lock(&rs).resources["notification"].is_empty());
}

#[tokio::test]
async fn the_keys_page_lists_every_key() {
    let dir = tempfile::tempdir().expect("tmp");
    let xml = dir.path().join("config.xml");
    std::fs::write(&xml, "<Config><ApiKey>sonarr-key</ApiKey></Config>").expect("write");
    let t = svc(
        "",
        &[
            ("radarr", "radarr", "http://radarr:7878", "api_key = \"rk\""),
            ("sonarr", "sonarr", "http://sonarr:8989", &format!("api_key_file = \"{}\"", xml.display())),
            ("qbittorrent", "qbittorrent", "http://qb:8090", "username = \"u\"\npassword = \"p\""),
        ],
    )
    .await;
    let (st, r) = call(&t.app, "GET", "/api/keys", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        r["apps"],
        json!([
            {"name": "radarr", "kind": "radarr", "url": "http://radarr:7878", "source": "wranglarr.toml", "key": "rk"},
            {"name": "sonarr", "kind": "sonarr", "url": "http://sonarr:8989", "source": xml.display().to_string(), "key": "sonarr-key"},
        ]),
        "no qBittorrent: it has no API key"
    );
    assert_eq!(r["wranglarr"], json!({"url": "http://wranglarr:8787", "auth": false, "keys": []}));
    let (st, page) = call(&t.app, "GET", "/keys", None).await;
    assert_eq!(st, StatusCode::OK);
    let html = page.as_str().expect("html");
    assert!(html.contains(r#"data-key-app="sonarr""#) && !html.contains("sonarr-key"), "the keys aren't in the page");
}

#[tokio::test]
async fn a_key_changed_in_an_app_reconnects_the_apps_using_it() {
    let (radarr, _rs) = mock_arr(&[]).await;
    let (prowlarr, ps) = mock_arr(&[("applications", json!([app_schema("Radarr")]))]).await;
    let (rx, base) = Receiver::start().await;
    let dir = tempfile::tempdir().expect("tmp");
    let xml = dir.path().join("config.xml");
    std::fs::write(&xml, "<Config><ApiKey>old-key</ApiKey></Config>").expect("write");
    let t = svc(
        "",
        &[
            ("radarr", "radarr", &radarr, &format!("api_key_file = \"{}\"", xml.display())),
            ("prowlarr", "prowlarr", &prowlarr, "api_key = \"pk\""),
        ],
    )
    .await;
    t.app.start().await;
    let ch = json!({"kind": "webhook", "name": "hook", "config": {"url": format!("{base}/hook")}, "events": ["api_key_changed"]});
    let (st, r) = call(&t.app, "POST", "/api/notify/channels", Some(ch)).await;
    assert!(st.is_success(), "{r}");

    // connected with the old key, and the key seen once
    let (_, run) = call(&t.app, "POST", "/api/connect", Some(json!({}))).await;
    wait(&t.app, &run["run"]).await;
    let key_in_prowlarr = || field(&lock(&ps).resources["applications"][0], "apiKey").clone();
    assert_eq!(key_in_prowlarr(), "old-key");
    t.app.check_keys().await;
    t.app.check_keys().await;
    assert!(rx.all().is_empty(), "nothing changed, nothing to say");

    std::fs::write(&xml, "<Config><ApiKey>new-key</ApiKey></Config>").expect("write");
    t.app.check_keys().await;
    let got = rx.wait_for(|g| g.path == "/hook").await;
    assert_eq!(key_in_prowlarr(), "new-key");
    let body = got.json();
    assert_eq!(body["event"], "api_key_changed", "{body}");
    assert_eq!(body["title"], "radarr's API key changed", "{body}");
    assert_eq!(body["level"], "info", "{body}");
    assert!(body["message"].as_str().is_some_and(|m| m.contains("reconnected prowlarr")), "{body}");

    // the same key again: nothing more
    t.app.check_keys().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rx.all().len(), 1);
    drop(t.dir);
}

#[tokio::test]
async fn a_new_prowlarr_key_pushes_its_indexers_again() {
    let (radarr, _rs) = mock_arr(&[]).await;
    let (prowlarr, ps) = mock_arr(&[("applications", json!([app_schema("Radarr")]))]).await;
    let dir = tempfile::tempdir().expect("tmp");
    let xml = dir.path().join("config.xml");
    std::fs::write(&xml, "<Config><ApiKey>p1</ApiKey></Config>").expect("write");
    let t = svc(
        "",
        &[
            ("radarr", "radarr", &radarr, "api_key = \"rk\""),
            ("prowlarr", "prowlarr", &prowlarr, &format!("api_key_file = \"{}\"", xml.display())),
        ],
    )
    .await;
    t.app.start().await;
    t.app.check_keys().await;
    std::fs::write(&xml, "<Config><ApiKey>p2</ApiKey></Config>").expect("write");
    t.app.check_keys().await;
    for _ in 0..200 {
        if lock(&ps).calls.iter().any(|c| c.starts_with("POST command") && c.contains("ApplicationIndexerSync")) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no indexer sync: {:?}", lock(&ps).calls);
}

async fn wait(app: &App, id: &Value) {
    let id = id.as_i64().expect("a run id");
    for _ in 0..200 {
        let r = app.state().get_run(id).await.expect("db").expect("run");
        if r.status != "queued" && r.status != "running" {
            assert_eq!(r.status, "ok", "{}", r.log);
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("run {id} didn't finish");
}

#[tokio::test]
async fn key_repair_survives_restart_before_queued_run_executes() {
    let (radarr, _) = mock_arr(&[]).await;
    let (prowlarr, ps) = mock_arr(&[("applications", json!([app_schema("Radarr")]))]).await;
    let keys = tempfile::tempdir().expect("keys");
    let xml = keys.path().join("config.xml");
    std::fs::write(&xml, "<Config><ApiKey>before</ApiKey></Config>").expect("old key");
    let t = svc(
        "",
        &[
            ("radarr", "radarr", &radarr, &format!("api_key_file = \"{}\"", xml.display())),
            ("prowlarr", "prowlarr", &prowlarr, "api_key = \"pk\""),
        ],
    )
    .await;
    // Keep the worker unstarted so the reserved repair stays queued, modelling
    // a worker occupied by a backup when the key changes.
    t.app.check_keys().await;
    std::fs::write(&xml, "<Config><ApiKey>after</ApiKey></Config>").expect("new key");
    t.app.check_keys().await;
    let before = t.app.state().list_runs(Some("system"), 10).await.expect("runs");
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].status, "queued");
    let retry: Value = t.app.state().get_value("keys:repair").await.expect("retry").expect("stored retry");
    assert_eq!(retry, json!(["radarr"]));
    t.app.check_keys().await;
    assert_eq!(
        t.app.state().list_runs(Some("system"), 10).await.expect("runs").len(),
        1,
        "another scan must not duplicate an in-flight repair"
    );
    let cfg = t.app.cfg().clone();
    let deployment = t.app.deployment().clone();
    t.app.shutdown().await.expect("shutdown");
    let restarted = App::new(cfg, deployment).await.expect("restart");
    restarted.check_keys().await;
    let after = restarted.state().list_runs(Some("system"), 10).await.expect("runs");

    assert!(
        after.iter().any(|r| r.status == "queued" && r.kind == "connect"),
        "repair must be requeued after its original run was abandoned"
    );
    restarted.start().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending: Value = restarted.state().get_value("keys:repair").await.expect("retry").expect("retry list");
            if pending == json!([]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("successful repair clears retry intent");
    assert_eq!(field(&lock(&ps).resources["applications"][0], "apiKey"), "after");
    restarted.shutdown().await.expect("shutdown");
}
