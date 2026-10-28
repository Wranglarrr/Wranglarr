// SPDX-License-Identifier: GPL-3.0-or-later
//! Community configs over HTTP, against an in-process mock Radarr (a small REST
//! store seeded from `tests/fixtures/community/radarr.json`): check → apply →
//! undo round trips, the partial undo, export, inventory, try.
//!
//! `scenario` also compares every answer with the recorded answers for the same
//! steps against the same mock (`fixtures/community/steps.json`), so any change in
//! what the API says shows up as a diff to review.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

const KEY: &str = "radarrkey0000000000000000000000ab";
const SEED: &str = include_str!("fixtures/community/radarr.json");
const SEED_LIDARR: &str = include_str!("fixtures/community/lidarr.json");
const SCENARIO: &str = include_str!("fixtures/community/scenario.json");

// ---- the mock arr ------------------------------------------------------------------

#[derive(Default)]
struct Mock {
    store: Map<String, Value>,
    single: Map<String, Value>,
    statics: Map<String, Value>,
    nid: i64,
    faults: Vec<Value>,
    requests: Vec<String>,
}

type Shared = Arc<Mutex<Mock>>;

fn answer(status: u16, v: Value) -> Response {
    (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), axum::Json(v)).into_response()
}

/// /api/v3/customformat/2 → ("customformat", Some(2)); sub paths stay whole.
fn rest(p: &str) -> Option<(String, Option<i64>)> {
    let r = p.strip_prefix("/api/v3/").or_else(|| p.strip_prefix("/api/v1/"))?;
    if let Some((head, tail)) = r.rsplit_once('/')
        && let Ok(id) = tail.parse()
    {
        return Some((head.to_string(), Some(id)));
    }
    Some((r.to_string(), None))
}

async fn mock_any(State(s): State<Shared>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let p = uri.path().to_string();
    let data: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut m = s.lock().expect("lock");
    if let Some(c) = p.strip_prefix("/_mock/") {
        return match (c, &method) {
            ("seed", &Method::POST) => {
                let take = |k: &str| data[k].as_object().cloned().unwrap_or_default();
                *m = Mock {
                    store: take("store"),
                    single: take("single"),
                    statics: take("static"),
                    nid: 10,
                    ..Mock::default()
                };
                answer(200, json!({}))
            }
            ("state", &Method::GET) => {
                answer(200, json!({"store": m.store, "single": m.single, "requests": m.requests}))
            }
            ("fault", &Method::POST) => {
                m.faults.push(data);
                answer(200, json!({}))
            }
            ("fault", &Method::DELETE) => {
                m.faults.clear();
                answer(200, json!({}))
            }
            _ => answer(404, json!({"error": "no such control"})),
        };
    }
    m.requests.push(format!("{method} {p}"));
    let fault = m
        .faults
        .iter()
        .find(|f| {
            f["method"].as_str().is_none_or(|x| x == method.as_str())
                && p.starts_with(f["prefix"].as_str().unwrap_or(""))
        })
        .cloned();
    if let Some(f) = &fault
        && f["after_write"] != true
    {
        return answer(
            f["status"].as_u64().unwrap_or(500) as u16,
            f.get("body").cloned().unwrap_or(json!({"message": "mock fault"})),
        );
    }
    if headers.get("x-api-key").and_then(|v| v.to_str().ok()) != Some(KEY) {
        return answer(401, json!({"error": "Unauthorized"}));
    }
    let Some((res, rid)) = rest(&p) else { return answer(404, json!({"error": format!("nope {p}")})) };
    let not_found = || answer(404, json!({"message": "NotFound"}));
    match method {
        Method::GET => {
            if rid.is_none() {
                if let Some(v) = m.statics.get(&res) {
                    return answer(200, v.clone());
                }
                if let Some(v) = m.single.get(&res) {
                    return answer(200, v.clone());
                }
            }
            match (m.store.get(&res), rid) {
                (Some(rows), None) => answer(200, rows.clone()),
                (Some(rows), Some(id)) => rows
                    .as_array()
                    .and_then(|a| a.iter().find(|r| r["id"] == id))
                    .map_or_else(not_found, |r| answer(200, r.clone())),
                _ => answer(404, json!({"error": format!("nope {p}")})),
            }
        }
        Method::POST if rid.is_none() => {
            let mut j = if data.is_object() { data } else { json!({}) };
            j["id"] = json!(m.nid);
            m.nid += 1;
            if let Value::Array(a) = m.store.entry(res).or_insert_with(|| json!([])) {
                a.push(j.clone());
            }
            answer(201, j)
        }
        Method::PUT => {
            let Some(id) = rid else { return not_found() };
            let mut j = if data.is_object() { data } else { json!({}) };
            j["id"] = json!(id);
            if let Some(cur) = m.single.get_mut(&res)
                && cur["id"] == id
            {
                *cur = j.clone();
                if let Some(f) = &fault {
                    return answer(
                        f["status"].as_u64().unwrap_or(500) as u16,
                        f.get("body").cloned().unwrap_or(json!({"message":"mock fault"})),
                    );
                }
                return answer(202, j);
            }
            if let Some(Value::Array(a)) = m.store.get_mut(&res)
                && let Some(r) = a.iter_mut().find(|r| r["id"] == id)
            {
                *r = j.clone();
                if let Some(f) = &fault {
                    return answer(
                        f["status"].as_u64().unwrap_or(500) as u16,
                        f.get("body").cloned().unwrap_or(json!({"message":"mock fault"})),
                    );
                }
                return answer(202, j);
            }
            not_found()
        }
        Method::DELETE => {
            if let (Some(Value::Array(a)), Some(id)) = (m.store.get_mut(&res), rid)
                && let Some(i) = a.iter().position(|r| r["id"] == id)
            {
                a.remove(i);
                if res == "customformat" {
                    // like the arrs: the profiles stop scoring it
                    for p in m.store.get_mut("qualityprofile").and_then(Value::as_array_mut).into_iter().flatten() {
                        if let Some(Value::Array(fi)) = p.get_mut("formatItems") {
                            fi.retain(|f| f["format"] != id);
                        }
                    }
                }
                return answer(200, json!({}));
            }
            not_found()
        }
        _ => answer(404, json!({"error": format!("nope {p}")})),
    }
}

