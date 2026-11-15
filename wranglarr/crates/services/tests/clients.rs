// SPDX-License-Identifier: GPL-3.0-or-later
//! The app clients against small mock apps over HTTP (axum on a free port): built
//! by their descriptors and used through their capabilities, and, for what only
//! one kind has, the concrete client itself.

use std::collections::HashMap;
use std::net::TcpListener as StdListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use wranglarr_core::config::AppConfig;
use wranglarr_services as service;

// ---- helpers ---------------------------------------------------------------------

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

fn app(name: &str, kind: &str, url: &str) -> AppConfig {
    AppConfig { name: name.into(), kind: kind.into(), url: url.into(), enabled: true, ..Default::default() }
}

fn free_port() -> u16 {
    StdListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).expect("free port")
}

// ---- qBittorrent -------------------------------------------------------------------

#[derive(Default)]
struct Qb {
    logins: usize,
    session: String,
}

async fn qb_login(State(st): State<Arc<Mutex<Qb>>>, body: String) -> Response {
    if !body.contains("username=admin") || !body.contains("password=p%26ss") {
        return (StatusCode::OK, "Fails.").into_response();
    }
    let mut s = st.lock().expect("lock");
    s.logins += 1;
    s.session = format!("s{}", s.logins);
    // qBittorrent 5.1+: 204 and a QBT_SID_<port> cookie
    (StatusCode::NO_CONTENT, [("set-cookie", format!("QBT_SID_8080={}; HttpOnly; path=/", s.session))]).into_response()
}

fn qb_authed(st: &Arc<Mutex<Qb>>, h: &HeaderMap) -> bool {
    let s = st.lock().expect("lock");
    h.get("cookie").and_then(|c| c.to_str().ok()) == Some(&format!("QBT_SID_8080={}", s.session))
}

async fn qb_router() -> (String, Arc<Mutex<Qb>>) {
    let st = Arc::new(Mutex::new(Qb::default()));
    let r = Router::new()
        .route("/api/v2/auth/login", post(qb_login))
        .route(
            "/api/v2/app/version",
            get(|State(st): State<Arc<Mutex<Qb>>>, h: HeaderMap| async move {
                if !qb_authed(&st, &h) {
                    return (StatusCode::FORBIDDEN, "Forbidden").into_response();
                }
                // the session ends after this answer, like a restarted qBittorrent
                st.lock().expect("lock").session = "gone".into();
                "v5.1.0\n".into_response()
            }),
        )
        .route(
            "/api/v2/app/webapiVersion",
            get(|State(st): State<Arc<Mutex<Qb>>>, h: HeaderMap| async move {
                if !qb_authed(&st, &h) {
                    return (StatusCode::FORBIDDEN, "Forbidden").into_response();
                }
                "2.11.4".into_response()
            }),
        )
        .route(
            "/api/v2/app/preferences",
            get(|State(st): State<Arc<Mutex<Qb>>>, h: HeaderMap| async move {
                if !qb_authed(&st, &h) {
                    return (StatusCode::FORBIDDEN, "Forbidden").into_response();
                }
                axum::Json(json!({"save_path": "/data/torrents/"})).into_response()
            }),
        )
        .route(
            "/api/v2/torrents/info",
            get(
                |State(st): State<Arc<Mutex<Qb>>>, h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    if !qb_authed(&st, &h) {
                        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
                    }
                    match q.get("filter").map(String::as_str) {
                    Some("downloading") => axum::Json(json!([
                        {"hash": "h1", "name": "Movie", "category": "radarr", "state": "downloading", "progress": 0.25,
                         "size": 4000, "completed": 1000, "dlspeed": 300, "eta": 10, "added_on": 1761000000},
                        {"hash": "h2", "name": "Show", "category": "sonarr", "state": "stalledDL", "progress": 0.0,
                         "size": -1, "total_size": -1, "eta": 8640000},
                    ]))
                    .into_response(),
                    Some("errored") => (StatusCode::BAD_REQUEST, "unknown filter").into_response(),
                    _ => axum::Json(json!([{"state": "uploading", "completed": 7}])).into_response(),
                }
                },
            ),
        )
        .with_state(st.clone());
    (serve(r).await, st)
}

