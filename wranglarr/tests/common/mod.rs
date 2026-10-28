// SPDX-License-Identifier: GPL-3.0-or-later
//! Helpers for the notify / connections HTTP tests: an App on a temp config,
//! in-process requests, a recording HTTP receiver and a mock arr.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt;
use wranglarr::app::App;

pub async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

/// A URL nothing listens on.
pub fn closed_url() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    drop(l);
    format!("http://127.0.0.1:{port}")
}

pub struct TestApp {
    pub app: App,
    pub dir: tempfile::TempDir,
}

/// An App whose suite "media" has the given `[[suite.app]]` tables (TOML).
pub async fn app_with(apps_toml: &str) -> TestApp {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path().display();
    let toml = format!(
        "[storage]\nroot = \"{d}/backups\"\nstate_db = \"{d}/state/wranglarr.db\"\n\n[[suite]]\nname = \"media\"\n{apps_toml}\n"
    );
    std::fs::create_dir_all(dir.path().join("backups")).expect("mkdir");
    let cfg = wranglarr_services::apps::parse(&toml, |_| None).expect("config");
    let login_off = wranglarr::auth::Options { enabled: false, ..Default::default() };
    let app =
        App::new(cfg, wranglarr::deployment::Deployment { auth: login_off, ..Default::default() }).await.expect("app");
    TestApp { app, dir }
}

pub fn arr_toml(name: &str, kind: &str, url: &str) -> String {
    format!("[[suite.app]]\nname = \"{name}\"\nkind = \"{kind}\"\nurl = \"{url}\"\napi_key = \"key-{name}\"\n")
}

