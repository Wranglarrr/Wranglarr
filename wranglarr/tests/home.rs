// SPDX-License-Identifier: GPL-3.0-or-later
//! The home page and its API (links, home settings, feeds, app logins, logos) and the
//! reverse proxy API, in-process against mock apps and a mock Docker socket.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{TimeDelta, Utc};
use futures::StreamExt;
use serde_json::{Value, json};
use wranglarr::app::App;

// ---- helpers ------------------------------------------------------------------------------

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await;
    });
    format!("http://{addr}")
}

/// wranglarr itself, on a free port, with this config (apps as TOML [[suite.app]] blocks).
async fn wranglarr(dir: &Path, apps: &str, docker: &str) -> (App, String) {
    let toml = format!(
        "[storage]\nroot = \"{d}/store\"\nstate_db = \"{d}/state/wranglarr.db\"\n[docker]\nsocket = \"{docker}\"\n\
         [[suite]]\nname = \"media\"\n{apps}",
        d = dir.display()
    );
    let cfg = wranglarr_services::apps::parse(&toml, |_| None).expect("config");
    let login_off = wranglarr::auth::Options { enabled: false, ..Default::default() };
    let app =
        App::new(cfg, wranglarr::deployment::Deployment { auth: login_off, ..Default::default() }).await.expect("app");
    let url = serve(wranglarr::web::router(app.clone())).await;
    (app, url)
}

fn app_toml(name: &str, kind: &str, url: &str, extra: &str) -> String {
    format!("[[suite.app]]\nname = \"{name}\"\nkind = \"{kind}\"\nurl = \"{url}\"\napi_key = \"key\"\n{extra}\n")
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("client")
}