#[tokio::test]
async fn qbittorrent_sessions() {
    let (url, st) = qb_router().await;
    let mut a = app("qb", "qbittorrent", &url);
    a.username = "admin".into();
    a.password = "p&ss".into();
    let c = service::client(&a).await.expect("client");
    assert!(c.ping().await);

    // app/version ends the session: webapiVersion gets a 403, signs in again and retries
    let s = c.status().await.expect("status");
    assert_eq!(s["version"], "v5.1.0");
    assert_eq!(s["databaseVersion"], "2.11.4");
    assert_eq!(s["databaseType"], "files");
    assert_eq!(s["instanceName"], "qBittorrent");
    assert_eq!(st.lock().expect("lock").logins, 2);

    // a clone shares the session
    let c2 = c.clone();
    assert_eq!(c2.download_root().await.expect("root"), "/data/torrents/");
    assert_eq!(st.lock().expect("lock").logins, 2);

    assert_eq!(
        c.collection().await.expect("collection"),
        json!({"torrents": 1, "downloading": 0, "seeding": 1, "paused": 0, "size_on_disk": 7})
    );
    // "errored" isn't known to this qBittorrent: left out
    let d = c.downloads().await.expect("downloads");
    assert_eq!(d.len(), 2);
    assert_eq!((d[0].id.as_str(), d[0].state.as_str(), d[0].done, d[0].eta), ("h1", "downloading", 1000, Some(10)));
    assert_eq!(
        (d[1].state.as_str(), d[1].state_text.as_str(), d[1].size, d[1].eta),
        ("stalled", "stalled, no peers", 0, None)
    );

    a.password = "wrong".into();
    let e = service::client(&a).await.expect("client").status().await.expect_err("bad password");
    assert_eq!(e.message(), "qb: login failed (200 Fails.); check QBITTORRENT_USER / QBITTORRENT_PASSWORD");
}

#[tokio::test]
async fn unreachable_apps() {
    let url = format!("http://127.0.0.1:{}", free_port());
    for kind in ["qbittorrent", "sabnzbd", "nzbget", "seerr", "bazarr"] {
        let mut a = app(kind, kind, &url);
        a.api_key = "k".into();
        let c = service::client(&a).await.expect("client");
        assert!(!c.ping().await, "{kind}");
        assert!(!c.wait_ready(Duration::from_millis(100)).await, "{kind}");
        let e = c.status().await.expect_err("down");
        assert!(e.message().starts_with(&format!("{kind}: ")), "{}", e.message());
        assert!(!e.message().contains("apikey="), "no API key in messages: {}", e.message());
    }
}

// ---- SABnzbd + NZBGet ------------------------------------------------------------------

type Calls = Arc<Mutex<Vec<HashMap<String, String>>>>;

#[tokio::test]
async fn sabnzbd_categories() {
    let set: Calls = Arc::default();
    let r = Router::new()
        .route("/api", get(|State(set): State<Calls>, Query(q): Query<HashMap<String, String>>| async move {
            // like SABnzbd: the version needs no key
            if q.get("mode").map(String::as_str) == Some("version") {
                return axum::Json(json!({"version": "4.5.1"}));
            }
            if q.get("apikey").map(String::as_str) != Some("sabkey") || q.get("output").map(String::as_str) != Some("json") {
                return axum::Json(json!({"status": false, "error": "API Key Incorrect"}));
            }
            axum::Json(match q.get("mode").map(String::as_str).unwrap_or("") {
                "get_cats" => json!({"categories": ["*", "Radarr"]}),
                "set_config" => {
                    set.lock().expect("lock").push(q.clone());
                    json!({"status": true})
                }
                "get_config" => json!({"config": {"misc": {"complete_dir": "/data/usenet/complete"}}}),
                "warnings" => json!({"warnings": [{"text": "disk almost full", "type": "WARNING"}]}),
                "queue" => json!({"queue": {"paused": false, "kbpersec": "10", "noofslots_total": 1, "slots": [
                    {"nzo_id": "SABnzbd_nzo_1", "filename": "Thing", "cat": "sonarr", "status": "Downloading",
                     "mb": "2", "mbleft": "1", "percentage": "50", "timeleft": "0:00:30"}]}}),
                "history" => json!({"history": {"noofslots": 1, "slots": [
                    {"nzo_id": "SABnzbd_nzo_2", "name": "Done", "category": "radarr", "status": "Verifying", "bytes": 10}]}}),
                _ => json!({"status": false, "error": "not implemented"}),
            })
        }))
        .with_state(set.clone());
    let url = serve(r).await;
    let mut a = app("sab", "sabnzbd", &url);
    a.api_key = "sabkey".into();
    let c = service::client(&a).await.expect("client");
    let added = c.ensure_categories(&["radarr".into(), "Sonarr".into()]).await.expect("categories");
    assert_eq!(added, vec!["sonarr".to_string()]);
    let calls = set.lock().expect("lock").clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (calls[0]["section"].as_str(), calls[0]["name"].as_str(), calls[0]["dir"].as_str()),
        ("categories", "sonarr", "sonarr")
    );
    assert_eq!(c.download_root().await.expect("root"), "/data/usenet/complete");
    assert_eq!(c.health().await, vec![json!({"type": "warning", "message": "disk almost full"})]);
    let d = c.downloads().await.expect("downloads");
    assert_eq!(d.len(), 2);
    assert_eq!((d[0].speed, d[0].eta, d[0].progress), (10240, Some(30), 0.5));
    assert_eq!((d[1].state.as_str(), d[1].state_text.as_str()), ("processing", "verifying"));
    assert_eq!(c.status().await.expect("status")["version"], "4.5.1");
    assert_eq!(c.collection().await.expect("collection"), json!({"queue": 1, "history": 1, "paused": 0}));

    a.api_key = "wrong".into();
    let e = service::client(&a).await.expect("client").collection().await.expect_err("bad key");
    assert_eq!(e.message(), "sab: queue: API Key Incorrect");
    assert!(service::client(&a).await.expect("client").health().await.is_empty());
}