/// One request through the router: (status, JSON body).
pub async fn call(app: &App, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut rb = axum::http::Request::builder().method(method).uri(path).header("host", "wranglarr.test");
    let b = match body {
        Some(v) => {
            rb = rb.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let r = wranglarr::web::router(app.clone()).oneshot(rb.body(b).expect("request")).await.expect("response");
    let status = r.status();
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 24).await.expect("body");
    let v = serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

// ---- a receiver that records what it gets ---------------------------------------------------

#[derive(Debug, Clone)]
pub struct Got {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Got {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    pub fn header(&self, k: &str) -> &str {
        self.headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("")
    }
    pub fn header_bytes(&self, k: &str) -> Vec<u8> {
        self.headers.get(k).map(|v| v.as_bytes().to_vec()).unwrap_or_default()
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into()
    }
}

/// Records every request; a path with "fail" in it answers 401 `{"message": "nope"}`,
/// the rest 200 `{}`.
#[derive(Clone, Default)]
pub struct Receiver {
    pub got: Arc<Mutex<Vec<Got>>>,
}

impl Receiver {
    pub async fn start() -> (Receiver, String) {
        let r = Receiver::default();
        let url = serve(Router::new().fallback(record).with_state(r.clone())).await;
        (r, url)
    }

    pub fn all(&self) -> Vec<Got> {
        self.got.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Wait (up to 5 s) for a request matching `f`.
    pub async fn wait_for(&self, f: impl Fn(&Got) -> bool) -> Got {
        for _ in 0..100 {
            if let Some(g) = self.all().into_iter().find(|g| f(g)) {
                return g;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no such request; got {:?}", self.all().iter().map(|g| (&g.path, g.text())).collect::<Vec<_>>());
    }
}

async fn record(State(r): State<Receiver>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
    let path = parts.uri.path().to_string();
    if let Ok(mut g) = r.got.lock() {
        g.push(Got {
            method: parts.method.to_string(),
            path: path.clone(),
            query: parts.uri.query().unwrap_or("").into(),
            headers: parts.headers,
            body,
        });
    }
    if path.contains("fail") {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"message": "nope"}))).into_response();
    }
    axum::Json(json!({})).into_response()
}

// ---- a mock arr: its notification resources -----------------------------------------------------

#[derive(Clone, Default)]
pub struct MockArr {
    pub notifications: Arc<Mutex<Vec<Value>>>,
    pub calls: Arc<Mutex<Vec<String>>>,
}

pub fn schema() -> Value {
    let flags = json!({"onGrab": false, "onDownload": false, "onUpgrade": false, "onRename": false,
                       "onHealthIssue": false, "onHealthRestored": false, "onApplicationUpdate": false,
                       "onManualInteractionRequired": false, "supportsOnGrab": true, "supportsOnDownload": true,
                       "supportsOnUpgrade": true, "supportsOnRename": true, "supportsOnHealthIssue": true,
                       "supportsOnHealthRestored": true, "supportsOnApplicationUpdate": true,
                       "supportsOnManualInteractionRequired": true, "includeHealthWarnings": false});
    let mut out = vec![];
    for (imp, fields) in [
        ("Webhook", vec!["url", "method", "username", "password"]),
        ("Discord", vec!["webHookUrl", "username"]),
        ("Ntfy", vec!["serverUrl", "topics", "accessToken", "userName", "password", "priority"]),
        ("PlexServer", vec!["host", "port", "useSsl", "urlBase", "authToken", "updateLibrary", "mapFrom", "mapTo"]),
        ("MediaBrowser", vec!["host", "port", "useSsl", "urlBase", "apiKey", "notify", "updateLibrary"]),
    ] {
        let mut s = flags.clone();
        s["implementation"] = json!(imp);
        s["implementationName"] = json!(imp);
        s["configContract"] = json!(format!("{imp}Settings"));
        s["id"] = json!(0);
        s["presets"] = json!([]);
        s["fields"] = json!(fields.iter().map(|f| json!({"name": f, "value": null})).collect::<Vec<_>>());
        out.push(s);
    }
    Value::Array(out)
}

impl MockArr {
    pub async fn start() -> (MockArr, String) {
        let m = MockArr::default();
        let api = Router::new()
            .route("/notification", get(list_n).post(add_n))
            .route("/notification/schema", get(|| async { axum::Json(schema()) }))
            .route("/notification/{id}", axum::routing::put(put_n).delete(del_n))
            .route("/health", get(|| async { axum::Json(json!([])) }))
            .route("/queue", get(|| async { axum::Json(json!({"records": []})) }))
            .route("/rootfolder", get(|| async { axum::Json(json!([])) }))
            .route("/system/status", get(|| async { axum::Json(json!({"version": "5.0"})) }));
        let r = Router::new()
            .route("/ping", get(|| async { axum::Json(json!({"status": "OK"})) }))
            .nest("/api/v3", api)
            .with_state(m.clone());
        let url = serve(r).await;
        (m, url)
    }

    pub fn names(&self) -> Vec<String> {
        self.notifications
            .lock()
            .map(|n| n.iter().filter_map(|x| x["name"].as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        self.notifications.lock().ok()?.iter().find(|x| x["name"] == name).cloned()
    }

    fn log(&self, s: String) {
        if let Ok(mut c) = self.calls.lock() {
            c.push(s);
        }
    }
}

async fn list_n(State(m): State<MockArr>) -> axum::Json<Value> {
    axum::Json(Value::Array(m.notifications.lock().map(|n| n.clone()).unwrap_or_default()))
}

async fn add_n(State(m): State<MockArr>, axum::Json(mut v): axum::Json<Value>) -> axum::Json<Value> {
    m.log(format!("POST {}", v["name"]));
    let mut n = m.notifications.lock().expect("lock");
    let id = n.iter().filter_map(|x| x["id"].as_i64()).max().unwrap_or(0) + 1;
    v["id"] = json!(id);
    n.push(v.clone());
    axum::Json(v)
}

async fn put_n(State(m): State<MockArr>, Path(id): Path<i64>, axum::Json(v): axum::Json<Value>) -> Response {
    m.log(format!("PUT {id}"));
    let mut n = m.notifications.lock().expect("lock");
    match n.iter_mut().find(|x| x["id"] == id) {
        Some(slot) => {
            *slot = v.clone();
            axum::Json(v).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn del_n(State(m): State<MockArr>, Path(id): Path<i64>) -> StatusCode {
    m.log(format!("DELETE {id}"));
    let mut n = m.notifications.lock().expect("lock");
    n.retain(|x| x["id"] != id);
    StatusCode::OK
}

pub fn field<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["fields"].as_array().and_then(|f| f.iter().find(|x| x["name"] == name)).map_or(&Value::Null, |f| &f["value"])
}