async fn serve_mock() -> String {
    let shared: Shared = Arc::new(Mutex::new(Mock::default()));
    let router = Router::new().fallback(mock_any).with_state(shared);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

async fn mock_call(url: &str, method: reqwest::Method, path: &str, body: Option<&Value>) -> Value {
    let mut rb = reqwest::Client::new().request(method, format!("{url}{path}"));
    if let Some(b) = body {
        rb = rb.json(b);
    }
    rb.send().await.expect("mock").json().await.expect("mock json")
}

async fn seed(url: &str, text: &str) {
    let s: Value = serde_json::from_str(text).expect("seed");
    mock_call(url, reqwest::Method::POST, "/_mock/seed", Some(&s)).await;
}

async fn store(url: &str) -> Value {
    let s = mock_call(url, reqwest::Method::GET, "/_mock/state", None).await;
    json!({"store": s["store"], "single": s["single"]})
}

// ---- wranglarr ---------------------------------------------------------------------

struct Env {
    router: Router,
    url: String,
    lidarr: String,
    _tmp: tempfile::TempDir,
}

async fn env() -> Env {
    let url = serve_mock().await;
    seed(&url, SEED).await;
    let lidarr = serve_mock().await;
    seed(&lidarr, SEED_LIDARR).await;
    let tmp = tempfile::tempdir().expect("tmp");
    let toml = format!(
        "[storage]\nroot = \"{0}/store\"\nstate_db = \"{0}/state/wranglarr.db\"\n\
         [[suite]]\nname = \"media\"\n[[suite.app]]\nname = \"radarr\"\nkind = \"radarr\"\nurl = \"{url}\"\napi_key = \"{KEY}\"\n\
         [[suite.app]]\nname = \"lidarr\"\nkind = \"lidarr\"\nurl = \"{lidarr}\"\napi_key = \"{KEY}\"\n",
        tmp.path().display()
    );
    let cfg = wranglarr_services::apps::parse(&toml, |_| None).expect("config");
    let login_off = wranglarr::auth::Options { enabled: false, ..Default::default() };
    let app =
        wranglarr::app::App::new(cfg, wranglarr::deployment::Deployment { auth: login_off, ..Default::default() })
            .await
            .expect("app");
    Env { router: wranglarr::web::router(app), url, lidarr, _tmp: tmp }
}

async fn call(e: &Env, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let req = Request::builder().method(method).uri(path).header("content-type", "application/json");
    let req = req.body(body.map_or_else(Body::empty, |b| Body::from(b.to_string()))).expect("request");
    let r = e.router.clone().oneshot(req).await.expect("response");
    let status = r.status().as_u16();
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 24).await.expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))))
}

fn example(id: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("configs/{id}.json"));
    std::fs::read_to_string(p).expect("example")
}

/// The scenario's steps through wranglarr: [{status, body}], and the history ids it made.
async fn run_scenario(e: &Env) -> (Vec<Value>, Vec<String>) {
    let steps: Vec<Value> = serde_json::from_str(SCENARIO).expect("scenario");
    let mut hids: Vec<String> = vec![];
    let mut out = vec![];
    for st in steps {
        let op = st["op"].as_str().expect("op");
        if op == "STORE" {
            out.push(json!({"status": 200, "body": {"radarr": store(&e.url).await, "lidarr": store(&e.lidarr).await}}));
            continue;
        }
        let mut path = st["path"].as_str().expect("path").to_string();
        for (i, h) in hids.iter().enumerate() {
            path = path.replace(&format!("$H{}", i + 1), h);
        }
        let mut body = st.get("body").cloned();
        if let Some(b) = &mut body
            && let Some(t) = b["text"].as_str()
            && let Some(id) = t.strip_prefix("$example:")
        {
            b["text"] = json!(example(id));
        }
        let (status, resp) = call(e, op, &path, body.as_ref()).await;
        if path == "/api/configs/apply" && status == 200 {
            for r in resp["results"].as_array().into_iter().flatten() {
                if let Some(h) = r["history"].as_str() {
                    hids.push(h.to_string());
                }
            }
        }
        out.push(json!({"status": status, "body": resp}));
    }
    (out, hids)
}

/// History ids → $H1…, times → T.
fn normalize(v: &mut Value, hids: &[String]) {
    match v {
        Value::String(s) => {
            if let Some(i) = hids.iter().position(|h| h == s) {
                *s = format!("$H{}", i + 1);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| normalize(x, hids)),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if (k == "at" || k == "undone") && x.is_string() {
                    *x = json!("T");
                } else if k == "installed" && x.is_string() {
                    *x = json!("M"); // a timeline mark: a new id each run
                } else if k == "url" && x.is_string() {
                    *x = json!("U"); // the mock's address: a new port each run
                } else {
                    normalize(x, hids);
                }
            }
        }
        _ => {}
    }
}