#[derive(Default)]
struct Nzb {
    saved: Option<Value>,
    reloads: usize,
}

#[tokio::test]
async fn nzbget_categories() {
    let st: Arc<Mutex<Nzb>> = Arc::default();
    let r = Router::new()
        .route(
            "/jsonrpc",
            post(|State(st): State<Arc<Mutex<Nzb>>>, h: HeaderMap, axum::Json(req): axum::Json<Value>| async move {
                // nzbget:secret
                if h.get("authorization").and_then(|v| v.to_str().ok()) != Some("Basic bnpiZ2V0OnNlY3JldA==") {
                    return (StatusCode::UNAUTHORIZED, "").into_response();
                }
                let mut s = st.lock().expect("lock");
                let result = match req["method"].as_str().unwrap_or("") {
                    "version" => json!("25.0"),
                    "loadconfig" => {
                        json!([{"Name": "MainDir", "Value": "/config"}, {"Name": "Category1.Name", "Value": "Radarr"},
                                       {"Name": "Category2.Name", "Value": "Sonarr"}])
                    }
                    "config" => json!([{"Name": "DestDir", "Value": "/data/usenet/completed"}]),
                    "listgroups" => json!([{"NZBID": 1}, {"NZBID": 2}]),
                    "history" => json!([{"NZBID": 3}]),
                    "status" => json!({"DownloadPaused": true, "DownloadedSizeMB": 2048}),
                    "saveconfig" => {
                        s.saved = Some(req["params"][0].clone());
                        json!(true)
                    }
                    "reload" => {
                        s.reloads += 1;
                        json!(true)
                    }
                    "bad" => {
                        return axum::Json(json!({"result": null, "error": "Invalid procedure", "id": 1}))
                            .into_response();
                    }
                    _ => Value::Null,
                };
                axum::Json(json!({"result": result, "error": null, "id": req["id"]})).into_response()
            }),
        )
        .with_state(st.clone());
    let url = serve(r).await;
    let mut a = app("nzb", "nzbget", &url);
    a.username = "nzbget".into();
    a.password = "secret".into();
    let c = service::client(&a).await.expect("client");
    assert_eq!(c.status().await.expect("status")["version"], "25.0");
    assert_eq!(c.download_root().await.expect("root"), "/data/usenet/completed");
    assert_eq!(
        c.collection().await.expect("collection"),
        json!({"queue": 2, "history": 1, "paused": 1, "size_on_disk": 2048_i64 * 1024 * 1024})
    );
    assert!(c.ensure_categories(&["radarr".into(), "sonarr".into()]).await.expect("none").is_empty());
    let added = c.ensure_categories(&["sonarr".into(), "Lidarr".into()]).await.expect("categories");
    assert_eq!(added, vec!["Lidarr".to_string()]);
    {
        let s = st.lock().expect("lock");
        assert_eq!(s.reloads, 1);
        let saved = s.saved.as_ref().and_then(Value::as_array).expect("saved");
        assert_eq!(saved.len(), 6);
        assert_eq!(saved[3], json!({"Name": "Category3.Name", "Value": "Lidarr"}));
        assert_eq!(saved[5], json!({"Name": "Category3.Unpack", "Value": "yes"}));
    }
    let nzb = service::nzbget::NzbgetClient::new("nzb", &url, "nzbget", "secret", "");
    let e = nzb.rpc("bad", &[]).await.expect_err("rpc error");
    assert_eq!(e.message(), "nzb: bad: Invalid procedure");

    a.password = "nope".into();
    let c = service::client(&a).await.expect("client");
    assert!(c.ping().await, "401 counts as up");
    assert_eq!(c.status().await.expect_err("401").message(), "nzb: version -> 401: ");
}

