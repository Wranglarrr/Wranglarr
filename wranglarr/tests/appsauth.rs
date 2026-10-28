// SPDX-License-Identifier: GPL-3.0-or-later
//! `wranglarr apps-auth` against mock apps: the binary runs as a child process
//! with the environment init.sh gives it, and what it prints, writes and changes
//! is compared with the recorded expected output (fixtures/appsauth.json).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Form, Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use serde_json::{Value, json};

#[derive(Default)]
struct Apps {
    host: Value,
    dl: Vec<Value>,
    bazarr_auth: Value,
    sab_user: String,
    sab_pass: String,
    qb_user: String,
    qb_pass: String,
    qb_whitelist: bool,
    nzb_auth: String,
    calls: Vec<String>,
}

type S = Arc<Mutex<Apps>>;

fn lock(s: &S) -> std::sync::MutexGuard<'_, Apps> {
    s.lock().expect("lock")
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

fn arr_router(s: S) -> Router {
    Router::new()
        .route("/ping", get(|| async { "Pong" }))
        .route("/api/v3/config/host", get(|State(s): State<S>| async move { axum::Json(lock(&s).host.clone()) }))
        .route(
            "/api/v3/config/host/{id}",
            put(|State(s): State<S>, axum::Json(b): axum::Json<Value>| async move {
                lock(&s).host = b.clone();
                (StatusCode::ACCEPTED, axum::Json(b))
            }),
        )
        .route(
            "/login",
            post(|State(s): State<S>, Form(f): Form<HashMap<String, String>>| async move {
                let h = lock(&s).host.clone();
                let ok = f.get("username").map(String::as_str) == h["username"].as_str()
                    && f.get("password").map(String::as_str) == h["password"].as_str();
                let to = if ok { "/" } else { "/login?returnUrl=%2F&loginFailed=true" };
                (StatusCode::FOUND, [(header::LOCATION, to)])
            }),
        )
        .route("/api/v3/downloadclient", get(|State(s): State<S>| async move { axum::Json(json!(lock(&s).dl)) }))
        .route(
            "/api/v3/downloadclient/test",
            post(|| async {
                (StatusCode::BAD_REQUEST, axum::Json(json!([{"errorMessage": "Authentication failure"}])))
            }),
        )
        .route(
            "/api/v3/downloadclient/{id}",
            put(|State(s): State<S>, UrlPath(id): UrlPath<i64>, axum::Json(b): axum::Json<Value>| async move {
                let mut st = lock(&s);
                st.calls.push(format!("put downloadclient {id}"));
                if let Some(d) = st.dl.iter_mut().find(|d| d["id"] == json!(id)) {
                    *d = b.clone();
                }
                (StatusCode::ACCEPTED, axum::Json(b))
            }),
        )
        .with_state(s)
}

fn bazarr_router(s: S) -> Router {
    Router::new()
        .route("/api/system/ping", get(|| async { axum::Json(json!({"status": "OK"})) }))
        .route(
            "/api/system/settings",
            post(|State(s): State<S>, Form(f): Form<HashMap<String, String>>| async move {
                let mut st = lock(&s);
                st.bazarr_auth = json!({"type": f.get("settings-auth-type"), "username": f.get("settings-auth-username"),
                                        "password": f.get("settings-auth-password")});
                StatusCode::NO_CONTENT
            })
            .get(|State(s): State<S>| async move { axum::Json(json!({"auth": lock(&s).bazarr_auth.clone()})) }),
        )
        .route(
            "/api/system/account",
            post(|State(s): State<S>, Form(f): Form<HashMap<String, String>>| async move {
                let a = lock(&s).bazarr_auth.clone();
                let ok = f.get("username").map(String::as_str) == a["username"].as_str()
                    && f.get("password").map(String::as_str) == a["password"].as_str();
                if ok { StatusCode::NO_CONTENT } else { StatusCode::UNAUTHORIZED }
            }),
        )
        .with_state(s)
}