/// Where two values first differ, for the failure message.
fn first_diff(a: &Value, b: &Value, at: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                let (p, q) = (x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null));
                if let Some(d) = first_diff(p, q, &format!("{at}/{k}")) {
                    return Some(d);
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            x.iter().zip(y).enumerate().find_map(|(i, (p, q))| first_diff(p, q, &format!("{at}/{i}")))
        }
        _ if a == b => None,
        _ => Some(format!("{at}: {a} vs {b}")),
    }
}

fn rows_of(check: &Value) -> Vec<(String, String, String)> {
    check["plans"][0]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            (
                r["part"].as_str().unwrap_or("").to_string(),
                r["name"].as_str().unwrap_or("").to_string(),
                r["action"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario() {
    let e = env().await;
    let initial = json!({"radarr": store(&e.url).await, "lidarr": store(&e.lidarr).await});
    let (out, hids) = run_scenario(&e).await;
    if let Some(p) = std::env::var_os("CC_DUMP") {
        std::fs::write(p, Value::Array(out.clone()).to_string()).expect("dump");
    }
    let steps: Vec<Value> = serde_json::from_str(SCENARIO).expect("scenario");
    let at = |i: usize| (&steps[i], out[i]["status"].as_u64().unwrap_or(0), &out[i]["body"]);

    // overview + examples
    let (_, s, b) = at(0);
    assert_eq!(s, 200);
    assert_eq!(b["format"], "wranglarr-config/1");
    assert_eq!(b["apps"], json!([{"name": "radarr", "kind": "radarr"}, {"name": "lidarr", "kind": "lidarr"}]));
    assert_eq!(b["examples"].as_array().map(Vec::len), Some(2));
    assert_eq!(at(1).1, 200);
    assert_eq!(at(2), (&steps[2], 404, &json!({"error": "not found"})));

    // x265-small: 15 formats made, two profiles made, sizes changed
    let (_, s, b) = at(3);
    assert_eq!(s, 200, "{b}");
    let rows = rows_of(b);
    assert!(rows.contains(&("customFormats".into(), "x265-HEVC".into(), "update".into())), "{rows:?}");
    assert!(rows.contains(&("customFormats".into(), "x265-Res 1080p".into(), "create".into())));
    assert!(rows.contains(&("qualityProfiles".into(), "x265-4k".into(), "create".into())));
    assert!(rows.iter().any(|r| r.0 == "qualityDefinitions" && r.2 == "update"));
    assert_eq!(b["plans"][0]["errors"], 0);
    assert_eq!(b["chosen"], json!({"radarr": ["radarr"], "sonarr": []}));

    // the mixed one with choices
    let (_, s, b) = at(4);
    assert_eq!(s, 200, "{b}");
    let rows = rows_of(b);
    assert!(rows.contains(&("tags".into(), "mine-anime".into(), "create".into())), "{rows:?}");
    assert!(rows.contains(&("customFormats".into(), "Mine Other".into(), "create".into())), "{rows:?}");
    assert!(rows.contains(&("qualityProfiles".into(), "HD-1080p".into(), "keep".into())), "{rows:?}");
    assert!(rows.contains(&("naming".into(), "File naming".into(), "keep".into())), "{rows:?}");
    assert!(rows.contains(&("customFormats".into(), "Opt".into(), "create".into())), "the extra is on: {rows:?}");

    // broken: every problem is a row; apply refuses with the first
    let (_, s, b) = at(5);
    assert_eq!(s, 200, "{b}");
    assert!(b["plans"][0]["errors"].as_u64().unwrap_or(0) >= 9, "{b}");
    let (_, s, b) = at(6);
    assert_eq!(s, 200);
    assert_eq!(b["results"][0]["ok"], false);
    assert_eq!(b["results"][0]["changed"], 0);
    assert!(b["results"][0].get("history").is_none());
    assert_eq!(at(7).1, 400);
    assert_eq!(at(8).2["error"], "\"ghost\" can't take the radarr part");

    // apply x265-small, then the check says it's all there
    let (_, s, b) = at(9);
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["results"][0]["ok"], true, "{b}");
    assert_eq!(b["results"][0]["changed"], 15 + 1 + 2);
    let (_, s, b) = at(10);
    assert_eq!(s, 200);
    assert_eq!(b["plans"][0]["changes"], 0, "{b}");

    // the mixed one, with copies
    let (_, s, b) = at(11);
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["results"][0]["ok"], true, "{b}");
    assert_eq!(at(14).1, 400);
    let (_, s, b) = at(15);
    assert_eq!(s, 200, "{b}");
    assert!(b["config"]["apps"]["radarr"]["customFormats"].as_array().is_some_and(|a| !a.is_empty()));
    assert_eq!(at(16).2["error"], "pick something to export");
    let (_, s, b) = at(18);
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["quality"], "Bluray-1080p");
    assert_eq!(b["formats"], json!(["x265-HEVC"]));
    assert_eq!(at(19).1, 400);

    // undo both, newest first: the store is back to the seed
    let (_, s, b) = at(21);
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["notes"], json!([]), "{b}");
    assert_eq!(at(22).2["error"], "already undone");
    assert_eq!(at(24).1, 200);
    assert_eq!(out[25]["body"], initial, "undo puts everything back");
    assert_eq!(at(26).1, 200);
    assert_eq!(at(27).1, 404);
    assert_eq!(at(28).1, 404);
    let last = &out[29]["body"]["history"];
    assert_eq!(last.as_array().map(Vec::len), Some(1), "{last}");

    // Lidarr: metadata profiles, protocol delay profiles; a bare custom format into two apps
    let (_, s, b) = at(30);
    assert_eq!(s, 200, "{b}");
    let rows = rows_of(b);
    assert!(rows.contains(&("metadataProfiles".into(), "Singles".into(), "create".into())), "{rows:?}");
    assert!(rows.contains(&("metadataProfiles".into(), "Bad".into(), "error".into())), "{rows:?}");
    let (_, s, b) = at(32);
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["chosen"], json!({"*": ["radarr", "lidarr"]}));
    let (_, s, b) = at(34);
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["results"].as_array().map(Vec::len), Some(2));
    assert_eq!(b["results"][1]["changed"], 2, "{b}");
    let (_, s, b) = at(35);
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["results"][0]["ok"], true, "{b}");
    // all there, but the Standard profile itself: a copy was made instead
    let left: Vec<_> = rows_of(at(36).2).into_iter().filter(|r| r.2 != "same").collect();
    assert_eq!(left, [("metadataProfiles".into(), "Standard".into(), "update".into())]);
    for i in [40, 41, 42] {
        assert_eq!(at(i).1, 200, "{}", at(i).2);
        assert_eq!(at(i).2["notes"], json!([]), "{}", at(i).2);
    }
    assert_eq!(first_diff(&out[43]["body"], &initial, ""), None, "undo puts everything back");

    // and every answer is the recorded one
    let want: Value = serde_json::from_str(include_str!("fixtures/community/steps.json")).expect("fixture");
    let mut got = Value::Array(out);
    normalize(&mut got, &hids);
    if let Some(p) = std::env::var_os("CC_RECORD") {
        // re-record the answers: review the diff of steps.json before keeping it
        std::fs::write(p, got.to_string()).expect("record");
    }
    assert_eq!(got.as_array().map(Vec::len), want.as_array().map(Vec::len), "as many answers");
    let mut bad = 0;
    for (i, (g, w)) in got.as_array().into_iter().flatten().zip(want.as_array().into_iter().flatten()).enumerate() {
        if g != w {
            bad += 1;
            eprintln!("step {i} {}: differs: {}", steps[i]["path"], first_diff(g, w, "").unwrap_or_default());
        }
    }
    assert_eq!(bad, 0, "answers that differ from the recorded ones");
}