// ---- Seerr -------------------------------------------------------------------------------

#[tokio::test]
async fn seerr_setup_state() {
    let initialized = Arc::new(Mutex::new(false));
    let r = Router::new()
        .route("/api/v1/settings/public", get(|State(i): State<Arc<Mutex<bool>>>| async move {
            axum::Json(json!({"initialized": *i.lock().expect("lock"), "mediaServerType": 2}))
        }))
        .route("/api/v1/status", get(|| async {
            axum::Json(json!({"version": "2.7.3", "commitTag": "v2.7.3", "updateAvailable": false, "restartRequired": true}))
        }))
        .route("/api/v1/request/count", get(|State(i): State<Arc<Mutex<bool>>>, h: HeaderMap| async move {
            if !*i.lock().expect("lock") || h.get("x-api-key").and_then(|v| v.to_str().ok()) != Some("seerkey") {
                return (StatusCode::FORBIDDEN, axum::Json(json!({"message": "You do not have permission"}))).into_response();
            }
            axum::Json(json!({"total": 5, "pending": 1, "available": 3})).into_response()
        }))
        .route("/api/v1/settings/radarr", get(|| async { axum::Json(json!([{"id": 0, "name": "radarr"}])) })
            .post(|axum::Json(b): axum::Json<Value>| async move { axum::Json(json!({"id": 1, "name": b["name"]})) }))
        .route("/api/v1/settings/radarr/test", post(|| async { axum::Json(json!({"profiles": [{"id": 1, "name": "HD"}], "rootFolders": []})) }))
        .route("/api/v1/settings/sonarr", get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }))
        .with_state(initialized.clone());
    let url = serve(r).await;
    let mut a = app("seerr", "seerr", &url);
    a.api_key = "seerkey".into();
    let c = service::client(&a).await.expect("client");

    let st = c.status().await.expect("status");
    assert_eq!(
        (st["version"].as_str(), st["branch"].as_str(), st["databaseType"].as_str()),
        (Some("2.7.3"), Some("v2.7.3"), None)
    );
    assert_eq!(st["restartRequired"], true);
    assert_eq!(c.collection().await.expect("collection"), json!({}));
    assert_eq!(c.scope().await, json!({}));
    let h = c.health().await;
    assert_eq!(h.len(), 1);
    assert_eq!(h[0]["source"], "setup");
    let s = service::seerr::SeerrClient::new("seerr", &url, "seerkey", "");
    let e = s.get("request/count").await.expect_err("not set up");
    assert_eq!(e.message(), "seerr: Seerr isn't set up yet: open it and sign in with Plex, Jellyfin or Emby");

    *initialized.lock().expect("lock") = true;
    assert_eq!(c.collection().await.expect("collection"), json!({"requests": 5, "pending": 1, "available": 3}));
    assert_eq!(c.scope().await, json!({"media_server": "Jellyfin", "radarr": 1}));
    assert_eq!(
        c.health().await,
        vec![json!({"type": "warning", "source": "restart", "message": "Seerr asks for a restart to apply settings"})]
    );
    assert_eq!(s.servers("radarr").await.expect("servers").len(), 1);
    assert_eq!(s.test_server("radarr", &json!({})).await.expect("test")["profiles"][0]["name"], "HD");
    assert_eq!(s.save_server("radarr", &json!({"name": "radarr4k"}), None).await.expect("save")["name"], "radarr4k");
    let e = s.servers("sonarr").await.expect_err("500");
    assert_eq!(e.message(), "seerr: GET settings/sonarr -> 500: boom");

    let e = service::seerr::SeerrClient::new("seerr", &url, "wrong", "").get("request/count").await.expect_err("403");
    assert_eq!(e.message(), "seerr: GET request/count -> 403: You do not have permission");
}