fn sab_router(s: S) -> Router {
    Router::new()
        .route("/", get(|| async { "SABnzbd" }))
        .route(
            "/api",
            get(|State(s): State<S>, Query(q): Query<HashMap<String, String>>| async move {
                let mut st = lock(&s);
                let g = |k: &str| q.get(k).cloned().unwrap_or_default();
                if g("apikey") != "sabkey" {
                    return (StatusCode::FORBIDDEN, "API Key Incorrect").into_response();
                }
                match g("mode").as_str() {
                    "set_config" if g("keyword") == "username" => st.sab_user = g("value"),
                    "set_config" if g("keyword") == "password" => st.sab_pass = g("value"),
                    "get_config" => {
                        return axum::Json(json!({"config": {"misc": {"username": st.sab_user}}})).into_response();
                    }
                    _ => {}
                }
                axum::Json(json!({"status": true})).into_response()
            }),
        )
        .route(
            "/login/",
            post(|State(s): State<S>, Form(f): Form<HashMap<String, String>>| async move {
                let st = lock(&s);
                if f.get("username") == Some(&st.sab_user) && f.get("password") == Some(&st.sab_pass) {
                    (StatusCode::SEE_OTHER, [(header::SET_COOKIE, "login_cookie=abc; Path=/")]).into_response()
                } else {
                    StatusCode::OK.into_response()
                }
            }),
        )
        .with_state(s)
}

fn qb_router(s: S) -> Router {
    Router::new()
        .route("/", get(|| async { "qBittorrent" }))
        .route(
            "/api/v2/auth/login",
            post(|State(s): State<S>, Form(f): Form<HashMap<String, String>>| async move {
                let st = lock(&s);
                if f.get("username") == Some(&st.qb_user) && f.get("password") == Some(&st.qb_pass) {
                    ([(header::SET_COOKIE, "SID=sess; HttpOnly; path=/")], "Ok.").into_response()
                } else {
                    "Fails.".into_response()
                }
            }),
        )
        .route(
            "/api/v2/app/preferences",
            get(|State(s): State<S>, h: HeaderMap| async move {
                let st = lock(&s);
                if h.get(header::COOKIE).and_then(|v| v.to_str().ok()) != Some("SID=sess") {
                    return StatusCode::FORBIDDEN.into_response();
                }
                axum::Json(
                    json!({"web_ui_username": st.qb_user, "bypass_auth_subnet_whitelist_enabled": st.qb_whitelist}),
                )
                .into_response()
            }),
        )
        .with_state(s)
}

fn nzb_router(s: S) -> Router {
    Router::new()
        .route("/", get(|| async { "NZBGet" }))
        .route(
            "/jsonrpc/version",
            get(|State(s): State<S>, h: HeaderMap| async move {
                let want = format!("Basic {}", lock(&s).nzb_auth);
                if h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(want.as_str()) {
                    axum::Json(json!({"result": "24.0"})).into_response()
                } else {
                    StatusCode::UNAUTHORIZED.into_response()
                }
            }),
        )
        .with_state(s)
}