/// C13: an undo that partly fails keeps only the failed change, so pressing
/// Undo again finishes it.
#[tokio::test]
async fn partial_undo_keeps_what_failed() {
    let e = env().await;
    let cf_old = json!({"id": 2, "name": "cf", "includeCustomFormatWhenRenaming": false, "specifications": [
        {"name": "title", "implementation": "ReleaseTitleSpecification", "negate": false, "required": false,
         "fields": [{"name": "value", "value": "old"}]}]});
    let mut s2: Value = serde_json::from_str(SEED).expect("seed");
    s2["store"]["customformat"] = json!([cf_old]);
    s2["store"]["tag"] = json!([]);
    s2["store"]["qualityprofile"] = json!([]);
    mock_call(&e.url, reqwest::Method::POST, "/_mock/seed", Some(&s2)).await;
    let config = json!({"wranglarr": "wranglarr-config/1", "name": "test config", "apps": {"radarr": {
        "tags": ["x"],
        "customFormats": [{"name": "cf", "specifications": [
            {"name": "title", "implementation": "ReleaseTitleSpecification", "fields": {"value": "new"}}]}]}}});
    let text = json!({"text": config.to_string()});
    let (s, b) = call(&e, "POST", "/api/configs/check", Some(&text)).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        rows_of(&b),
        [("tags".into(), "x".into(), "create".into()), ("customFormats".into(), "cf".into(), "update".into())]
    );
    let (s, b) = call(&e, "POST", "/api/configs/apply", Some(&text)).await;
    assert_eq!(s, 200, "{b}");
    let res = &b["results"][0];
    assert_eq!((res["ok"].as_bool(), res["changed"].as_u64()), (Some(true), Some(2)), "{res}");
    let hid = res["history"].as_str().expect("history").to_string();
    let st = store(&e.url).await;
    assert_eq!(st["store"]["tag"], json!([{"label": "x", "id": 10}]));
    assert_eq!(
        st["store"]["customformat"][0]["specifications"][0]["fields"],
        json!([{"name": "value", "value": "new"}])
    );

    let fault =
        json!({"method": "PUT", "prefix": "/api/v3/customformat", "status": 500, "body": {"message": "server error"}});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let (s, b) = call(&e, "POST", &format!("/api/configs/history/{hid}/undo"), None).await;
    mock_call(&e.url, reqwest::Method::DELETE, "/_mock/fault", None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b["notes"],
        json!([
            "cf: radarr: PUT customformat/2 -> 500: server error",
            "1 change(s) couldn't be undone; press Undo again to retry them"
        ])
    );
    let h = b["history"].as_array().and_then(|a| a.iter().find(|h| h["id"] == hid.as_str())).cloned().expect("entry");
    assert!(h["undone"].is_null(), "a partial undo isn't marked undone: {h}");
    assert_eq!(h["items"], json!([{"res": "customformat", "name": "cf", "created": false}]));
    assert_eq!(store(&e.url).await["store"]["tag"], json!([]), "the tag it made is gone");

    let (s, b) = call(&e, "POST", &format!("/api/configs/history/{hid}/undo"), None).await;
    assert_eq!(s, 200, "{b}");
    let h = b["history"].as_array().and_then(|a| a.iter().find(|h| h["id"] == hid.as_str())).cloned().expect("entry");
    assert!(h["undone"].is_string(), "the retry finishes the undo: {h}");
    assert_eq!(store(&e.url).await["store"]["customformat"][0], cf_old, "the custom format is back as it was");
    let (s, b) = call(&e, "POST", &format!("/api/configs/history/{hid}/undo"), None).await;
    assert_eq!((s, b["error"].as_str()), (400, Some("already undone")));
}