// ---- config folder restore (download clients, Seerr) -----------------------------------

#[tokio::test]
async fn file_app_restores_snapshot() {
    let t = tempfile::tempdir().expect("tmp");
    let live = t.path().join("qbittorrent");
    std::fs::create_dir_all(live.join("qBittorrent")).expect("mkdir");
    std::fs::write(live.join("qBittorrent/qBittorrent.conf"), "saved").expect("w");
    let vdir = t.path().join("store/v1");
    let snap = wranglarr_core::files::snapshot_config(
        "qb",
        &service::qbittorrent::DEF.copy_spec(),
        &live,
        &vdir.join("qb"),
        None,
    )
    .expect("snapshot");
    std::fs::write(live.join("qBittorrent/qBittorrent.conf"), "changed since").expect("w");

    let mut a = app("qb", "qbittorrent", "http://127.0.0.1:9");
    a.config_dir = live.to_string_lossy().into_owned();
    let c = service::client(&a).await.expect("client");
    let r = c.restore_config(&vdir.join(&snap.path), true).await.expect("restore");
    assert_eq!(r, json!({"restartRequired": true, "method": "files"}));
    assert_eq!(std::fs::read_to_string(live.join("qBittorrent/qBittorrent.conf")).expect("r"), "saved");
    assert_eq!(
        std::fs::read_to_string(t.path().join("qbittorrent.pre-wranglarr/qBittorrent/qBittorrent.conf")).expect("r"),
        "changed since"
    );

    let e = c.restore_config(&vdir.join("nope"), true).await.expect_err("no snapshot");
    assert!(e.message().starts_with("qb: ") && !e.message().starts_with("qb: qb: "), "the name once: {}", e.message());
    a.config_dir.clear();
    let e = service::client(&a)
        .await
        .expect("client")
        .restore_config(&vdir.join(&snap.path), true)
        .await
        .expect_err("no dir");
    assert_eq!(e.message(), "qb: config_dir is required");
}

// ---- Bazarr -------------------------------------------------------------------------------

/// A Bazarr that keeps its backups as zips in `<config>/backup`, like the real one.
struct Bz {
    backup_dir: PathBuf,
    backups: Vec<String>,
    deleted: Vec<String>,
    restarts: usize,
}

type BzState = Arc<Mutex<Bz>>;

fn bz_authed(h: &HeaderMap) -> bool {
    h.get("x-api-key").and_then(|v| v.to_str().ok()) == Some("bazarrkey")
}

fn bz_data(v: Value) -> Response {
    axum::Json(json!({"data": v})).into_response()
}

