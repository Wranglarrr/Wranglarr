// SPDX-License-Identifier: GPL-3.0-or-later
//! App connections, indexer sources and the download queue over HTTP, against
//! small in-process mocks of the arrs, Prowlarr, Jackett and qBittorrent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use serde_json::{Value, json};
use wranglarr::app::App;

// ---- mocks ------------------------------------------------------------------------

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

/// An arr's resource API: list, schema, test, create, update, delete.
#[derive(Default)]
struct Arr {
    resources: HashMap<String, Vec<Value>>,
    schemas: HashMap<String, Vec<Value>>,
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
    a.calls.push(format!("{method} {rest}"));
    let parts: Vec<&str> = rest.split('/').collect();
    match (method.as_str(), parts.as_slice()) {
        ("GET", [res, "schema"]) => axum::Json(json!(a.schemas.get(*res).cloned().unwrap_or_default())).into_response(),
        ("POST", [_, "test"]) => axum::Json(json!({})).into_response(),
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

async fn mock_arr(schemas: &[(&str, Vec<Value>)]) -> (String, ArrState) {
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

fn fields(names: &[&str]) -> Value {
    json!(names.iter().map(|n| json!({"name": n, "value": null})).collect::<Vec<_>>())
}

fn qb_schema(cat: &str) -> Value {
    json!({"implementation": "QBittorrent", "name": "", "id": 0, "presets": [], "infoLink": "x",
           "fields": fields(&["host", "port", "useSsl", "urlBase", "username", "password", cat, "recentPriority"])})
}

fn app_schema(imp: &str) -> Value {
    json!({"implementation": imp, "name": "", "fields": fields(&["prowlarrUrl", "baseUrl", "apiKey", "syncCategories"])})
}

fn indexer_schema(imp: &str) -> Value {
    json!({"implementation": imp, "name": "", "priority": 25,
           "fields": fields(&["baseUrl", "apiPath", "apiKey", "categories", "animeCategories", "minimumSeeders"])})
}

/// qBittorrent: login, preferences, torrents.
async fn mock_qb(torrents: Value) -> String {
    let r = Router::new()
        .route("/api/v2/auth/login", post(|| async { ([(header::SET_COOKIE, "SID=abc; HttpOnly; path=/")], "Ok.") }))
        .route("/api/v2/app/version", get(|| async { "v4.6.0" }))
        .route("/api/v2/app/preferences", get(|| async { axum::Json(json!({"save_path": "/data/torrents/"})) }))
        .route(
            "/api/v2/torrents/info",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let t = torrents.clone();
                async move {
                    if q.get("filter").map(String::as_str) == Some("errored") {
                        return axum::Json(json!([]));
                    }
                    axum::Json(t)
                }
            }),
        );
    serve(r).await
}

async fn mock_jackett() -> String {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><indexers>
        <indexer id="t1" configured="true"><title>One</title></indexer>
        <indexer id="t2" configured="true"><title>Two</title></indexer></indexers>"#;
    let r = Router::new().route(
        "/api/v2.0/indexers/all/results/torznab/api",
        get(move |Query(q): Query<HashMap<String, String>>| async move {
            if q.get("apikey").map(String::as_str) != Some("jk") {
                return (StatusCode::OK, r#"<error code="100" description="Invalid API Key"/>"#).into_response();
            }
            (StatusCode::OK, xml).into_response()
        }),
    );
    serve(r).await
}

// ---- the service -------------------------------------------------------------------

struct Svc {
    app: App,
    base: String,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

/// `apps`: (name, kind, url, extra TOML lines).
async fn svc(apps: &[(&str, &str, &str, &str)], start: bool) -> Svc {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut toml = format!(
        "[storage]\nroot = \"{}\"\nstate_db = \"{}\"\n[connect]\nauto = false\n[[suite]]\nname = \"media\"\n",
        dir.path().join("backups").display(),
        dir.path().join("state.db").display()
    );
    for (name, kind, url, extra) in apps {
        toml += &format!("[[suite.app]]\nname = \"{name}\"\nkind = \"{kind}\"\nurl = \"{url}\"\n{extra}\n");
    }
    let cfg = wranglarr_services::apps::parse(&toml, |_| None).expect("config");
    let login_off = wranglarr::auth::Options { enabled: false, ..Default::default() };
    let app =
        App::new(cfg, wranglarr::deployment::Deployment { auth: login_off, ..Default::default() }).await.expect("app");
    if start {
        app.start().await;
    }
    let base = serve(wranglarr::web::router(app.clone())).await;
    Svc { app, base, http: reqwest::Client::new(), _dir: dir }
}

impl Svc {
    async fn get(&self, path: &str) -> (u16, Value) {
        let r = self.http.get(format!("{}{path}", self.base)).send().await.expect("get");
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }
    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut rb = self.http.request(method, format!("{}{path}", self.base));
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.expect("send");
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }
    /// Wait for a run to finish: (status, result, log).
    async fn wait(&self, id: &Value) -> (String, Value, String) {
        let id = id.as_i64().expect("a run id");
        for _ in 0..200 {
            let r = self.app.state().get_run(id).await.expect("db").expect("run");
            if r.status != "queued" && r.status != "running" {
                return (r.status, r.result, r.log);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("run {id} didn't finish");
    }
}

fn find<'a>(list: &'a [Value], name: &str) -> &'a Value {
    list.iter().find(|r| r["name"] == name).unwrap_or_else(|| panic!("no {name} in {list:?}"))
}

fn field<'a>(r: &'a Value, name: &str) -> &'a Value {
    r["fields"].as_array().and_then(|f| f.iter().find(|f| f["name"] == name)).map_or(&Value::Null, |f| &f["value"])
}