/// Undo counts a change the app answers 404 for as already gone; any other
/// failure stays to retry, even one whose text mentions 404.
#[tokio::test]
async fn undo_counts_a_404_as_already_gone() {
    let e = env().await;
    let mut s2: Value = serde_json::from_str(SEED).expect("seed");
    s2["store"]["tag"] = json!([]);
    mock_call(&e.url, reqwest::Method::POST, "/_mock/seed", Some(&s2)).await;
    let config = json!({"wranglarr": "wranglarr-config/1", "name": "test config", "apps": {"radarr": {"tags": ["x"]}}});
    let (s, b) = call(&e, "POST", "/api/configs/apply", Some(&json!({"text": config.to_string(), "ack": true}))).await;
    assert_eq!(s, 200, "{b}");
    let hid = b["results"][0]["history"].as_str().expect("history").to_string();
    let undo = format!("/api/configs/history/{hid}/undo");
    let entry = |b: &Value| {
        b["history"].as_array().and_then(|a| a.iter().find(|h| h["id"] == hid.as_str())).cloned().expect("entry")
    };

    let fault = json!({"method": "DELETE", "prefix": "/api/v3/tag", "status": 500,
                       "body": {"message": "tag 404 is busy"}});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let (s, b) = call(&e, "POST", &undo, None).await;
    mock_call(&e.url, reqwest::Method::DELETE, "/_mock/fault", None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b["notes"],
        json!([
            "tag x: radarr: DELETE tag/10 -> 500: tag 404 is busy",
            "1 change(s) couldn't be undone; press Undo again to retry them"
        ])
    );
    assert!(entry(&b)["undone"].is_null(), "a 500 isn't gone: {b}");

    let fault = json!({"method": "DELETE", "prefix": "/api/v3/tag", "status": 404, "body": {"message": "NotFound"}});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let (s, b) = call(&e, "POST", &undo, None).await;
    mock_call(&e.url, reqwest::Method::DELETE, "/_mock/fault", None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["notes"], json!(["tag x: already gone"]));
    assert!(entry(&b)["undone"].is_string(), "the undo is done: {b}");
}