async fn bz_get(
    State(st): State<BzState>,
    axum::extract::Path(path): axum::extract::Path<String>,
    h: HeaderMap,
) -> Response {
    if path == "system/ping" {
        return axum::Json(json!({"status": "OK"})).into_response();
    }
    if !bz_authed(&h) {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
    let total = |n: i64| axum::Json(json!({"data": [{}], "total": n})).into_response();
    match path.as_str() {
        "system/status" => bz_data(json!({"bazarr_version": "1.5.2", "database_engine": "PostgreSQL 17.2",
                                          "database_migration": "abc123", "operating_system": "Linux"})),
        "series" => total(42),
        "movies" => total(7),
        "episodes/wanted" => total(3),
        "movies/wanted" => total(1),
        "providers" => bz_data(json!([{"name": "opensubtitles"}, {"name": "podnapisi"}])),
        "system/languages/profiles" => bz_data(json!([{"profileId": 1}])),
        "system/settings" => axum::Json(json!({"notifications": {"providers": [
            {"name": "Discord", "enabled": true}, {"name": "Email", "enabled": false}]}}))
        .into_response(),
        "system/health" => bz_data(json!([{"object": "Sonarr", "issue": "unreachable"}])),
        "system/backups" => {
            let s = st.lock().expect("lock");
            bz_data(Value::Array(
                s.backups.iter().map(|f| json!({"filename": f, "date": "2026-10-27", "size": "1 KB"})).collect(),
            ))
        }
        _ => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// POST and DELETE: make, delete and restore backups, restart.
async fn bz_change(
    State(st): State<BzState>,
    method: axum::http::Method,
    axum::extract::Path(path): axum::extract::Path<String>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !bz_authed(&h) {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
    let mut s = st.lock().expect("lock");
    match (method.as_str(), path.as_str()) {
        ("DELETE", "system/backups") => {
            let f = q.get("filename").cloned().unwrap_or_default();
            let _ = std::fs::remove_file(s.backup_dir.join(&f));
            s.backups.retain(|b| *b != f);
            s.deleted.push(f);
        }
        ("POST", "system/backups") => {
            // config.yaml only, as with Postgres
            let name = format!("bazarr_backup_v1.5.2_2026.10.27_12.00.{:02}.zip", s.backups.len());
            let mut z = zip::ZipWriter::new(std::fs::File::create(s.backup_dir.join(&name)).expect("zip"));
            z.start_file("config.yaml", zip::write::SimpleFileOptions::default()).expect("start");
            std::io::Write::write_all(&mut z, b"from_backup: true\n").expect("write");
            z.finish().expect("finish");
            s.backups.push(name);
        }
        ("POST", "system") if q.get("action").map(String::as_str) == Some("restart") => s.restarts += 1,
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    }
    StatusCode::NO_CONTENT.into_response()
}

#[tokio::test]
async fn bazarr_backup_and_restore() {
    let t = tempfile::tempdir().expect("tmp");
    let cfg = t.path().join("bazarr");
    std::fs::create_dir_all(cfg.join("backup")).expect("mkdir");
    std::fs::create_dir_all(cfg.join("config")).expect("mkdir");
    std::fs::write(cfg.join("config/config.yaml"), "before: true\n").expect("w");
    let st: BzState =
        Arc::new(Mutex::new(Bz { backup_dir: cfg.join("backup"), backups: vec![], deleted: vec![], restarts: 0 }));
    let r = Router::new().route("/api/{*path}", get(bz_get).post(bz_change).delete(bz_change)).with_state(st.clone());
    let url = serve(r).await;

    let mut a = app("bazarr", "bazarr", &url);
    a.api_key = "bazarrkey".into();
    a.config_dir = cfg.to_string_lossy().into_owned();
    let c = service::client(&a).await.expect("client");
    assert_eq!(c.kind(), "bazarr");
    assert!(c.ping().await);
    assert!(c.wait_ready(Duration::from_secs(5)).await);
    let s = c.status().await.expect("status");
    assert_eq!((s["version"].as_str(), s["databaseType"].as_str()), (Some("1.5.2"), Some("PostgreSQL 17.2")));
    assert_eq!((s["migrationVersion"].as_str(), s["osName"].as_str()), (Some("abc123"), Some("Linux")));
    assert_eq!(
        c.collection().await.expect("collection"),
        json!({"subtitles_series": 42, "subtitles_movies": 7, "wanted_episodes": 3, "wanted_movies": 1})
    );
    assert_eq!(c.scope().await, json!({"providers": 2, "languages_profiles": 1, "notification": 1}));
    assert_eq!(c.health().await, vec![json!({"type": "warning", "message": "Sonarr: unreachable"})]);

    let dest = t.path().join("store/v1/bazarr");
    let zip = c.capture_config(&dest).await.expect("capture").expect("a zip");
    assert!(zip.starts_with(&dest) && zip.is_file());
    assert_eq!(std::fs::read_dir(cfg.join("backup")).expect("ls").count(), 0, "deleted from bazarr");
    assert_eq!(st.lock().expect("lock").deleted.len(), 1);

    let r = c.restore_config(&zip, false).await.expect("restore");
    assert_eq!(r, json!({"restartRequired": true, "method": "direct"}));
    assert_eq!(std::fs::read_to_string(cfg.join("config/config.yaml")).expect("r"), "from_backup: true\n");
    assert_eq!(std::fs::read_to_string(cfg.join("config/config.yaml.pre-wranglarr")).expect("r"), "before: true\n");
    assert_eq!(st.lock().expect("lock").restarts, 1);

    let e = c.test_resource("downloadclient", &json!({})).await.expect_err("no resources");
    assert_eq!(e.message(), "bazarr: bazarr has no 'downloadclient' resources");

    a.api_key = "wrong".into();
    let e = service::client(&a).await.expect("client").status().await.expect_err("bad key");
    assert_eq!(e.message(), "bazarr: GET system/status -> 401: Unauthorized");
}

#[tokio::test]
async fn malformed_nzbget_config_cannot_overwrite_existing_settings() {
    for response in [Value::Null, json!({}), json!([{"Name":"MainDir"}]), json!(["bad row"])] {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = calls.clone();
        let url = serve(Router::new().route(
            "/jsonrpc",
            post(move |axum::Json(req): axum::Json<Value>| {
                let log = log.clone();
                let response = response.clone();
                async move {
                    log.lock().expect("calls").push(req["method"].as_str().expect("method").into());
                    axum::Json(json!({"result": response, "error": null, "id": req["id"]}))
                }
            }),
        ))
        .await;
        let client = service::nzbget::NzbgetClient::new("nzb", &url, "", "", "");
        assert!(
            client
                .ensure_categories(&["new-category".into()])
                .await
                .expect_err("reject bad config")
                .message()
                .contains("expected an array of configuration objects")
        );
        assert_eq!(*calls.lock().expect("calls"), ["loadconfig"]);
    }
}

// ---- Weaver ------------------------------------------------------------------------

/// A Weaver with a login (admin / weaver-pass), its API keys, its categories and its
/// NZBGet API, which takes a key as the password.
#[derive(Default)]
struct Wv {
    keys: Vec<(i64, String, String)>,
    next: i64,
    logins: usize,
    logouts: usize,
    categories: Vec<String>,
}

fn wv_key(st: &Wv, h: &HeaderMap) -> bool {
    let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    st.keys.iter().any(|(_, _, k)| {
        auth == format!("Bearer {k}") || auth == format!("Basic {}", b64(&format!("{}:{k}", service::MINTED_KEY_USER)))
    })
}

/// Standard base64, padded (Basic auth).
fn b64(s: &str) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in s.as_bytes().chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= c.len() { A[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

async fn wv_graphql(State(st): State<Arc<Mutex<Wv>>>, h: HeaderMap, axum::Json(req): axum::Json<Value>) -> Response {
    let mut s = st.lock().expect("lock");
    let session = h.get("cookie").and_then(|v| v.to_str().ok()) == Some("weaver_session=sess")
        && h.get("x-weaver-csrf").and_then(|v| v.to_str().ok()) == Some("tok");
    if !session && !wv_key(&s, &h) {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "authentication required"}))).into_response();
    }
    let q = req["query"].as_str().unwrap_or("");
    let v = &req["variables"];
    let data = if q.contains("createApiKey") {
        s.next += 1;
        let (id, raw) = (s.next, format!("wvr_key{}", s.next));
        s.keys.push((id, text(&v["name"]), raw.clone()));
        json!({"createApiKey": {"key": {"id": id}, "rawKey": raw}})
    } else if q.contains("deleteApiKey") {
        let id = v["id"].as_i64().unwrap_or(-1);
        s.keys.retain(|(i, _, _)| *i != id);
        json!({"deleteApiKey": []})
    } else if q.contains("apiKeys") {
        json!({"apiKeys": s.keys.iter().map(|(id, n, _)| json!({"id": id, "name": n})).collect::<Vec<_>>()})
    } else if q.contains("addCategory") {
        s.categories.push(text(&v["input"]["name"]));
        json!({"addCategory": {"id": s.categories.len()}})
    } else if q.contains("categories") {
        json!({"categories": s.categories.iter().map(|n| json!({"name": n})).collect::<Vec<_>>()})
    } else if q.contains("version") {
        json!({"version": "0.16.15"})
    } else {
        return axum::Json(json!({"data": null, "errors": [{"message": "unknown"}]})).into_response();
    };
    axum::Json(json!({"data": data})).into_response()
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn weaver_mock(st: Arc<Mutex<Wv>>) -> Router {
    Router::new()
        .route(
            "/api/login",
            post(|State(st): State<Arc<Mutex<Wv>>>, h: HeaderMap, axum::Json(b): axum::Json<Value>| async move {
                if h.get("origin").is_none() {
                    return (StatusCode::BAD_REQUEST, "a single valid Origin is required").into_response();
                }
                if b["username"] != "admin" || b["password"] != "weaver-pass" {
                    return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "invalid credentials"})))
                        .into_response();
                }
                st.lock().expect("lock").logins += 1;
                (
                    [("set-cookie", "weaver_session=sess; HttpOnly; Path=/")],
                    axum::Json(json!({"ok": true, "csrfToken": "tok"})),
                )
                    .into_response()
            }),
        )
        .route(
            "/api/logout",
            post(|State(st): State<Arc<Mutex<Wv>>>| async move {
                st.lock().expect("lock").logouts += 1;
                StatusCode::OK
            }),
        )
        .route("/graphql", post(wv_graphql))
        .route(
            "/jsonrpc",
            post(|State(st): State<Arc<Mutex<Wv>>>, h: HeaderMap, axum::Json(req): axum::Json<Value>| async move {
                let s = st.lock().expect("lock");
                if !wv_key(&s, &h) {
                    return (StatusCode::UNAUTHORIZED, "").into_response();
                }
                let result = match req["method"].as_str().unwrap_or("") {
                    "config" => json!([{"Name": "DestDir", "Value": "/data/usenet/complete"}]),
                    "listgroups" => json!([{"NZBID": 1}]),
                    "history" => json!([{"NZBID": 2}, {"NZBID": 3}]),
                    "status" => json!({"DownloadPaused": false, "DownloadedSizeMB": 1}),
                    _ => Value::Null,
                };
                axum::Json(json!({"result": result, "error": null})).into_response()
            }),
        )
        .route("/readyz", get(|| async { "ready" }))
        .with_state(st)
}