// ---- app connections ----------------------------------------------------------------

#[tokio::test]
async fn connect_wires_clients_and_prowlarr() {
    let (radarr, rs) = mock_arr(&[("downloadclient", vec![qb_schema("movieCategory")])]).await;
    let (prowlarr, ps) = mock_arr(&[
        ("downloadclient", vec![qb_schema("category")]),
        ("applications", vec![app_schema("Radarr"), app_schema("Sonarr")]),
    ])
    .await;
    let qb = mock_qb(json!([])).await;
    let s = svc(
        &[
            ("radarr", "radarr", &radarr, "api_key = \"rk\""),
            ("prowlarr", "prowlarr", &prowlarr, "api_key = \"pk\""),
            ("qbittorrent", "qbittorrent", &qb, "username = \"u\"\npassword = \"p\""),
        ],
        true,
    )
    .await;

    let (code, st) = s.get("/api/connect").await;
    assert_eq!(code, 200);
    let keys: Vec<&str> = st["links"].as_array().expect("links").iter().filter_map(|l| l["key"].as_str()).collect();
    assert_eq!(keys, ["qbittorrent>radarr", "qbittorrent>prowlarr", "radarr>prowlarr"]);
    assert_eq!(st["missing"], 3);
    assert_eq!(st["auto"], false);
    assert_eq!(st["links"][0]["target"], qb.trim_start_matches("http://"));

    let (code, j) = s.post("/api/connect", json!({"links": ["x>y", "a>b"]})).await;
    assert_eq!((code, j), (404, json!({"error": r#"no such link: "x>y, a>b""#})));
    let (code, j) = s.post("/api/connect", json!({"mode": "bogus"})).await;
    assert_eq!((code, j), (400, json!({"error": "mode is 'all' or 'new'"})));
    let (code, _) = s.post("/api/connect", json!({"links": "x"})).await;
    assert_eq!(code, 422);

    // no body: every link, mode all
    let r = s.http.post(format!("{}/api/connect", s.base)).send().await.expect("post");
    assert_eq!(r.status().as_u16(), 202);
    let run: Value = r.json().await.expect("json");
    let (status, result, log) = s.wait(&run["run"]).await;
    assert_eq!(status, "ok", "{log}");
    let actions: Vec<&str> =
        result["results"].as_array().expect("results").iter().filter_map(|r| r["action"].as_str()).collect();
    assert_eq!(actions, ["created", "created", "created"]);
    assert!(
        log.contains("qbittorrent: create /data/torrents/prowlarr, /data/torrents/radarr (owned by PUID:PGID)"),
        "{log}"
    );
    assert!(log.contains("radarr ← qbittorrent: created: 'qBittorrent' → 127.0.0.1:"), "{log}");
    let r = s.app.state().get_run(run["run"].as_i64().expect("id")).await.expect("db").expect("run");
    assert_eq!((r.suite.as_str(), r.kind.as_str(), r.target.as_deref()), ("system", "connect", Some("all")));

    {
        let a = lock(&rs);
        let dc = find(&a.resources["downloadclient"], "qBittorrent");
        assert_eq!(field(dc, "host"), "127.0.0.1");
        assert_eq!(field(dc, "movieCategory"), "radarr");
        assert_eq!((field(dc, "username"), field(dc, "password")), (&json!("u"), &json!("p")));
        assert_eq!((dc["enable"].clone(), dc["priority"].clone()), (json!(true), json!(1)));
        assert!(dc.get("presets").is_none() && dc.get("infoLink").is_none());
        assert!(a.calls.contains(&"POST downloadclient/test".to_string()), "tested before saving");
    }
    {
        let p = lock(&ps);
        let app = find(&p.resources["applications"], "Radarr");
        assert_eq!(field(app, "baseUrl"), &json!(radarr));
        assert_eq!(field(app, "prowlarrUrl"), &json!(prowlarr));
        assert_eq!(field(app, "apiKey"), "rk");
        assert_eq!(app["syncLevel"], "fullSync");
        assert_eq!(field(find(&p.resources["downloadclient"], "qBittorrent"), "category"), "prowlarr");
    }
    let done: Vec<String> = s.app.state().get("connect:done").await.expect("db").expect("stored");
    assert_eq!(done, ["qbittorrent>prowlarr", "qbittorrent>radarr", "radarr>prowlarr"], "sorted");

    let (_, st) = s.get("/api/connect").await;
    assert_eq!(st["missing"], 0);
    for l in st["links"].as_array().expect("links") {
        assert_eq!(l["state"], "connected", "{l}");
        assert_eq!(l["handled"], true);
    }
    assert_eq!(st["links"][0]["detail"], "'qBittorrent', category radarr");
    let (_, st) = s.get("/api/connect").await;
    assert_eq!(
        s.app.connect().status(Some(wranglarr_services::Roles::INDEXERS)).await.expect("status")["links"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(st["links"].as_array().map(Vec::len), Some(3));

    // new: everything handled already
    let (code, run) = s.post("/api/connect", json!({"mode": "new"})).await;
    assert_eq!(code, 202);
    let (status, result, log) = s.wait(&run["run"]).await;
    assert_eq!((status.as_str(), result), ("ok", json!({"results": []})));
    assert!(log.contains("nothing to connect"));

    // all again, one link: the entry is updated in place
    lock(&rs).resources.get_mut("downloadclient").expect("list")[0]["enable"] = json!(false);
    let (_, st) = s.get("/api/connect").await;
    assert_eq!(
        (st["links"][0]["state"].clone(), st["links"][0]["detail"].clone()),
        (json!("differs"), json!("disabled"))
    );
    let (_, run) = s.post("/api/connect", json!({"links": ["qbittorrent>radarr"]})).await;
    let (status, result, _) = s.wait(&run["run"]).await;
    assert_eq!(status, "ok");
    assert_eq!(result["results"][0]["action"], "updated");
    let a = lock(&rs);
    assert_eq!(a.resources["downloadclient"].len(), 1);
    assert_eq!(a.resources["downloadclient"][0]["enable"], true);
}

#[tokio::test]
async fn connect_reports_failures_and_busy() {
    // an arr without a qBittorrent schema, and a client that isn't there
    let (radarr, _) = mock_arr(&[]).await;
    let s = svc(
        &[
            ("radarr", "radarr", &radarr, "api_key = \"rk\""),
            ("qbittorrent", "qbittorrent", "http://127.0.0.1:1", "username = \"u\"\npassword = \"p\""),
        ],
        true,
    )
    .await;
    let (_, run) = s.post("/api/connect", json!({"mode": "new"})).await;
    let (status, result, log) = s.wait(&run["run"]).await;
    assert_eq!(status, "failed", "{log}");
    assert_eq!(result["results"][0]["detail"], "radarr has no QBittorrent download client");
    assert!(log.contains("qbittorrent: download folder unknown (qbittorrent: "), "{log}");
    assert!(s.app.state().get_value("connect:done").await.expect("db").is_none(), "a failed link isn't marked");

    // without the worker, the first run stays queued
    let idle = svc(
        &[("radarr", "radarr", &radarr, "api_key = \"rk\""), ("qbittorrent", "qbittorrent", "http://127.0.0.1:1", "")],
        false,
    )
    .await;
    assert_eq!(idle.post("/api/connect", json!({})).await.0, 202);
    let (code, j) = idle.post("/api/connect", json!({})).await;
    assert_eq!((code, j), (409, json!({"error": "suite system already has a run in progress"})));
    let (code, j) = idle.post("/api/indexers/sync", json!(null)).await;
    assert_eq!((code, j), (409, json!({"error": "suite system already has a run in progress"})));
    let (code, j) =
        idle.post("/api/indexers/sources", json!({"type": "torznab", "name": "T", "url": "http://t/api"})).await;
    assert_eq!(code, 202);
    assert_eq!(j["run"], Value::Null, "saved; the sync waits for the busy system run");
    assert_eq!(j["source"]["name"], "T");
}

#[tokio::test]
async fn a_source_deleted_while_busy_still_leaves_the_arrs() {
    let (radarr, rs) = mock_arr(&[("indexer", vec![indexer_schema("Torznab")])]).await;
    let s = svc(&[("radarr", "radarr", &radarr, "api_key = \"rk\"")], true).await;
    let has_t = || lock(&rs).resources.get("indexer").into_iter().flatten().any(|r| r["name"] == "T");
    let body = json!({"type": "torznab", "name": "T", "url": "http://t/api", "apply_to": ["radarr"]});
    let (_, j) = s.post("/api/indexers/sources", body).await;
    assert_eq!(s.wait(&j["run"]).await.0, "ok");
    assert!(has_t());
    // another system run holds the suite while T is deleted
    let (release, hold) = tokio::sync::oneshot::channel::<()>();
    s.app
        .submit("system", "hold", "test", Default::default(), move |_| async move {
            let _ = hold.await;
            Err(wranglarr_core::error::Error::internal("held for the test"))
        })
        .await
        .expect("hold");
    let sid = j["source"]["id"].as_str().expect("id").to_owned();
    let (code, j) = s.send(reqwest::Method::DELETE, &format!("/api/indexers/sources/{sid}"), None).await;
    assert_eq!((code, j), (202, json!({"run": null})));
    assert!(has_t(), "nothing runs while the suite is busy");
    let _ = release.send(());
    for _ in 0..80 {
        if !has_t() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("T stayed in radarr after the busy run ended");
}

/// Bazarr's settings: GET answers what the POSTed form set.
async fn mock_bazarr() -> (String, Arc<Mutex<Value>>) {
    let st = Arc::new(Mutex::new(
        json!({"general": {"use_sonarr": false, "use_radarr": false}, "sonarr": {}, "radarr": {}}),
    ));
    let (g, p) = (st.clone(), st.clone());
    let r = Router::new().route(
        "/api/system/settings",
        get(move || {
            let st = g.clone();
            async move { axum::Json(st.lock().unwrap_or_else(|e| e.into_inner()).clone()) }
        })
        .post(move |axum::Form(f): axum::Form<HashMap<String, String>>| {
            let st = p.clone();
            async move {
                let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                for (k, v) in f {
                    let parts: Vec<&str> = k.splitn(3, '-').collect();
                    let val = match v.as_str() {
                        "true" => json!(true),
                        "false" => json!(false),
                        _ => v.parse::<i64>().map_or(json!(v), |n| json!(n)),
                    };
                    if let [_, sec, key] = parts.as_slice() {
                        s[*sec][*key] = val;
                    }
                }
                StatusCode::NO_CONTENT
            }
        }),
    );
    (serve(r).await, st)
}

/// Seerr: set up or not, its Radarr entries.
async fn mock_seerr(initialized: bool) -> (String, Arc<Mutex<Vec<Value>>>) {
    let saved: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (g, p) = (saved.clone(), saved.clone());
    let r = Router::new()
        .route("/api/v1/status", get(|| async { axum::Json(json!({"version": "2.0"})) }))
        .route("/api/v1/settings/public", get(move || async move { axum::Json(json!({"initialized": initialized})) }))
        .route(
            "/api/v1/settings/radarr",
            get(move || {
                let s = g.clone();
                async move { axum::Json(json!(s.lock().unwrap_or_else(|e| e.into_inner()).clone())) }
            })
            .post(move |axum::Json(mut b): axum::Json<Value>| {
                let s = p.clone();
                async move {
                    let mut l = s.lock().unwrap_or_else(|e| e.into_inner());
                    b["id"] = json!(l.len());
                    l.push(b.clone());
                    axum::Json(b)
                }
            }),
        )
        .route(
            "/api/v1/settings/radarr/test",
            post(|| async {
                axum::Json(json!({"profiles": [{"id": 4, "name": "HD-1080p"}, {"id": 6, "name": "Any"}],
                                  "rootFolders": [{"id": 1, "path": "/data/movies"}]}))
            }),
        );
    (serve(r).await, saved)
}

#[tokio::test]
async fn connect_bazarr_and_seerr() {
    let (radarr, _) = mock_arr(&[]).await;
    let (bazarr, bs) = mock_bazarr().await;
    let (seerr, saved) = mock_seerr(true).await;
    let s = svc(
        &[
            ("radarr", "radarr", &radarr, "api_key = \"rk\""),
            ("bazarr", "bazarr", &bazarr, "api_key = \"bk\""),
            ("seerr", "seerr", &seerr, "api_key = \"sk\""),
        ],
        true,
    )
    .await;
    let (_, st) = s.get("/api/connect").await;
    let keys: Vec<&str> = st["links"].as_array().expect("links").iter().filter_map(|l| l["key"].as_str()).collect();
    assert_eq!(keys, ["radarr>bazarr", "radarr>seerr"]); // the registry's order
    assert_eq!(st["missing"], 2);
    let (_, run) = s.post("/api/connect", json!({"mode": "new"})).await;
    let (status, result, log) = s.wait(&run["run"]).await;
    assert_eq!(status, "ok", "{log}");
    let port = radarr.rsplit(':').next().unwrap_or("");
    assert_eq!(result["results"][0]["detail"], format!("127.0.0.1:{port}"));
    assert_eq!(result["results"][1]["detail"], format!("'radarr' → 127.0.0.1:{port}, HD-1080p, /data/movies"));
    {
        let b = bs.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(b["general"]["use_radarr"], true);
        assert_eq!((b["radarr"]["ip"].clone(), b["radarr"]["apikey"].clone()), (json!("127.0.0.1"), json!("rk")));
        assert_eq!(b["radarr"]["base_url"], "/");
        let l = saved.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(l.len(), 1);
        let e = &l[0];
        assert_eq!(
            (e["hostname"].clone(), e["apiKey"].clone(), e["activeProfileId"].clone()),
            (json!("127.0.0.1"), json!("rk"), json!(4))
        );
        assert_eq!(
            (e["isDefault"].clone(), e["is4k"].clone(), e["minimumAvailability"].clone()),
            (json!(true), json!(false), json!("released"))
        );
        assert_eq!(e["externalUrl"], "");
    }
    let (_, st) = s.get("/api/connect").await;
    assert_eq!(st["links"][0]["state"], "connected", "{st}");
    assert_eq!(st["links"][1]["state"], "connected");
    assert_eq!(st["links"][1]["detail"], "'radarr', HD-1080p, /data/movies");

    // Seerr not set up yet: the link waits, and a watcher retries it
    let (seerr2, _) = mock_seerr(false).await;
    let s2 = svc(
        &[("radarr", "radarr", &radarr, "api_key = \"rk\""), ("seerr", "seerr", &seerr2, "api_key = \"sk\"")],
        true,
    )
    .await;
    let (_, st) = s2.get("/api/connect").await;
    assert_eq!(st["links"][0]["state"], "waiting");
    let (_, run) = s2.post("/api/connect", json!({})).await;
    let (status, result, _) = s2.wait(&run["run"]).await;
    assert_eq!((status.as_str(), result["results"][0]["action"].clone()), ("ok", json!("waiting")));
    assert!(s2.app.sched().has("connect:seerr"));
    assert!(s2.app.state().get_value("connect:done").await.expect("db").is_none(), "waiting links aren't handled yet");
}

// ---- indexers --------------------------------------------------------------------------

#[tokio::test]
async fn indexer_sources_are_pushed() {
    let schema = vec![indexer_schema("Torznab"), indexer_schema("Newznab")];
    let (radarr, rs) = mock_arr(&[("indexer", schema.clone())]).await;
    let (sonarr, ss) = mock_arr(&[("indexer", schema)]).await;
    let (ext, es) = mock_arr(&[("applications", vec![app_schema("Radarr"), app_schema("Sonarr")])]).await;
    let jackett = mock_jackett().await;
    let s = svc(
        &[("radarr", "radarr", &radarr, "api_key = \"rk\""), ("sonarr", "sonarr", &sonarr, "api_key = \"sk\"")],
        true,
    )
    .await;

    // validation
    for (body, msg) in [
        (
            json!({"type": "x", "name": "T", "url": "http://t"}),
            "type must be one of torznab, newznab, jackett, nzbhydra, prowlarr",
        ),
        (
            json!({"type": "torznab", "name": "bad/name", "url": "http://t"}),
            "name: 1-60 letters, digits, spaces and . _ ( ) - + &",
        ),
        (json!({"type": "torznab", "name": "T", "url": "ftp://t"}), "url must be http(s)://host…"),
        (json!({"type": "newznab", "name": "N", "url": "http://n"}), "api_key is required"),
    ] {
        assert_eq!(s.post("/api/indexers/sources", body).await, (400, json!({"error": msg})));
    }
    let (code, j) =
        s.post("/api/indexers/sources", json!({"type": "torznab", "name": "T", "url": "u", "min_seeders": "x"})).await;
    assert_eq!(code, 422);
    assert!(j["error"].as_str().unwrap_or("").contains("min_seeders"), "{j}");

    // a torznab feed into both arrs
    let (code, j) = s
        .post(
            "/api/indexers/sources",
            json!({"type": "torznab", "name": "T", "url": "http://tracker/api",
                                              "api_key": "tk", "min_seeders": 0, "priority": 99}),
        )
        .await;
    assert_eq!(code, 202, "{j}");
    assert!(j["source"].get("api_key").is_none());
    assert_eq!(
        (j["source"]["has_key"].clone(), j["source"]["type_label"].clone()),
        (json!(true), json!("Tracker (Torznab)"))
    );
    assert_eq!(j["source"]["priority"], 50, "clamped");
    let sid = j["source"]["id"].as_str().expect("id").to_string();
    let (status, result, log) = s.wait(&j["run"]).await;
    assert_eq!(status, "ok", "{log}");
    assert_eq!(result["results"].as_array().map(Vec::len), Some(2));
    assert!(log.contains("radarr: created 'T'") && log.contains("sonarr: created 'T'"), "{log}");
    let r = s.app.state().get_run(j["run"].as_i64().expect("id")).await.expect("db").expect("run");
    assert_eq!((r.kind.as_str(), r.target.as_deref()), ("indexers", Some("saved T")));
    {
        let a = lock(&rs);
        let ix = find(&a.resources["indexer"], "T");
        assert_eq!(
            (field(ix, "baseUrl"), field(ix, "apiPath"), field(ix, "apiKey")),
            (&json!("http://tracker"), &json!("/api"), &json!("tk"))
        );
        assert_eq!(field(ix, "categories")[0], 2000);
        assert_eq!(field(ix, "minimumSeeders"), 0);
        assert!(field(ix, "animeCategories").is_null(), "radarr has no anime categories");
        assert_eq!((ix["priority"].clone(), ix["enableRss"].clone()), (json!(50), json!(true)));
        let b = lock(&ss);
        assert_eq!(field(find(&b.resources["indexer"], "T"), "animeCategories"), &json!([5070]));
    }
    let (code, st) = s.get("/api/indexers").await;
    assert_eq!(code, 200);
    assert_eq!(st["sources"][0]["cells"], json!({"radarr": "ok", "sonarr": "ok"}));
    assert_eq!(st["indexer_counts"], json!({"radarr": 1, "sonarr": 1}));
    let types: Vec<&String> = st["types"].as_object().expect("types").keys().collect();
    assert_eq!(types, ["torznab", "newznab", "jackett", "nzbhydra", "prowlarr"], "in the form's order");

    // the page
    let html = s.http.get(format!("{}/indexers", s.base)).send().await.expect("page").text().await.expect("text");
    assert!(
        html.contains("<option value=\"torznab\">Tracker (Torznab)</option><option value=\"newznab\">"),
        "types in order"
    );
    assert!(html.contains(&format!("data-ix-edit=\"{sid}\"")));
    assert!(!html.contains("\"tk\""), "no API key in the page");

    // nothing changed: unchanged; then edit with a blank key: kept, only radarr
    let (_, j) = s.post("/api/indexers/sync", json!(null)).await;
    let (_, result, _) = s.wait(&j["run"]).await;
    assert_eq!(result["results"][0]["action"], "unchanged");
    let (code, j) = s
        .post(
            "/api/indexers/sources",
            json!({"id": sid, "type": "torznab", "name": "T", "url": "http://tracker/api",
                                              "api_key": "", "apply_to": ["radarr", "nope"]}),
        )
        .await;
    assert_eq!(code, 202);
    assert_eq!(j["source"]["apply_to"], json!(["radarr"]), "unknown arrs dropped");
    let (status, _, log) = s.wait(&j["run"]).await;
    assert_eq!(status, "ok", "{log}");
    assert!(log.contains("sonarr: removed 'T' (no longer selected)"), "{log}");
    let srcs: Vec<Value> = s.app.state().get("indexers:sources").await.expect("db").expect("stored");
    assert_eq!((srcs.len(), srcs[0]["api_key"].clone()), (1, json!("tk")), "blank key on edit keeps the stored one");
    assert!(lock(&ss).resources["indexer"].is_empty());
    let pushed: Value = s.app.state().get("indexers:pushed").await.expect("db").expect("stored");
    assert_eq!(pushed, json!({"radarr": {"T": sid}, "sonarr": {}}));

    // a name taken by a hand-made indexer is left alone
    lock(&ss).resources.insert("indexer".into(), vec![json!({"id": 77, "name": "One (J)", "fields": []})]);
    let (_, j) = s
        .post("/api/indexers/sources", json!({"type": "jackett", "name": "J", "url": &jackett, "api_key": "jk"}))
        .await;
    let (status, result, log) = s.wait(&j["run"]).await;
    assert_eq!(status, "partial", "{log}");
    assert!(log.contains("J: 2 tracker(s) in Jackett"), "{log}");
    assert!(log.contains("sonarr: 'One (J)' already exists and wasn't added by wranglarr"), "{log}");
    let failed: Vec<&Value> =
        result["results"].as_array().expect("r").iter().filter(|r| r["action"] == "failed").collect();
    assert_eq!(failed, [&json!({"app": "sonarr", "indexer": "One (J)", "action": "failed", "error": "name taken"})]);
    {
        let a = lock(&rs);
        let ix = find(&a.resources["indexer"], "Two (J)");
        assert_eq!(field(ix, "baseUrl"), &json!(format!("{jackett}/api/v2.0/indexers/t2/results/torznab")));
    }
    let (_, st) = s.get("/api/indexers").await;
    let j_row = st["sources"].as_array().expect("rows").iter().find(|r| r["name"] == "J").expect("J").clone();
    assert_eq!(j_row["detail"], "2 tracker(s) configured in Jackett");
    // matched by name: the hand-made "One (J)" counts
    assert_eq!(j_row["cells"], json!({"radarr": "ok", "sonarr": "ok"}));

    // an external Prowlarr: the arrs are registered in it
    let (code, j) =
        s.post("/api/indexers/sources", json!({"type": "prowlarr", "name": "P", "url": &ext, "api_key": "pk"})).await;
    assert_eq!(code, 202);
    let (status, _, log) = s.wait(&j["run"]).await;
    assert_eq!(status, "partial", "{log}"); // still the taken name in sonarr
    assert!(log.contains(&format!("P: created application 'Radarr' → {radarr}")), "{log}");
    {
        let p = lock(&es);
        let app = find(&p.resources["applications"], "Sonarr");
        assert_eq!(
            (field(app, "baseUrl"), field(app, "apiKey"), field(app, "prowlarrUrl")),
            (&json!(sonarr), &json!("sk"), &json!(ext))
        );
    }
    let (_, st) = s.get("/api/indexers").await;
    let p_row = st["sources"].as_array().expect("rows").iter().find(|r| r["name"] == "P").expect("P").clone();
    assert_eq!(p_row["cells"], json!({"radarr": "ok", "sonarr": "ok"}));

    // delete: the indexers go, 404 for an unknown one
    let (code, j) = s.send(reqwest::Method::DELETE, &format!("/api/indexers/sources/{sid}"), None).await;
    assert_eq!(code, 202);
    let (_, _, log) = s.wait(&j["run"]).await;
    assert!(log.contains("radarr: removed 'T' (no longer selected)"), "{log}");
    assert!(lock(&rs).resources["indexer"].iter().all(|r| r["name"] != "T"));
    let (code, j) = s.send(reqwest::Method::DELETE, "/api/indexers/sources/nope", None).await;
    assert_eq!((code, j), (404, json!({"error": "no such source"})));
    let (_, j) = s.post("/api/indexers/sources", json!({"type": "torznab", "name": "j", "url": "http://x"})).await;
    assert_eq!(j, json!({"error": r#"a source named "j" exists"#}));
}

// ---- downloads ------------------------------------------------------------------------------

#[tokio::test]
async fn downloads_are_merged() {
    let qb1 = mock_qb(json!([
        {"hash": "a", "name": "Movie", "category": "radarr", "state": "pausedDL", "progress": 0.5, "size": 100, "dlspeed": 0},
        {"hash": "b", "name": "Show", "category": "tv", "state": "downloading", "progress": 0.2, "size": 100, "dlspeed": 10},
    ]))
    .await;
    let qb2 = mock_qb(json!([
        {"hash": "c", "name": "Fast", "category": "other", "state": "downloading", "progress": 0.1, "size": 100, "dlspeed": 50},
    ]))
    .await;
    let s = svc(
        &[
            ("radarr", "radarr", "http://127.0.0.1:1", "api_key = \"k\""),
            ("sonarr", "sonarr", "http://127.0.0.1:1", "api_key = \"k\""),
            ("qb1", "qbittorrent", &qb1, "username = \"u\"\npassword = \"p\""),
            ("qb2", "qbittorrent", &qb2, "username = \"u\"\npassword = \"p\""),
            ("qb3", "qbittorrent", "http://127.0.0.1:1", "username = \"u\"\npassword = \"p\""),
        ],
        false,
    )
    .await;
    let (code, d) = s.get("/api/downloads").await;
    assert_eq!(code, 200);
    let names: Vec<&str> = d["items"].as_array().expect("items").iter().filter_map(|i| i["name"].as_str()).collect();
    assert_eq!(names, ["Fast", "Show", "Movie"], "downloading (fastest first), then paused");
    assert_eq!(d["items"][1]["arr"], json!({"name": "sonarr", "kind": "sonarr"}));
    assert_eq!(
        (d["items"][1]["client"].clone(), d["items"][1]["client_kind"].clone()),
        (json!("qb1"), json!("qbittorrent"))
    );
    assert_eq!(d["items"][0]["arr"], Value::Null);
    assert_eq!(d["speed"], 60);
    let clients = d["clients"].as_array().expect("clients");
    assert_eq!(
        clients[0],
        json!({"name": "qb1", "kind": "qbittorrent", "ok": true, "error": "", "count": 2, "speed": 10})
    );
    assert_eq!(clients[2]["ok"], false);
    let err = clients[2]["error"].as_str().unwrap_or("");
    assert!(!err.is_empty() && !err.starts_with("qb3: "), "{err}");
    let keys: Vec<&String> = d.as_object().expect("obj").keys().collect();
    assert_eq!(keys, ["at", "clients", "items", "speed"]);

    let (_, again) = s.get("/api/downloads").await;
    assert_eq!(again["at"], d["at"], "cached");
    tokio::time::sleep(Duration::from_millis(5)).await;
    let (_, fresh) = s.get("/api/downloads?refresh=1").await;
    assert_ne!(fresh["at"], d["at"]);
    assert_eq!(s.get("/api/downloads?refresh=maybe").await.0, 422);
}