/// Quality sizes go in one at a time and are recorded as they go, so a
/// failure halfway can still be undone.
#[tokio::test]
async fn sizes_recorded_as_they_go() {
    let e = env().await;
    let initial = store(&e.url).await;
    let config = json!({"name": "sizes", "apps": {"radarr": {"qualityDefinitions": {
        "SDTV": {"min": 1}, "DVD": {"min": 2}, "Bluray-1080p": {"min": 3}}}}});
    // the third PUT fails
    let fault =
        json!({"method": "PUT", "prefix": "/api/v3/qualitydefinition/15", "status": 500, "body": {"message": "boom"}});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let (s, b) = call(&e, "POST", "/api/configs/apply", Some(&json!({"text": config.to_string(), "ack": true}))).await;
    mock_call(&e.url, reqwest::Method::DELETE, "/_mock/fault", None).await;
    assert_eq!(s, 200, "{b}");
    let res = &b["results"][0];
    assert_eq!(res["ok"], false);
    assert_eq!(res["error"], "radarr: PUT qualitydefinition/15 -> 500: boom");
    assert_eq!(res["changed"], 1);
    assert_eq!(b["history"][0]["partial"], true);
    assert_eq!(
        b["history"][0]["items"],
        json!([{"res": "qualitydefinition", "name": "3 quality sizes", "created": false}])
    );
    assert_ne!(store(&e.url).await, initial);
    let hid = res["history"].as_str().expect("history");
    let (s, b) = call(&e, "POST", &format!("/api/configs/history/{hid}/undo"), None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(store(&e.url).await, initial);
}

#[tokio::test]
async fn page_and_bad_bodies() {
    let e = env().await;
    let req = Request::builder().uri("/configs").body(Body::empty()).expect("request");
    let r = e.router.clone().oneshot(req).await.expect("response");
    assert_eq!(r.status(), StatusCode::OK);
    let html = axum::body::to_bytes(r.into_body(), 1 << 24).await.expect("body");
    let html = String::from_utf8_lossy(&html);
    assert!(html.contains("Community configs"));
    assert!(html.contains(r#"data-cc-tab="import""#) && html.contains(r#"data-cc-tab="export""#), "two tabs");
    assert!(
        html.contains(r#"id="pane-export" role="tabpanel" aria-labelledby="tab-export" hidden"#),
        "Export starts hidden"
    );
    let (s, _) = call(&e, "POST", "/api/configs/check", Some(&json!([1]))).await;
    assert_eq!(s, 422);
    let (s, b) = call(&e, "POST", "/api/configs/check", Some(&json!({}))).await;
    assert_eq!((s, b["error"].as_str()), (400, Some("paste a config first")));
    // an arr that isn't there is a 502
    let (s, b) = call(&e, "GET", "/api/configs/inventory/radarr", None).await;
    assert_eq!(s, 200, "{b}");
    let fault = json!({"prefix": "/api/v3/customformat", "status": 500, "body": {"message": "down"}});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let (s, b) = call(&e, "GET", "/api/configs/inventory/radarr", None).await;
    assert_eq!((s, b["error"].as_str()), (502, Some("radarr: GET customformat -> 500: down")));
}

/// The arr's own API on the mock (as someone changing it by hand would).
async fn by_hand(url: &str, method: reqwest::Method, path: &str, body: Option<&Value>) -> Value {
    let mut rb = reqwest::Client::new().request(method, format!("{url}/api/v3/{path}")).header("x-api-key", KEY);
    if let Some(b) = body {
        rb = rb.json(b);
    }
    rb.send().await.expect("mock").json().await.expect("mock json")
}

async fn set_min(url: &str, id: u64, min: f64) {
    let mut d = by_hand(url, reqwest::Method::GET, &format!("qualitydefinition/{id}"), None).await;
    d["minSize"] = json!(min);
    by_hand(url, reqwest::Method::PUT, &format!("qualitydefinition/{id}"), Some(&d)).await;
}

async fn min_of(url: &str, id: u64) -> f64 {
    by_hand(url, reqwest::Method::GET, &format!("qualitydefinition/{id}"), None).await["minSize"]
        .as_f64()
        .unwrap_or(-1.0)
}

/// Installs as keyframes: an OK for app-wide parts, changes made by hand noticed,
/// and removing an install either keeping later changes or taking it out throughout.
#[tokio::test]
async fn timeline_keyframes() {
    let e = env().await;
    let initial = store(&e.url).await;
    let better = json!({"name": "Better 1080p", "source": {"name": "Somewhere", "url": "https://x.example/better.json"},
        "apps": {"radarr": {
        "qualityDefinitions": {"Bluray-1080p": {"min": 40}},
        "customFormats": [{"name": "x265", "specifications": [
            {"name": "x265", "implementation": "ReleaseTitleSpecification", "fields": {"value": "x265"}}]}],
        "qualityProfiles": [{"name": "x265 HD", "qualities": ["Bluray-1080p"], "formatScores": {"x265": 100}}]}}});
    let body = |ack: bool| json!({"text": better.to_string(), "ack": ack});
    let installed = || call(&e, "GET", "/api/configs/installed", None);

    // quality sizes are app-wide: the check says so, and apply wants an OK
    let (_, b) = call(&e, "POST", "/api/configs/check", Some(&body(false))).await;
    assert_eq!(b["ack"], true, "{b}");
    assert_eq!(b["plans"][0]["protected"][0]["name"], "Quality sizes");
    let (s, b) = call(&e, "POST", "/api/configs/apply", Some(&body(false))).await;
    assert_eq!((s, &b["results"][0]["ok"]), (200, &json!(false)), "{b}");
    assert!(b["results"][0]["error"].as_str().is_some_and(|x| x.contains("\"ack\": true")), "{b}");
    assert_eq!(store(&e.url).await, initial, "nothing changed");
    assert_eq!(installed().await.1, json!({"installed": []}));

    let (_, b) = call(&e, "POST", "/api/configs/apply", Some(&body(true))).await;
    assert_eq!(b["results"][0]["ok"], true, "{b}");
    let first = b["results"][0]["installed"].as_str().expect("mark").to_string();
    let (s, b) = installed().await;
    assert_eq!(s, 200, "{b}");
    let marks = &b["installed"][0]["marks"];
    assert_eq!(marks.as_array().map(Vec::len), Some(2), "{b}");
    assert_eq!((&marks[0]["label"], &marks[1]["kind"]), (&json!("Better 1080p"), &json!("before")));
    assert_eq!(marks[0]["source"], json!({"name": "Somewhere", "url": "https://x.example/better.json"}));
    let sizes =
        marks[0]["items"].as_array().expect("items").iter().find(|i| i["part"] == "qualityDefinitions").cloned();
    assert_eq!(sizes.map(|i| i["changes"].clone()), Some(json!(["min 35 → 40"])), "{marks}");
    let made = marks[0]["items"].as_array().expect("items").iter().find(|i| i["name"] == "x265 HD").cloned();
    assert!(made.is_some_and(|i| rows_has(&i["changes"], "score for x265 100")), "{marks}");

    // changed by hand: noticed once, as its own mark
    set_min(&e.url, 15, 38.0).await;
    let (_, b) = installed().await;
    let marks = b["installed"][0]["marks"].clone();
    assert_eq!((&marks[0]["kind"], &marks[0]["label"]), (&json!("external"), &json!("External manual change")));
    assert_eq!(installed().await.1["installed"][0]["marks"].as_array().map(Vec::len), Some(3), "not twice");

    // removing it: keep the change by hand, or take it out throughout
    let (s, b) = call(&e, "GET", &format!("/api/configs/installed/radarr/{first}"), None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["mode"], "choose", "{b}");
    let names = |w: &Value| -> Vec<String> {
        w["effects"].as_array().into_iter().flatten().map(|i| i["name"].as_str().unwrap_or("").to_string()).collect()
    };
    assert_eq!(names(&b["keep"]), ["x265", "x265 HD"], "{b}");
    assert_eq!(b["keep"]["effects"][1]["changes"], json!(["removed"]), "{b}");
    assert_eq!(names(&b["throughout"]), ["x265", "Bluray-1080p", "x265 HD"], "{b}");
    assert_eq!(b["throughout"]["overrides"][0]["label"], "External manual change", "{b}");
    let (s, b) = call(&e, "POST", &format!("/api/configs/installed/radarr/{first}/remove"), None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["notes"], json!([]), "{b}");
    assert_eq!(min_of(&e.url, 15).await, 38.0, "the change by hand stays");
    let st = store(&e.url).await;
    assert_eq!(st["store"]["customformat"], initial["store"]["customformat"], "its format went");
    assert_eq!(st["store"]["qualityprofile"], initial["store"]["qualityprofile"], "its profile went");
    let marks = b["installed"][0]["marks"].clone();
    assert_eq!(
        marks.as_array().map(|m| m.iter().map(|x| x["kind"].clone()).collect::<Vec<_>>()),
        Some(vec![json!("external"), json!("before")])
    );

    // again, then throughout: back to the 38 from before it, over a later change by hand
    let (_, b) = call(&e, "POST", "/api/configs/apply", Some(&body(true))).await;
    let second = b["results"][0]["installed"].as_str().expect("mark").to_string();
    set_min(&e.url, 15, 30.0).await;
    let (s, b) =
        call(&e, "POST", &format!("/api/configs/installed/radarr/{second}/remove"), Some(&json!({"throughout": true})))
            .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(min_of(&e.url, 15).await, 38.0);
    let st = store(&e.url).await;
    assert_eq!(st["store"]["customformat"], initial["store"]["customformat"]);
    let marks = b["installed"][0]["marks"].clone();
    assert_eq!(marks.as_array().map(Vec::len), Some(2), "the emptied change by hand went: {marks}");

    // what isn't an install can't be removed; an unknown mark is 404
    let before = marks[1]["id"].as_str().expect("id");
    let (s, _) = call(&e, "POST", &format!("/api/configs/installed/radarr/{before}/remove"), None).await;
    assert_eq!(s, 400);
    let (s, _) = call(&e, "GET", "/api/configs/installed/radarr/nope", None).await;
    assert_eq!(s, 404);
}

fn rows_has(v: &Value, s: &str) -> bool {
    v.as_array().is_some_and(|a| a.iter().any(|x| x == s))
}

/// A score given by hand to a format an install made holds the format up when the
/// install is removed keeping later changes; throughout, it goes with it.
#[tokio::test]
async fn timeline_keep_holds_what_later_changes_use() {
    let e = env().await;
    let cfg = json!({"name": "x265", "apps": {"radarr": {
        "customFormats": [{"name": "x265", "specifications": [
            {"name": "x265", "implementation": "ReleaseTitleSpecification", "fields": {"value": "x265"}}]}]}}});
    let (_, b) = call(&e, "POST", "/api/configs/apply", Some(&json!({"text": cfg.to_string()}))).await;
    assert_eq!(b["results"][0]["ok"], true, "{b}");
    let mark = b["results"][0]["installed"].as_str().expect("mark").to_string();
    // by hand: HD-1080p scores it
    let cfs = by_hand(&e.url, reqwest::Method::GET, "customformat", None).await;
    let fid = cfs.as_array().and_then(|a| a.iter().find(|c| c["name"] == "x265")).map(|c| c["id"].clone()).expect("cf");
    let profiles = by_hand(&e.url, reqwest::Method::GET, "qualityprofile", None).await;
    let mut p = profiles.as_array().and_then(|a| a.iter().find(|p| p["name"] == "HD-1080p")).cloned().expect("profile");
    let pid = p["id"].as_u64().expect("id");
    let mut items: Vec<Value> = p["formatItems"].as_array().cloned().unwrap_or_default();
    items.retain(|f| f["format"] != fid);
    items.push(json!({"format": fid, "name": "x265", "score": 25}));
    p["formatItems"] = json!(items);
    by_hand(&e.url, reqwest::Method::PUT, &format!("qualityprofile/{pid}"), Some(&p)).await;

    let at = format!("/api/configs/installed/radarr/{mark}");
    let (_, b) = call(&e, "GET", &at, None).await;
    assert_eq!(b["mode"], "choose", "{b}");
    assert_eq!(b["keep"]["effects"], json!([]), "{b}");
    assert!(b["keep"]["kept"][0].as_str().is_some_and(|k| k.contains("External manual change")), "{b}");
    assert_eq!(b["throughout"]["effects"][0]["name"], "x265", "{b}");
    // keep: nothing changes, and the install stays as partly removed
    let (_, b) = call(&e, "POST", &format!("{at}/remove"), None).await;
    let m = &b["installed"][0]["marks"];
    let it = m.as_array().and_then(|a| a.iter().find(|x| x["id"] == mark.as_str())).cloned().expect("still there");
    assert!(!it["kept"].as_array().is_none_or(Vec::is_empty), "{it}");
    let cfs = by_hand(&e.url, reqwest::Method::GET, "customformat", None).await;
    assert!(cfs.as_array().is_some_and(|a| a.iter().any(|c| c["name"] == "x265")));
    // then throughout: the format goes, and the profile stops scoring it
    let (s, b) = call(&e, "POST", &format!("{at}/remove"), Some(&json!({"throughout": true}))).await;
    assert_eq!(s, 200, "{b}");
    let cfs = by_hand(&e.url, reqwest::Method::GET, "customformat", None).await;
    assert!(!cfs.as_array().is_some_and(|a| a.iter().any(|c| c["name"] == "x265")), "{cfs}");
    let marks = b["installed"].as_array().map_or(0, |a| a.len());
    assert_eq!(marks, 0, "nothing left on the timeline but the before mark: {b}");
}

#[tokio::test]
async fn apply_journal_survives_a_lost_mutation_response() {
    let e = env().await;
    let initial = store(&e.url).await;
    let config = json!({"name":"sizes", "apps":{"radarr":{"qualityDefinitions":{"SDTV":{"min":1}}}}});
    let fault = json!({"method":"PUT", "prefix":"/api/v3/qualitydefinition/1", "status":500,
                       "after_write":true, "body":{"message":"response lost"}});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let (_, b) = call(&e, "POST", "/api/configs/apply", Some(&json!({"text":config.to_string(),"ack":true}))).await;
    mock_call(&e.url, reqwest::Method::DELETE, "/_mock/fault", None).await;
    assert_eq!(b["results"][0]["ok"], false, "{b}");
    assert_ne!(store(&e.url).await, initial);
    let hid = b["results"][0]["history"].as_str().expect("durable history");
    assert_eq!(b["history"][0]["partial"], true);
    let (status, undone) = call(&e, "POST", &format!("/api/configs/history/{hid}/undo"), None).await;
    assert_eq!(status, 200, "{undone}");
    assert_eq!(store(&e.url).await, initial);
}

#[tokio::test]
async fn unread_custom_formats_do_not_become_deletions() {
    let e = env().await;
    let cf = json!({"name":"format", "apps":{"radarr":{"customFormats":[{"name":"test cf", "specifications":[{"name":"test", "implementation":"ReleaseTitleSpecification", "fields":{"value":"test"}}]}]}}});
    let (_, first) = call(&e, "POST", "/api/configs/apply", Some(&json!({"text":cf.to_string(),"ack":true}))).await;
    assert_eq!(first["results"][0]["ok"], true, "{first}");
    let fault = json!({"method":"GET", "prefix":"/api/v3/customformat", "status":500});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let tags = json!({"name":"tags", "apps":{"radarr":{"tags":["new-tag"]}}});
    let (_, second) = call(&e, "POST", "/api/configs/apply", Some(&json!({"text":tags.to_string(),"ack":true}))).await;
    assert_eq!(second["results"][0]["ok"], true, "{second}");
    mock_call(&e.url, reqwest::Method::DELETE, "/_mock/fault", None).await;
    let (_, installed) = call(&e, "GET", "/api/configs/installed", None).await;
    let marks = installed["installed"][0]["marks"].as_array().expect("marks");
    assert_eq!(marks.len(), 3, "false deletion causes a later drift mark: {installed}");
    assert!(
        store(&e.url).await["store"]["customformat"]
            .as_array()
            .expect("custom format list")
            .iter()
            .any(|v| v["name"] == "test cf")
    );
}

#[tokio::test]
async fn unknown_creation_is_retained_for_manual_verification() {
    let e = env().await;
    let config = json!({"name":"tags", "apps":{"radarr":{"tags":["pending-tag"]}}});
    let fault = json!({"method":"POST", "prefix":"/api/v3/tag", "status":500});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let (_, applied) =
        call(&e, "POST", "/api/configs/apply", Some(&json!({"text":config.to_string(),"ack":true}))).await;
    mock_call(&e.url, reqwest::Method::DELETE, "/_mock/fault", None).await;
    let hid = applied["results"][0]["history"].as_str().expect("pending history");
    let (_, undone) = call(&e, "POST", &format!("/api/configs/history/{hid}/undo"), None).await;
    assert!(undone["notes"].to_string().contains("creation outcome is unknown"), "{undone}");
    assert!(undone["history"][0]["undone"].is_null());
}

#[tokio::test]
async fn malformed_list_response_is_not_treated_as_empty() {
    let e = env().await;
    let initial = store(&e.url).await;
    let fault = json!({"method":"GET", "prefix":"/api/v3/customformat", "status":200, "body":{"unexpected":true}});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let config = json!({"name":"format", "apps":{"radarr":{"customFormats":[{"name":"test cf", "specifications":[{"name":"test", "implementation":"ReleaseTitleSpecification", "fields":{"value":"test"}}]}]}}});
    let (_, response) =
        call(&e, "POST", "/api/configs/apply", Some(&json!({"text":config.to_string(),"ack":true}))).await;
    assert_eq!(response["results"][0]["ok"], false, "{response}");
    assert!(response["results"][0]["error"].as_str().expect("upstream error").contains("non-list"), "{response}");
    assert_eq!(store(&e.url).await, initial);
}

#[tokio::test]
async fn interrupted_removal_keeps_a_recovery_journal() {
    let e = env().await;
    let config = json!({"name":"sizes", "apps":{"radarr":{"qualityDefinitions":{"SDTV":{"min":1}}}}});
    let (_, installed) =
        call(&e, "POST", "/api/configs/apply", Some(&json!({"text":config.to_string(),"ack":true}))).await;
    let mark = installed["results"][0]["installed"].as_str().expect("installed mark");
    let installed_state = store(&e.url).await;
    let fault = json!({"method":"PUT", "prefix":"/api/v3/qualitydefinition/1", "status":500,
                       "after_write":true, "body":{"message":"response lost"}});
    mock_call(&e.url, reqwest::Method::POST, "/_mock/fault", Some(&fault)).await;
    let (status, _) = call(&e, "POST", &format!("/api/configs/installed/radarr/{mark}/remove"), None).await;
    assert_eq!(status, 502);
    mock_call(&e.url, reqwest::Method::DELETE, "/_mock/fault", None).await;
    assert_ne!(store(&e.url).await, installed_state);
    let (_, page) = call(&e, "GET", "/api/configs", None).await;
    let entry = &page["history"][0];
    assert_eq!(entry["config"], "Removing sizes", "{page}");
    assert_eq!(entry["partial"], true);
    let hid = entry["id"].as_str().expect("recovery history");
    let (status, response) = call(&e, "POST", &format!("/api/configs/history/{hid}/undo"), None).await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(store(&e.url).await, installed_state);
}