#[tokio::test]
async fn weaver_creates_its_key_and_categories() {
    let st: Arc<Mutex<Wv>> = Arc::default();
    // someone's own key, which wranglarr leaves alone
    st.lock().expect("lock").keys.push((100, "phone".into(), "wvr_phone".into()));
    let url = serve(weaver_mock(st.clone())).await;
    let dir = tempfile::tempdir().expect("tmp");
    let key_file = dir.path().join("wranglarr-api-key.json");
    let mut a = app("weaver", "weaver", &url);
    a.username = "admin".into();
    a.password = "weaver-pass".into();
    a.api_key_file = key_file.to_string_lossy().into_owned();

    // first use: signs in, creates the key, keeps it beside Weaver's database
    let c = service::client(&a).await.expect("client");
    assert_eq!(c.status().await.expect("status")["version"], "0.16.15");
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&key_file).expect("key file")).expect("json");
    assert_eq!(kept["api_key"], "wvr_key1");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_eq!(service::api_key(&a).await.expect("key"), "wvr_key1");
    {
        let s = st.lock().expect("lock");
        assert_eq!((s.logins, s.logouts), (1, 1), "the session is closed again");
    }

    // its NZBGet API, with the key as the password
    assert_eq!(c.download_root().await.expect("root"), "/data/usenet/complete");
    assert_eq!(c.collection().await.expect("collection")["history"], 2);

    // categories through GraphQL (its NZBGet API can't save them)
    let added = c.ensure_categories(&["radarr".into(), "sonarr".into()]).await.expect("categories");
    assert_eq!(added, ["radarr", "sonarr"]);
    assert!(c.ensure_categories(&["Radarr".into()]).await.expect("again").is_empty());

    // the key was deleted in Weaver: refused once, replaced, the call goes through
    st.lock().expect("lock").keys.retain(|(_, n, _)| n != "wranglarr");
    let c = service::client(&a).await.expect("client");
    assert_eq!(c.collection().await.expect("after a new key")["queue"], 1);
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&key_file).expect("key file")).expect("json");
    assert_eq!(kept["api_key"], "wvr_key2");
    {
        let s = st.lock().expect("lock");
        let names: Vec<&str> = s.keys.iter().map(|(_, n, _)| n.as_str()).collect();
        assert_eq!(names, ["phone", "wranglarr"], "one key of its own, the others kept");
    }
}

#[tokio::test]
async fn weaver_without_its_login_says_so() {
    let st: Arc<Mutex<Wv>> = Arc::default();
    let url = serve(weaver_mock(st.clone())).await;
    let dir = tempfile::tempdir().expect("tmp");
    let mut a = app("weaver2", "weaver", &url);
    a.username = "admin".into();
    a.password = "wrong-pass".into();
    a.api_key_file = dir.path().join("k.json").to_string_lossy().into_owned();
    let e = service::api_key(&a).await.expect_err("no key without the login");
    assert!(e.to_string().contains("WEAVER_USER / WEAVER_PASS don't log in"), "{e}");
    // and it doesn't try again at once (each try is a failed login in Weaver)
    let e2 = service::api_key(&a).await.expect_err("still none");
    assert_eq!(e.to_string(), e2.to_string());
    assert_eq!(st.lock().expect("lock").logins, 0);
}