async fn get_json(url: &str) -> (u16, Value) {
    let r = http().get(url).header("X-Forwarded-Host", "192.168.1.10:8787").send().await.expect("get");
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

async fn send_json(method: reqwest::Method, url: &str, body: &Value) -> (u16, Value) {
    let r = http()
        .request(method, url)
        .header("X-Forwarded-Host", "192.168.1.10:8787")
        .json(body)
        .send()
        .await
        .expect("send");
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

fn json_resp(v: Value) -> Response {
    axum::Json(v).into_response()
}

// ---- mock Docker -----------------------------------------------------------------------------

async fn mock_docker(dir: &Path, containers: HashMap<String, Value>) -> String {
    let sock = dir.join("docker.sock");
    let listener = tokio::net::UnixListener::bind(&sock).expect("bind socket");
    let containers = Arc::new(containers);
    let router = Router::new()
        .route("/_ping", get(|| async { "OK" }))
        .route(
            "/containers/{name}/json",
            get(|State(c): State<Arc<HashMap<String, Value>>>, AxPath(name): AxPath<String>| async move {
                match c.get(&name) {
                    Some(v) => json_resp(v.clone()),
                    None => {
                        (StatusCode::NOT_FOUND, axum::Json(json!({"message": "No such container"}))).into_response()
                    }
                }
            }),
        )
        .with_state(containers);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    sock.display().to_string()
}

fn ports(inner: u16, host_port: &str, ip: &str) -> Value {
    json!({"HostConfig": {"NetworkMode": "bridge"},
           "NetworkSettings": {"Ports": {format!("{inner}/tcp"): [{"HostIp": ip, "HostPort": host_port}]}}})
}

// ---- links ---------------------------------------------------------------------------------

#[tokio::test]
async fn links_from_docker_ports() {
    let dir = tempfile::tempdir().expect("tmp");
    let containers = HashMap::from([
        ("radarr".to_string(), ports(7878, "7878", "0.0.0.0")),
        // shares the VPN's network: the VPN publishes its port
        ("sonarr".to_string(), json!({"HostConfig": {"NetworkMode": "container:gluetun"}})),
        ("gluetun".to_string(), ports(8989, "18989", "127.0.0.1")),
        // AuthTunnel: the gate publishes qBittorrent's port
        ("qbit".to_string(), json!({"HostConfig": {}, "NetworkSettings": {"Ports": {}}})),
        ("wranglarr-gate".to_string(), ports(8090, "8090", "")),
        ("bazarr".to_string(), json!({"HostConfig": {}, "NetworkSettings": {"Ports": {}}})),
    ]);
    let sock = mock_docker(dir.path(), containers).await;
    let apps = [
        app_toml("radarr", "radarr", "http://radarr:7878", ""),
        app_toml("sonarr", "sonarr", "http://sonarr:8989/sonarr/", ""),
        app_toml("qbittorrent", "qbittorrent", "http://qbittorrent:8090", "container = \"qbit\""),
        app_toml("bazarr", "bazarr", "http://bazarr:6767", ""),
        app_toml("sabnzbd", "sabnzbd", "http://sabnzbd:8080", ""),
    ]
    .concat();
    let (_app, url) = wranglarr(dir.path(), &apps, &sock).await;

    let (code, j) = get_json(&format!("{url}/api/links")).await;
    assert_eq!(code, 200, "{j}");
    assert_eq!(j["settings"], json!({"mode": "port", "scheme": "http", "host": "", "pattern": "", "apps": {}}));
    let l = &j["links"];
    assert_eq!(l["radarr"], json!({"url": "http://192.168.1.10:7878/", "how": "port", "note": "published port"}));
    assert_eq!(l["sonarr"]["url"], "http://192.168.1.10:18989/sonarr/");
    assert_eq!(l["sonarr"]["note"], "published on 127.0.0.1 only: opens on the host itself, not from other machines");
    assert_eq!(
        l["qbittorrent"],
        json!({"url": "http://192.168.1.10:8090/", "how": "port", "note": "through wranglarr-gate"})
    );
    assert_eq!(
        l["bazarr"],
        json!({"url": null, "how": "none", "note": "not published on the host: set an address for it"})
    );
    assert_eq!(l["sabnzbd"]["url"], "http://192.168.1.10:8080/", "no such container: the app's own port");
    assert!(l["sabnzbd"]["note"].as_str().unwrap_or("").starts_with("guessed: Docker couldn't find the container ("));

    // bad values: 400 with the reason
    for (body, msg) in [
        (json!({"mode": "sideways"}), "mode: port or domain"),
        (json!({"scheme": "ftp"}), "scheme: http or https"),
        (json!({"apps": {"nope": "http://x"}}), r#"no app called "nope""#),
        (
            json!({"apps": {"radarr": "radarr.example.com"}}),
            r#"radarr: "radarr.example.com" isn't an http:// or https:// address"#,
        ),
        (
            json!({"mode": "domain"}),
            "domain pattern: needed for 'a domain per app', e.g. https://{name}.home.example.com",
        ),
        (json!({"host": "http://x"}), r#"host: "http://x" isn't a host name or IP address (no scheme or port here)"#),
    ] {
        let (code, j) = send_json(reqwest::Method::PUT, &format!("{url}/api/links"), &body).await;
        assert_eq!((code, j["error"].as_str()), (400, Some(msg)), "{body}");
    }
    let (code, j) = send_json(reqwest::Method::PUT, &format!("{url}/api/links"), &json!({"mode": 3})).await;
    assert_eq!(code, 422, "a body of the wrong shape: {j}");

    // a fixed host, https, one set by hand
    let body =
        json!({"scheme": "https", "host": " nas.lan ", "apps": {"bazarr": "https://subs.example.com", "radarr": ""}});
    let (code, j) = send_json(reqwest::Method::PUT, &format!("{url}/api/links"), &body).await;
    assert_eq!(code, 200, "{j}");
    assert_eq!(j["settings"]["host"], "nas.lan");
    assert_eq!(j["settings"]["apps"], json!({"bazarr": "https://subs.example.com"}), "blank: use the rule");
    assert_eq!(j["links"]["radarr"]["url"], "https://nas.lan:7878/");
    assert_eq!(
        j["links"]["bazarr"],
        json!({"url": "https://subs.example.com", "how": "set", "note": "set on this page"})
    );

    // a domain per app
    let body = json!({"mode": "domain", "pattern": "https://{name}.home.example.com"});
    let (_, j) = send_json(reqwest::Method::PUT, &format!("{url}/api/links"), &body).await;
    assert_eq!(
        j["links"]["sonarr"],
        json!({"url": "https://sonarr.home.example.com", "how": "domain",
                                             "note": "from the domain pattern"})
    );

    let (code, j) = send_json(reqwest::Method::DELETE, &format!("{url}/api/links"), &Value::Null).await;
    assert_eq!((code, j["settings"]["mode"].as_str()), (200, Some("port")));
}

#[tokio::test]
async fn links_without_docker_guess_the_port() {
    let dir = tempfile::tempdir().expect("tmp");
    let apps = app_toml("radarr", "radarr", "https://radarr", "");
    let (_app, url) = wranglarr(dir.path(), &apps, "").await;
    let (_, j) = get_json(&format!("{url}/api/links")).await;
    assert_eq!(
        j["links"]["radarr"],
        json!({"url": "http://192.168.1.10:443/", "how": "port",
                                            "note": "guessed: the app's own port (no Docker socket to ask)"})
    );
}

// ---- home settings and feeds ------------------------------------------------------------------

fn iso(d: chrono::DateTime<Utc>) -> String {
    d.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Sonarr, Radarr, Bazarr, Prowlarr and Seerr in one server, told apart by path prefix.
async fn mock_feeds() -> String {
    async fn handle(
        AxPath(path): AxPath<String>,
        headers: HeaderMap,
        Query(q): Query<HashMap<String, String>>,
    ) -> Response {
        let key = headers.get("x-api-key").and_then(|v| v.to_str().ok()).unwrap_or("");
        if key != "key" && !path.ends_with("settings/public") {
            return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "Unauthorized"}))).into_response();
        }
        let now = Utc::now();
        let v = match path.as_str() {
            "sonarr/api/v3/calendar" => {
                assert_eq!(q.get("includeSeries").map(String::as_str), Some("true"));
                json!([{"series": {"title": "X", "titleSlug": "x"}, "seasonNumber": 2, "episodeNumber": 3, "title": "T",
                        "airDateUtc": iso(now + TimeDelta::days(1)), "hasFile": true},
                       {"series": {"title": "A"}, "seasonNumber": 1, "episodeNumber": 1, "airDateUtc": iso(now)}])
            }
            "sonarr/api/v3/wanted/missing" => {
                assert_eq!(q.get("sortKey").map(String::as_str), Some("episodes.airDateUtc"));
                let recs: Vec<Value> = (1..8)
                    .map(|n| json!({"series": {"title": "S", "titleSlug": "s"}, "seasonNumber": 1, "episodeNumber": n,
                                    "title": format!("e{n}"), "airDateUtc": iso(now - TimeDelta::days(n * 10) - TimeDelta::hours(1))}))
                    .collect();
                json!({"records": recs, "totalRecords": 99})
            }
            "radarr/api/v3/calendar" => {
                json!([{"title": "Film", "year": 2026, "inCinemas": iso(now + TimeDelta::days(2)),
                                                "digitalRelease": iso(now + TimeDelta::days(60)),
                                                "physicalRelease": iso(now + TimeDelta::days(3)), "titleSlug": "film"}])
            }
            "radarr/api/v3/wanted/missing" => json!({"records": [
                {"title": "Old", "year": 2001, "isAvailable": true, "releaseDate": "2001-01-01T00:00:00Z"},
                {"title": "New", "year": 2026, "isAvailable": true, "releaseDate": iso(now - TimeDelta::days(3))},
                {"title": "Soon", "year": 2027, "isAvailable": false, "releaseDate": iso(now + TimeDelta::days(90))}]}),
            "bazarr/api/episodes/wanted" => {
                assert_eq!(q.get("length").map(String::as_str), Some("5"));
                json!({"data": [{"seriesTitle": "Show", "episode_number": "1x02", "episodeTitle": "Two", "sonarrSeriesId": 4,
                                 "missing_subtitles": [{"name": "English", "code2": "en"}, {"name": "French"}]}], "total": 3})
            }
            "bazarr/api/movies/wanted" => {
                json!({"data": [{"title": "Film", "radarrId": 9, "missing_subtitles": [{"code2": "de"}]}],
                                                 "total": 1})
            }
            "prowlarr/api/v1/indexer" => json!([{"id": 1, "name": "B ok", "enable": true, "protocol": "torrent"},
                                                {"id": 2, "name": "A off", "enable": false},
                                                {"id": 3, "name": "Z bad", "enable": true, "protocol": "usenet", "privacy": "private"}]),
            "prowlarr/api/v1/indexerstatus" => json!([{"indexerId": 3, "disabledTill": iso(now + TimeDelta::hours(1)),
                                                       "mostRecentFailure": iso(now)}]),
            "seerr/api/v1/settings/public" => json!({"initialized": true}),
            "seerr/api/v1/request" => json!({"results": [
                {"status": 1, "type": "movie", "media": {"tmdbId": 11, "status": 2}, "requestedBy": {"displayName": "ann"},
                 "createdAt": "2026-01-02T00:00:00Z"},
                {"status": 2, "type": "tv", "media": {"tmdbId": 22, "status": 5}, "seasons": [{"seasonNumber": 1}, {"seasonNumber": 2}],
                 "is4k": true, "createdAt": "2026-01-01T00:00:00Z"},
                {"status": 3, "media": {"mediaType": "movie"}}], "pageInfo": {"results": 7}}),
            "seerr/api/v1/movie/11" => json!({"title": "Eleven", "releaseDate": "2011-05-05"}),
            "seerr/api/v1/tv/22" => json!({"name": "Twenty-two", "firstAirDate": "2022-02-02"}),
            _ => return (StatusCode::NOT_FOUND, axum::Json(json!({"error": format!("nope {path}")}))).into_response(),
        };
        json_resp(v)
    }
    serve(Router::new().route("/{*path}", get(handle))).await
}

#[tokio::test]
async fn home_settings_and_feeds() {
    let dir = tempfile::tempdir().expect("tmp");
    let m = mock_feeds().await;
    let apps = [
        app_toml("sonarr", "sonarr", &format!("{m}/sonarr"), ""),
        app_toml("radarr", "radarr", &format!("{m}/radarr"), ""),
        app_toml("bazarr", "bazarr", &format!("{m}/bazarr"), ""),
        app_toml("prowlarr", "prowlarr", &format!("{m}/prowlarr"), ""),
        app_toml("seerr", "seerr", &format!("{m}/seerr"), ""),
        app_toml("lidarr", "lidarr", "http://127.0.0.1:9", ""),
        app_toml("qb", "qbittorrent", "http://127.0.0.1:9", ""),
    ]
    .concat();
    let (_app, url) = wranglarr(dir.path(), &apps, "").await;
    let settings = format!("{url}/api/home/settings");

    let (code, d) = get_json(&settings).await;
    assert_eq!(code, 200);
    assert_eq!((d["days"].as_i64(), d["limit"].as_i64(), d["details"].as_str()), (Some(7), Some(25), Some("hover")));
    let body = json!({"days": "500", "missing_days": "-3", "limit": "1", "details": "always", "queue": false,
                      "release_days": "9999", "release_next": "0"});
    let (code, s) = send_json(reqwest::Method::PUT, &settings, &body).await;
    assert_eq!(code, 200, "{s}");
    assert_eq!(
        (s["release_days"].as_i64(), s["release_next"].as_i64(), s["days"].as_i64()),
        (Some(365), Some(1), Some(90))
    );
    assert_eq!(
        (s["missing_days"].as_i64(), s["limit"].as_i64(), s["queue"].as_bool()),
        (Some(0), Some(3), Some(false))
    );
    assert_eq!(get_json(&settings).await.1, s, "saved");
    let (code, e) = send_json(reqwest::Method::PUT, &settings, &json!({"details": "sideways"})).await;
    assert_eq!((code, e["error"].as_str()), (400, Some("details: hover or always")));
    let (code, e) = send_json(reqwest::Method::PUT, &settings, &json!({"days": "lots"})).await;
    assert_eq!((code, e["error"].as_str()), (400, Some("the days and items per list must be numbers")));
    let (code, e) = send_json(reqwest::Method::PUT, &settings, &json!([1])).await;
    assert_eq!(code, 422, "not an object: {e}");
    let (_, d) = send_json(reqwest::Method::DELETE, &settings, &Value::Null).await;
    assert_eq!((d["days"].as_i64(), d["limit"].as_i64(), d["details"].as_str()), (Some(7), Some(25), Some("hover")));

    let (code, _) = send_json(reqwest::Method::PUT, &settings, &json!({"limit": 5, "missing_days": 30})).await;
    assert_eq!(code, 200);
    let (code, j) = get_json(&format!("{url}/api/home/feeds?refresh=true")).await;
    assert_eq!(code, 200, "{j}");
    assert_eq!(j["settings"]["limit"], 5);
    let list = |app: &str, key: &str| -> Value {
        j["apps"][app]["lists"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|l| l["key"] == key)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert!(j["apps"].get("qb").is_none(), "download clients have no lists here");

    let m = list("sonarr", "missing");
    assert_eq!((m["total"].as_i64(), m["all"].as_i64()), (Some(2), Some(99)), "{m}");
    let subs: Vec<&str> = m["items"].as_array().into_iter().flatten().filter_map(|i| i["sub"].as_str()).collect();
    assert_eq!(subs, ["S01E01 · e1", "S01E02 · e2"]);
    let u = list("sonarr", "upcoming");
    assert_eq!(u["note"], "next 7 days");
    assert_eq!(u["items"][0]["title"], "A", "soonest first");
    assert_eq!(
        u["items"][1],
        json!({"title": "X", "sub": "S02E03 · T", "when": u["items"][1]["when"], "tag": "downloaded",
                                     "state": "", "path": "/series/x", "day": false})
    );

    let m = list("radarr", "missing");
    assert_eq!(
        (m["total"].as_i64(), m["all"].as_i64(), m["items"][0]["title"].as_str()),
        (Some(1), Some(2), Some("New (2026)"))
    );
    let u = list("radarr", "upcoming");
    let subs: Vec<&str> = u["items"].as_array().into_iter().flatten().filter_map(|i| i["sub"].as_str()).collect();
    assert_eq!(subs, ["in cinemas", "on disc", "digital release"], "{u}");
    assert_eq!(
        (u["days"].as_i64(), u["next"].as_i64(), u["note"].as_str()),
        (Some(90), Some(5), Some("next 5 · 90 days"))
    );

    let b = list("bazarr", "missing");
    assert_eq!((b["title"].as_str(), b["total"].as_i64()), (Some("Wanted subtitles"), Some(4)));
    assert_eq!(
        b["items"][0],
        json!({"title": "Show", "sub": "1x02 · Two", "when": "", "tag": "English, French", "state": "",
                                     "path": "/series/4", "day": false})
    );
    assert_eq!(
        (b["items"][1]["sub"].as_str(), b["items"][1]["tag"].as_str(), b["items"][1]["path"].as_str()),
        (Some("movie"), Some("de"), Some("/movies/9"))
    );

    let p = list("prowlarr", "indexers");
    let names: Vec<&str> = p["items"].as_array().into_iter().flatten().filter_map(|i| i["title"].as_str()).collect();
    assert_eq!(names, ["Z bad", "B ok", "A off"]);
    assert_eq!(
        (p["note"].as_str(), p["items"][0]["tag"].as_str(), p["items"][0]["sub"].as_str()),
        (Some("1 failing"), Some("failing"), Some("usenet · private"))
    );
    assert_eq!(p["items"][2]["state"], "muted");

    let r = list("seerr", "requests");
    assert_eq!((r["note"].as_str(), r["total"].as_i64()), (Some("1 waiting for approval"), Some(7)), "{r}");
    assert_eq!(
        r["items"][0],
        json!({"title": "Eleven (2011)", "sub": "ann", "when": "2026-01-02T00:00:00Z",
                                     "tag": "pending approval", "state": "warn", "path": "/movie/11", "day": false})
    );
    assert_eq!(
        (r["items"][1]["title"].as_str(), r["items"][1]["sub"].as_str(), r["items"][1]["tag"].as_str()),
        (Some("Twenty-two (2022)"), Some("season 1, 2 · 4K"), Some("available"))
    );
    assert_eq!(
        (r["items"][2]["title"].as_str(), r["items"][2]["state"].as_str(), r["items"][2]["path"].as_str()),
        (Some("unknown title"), Some("err"), Some(""))
    );

    // an app that doesn't answer: each list says why
    let l = &j["apps"]["lidarr"]["lists"];
    assert_eq!(l.as_array().map(Vec::len), Some(2));
    assert_eq!((l[0]["key"].as_str(), l[0]["total"].as_i64()), (Some("upcoming"), Some(0)));
    assert!(l[0]["error"].as_str().is_some_and(|e| e.starts_with("lidarr: GET")), "{l}");

    // lists switched off
    let off = json!({"upcoming": false, "missing": false, "indexers": false, "requests": false});
    send_json(reqwest::Method::PUT, &settings, &off).await;
    let (_, j) = get_json(&format!("{url}/api/home/feeds")).await;
    for (app, v) in j["apps"].as_object().into_iter().flatten() {
        assert_eq!(v["lists"], json!([]), "{app}");
    }
    let (code, _) = get_json(&format!("{url}/api/home/feeds?refresh=maybe")).await;
    assert_eq!(code, 422);
}

// ---- logos ------------------------------------------------------------------------------------

#[tokio::test]
async fn logos_pass_through_or_fall_back() {
    let dir = tempfile::tempdir().expect("tmp");
    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
    let m = serve(
        Router::new()
            .route(
                "/Content/Images/logo.svg",
                get(move || async move { ([("content-type", "image/svg+xml; charset=utf-8")], svg) }),
            )
            // NZBGet: the logo needs the login
            .route(
                "/img/favicon-256x256.png",
                get(|h: HeaderMap| async move {
                    if h.get("authorization").is_some() {
                        ([("content-type", "image/png")], "PNG").into_response()
                    } else {
                        StatusCode::UNAUTHORIZED.into_response()
                    }
                }),
            )
            .route("/images/logo128.png", get(|| async { ([("content-type", "text/html")], "<html>") })),
    )
    .await;
    let apps = [
        app_toml("radarr", "radarr", &m, ""),
        app_toml("nzbget", "nzbget", &format!("{m}/"), "username = \"u\"\npassword = \"p\""),
        app_toml("bazarr", "bazarr", &m, ""),
        app_toml("sab", "sabnzbd", "http://127.0.0.1:9", ""),
    ]
    .concat();
    let (_app, url) = wranglarr(dir.path(), &apps, "").await;
    let csp = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; sandbox";
    for (name, ctype, body) in [
        ("radarr", "image/svg+xml", Some(svg)),
        ("nzbget", "image/png", Some("PNG")),
        ("bazarr", "image/svg+xml", None), // not an image: the shipped icon
        ("sab", "image/svg+xml", None),    // down: the shipped icon
    ] {
        let r = http().get(format!("{url}/api/apps/{name}/logo")).send().await.expect("logo");
        assert_eq!(r.status(), 200, "{name}");
        let h = r.headers().clone();
        assert_eq!(h["content-type"], ctype, "{name}");
        assert_eq!(h["cache-control"], "private, max-age=86400");
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["content-security-policy"], csp);
        let text = r.text().await.expect("body");
        match body {
            Some(b) => assert_eq!(text, b, "{name}"),
            None => assert!(text.contains("<svg"), "{name}: the shipped icon"),
        }
    }
    let (code, j) = get_json(&format!("{url}/api/apps/nope/logo")).await;
    assert_eq!((code, j["error"].as_str()), (404, Some(r#"no app "nope""#)));
}

/// An app that answers its logo with something huge isn't read to the end (and
/// not into memory): reading stops at the size limit. Requests that come in
/// while a logo is being fetched wait for that fetch instead of starting more.
#[tokio::test]
async fn huge_logos_are_cut_short_and_fetched_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const CHUNK: usize = 64 * 1024;
    const TOTAL: usize = 64 * 1024 * 1024;
    let sent = Arc::new(AtomicUsize::new(0));
    let asked = Arc::new(AtomicUsize::new(0));
    let (s, a) = (sent.clone(), asked.clone());
    let m = serve(
        Router::new()
            // radarr: 64 MB with no length up front
            .route(
                "/radarr/Content/Images/logo.svg",
                get(move || {
                    let s = s.clone();
                    async move {
                        let chunks = futures::stream::iter(0..TOTAL / CHUNK).map(move |_| {
                            s.fetch_add(CHUNK, Ordering::SeqCst);
                            Ok::<_, std::io::Error>(bytes::Bytes::from(vec![b' '; CHUNK]))
                        });
                        ([("content-type", "image/svg+xml")], axum::body::Body::from_stream(chunks))
                    }
                }),
            )
            // sonarr: says up front it's too big
            .route(
                "/sonarr/Content/Images/logo.svg",
                get(|| async { ([("content-type", "image/svg+xml")], vec![b' '; 600_000]) }),
            )
            .route(
                "/sonarr/Content/Images/Icons/android-chrome-192x192.png",
                get(move || {
                    let a = a.clone();
                    async move {
                        a.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        ([("content-type", "image/png")], "PNG")
                    }
                }),
            ),
    )
    .await;
    let apps = [
        app_toml("radarr", "radarr", &format!("{m}/radarr"), ""),
        app_toml("sonarr", "sonarr", &format!("{m}/sonarr"), ""),
    ]
    .concat();
    let dir = tempfile::tempdir().expect("tmp");
    let (_app, url) = wranglarr(dir.path(), &apps, "").await;

    let r = http().get(format!("{url}/api/apps/radarr/logo")).send().await.expect("logo");
    assert_eq!(r.status(), 200);
    assert!(r.text().await.expect("body").contains("<svg"), "the shipped icon");
    let n = sent.load(Ordering::SeqCst);
    assert!(n < TOTAL / 4, "read {n} of {TOTAL} bytes");

    // five at once: one fetch; the too-big svg is passed over for the png
    let gets = (0..5).map(|_| {
        let u = format!("{url}/api/apps/sonarr/logo");
        async move { http().get(u).send().await.expect("logo").text().await.expect("body") }
    });
    let bodies = futures::future::join_all(gets).await;
    assert!(bodies.iter().all(|b| b == "PNG"), "{bodies:?}");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

// ---- app logins -------------------------------------------------------------------------------

#[tokio::test]
async fn app_logins_read_each_app() {
    let dir = tempfile::tempdir().expect("tmp");
    let m = serve(
        Router::new()
            .route("/radarr/api/v3/config/host", get(|| async {
                json_resp(json!({"authenticationMethod": "Forms", "authenticationRequired": "enabled", "username": "bob"}))
            }))
            .route("/sonarr/api/v3/config/host", get(|| async { json_resp(json!({"authenticationMethod": null})) }))
            .route("/bazarr/api/system/settings", get(|| async { json_resp(json!({"auth": {"type": "None"}})) }))
            .route("/sab/api", get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q.get("section").map(String::as_str), Some("misc"));
                json_resp(json!({"config": {"misc": {"username": "sab"}}}))
            }))
            .route("/qb/api/v2/auth/login", post(|| async { ([("set-cookie", "SID=abc; HttpOnly; path=/")], "Ok.") }))
            .route("/qb/api/v2/app/preferences", get(|h: HeaderMap| async move {
                assert_eq!(h.get("cookie").and_then(|v| v.to_str().ok()), Some("SID=abc"));
                json_resp(json!({"web_ui_username": "admin", "bypass_auth_subnet_whitelist_enabled": true}))
            }))
            .route("/nzb/jsonrpc/version", get(|| async { StatusCode::UNAUTHORIZED })),
    )
    .await;
    let apps = [
        app_toml("radarr", "radarr", &format!("{m}/radarr"), ""),
        app_toml("sonarr", "sonarr", &format!("{m}/sonarr"), ""),
        app_toml("bazarr", "bazarr", &format!("{m}/bazarr"), ""),
        app_toml("sab", "sabnzbd", &format!("{m}/sab"), ""),
        app_toml("qb", "qbittorrent", &format!("{m}/qb"), "username = \"admin\"\npassword = \"x\""),
        app_toml("nzb", "nzbget", &format!("{m}/nzb"), "username = \"n\"\npassword = \"x\""),
        app_toml("prowlarr", "prowlarr", "http://127.0.0.1:9", ""),
        app_toml("seerr", "seerr", "http://127.0.0.1:9", ""),
    ]
    .concat();
    let (_app, url) = wranglarr(dir.path(), &apps, "").await;
    std::fs::write(dir.path().join("state/apps-auth.json"), r#"{"mode": "zzz", "at": "then", "apps": {}}"#)
        .expect("report");
    let (code, j) = get_json(&format!("{url}/api/app-logins")).await;
    assert_eq!(code, 200, "{j}");
    let row =
        |n: &str| j["apps"].as_array().into_iter().flatten().find(|r| r["app"] == n).cloned().unwrap_or(Value::Null);
    let r = row("radarr");
    assert_eq!(
        (r["method"].as_str(), r["username"].as_str(), r["login"].as_bool()),
        (Some("forms"), Some("bob"), Some(true))
    );
    assert_eq!(r["required"], "enabled");
    let r = row("sonarr");
    assert_eq!((r["method"].as_str(), r["login"].as_bool()), (Some("none"), Some(false)));
    assert_eq!(row("bazarr")["method"], "none");
    assert_eq!((row("sab")["method"].as_str(), row("sab")["username"].as_str()), (Some("form"), Some("sab")));
    let r = row("qb");
    assert_eq!(
        (r["method"].as_str(), r["username"].as_str(), r["bypass"].as_bool()),
        (Some("form"), Some("admin"), Some(true))
    );
    let r = row("nzb");
    assert_eq!(
        (r["method"].as_str(), r["error"].as_str()),
        (Some("basic"), Some("NZBGET_USER / NZBGET_PASS from .env don't log in"))
    );
    let r = row("prowlarr");
    assert_eq!((r["method"].as_str(), r["login"].as_bool(), r["warn"].as_str()), (Some("?"), Some(false), Some("")));
    assert!(r["error"].as_str().is_some_and(|e| !e.is_empty()));
    assert_eq!(
        row("seerr"),
        json!({"app": "seerr", "kind": "seerr", "method": "own", "username": "media server accounts",
                                    "login": true, "gate": false, "warn": ""})
    );
    assert_eq!(j["last_run"], json!({"mode": "zzz", "at": "then", "username": null}));
    assert_eq!(j["rerun"][1], "./scripts/init.sh --auth");
    if j["mode"] == "manual" {
        assert_eq!(row("sonarr")["warn"], "no login: anyone who reaches it can use it");
        assert_eq!((j["required"].as_str(), j["cookie_domain"].as_str()), (Some(""), Some("")));
    }
}

// ---- pages and the proxy API ----------------------------------------------------------------------

#[tokio::test]
async fn home_and_proxy_pages_render() {
    let dir = tempfile::tempdir().expect("tmp");
    let apps = [
        app_toml("radarr", "radarr", "http://radarr:7878", ""),
        app_toml("qb", "qbittorrent", "http://qbittorrent:8090", ""),
        app_toml("prowlarr", "prowlarr", "http://prowlarr:9696", ""),
    ]
    .concat();
    let (_app, url) = wranglarr(dir.path(), &apps, "").await;
    let r = http().get(format!("{url}/")).header("Host", "nas.lan:8787").send().await.expect("home");
    assert_eq!(r.status(), 200);
    // minijinja escapes / as &#x2f; (Jinja2 leaves it): the same to a browser
    let html = r.text().await.expect("html").replace("&#x2f;", "/");
    assert!(html.contains("data-app=\"radarr\"") && html.contains("data-group=\"downloads\""), "{html}");
    assert!(html.contains("href=\"http://nas.lan:7878/\""), "the poster links to the app");
    assert!(html.contains("<span class=\"card-link\">nas.lan:9696</span>"), "the address, without the scheme");
    assert!(html.contains("placeholder=\"nas.lan\""), "the browser's host");

    let r = http().get(format!("{url}/proxy")).send().await.expect("proxy page");
    assert_eq!(r.status(), 200);
    let html = r.text().await.expect("html");
    let at = |s: &str| html.find(s).unwrap_or(usize::MAX);
    assert!(
        at("value=\"caddy\"") < at("value=\"nginx\"") && at("value=\"cloudflared\"") < at("value=\"npm\""),
        "the proxies in their order"
    );
    assert!(at("value=\"npm\"") < usize::MAX);
}

#[tokio::test]
async fn proxy_api() {
    let dir = tempfile::tempdir().expect("tmp");
    let apps = [
        app_toml("radarr", "radarr", "http://radarr:7878", ""),
        app_toml("sabnzbd", "sabnzbd", "http://sabnzbd:8080", ""),
        app_toml("qbittorrent", "qbittorrent", "http://qbittorrent:8090", ""),
    ]
    .concat();
    let (_app, url) = wranglarr(dir.path(), &apps, "").await;
    let api = format!("{url}/api/proxy");
    let (code, j) = get_json(&api).await;
    assert_eq!(code, 200);
    assert_eq!((j["settings"]["kind"].as_str(), j["filename"].as_str()), (Some("caddy"), Some("Caddyfile")));
    assert_eq!(j["plan"].as_array().map(Vec::len), Some(4), "wranglarr and the apps");
    let exposes = json!({"radarr": {"expose": "public"}, "sabnzbd": {"expose": "lan"},
                         "qbittorrent": {"expose": "lan", "sub": "qb"}, "wranglarr": {"expose": "public"}});
    for (kind, _) in wranglarr::proxy::KINDS {
        for style in ["subdomain", "path"] {
            let docker = kind.contains("labels");
            let body = json!({"kind": kind, "style": style, "domain": "example.com",
                              "upstream": if docker { "docker" } else { "host" }, "upstream_host": "192.168.1.10",
                              "apps": exposes});
            let (code, j) = send_json(reqwest::Method::PUT, &api, &body).await;
            assert_eq!(code, 200, "{j}");
            assert_eq!(j["settings"]["apps"]["qbittorrent"], json!({"expose": "lan", "sub": "qb"}));
            let text = j["text"].as_str().unwrap_or("");
            assert!(text.contains("wranglarr.example.com") && text.contains("radarr"), "{kind} {style}");
            if *kind == "cloudflared" {
                assert!(!text.contains("qb.example.com") && !text.contains("sabnzbd"), "{text}");
            } else {
                assert!(
                    text.contains(if style == "subdomain" { "qb.example.com" } else { "/qb" }),
                    "{kind} {style}: {text}"
                );
            }
            if !docker {
                assert!(text.contains("192.168.1.10"), "{kind}");
            }
            assert_eq!(get_json(&api).await.1["settings"]["kind"], *kind, "saved");
        }
    }
    for body in [
        json!({"kind": "apache"}),
        json!({"style": "sideways"}),
        json!({"domain": "https://example.com/x"}),
        json!({"upstream": "moon"}),
        json!({"lan": "not-a-range"}),
        json!({"apps": {"radarr": {"expose": "everyone"}}}),
    ] {
        let (code, j) = send_json(reqwest::Method::PUT, &api, &body).await;
        assert_eq!(code, 400, "{body}: {j}");
        assert!(j["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
}