/// base64 of "user:pass", for the NZBGet mock.
fn basic(user: &str, pass: &str) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = format!("{user}:{pass}").into_bytes();
    let mut out = String::new();
    for ch in b.chunks(3) {
        let n =
            (u32::from(ch[0]) << 16) | (u32::from(*ch.get(1).unwrap_or(&0)) << 8) | u32::from(*ch.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= ch.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

struct Stack {
    state: S,
    dir: tempfile::TempDir,
    config: PathBuf,
}

/// Fresh mocks and a wranglarr.toml pointing at them.
async fn stack(nzb_user: &str) -> Stack {
    let state: S = Arc::new(Mutex::new(Apps {
        host: json!({"id": 1, "bindAddress": "*", "authenticationMethod": "none", "username": "", "password": ""}),
        dl: vec![
            json!({"id": 3, "name": "qBittorrent", "implementation": "QBittorrent",
                   "fields": [{"name": "host", "value": "qbittorrent"}, {"name": "username", "value": "admin"},
                              {"name": "password", "value": "********"}]}),
            json!({"id": 4, "name": "SAB", "implementation": "Sabnzbd", "fields": []}),
        ],
        qb_user: "me".into(),
        qb_pass: "Correct-Horse-9".into(),
        nzb_auth: basic(nzb_user, "Correct-Horse-9"),
        ..Default::default()
    }));
    let arr = serve(arr_router(state.clone())).await;
    let bz = serve(bazarr_router(state.clone())).await;
    let sab = serve(sab_router(state.clone())).await;
    let qb = serve(qb_router(state.clone())).await;
    let nzb = serve(nzb_router(state.clone())).await;
    let dir = tempfile::tempdir().expect("tmp");
    let d = dir.path().display();
    let toml = format!(
        r#"
[storage]
root = "{d}/backups"
state_db = "{d}/state/wranglarr.db"

[[suite]]
name = "media"
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "{arr}/"
api_key = "arrkey"
[[suite.app]]
name = "qbittorrent"
kind = "qbittorrent"
url = "{qb}"
username = "me"
password = "Correct-Horse-9"
[[suite.app]]
name = "nzbget"
kind = "nzbget"
url = "{nzb}"
username = "{nzb_user}"
password = "Correct-Horse-9"
[[suite.app]]
name = "bazarr"
kind = "bazarr"
url = "{bz}"
api_key = "bazkey"
[[suite.app]]
name = "sabnzbd"
kind = "sabnzbd"
url = "{sab}"
api_key = "sabkey"

"#
    );
    std::fs::create_dir_all(dir.path().join("state")).expect("mkdir");
    let config = dir.path().join("wranglarr.toml");
    std::fs::write(&config, toml).expect("toml");
    Stack { state, dir, config }
}

struct Out {
    code: i32,
    stdout: String,
    report: Value,
}

async fn run(prog: &[String], st: &Stack, env: &[(&str, &str)], args: &[&str]) -> Out {
    let mut c = tokio::process::Command::new(&prog[0]);
    c.args(&prog[1..]).args(args).env("WRANGLARR_CONFIG", &st.config).env_remove("WRANGLARR_ENABLED_APPS");
    for k in ["APPS_AUTH", "APPS_LOGIN_USER", "APPS_LOGIN_PASSWORD", "APPS_AUTH_REQUIRED"] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().await.expect("runs");
    let report = std::fs::read_to_string(st.dir.path().join("state/apps-auth.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    Out { code: o.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&o.stdout).into_owned(), report }
}

fn rust() -> Vec<String> {
    vec![env!("CARGO_BIN_EXE_wranglarr").into(), "apps-auth".into()]
}

const APPS: &[(&str, &str)] =
    &[("APPS_AUTH", "apps"), ("APPS_LOGIN_USER", " me "), ("APPS_LOGIN_PASSWORD", "Correct-Horse-9")];

#[tokio::test]
async fn apps_mode() {
    let st = stack("other").await;
    let o = run(&rust(), &st, APPS, &[]).await;
    assert_eq!(o.code, 1, "{}", o.stdout);
    let want = "apps-auth: mode apps, login \"me\": radarr, bazarr, sabnzbd, qbittorrent, nzbget
apps-auth: radarr: login page, user me, asked everywhere: signed in with it
apps-auth: bazarr: login page, user me: signed in with it
apps-auth: sabnzbd: login page, user me: signed in with it
apps-auth: qbittorrent: login page, user me: signed in with it
apps-auth: radarr: updated its qbittorrent login
apps-auth: nzbget: FAILED: NZBGET_USER is \"other\", not \"me\": set NZBGET_USER and NZBGET_PASS in .env to the apps' login
apps-auth: 1 app(s) failed; fix the cause and re-run: ./scripts/init.sh --auth
";
    assert_eq!(o.stdout, want);
    {
        let s = lock(&st.state);
        assert_eq!(s.host["authenticationMethod"], "forms");
        assert_eq!(s.host["passwordConfirmation"], "Correct-Horse-9");
        assert_eq!(s.dl[0]["fields"][1]["value"], "me");
        assert_eq!(s.dl[0]["fields"][2]["value"], "Correct-Horse-9");
        assert_eq!(s.calls, vec!["put downloadclient 3"]);
    }
    let text = std::fs::read_to_string(st.dir.path().join("state/apps-auth.json")).expect("report");
    assert!(
        text.starts_with(
            "{\n  \"mode\": \"apps\",\n  \"username\": \"me\",\n  \"required\": \"enabled\",\n  \"at\": \""
        )
    );
    assert!(
        text.contains("\n  \"apps\": {\n    \"radarr\": {\n      \"ok\": true,\n      \"detail\": \"login page"),
        "{text}"
    );
    assert_eq!(o.report["apps"]["nzbget"]["ok"], json!(false));

    // only the clients, after the ports changed: the report keeps the apps' half
    let o = run(&rust(), &st, APPS, &["--only", "clients", "--wait=0"]).await;
    assert_eq!(o.code, 1);
    assert!(o.stdout.starts_with("apps-auth: mode apps, login \"me\": qbittorrent, nzbget\n"), "{}", o.stdout);
    assert_eq!(o.report["apps"].as_object().map(|a| a.len()), Some(5));
}

#[tokio::test]
async fn tunnel_mode() {
    let st = stack("me").await;
    lock(&st.state).qb_whitelist = true;
    let o = run(&rust(), &st, &[("APPS_AUTH", " Tunnel")], &["--only", "apps"]).await;
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert_eq!(
        o.stdout,
        "apps-auth: mode tunnel: radarr, bazarr, sabnzbd
apps-auth: radarr: External: no login of its own, the gate signs you in
apps-auth: bazarr: no login of its own, the gate signs you in
apps-auth: sabnzbd: no login of its own, the gate signs you in
apps-auth: done
"
    );
    assert_eq!(o.report["username"], "");
    let o = run(&rust(), &st, &[("APPS_AUTH", "tunnel")], &["--only", "clients"]).await;
    assert!(
        o.stdout.contains("apps-auth: qbittorrent: logs in as me; the gate's network gets in without it\n"),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains("apps-auth: nzbget: logs in as me; the gate signs you in to it\n"), "{}", o.stdout);
}

#[tokio::test]
async fn refusals() {
    let st = stack("me").await;
    let r = rust();
    let o = run(&r, &st, &[], &[]).await;
    assert_eq!(
        (o.code, o.stdout.as_str()),
        (0, "apps-auth: APPS_AUTH=manual: the apps' logins are left as they are\n")
    );
    let o = run(&r, &st, &[("APPS_AUTH", "x")], &[]).await;
    assert_eq!((o.code, o.stdout.as_str()), (2, "apps-auth: APPS_AUTH=\"x\": use apps, tunnel or manual\n"));
    let o = run(&r, &st, &[("APPS_AUTH", "apps"), ("APPS_LOGIN_PASSWORD", "short")], &[]).await;
    assert_eq!((o.code, o.stdout.as_str()), (2, "apps-auth: APPS_LOGIN_PASSWORD: use at least 12 characters\n"));
    let o = run(&r, &st, &[("APPS_AUTH", "apps"), ("APPS_LOGIN_USER", "a b")], &[]).await;
    assert_eq!(o.stdout, "apps-auth: APPS_LOGIN_USER: 1-64 letters, digits and . _ - @\n");
    let mut env = APPS.to_vec();
    env.push(("APPS_AUTH_REQUIRED", "never"));
    let o = run(&r, &st, &env, &[]).await;
    assert_eq!(o.stdout, "apps-auth: APPS_AUTH_REQUIRED: enabled or disabledForLocalAddresses\n");
    let o = run(&r, &st, &[], &["--only", "x"]).await;
    assert_eq!(o.code, 2);
    assert!(o.stdout.is_empty());
    let o = run(&r, &st, &[], &["-h"]).await;
    assert!(o.code == 0 && o.stdout.contains("Usage: wranglarr-apps-auth [OPTIONS]"), "{}", o.stdout);

    // an app that never answers
    std::fs::write(
        &st.config,
        format!(
            "[storage]\nstate_db = \"{}/state/x.db\"\n[[suite]]\nname = \"m\"\n[[suite.app]]\nname = \"radarr\"\nkind = \"radarr\"\nurl = \"http://127.0.0.1:9\"\napi_key = \"k\"\n",
            st.dir.path().display()
        ),
    )
    .expect("toml");
    let o = run(&r, &st, APPS, &["--wait", "0"]).await;
    assert_eq!(o.code, 1);
    assert!(
        o.stdout.contains("apps-auth: radarr: FAILED: not reachable at http://127.0.0.1:9 after 0 s\n"),
        "{}",
        o.stdout
    );
    let rep = std::fs::read_to_string(st.dir.path().join("state/apps-auth.json")).expect("report");
    assert!(rep.contains("\"detail\": \"not reachable at http://127.0.0.1:9 after 0 s\""), "{rep}");
}

/// What one run printed, wrote and left behind in the mock apps.
fn observe(o: Out, st: &Stack) -> Value {
    let mut report = o.report;
    if let Some(r) = report.as_object_mut() {
        r.remove("at");
    }
    let s = lock(&st.state);
    json!({"stdout": o.stdout, "code": o.code, "report": report, "host": s.host, "dl": s.dl,
           "bazarr_auth": s.bazarr_auth, "sab": [s.sab_user, s.sab_pass]})
}

/// Four runs (two logins, tunnel, a bad mode): the output, the report and the
/// changes in the apps, as recorded in `fixtures/appsauth.json`.
#[tokio::test]
async fn runs_as_recorded() {
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, Vec<&'a str>);
    let cases: Vec<Case> = vec![
        ("other", APPS.to_vec(), vec![]),
        ("me", APPS.to_vec(), vec!["--only", "apps"]),
        ("me", vec![("APPS_AUTH", "tunnel")], vec![]),
        ("me", vec![("APPS_AUTH", "x")], vec![]),
    ];
    let want: Vec<Value> = serde_json::from_str(include_str!("fixtures/appsauth.json")).expect("fixture");
    assert_eq!(want.len(), cases.len());
    for ((nzb, env, args), want) in cases.into_iter().zip(want) {
        let st = stack(nzb).await;
        let got = observe(run(&rust(), &st, &env, &args).await, &st);
        assert_eq!(got, want, "{env:?} {args:?}");
    }
}
