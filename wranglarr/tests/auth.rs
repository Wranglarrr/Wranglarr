// SPDX-License-Identifier: GPL-3.0-or-later
//! The login over HTTP, in-process: the guard, setup → login → session →
//! logout, TOTP, API keys, the CSRF refusal, passkey refusals, AuthTunnel's gate.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use wranglarr::app::App;
use wranglarr::auth::{Options, totp_at};
use wranglarr_core::time::unix_now;

const PW: &str = "correct horse battery";
const O: (&str, &str) = ("origin", "http://wr.test");

struct T {
    app: App,
    router: Router,
    jar: HashMap<String, String>,
    peer: Option<SocketAddr>,
    _dir: tempfile::TempDir,
}

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    text: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
    fn location(&self) -> &str {
        self.headers.get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("")
    }
    fn cookies(&self) -> Vec<String> {
        self.headers.get_all(header::SET_COOKIE).iter().map(|v| v.to_str().unwrap().to_string()).collect()
    }
}

async fn setup(opts: Options) -> T {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "[storage]\nroot = \"{0}/backups\"\nstate_db = \"{0}/state/wranglarr.db\"\n\n\
         [[suite]]\nname = \"media\"\n[[suite.app]]\nname = \"radarr\"\nkind = \"radarr\"\n\
         url = \"http://127.0.0.1:1\"\napi_key = \"x\"\n",
        dir.path().display()
    );
    let cfg = wranglarr_services::apps::parse(&toml, |_| None).unwrap();
    let app = App::new(cfg, wranglarr::deployment::Deployment { auth: opts, ..Default::default() }).await.unwrap();
    let router = wranglarr::web::router(app.clone());
    T { app, router, jar: HashMap::new(), peer: Some("127.0.0.1:12345".parse().unwrap()), _dir: dir }
}

impl T {
    async fn call(&mut self, method: &str, path: &str, body: Option<Value>, headers: &[(&str, &str)]) -> Resp {
        let mut b = Request::builder().method(method).uri(path);
        if !headers.iter().any(|(k, _)| *k == "host") {
            b = b.header("host", "wr.test");
        }
        if !self.jar.is_empty() {
            let c: Vec<String> = self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
            b = b.header("cookie", c.join("; "));
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = match body {
            Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())),
            None => b.body(Body::empty()),
        }
        .unwrap();
        if let Some(peer) = self.peer {
            req.extensions_mut().insert(ConnectInfo(peer));
        }
        let r = self.router.clone().oneshot(req).await.unwrap();
        let (parts, body) = r.into_parts();
        let bytes = axum::body::to_bytes(body, 1 << 22).await.unwrap();
        let resp = Resp { status: parts.status, headers: parts.headers, text: String::from_utf8_lossy(&bytes).into() };
        for c in resp.cookies() {
            let first = c.split(';').next().unwrap();
            let (k, v) = first.split_once('=').unwrap();
            if c.contains("Max-Age=0") {
                self.jar.remove(k);
            } else {
                self.jar.insert(k.to_string(), v.to_string());
            }
        }
        resp
    }
    async fn get(&mut self, path: &str) -> Resp {
        self.call("GET", path, None, &[]).await
    }
    async fn post(&mut self, path: &str, body: Value) -> Resp {
        self.call("POST", path, Some(body), &[O]).await
    }
    async fn create_account(&mut self) {
        assert_eq!(self.get("/setup").await.status, 200);
        let code = self.app.auth().setup_code();
        let r = self.post("/api/auth/setup", json!({"code": code, "username": "admin", "password": PW})).await;
        assert_eq!(r.status, 200, "{}", r.text);
    }
}

#[tokio::test]
async fn forwarded_metadata_requires_a_trusted_connection_peer() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    let forwarded = [("x-forwarded-host", "public.example"), ("x-forwarded-proto", "https")];
    for peer in [None, Some("10.0.0.5:12345".parse().unwrap())] {
        t.peer = peer;
        let me = t.call("GET", "/api/auth/me", None, &forwarded).await.json();
        assert_eq!(me["rp_id"], "wr.test", "untrusted host ignored: {peer:?}");
        assert_eq!(me["https"], false, "untrusted scheme ignored: {peer:?}");
        let bad = [("origin", "https://evil.example"), ("x-forwarded-host", "evil.example")];
        assert_eq!(t.call("POST", "/api/auth/logout", None, &bad).await.status, 403);
    }

    t.peer = Some("127.0.0.1:12345".parse().unwrap());
    let me = t.call("GET", "/api/auth/me", None, &forwarded).await.json();
    assert_eq!(me["rp_id"], "public.example");
    assert_eq!(me["https"], true);
    let same = [("origin", "https://public.example"), ("x-forwarded-host", "public.example")];
    assert_eq!(t.call("POST", "/api/auth/logout", None, &same).await.status, 200);
    let login = json!({"username": "admin", "password": PW});
    let headers = [O, ("x-forwarded-proto", "https")];
    for (peer, secure) in [("10.0.0.5:12345", false), ("127.0.0.1:12345", true)] {
        t.peer = Some(peer.parse().unwrap());
        let resp = t.call("POST", "/api/auth/login", Some(login.clone()), &headers).await;
        assert_eq!(resp.status, 200, "{}", resp.text);
        assert_eq!(resp.cookies()[0].contains("; Secure"), secure, "{peer}");
    }
}

#[tokio::test]
async fn setup_login_session_logout() {
    let mut t = setup(Options::default()).await;
    let r = t.get("/").await;
    assert_eq!((r.status.as_u16(), r.location()), (303, "/setup"));
    let r = t.get("/api/suites").await;
    assert_eq!(r.status, 401);
    assert_eq!(r.json(), json!({"error": "sign in first (or send an X-Api-Key header)"}));
    assert_eq!(t.get("/api/health").await.json(), json!({"ok": true}), "no details without a sign-in");
    assert_eq!(t.get("/login").await.location(), "/setup");

    let r = t.get("/setup").await;
    assert!(r.status == 200 && r.text.contains("setup-form"), "{}", r.text);
    let code = t.app.auth().setup_code();
    assert_eq!(code.len(), 14);
    let body = json!({"code": code, "username": "admin", "password": PW});
    let r = t.call("POST", "/api/auth/setup", Some(body.clone()), &[]).await;
    assert_eq!(r.status, 403, "no Origin: refused (CSRF)");
    assert_eq!(r.json()["error"], "refused: this request didn't come from wranglarr's own pages");
    let r = t.post("/api/auth/setup", json!({"code": "AAAA-BBBB-CCCC", "username": "admin", "password": PW})).await;
    assert_eq!(r.status, 403);
    let r = t.post("/api/auth/setup", json!({"code": code, "password": "short"})).await;
    assert_eq!((r.status.as_u16(), r.json()["error"].as_str()), (400, Some("use at least 12 characters")));
    let r = t.post("/api/auth/setup", json!({"code": code})).await;
    assert_eq!(r.status, 422, "missing field");
    let r = t.post("/api/auth/setup", body.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sc = r.cookies().join(", ");
    assert!(sc.starts_with("wranglarr_session=") && sc.contains("HttpOnly") && sc.contains("SameSite=strict"), "{sc}");
    assert!(!sc.contains("Max-Age=86400") && !sc.contains("Secure"), "a browser-session cookie over http: {sc}");
    assert!(sc.contains("wranglarr_mfa=\"\""), "{sc}");
    assert_eq!(t.post("/api/auth/setup", body).await.status, 409);

    let me = t.get("/api/auth/me").await.json();
    assert_eq!(me["enabled"], true);
    assert_eq!(me["username"], "admin");
    assert_eq!(me["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(me["sessions"][0]["current"], true);
    assert_eq!(me["rp_id"], "wr.test");
    assert_eq!(me["https"], false);
    assert!(t.get("/api/health").await.json().get("version").is_some(), "details with a session");
    let r = t.get("/account").await;
    assert!(r.status == 200 && r.text.contains("signed in as admin"), "{}", &r.text[..r.text.len().min(400)]);
    let r = t.get("/login?next=/suites/media").await;
    assert_eq!((r.status.as_u16(), r.location()), (303, "/suites/media"), "signed in: straight on");
    let r = t.get("/no-such-page").await;
    assert_eq!(r.status, 404, "error pages still work behind the guard");

    let r = t.call("POST", "/api/auth/logout", None, &[("origin", "http://evil.example")]).await;
    assert_eq!(r.status, 403, "session + foreign Origin: refused");
    let r = t.call("POST", "/api/auth/logout", None, &[O]).await;
    assert_eq!(r.status, 200);
    assert!(r.cookies()[0].starts_with("wranglarr_session=\"\""));
    assert_eq!(t.get("/api/auth/me").await.status, 401, "logout ends the session");
    let r = t.get("/suites/x%20y/caf%C3%A9").await;
    assert_eq!((r.status.as_u16(), r.location()), (303, "/login?next=/suites/x%20y/caf%C3%A9"));
    let r = t.get("/login?next=//evil.example").await;
    assert!(r.status == 200 && r.text.contains("data-next=\"/\""), "{}", r.text);

    // remember me: a 30-day cookie, login case-insensitive
    let r = t.post("/api/auth/login", json!({"username": "ADMIN", "password": PW, "remember": true})).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.cookies()[0].contains("Max-Age=2592000"), "{:?}", r.cookies());
    assert_eq!(t.get("/api/auth/me").await.json()["sessions"][0]["method"], "password");
}

#[tokio::test]
async fn stored_account_signs_in_without_unreadable_passkeys() {
    let mut t = setup(Options::default()).await;
    // a stored account: the scrypt$N$r$p$salt$key password hash, and a passkey kept only as
    // a raw public key, which can't be checked, so it is neither offered nor listed
    let acct = json!({"username": "admin", "created": "2026-01-01T00:00:00Z",
        "password": "scrypt$32768$8$1$AAECAwQFBgcICQoLDA0ODw$xy6UCICz8Vv_P6JqOkkUD4DOmhxN0tXSe6sQAUBxupQ",
        "user_handle": "q83vEjRWeJCrze8SNFZ4kKvN7xI0VniQq83vEjRWeJA",
        "totp": {"enabled": false, "secret": "", "last": 0}, "recovery": [],
        "passkeys": [{"id": "AAAA", "public_key": "pQECAyYgASFYIA", "sign_count": 3, "name": "phone",
                      "created": "2026-01-02T00:00:00Z", "last_used": null, "rp_id": "wr.test", "backed_up": true,
                      "transports": ["internal"]}]});
    t.app.state().put("auth:account", &acct).await.unwrap();
    let r = t.get("/login").await;
    assert!(r.status == 200 && r.text.contains("id=\"passkey-box\" hidden"), "old passkeys aren't offered");
    let r = t.post("/api/auth/login", json!({"username": "admin", "password": PW})).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let me = t.get("/api/auth/me").await.json();
    assert_eq!(me["passkeys"], json!([]), "old-format passkeys aren't listed");
    assert_eq!(me["created"], "2026-01-01T00:00:00Z");
}

#[tokio::test]
async fn totp_sign_in() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    assert_eq!(t.post("/api/auth/totp/begin", json!({"password": "nope"})).await.status, 403);
    let b = t.post("/api/auth/totp/begin", json!({"password": PW})).await.json();
    assert!(b["uri"].as_str().unwrap().starts_with("otpauth://totp/wranglarr:admin?secret="), "{b}");
    assert!(b["qr_svg"].as_str().unwrap().starts_with("<svg"));
    let secret = b["secret"].as_str().unwrap().replace(' ', "");
    let mut now = unix_now();
    if 30.0 - now % 30.0 < 4.0 {
        tokio::time::sleep(std::time::Duration::from_secs_f64(30.0 - now % 30.0 + 0.2)).await;
        now = unix_now();
    }
    let step = (now / 30.0).floor() as i64;
    let code = |s: i64| totp_at(&secret, s).unwrap();
    let wrong = if code(step) == "000000" { "111111" } else { "000000" };
    assert_eq!(t.post("/api/auth/totp/enable", json!({"code": wrong})).await.status, 400);
    let rec = t.post("/api/auth/totp/enable", json!({"code": code(step)})).await.json()["recovery"].clone();
    assert_eq!(rec.as_array().unwrap().len(), 10);
    t.post("/api/auth/logout", json!(null)).await;

    let r = t.post("/api/auth/login", json!({"username": "admin", "password": PW})).await;
    assert_eq!(r.json(), json!({"mfa": true}));
    let sc = r.cookies().join(", ");
    assert!(
        sc.starts_with("wranglarr_mfa=") && sc.contains("Max-Age=300") && !sc.contains("wranglarr_session"),
        "{sc}"
    );
    assert_eq!(t.get("/api/auth/me").await.status, 401, "no session yet");
    assert_eq!(t.post("/api/auth/login/totp", json!({"code": code(step)})).await.status, 401, "replay");
    let r = t.post("/api/auth/login/totp", json!({"code": code(step + 1)})).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(t.get("/api/auth/me").await.json()["sessions"][0]["method"], "password+totp");
    t.post("/api/auth/logout", json!(null)).await;
    t.post("/api/auth/login", json!({"username": "admin", "password": PW})).await;
    let r = t.post("/api/auth/login/totp", json!({"code": rec[0].as_str().unwrap().to_uppercase()})).await;
    assert_eq!(r.status, 200, "a recovery code");
    let me = t.get("/api/auth/me").await.json();
    assert_eq!((me["totp"].clone(), me["recovery_left"].clone()), (json!(true), json!(9)));
    let r = t.post("/api/auth/login/totp", json!({"code": "123456"})).await;
    assert_eq!(r.json()["error"], "that sign-in expired; start again");
    let r = t.post("/api/auth/recovery", json!({"password": PW})).await;
    assert_eq!(r.json()["recovery"].as_array().unwrap().len(), 10);
    assert_eq!(t.post("/api/auth/totp/disable", json!({"password": PW})).await.json(), json!({"ok": true}));
    assert_eq!(t.get("/api/auth/me").await.json()["totp"], false);
}

#[tokio::test]
async fn api_keys_and_sessions() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    assert_eq!(t.post("/api/auth/api-keys", json!({"name": "ha", "password": "nope"})).await.status, 403);
    let k = t.post("/api/auth/api-keys", json!({"name": "ha", "password": PW})).await.json();
    let key = k["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("wr_") && k["name"] == "ha" && k.get("hash").is_none(), "{k}");

    let mut raw = setup(Options::default()).await;
    raw.router = t.router.clone();
    let kh = [("x-api-key", key.as_str())];
    let me = raw.call("GET", "/api/auth/me", None, &kh).await;
    assert_eq!(me.status, 200);
    assert!(me.json()["sessions"].as_array().unwrap().iter().all(|s| s["current"] == false));
    assert_eq!(raw.call("POST", "/api/auth/logout", None, &kh).await.status, 200, "an API key POST needs no Origin");
    let r = raw.call("POST", "/api/auth/api-keys", Some(json!({"name": "x", "password": PW})), &kh).await;
    assert_eq!(
        (r.status.as_u16(), r.json()["error"].as_str()),
        (403, Some("this needs a signed-in browser, not an API key"))
    );
    assert_eq!(raw.call("GET", "/api/auth/me", None, &[("x-api-key", "wr_nope")]).await.status, 401);
    // wranglarr updating itself: a signed-in browser only (here it then finds no Docker)
    let r = raw.call("POST", "/api/updates/self", None, &kh).await;
    assert_eq!(
        (r.status.as_u16(), r.json()["error"].as_str()),
        (403, Some("updating wranglarr itself needs someone signed in on its pages; an API key can't start it"))
    );
    let r = t.post("/api/updates/self", json!(null)).await;
    assert_eq!(r.status, 409, "past the guard with a session: {}", r.text);
    assert!(r.json()["error"].as_str().is_some_and(|e| e.starts_with("docker control is off")), "{}", r.text);
    // the apps' API keys: a signed-in browser only
    let r = raw.call("GET", "/api/keys", None, &kh).await;
    assert_eq!(
        (r.status.as_u16(), r.json()["error"].as_str()),
        (403, Some("the apps' API keys are shown to someone signed in on wranglarr's pages"))
    );
    assert_eq!(raw.call("GET", "/keys", None, &kh).await.status, 403);
    let r = t.get("/api/keys").await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json()["apps"][0]["key"], "x");
    let own = &r.json()["wranglarr"];
    assert_eq!((own["url"].as_str(), own["auth"].as_bool()), (Some("http://wranglarr:8787"), Some(true)));
    assert_eq!(own["keys"][0]["name"], "ha", "its own keys by name");
    assert!(own["keys"][0].get("hash").is_none() && !r.text.contains(&key), "never the key or its hash");
    assert_eq!(r.headers[header::CACHE_CONTROL], "no-store");
    let r = t.get("/keys").await;
    assert_eq!(
        (r.status, &r.headers[header::CACHE_CONTROL]),
        (StatusCode::OK, &"no-store".parse::<header::HeaderValue>().unwrap())
    );
    let listed = t.get("/api/auth/me").await.json()["api_keys"].clone();
    assert!(listed[0]["last_used"].is_string() && listed[0].get("hash").is_none(), "{listed}");
    let r = t.call("DELETE", &format!("/api/auth/api-keys/{}", k["id"].as_str().unwrap()), None, &[O]).await;
    assert_eq!(r.status, 200);
    assert_eq!(raw.call("GET", "/api/auth/me", None, &kh).await.status, 401, "revoked");
    assert_eq!(t.call("DELETE", "/api/auth/api-keys/nope", None, &[O]).await.status, 404);

    // a second browser, then "sign out everywhere else"
    let mut other = setup(Options::default()).await;
    other.router = t.router.clone();
    assert_eq!(other.post("/api/auth/login", json!({"username": "admin", "password": PW})).await.status, 200);
    let sessions = t.get("/api/auth/me").await.json()["sessions"].clone();
    assert_eq!(sessions.as_array().unwrap().len(), 2);
    assert_eq!(t.post("/api/auth/sessions/end-others", json!(null)).await.json(), json!({"ended": 1}));
    assert_eq!(other.get("/api/auth/me").await.status, 401);
    assert_eq!(t.call("DELETE", "/api/auth/sessions/0000", None, &[O]).await.status, 404);
}

#[tokio::test]
async fn lockout_after_five_failures() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    for i in 0..5 {
        let r = t.post("/api/auth/login", json!({"username": "admin", "password": format!("wrong{i}")})).await;
        assert_eq!(r.status, 401);
        assert_eq!(r.json()["error"], "wrong username or password");
    }
    let r = t.post("/api/auth/login", json!({"username": "admin", "password": PW})).await;
    assert_eq!(r.status, 429);
    assert!(r.json()["error"].as_str().unwrap().starts_with("too many failed sign-ins; try again in 15 min"));
}

#[tokio::test]
async fn passkeys_need_a_name_and_https() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    let ip = [("host", "192.168.1.5:8787"), ("origin", "http://192.168.1.5:8787")];
    let r = t.call("POST", "/api/auth/passkey/register-options", Some(json!({"password": PW})), &ip).await;
    // the guard compares Origin with Host: send both for this address
    assert_eq!(r.status, 400, "{}", r.text);
    assert_eq!(r.json()["error"], "passkeys need a host name, not an IP address");
    let lan = [("host", "wranglarr.lan:8787"), ("origin", "http://wranglarr.lan:8787")];
    let r = t.call("POST", "/api/auth/passkey/register-options", Some(json!({"password": PW})), &lan).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let o = r.json();
    let opts = &o["options"];
    assert_eq!(opts["rp"]["id"], "wranglarr.lan");
    assert_eq!(opts["user"]["name"], "admin");
    assert_eq!(opts["authenticatorSelection"]["residentKey"], "required");
    assert_eq!(opts["authenticatorSelection"]["userVerification"], "required");
    assert!(opts["challenge"].as_str().unwrap().len() >= 43);
    let flow = o["flow"].as_str().unwrap();
    let r = t.call("POST", "/api/auth/passkey/register", Some(json!({"flow": flow, "credential": {}})), &lan).await;
    assert_eq!(r.status, 400);
    assert!(r.json()["error"].as_str().unwrap().contains("HTTPS"), "{}", r.text);
    let r = t.call("POST", "/api/auth/passkey/register", Some(json!({"flow": flow, "credential": {}})), &lan).await;
    assert_eq!(r.json()["error"], "that passkey request expired; try again", "a flow is used once");

    let r = t.call("GET", "/api/auth/passkey/login-options", None, &[("host", "wranglarr.lan:8787")]).await;
    let o = r.json();
    assert_eq!(o["options"]["rpId"], "wranglarr.lan", "{o}");
    assert_eq!(o["options"]["userVerification"], "required");
    let https = [("host", "wranglarr.lan"), ("origin", "https://wranglarr.lan"), ("x-forwarded-proto", "https")];
    let fake = |flow: &Value| {
        json!({"flow": flow, "credential": {"id": "AAAA", "rawId": "AAAA", "type": "public-key",
               "response": {"clientDataJSON": "", "authenticatorData": "", "signature": ""}}})
    };
    // asked for at http://wranglarr.lan:8787, finished from https://wranglarr.lan: not the same page
    let r = t.call("POST", "/api/auth/passkey/login", Some(fake(&o["flow"])), &https).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert_eq!(
        r.json()["error"],
        "this page (https://wranglarr.lan) isn't where the passkey request started (http://wranglarr.lan:8787); try again"
    );
    let o = t.call("GET", "/api/auth/passkey/login-options", None, &https).await.json();
    let r = t.call("POST", "/api/auth/passkey/login", Some(fake(&o["flow"])), &https).await;
    assert_eq!(
        (r.status.as_u16(), r.json()["error"].as_str()),
        (401, Some("this passkey isn't registered with wranglarr"))
    );
    assert_eq!(t.call("DELETE", "/api/auth/passkeys/AAAA", None, &[O]).await.status, 404);
}

#[tokio::test]
async fn auth_off() {
    let mut t = setup(Options { enabled: false, ..Options::default() }).await;
    assert_eq!(t.get("/api/auth/me").await.json(), json!({"enabled": false}));
    assert_eq!(t.get("/account").await.location(), "/system");
    let r = t.call("POST", "/api/auth/logout", None, &[("origin", "http://evil.example")]).await;
    assert_eq!(r.status, 403, "cross-site changes are refused even without the login");
    assert_eq!(t.call("POST", "/api/auth/logout", None, &[]).await.status, 200, "scripts send no Origin");
    let r = t.call("POST", "/api/auth/password", Some(json!({"current": "a", "new": "b"})), &[O]).await;
    assert_eq!(r.status, 403);
    let r = t.get("/api/auth/gate").await;
    assert_eq!(r.status, 404, "AuthTunnel off");
}

#[tokio::test]
async fn tunnel_gate() {
    let host = "wranglarr.example.com";
    let mut t = setup(Options {
        tunnel: true,
        cookie_domain: "example.com".into(),
        login_url: format!("https://{host}"),
        ..Options::default()
    })
    .await;
    let h = [("host", host), ("origin", "https://wranglarr.example.com"), ("x-forwarded-proto", "https")];
    t.call("GET", "/setup", None, &[("host", host)]).await;
    let code = t.app.auth().setup_code();
    let r = t.call("POST", "/api/auth/setup", Some(json!({"code": code, "password": PW})), &h).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sc = r.cookies()[0].clone();
    assert!(sc.contains("Domain=example.com") && sc.contains("SameSite=lax") && sc.contains("Secure"), "{sc}");
    let token = sc.split(';').next().unwrap().trim_start_matches("wranglarr_session=").to_string();

    let g =
        [("x-original-proto", "https"), ("x-original-host", "radarr.example.com"), ("x-original-uri", "/movie/1?a=b")];
    let mut anon = setup(Options::default()).await;
    anon.router = t.router.clone();
    let cookie = format!("wranglarr_session={token}");
    let with = |extra: &[(&'static str, &'static str)]| [&g[..], extra].concat();
    let r = anon.call("GET", "/api/auth/gate", None, &[&g[..], &[("cookie", cookie.as_str())]].concat()).await;
    assert_eq!(r.status, 204);
    let r = anon.call("GET", "/api/auth/gate", None, &g).await;
    assert_eq!(r.status, 401);
    let want = "https://wranglarr.example.com/login?next=https%3A%2F%2Fradarr.example.com%2Fmovie%2F1%3Fa%3Db";
    assert_eq!(r.headers["x-wranglarr-login"], want);
    let r = anon
        .call("GET", "/api/auth/gate", None, &with(&[("x-wranglarr-app", "radarr"), ("x-wranglarr-key", "x")]))
        .await;
    assert_eq!(r.status, 204, "the app's real API key");
    let r = anon
        .call("GET", "/api/auth/gate", None, &with(&[("x-wranglarr-app", "radarr"), ("x-wranglarr-key-arg", "x")]))
        .await;
    assert_eq!(r.status, 204, "apikey= argument");
    let r = anon
        .call("GET", "/api/auth/gate", None, &with(&[("x-wranglarr-app", "radarr"), ("x-wranglarr-key", "no")]))
        .await;
    assert_eq!(r.status, 401);
    let r = anon
        .call("GET", "/api/auth/gate", None, &with(&[("x-wranglarr-app", "sonarr"), ("x-wranglarr-key", "x")]))
        .await;
    assert_eq!(r.status, 401, "an app wranglarr doesn't know");

    // without AUTH_LOGIN_URL: the app's host name on WRANGLARR_PORT
    t.app.auth().set_options(Options { tunnel: true, cookie_domain: "example.com".into(), ..Options::default() });
    let r = anon.call("GET", "/api/auth/gate", None, &g).await;
    assert!(
        r.headers["x-wranglarr-login"].to_str().unwrap().starts_with("https://radarr.example.com:8787/login?next=")
    );

    let r = anon
        .call(
            "GET",
            "/login?next=https://radarr.example.com/movie/1",
            None,
            &[("host", host), ("cookie", cookie.as_str())],
        )
        .await;
    assert_eq!((r.status.as_u16(), r.location()), (303, "https://radarr.example.com/movie/1"));
    let r = anon.call("POST", "/api/auth/logout", None, &[&h[..], &[("cookie", cookie.as_str())]].concat()).await;
    assert_eq!(r.status, 200);
    assert!(
        r.cookies().iter().any(|c| c.contains("Domain=example.com") && c.contains("Max-Age=0")),
        "{:?}",
        r.cookies()
    );
    let r = anon.call("GET", "/api/auth/gate", None, &[&g[..], &[("cookie", cookie.as_str())]].concat()).await;
    assert_eq!(r.status, 401, "the session is gone");
}

// ---- a software authenticator, for a whole passkey ceremony ------------------------------------

mod soft {
    use openssl::bn::BigNumContext;
    use openssl::ec::{EcGroup, EcKey, PointConversionForm};
    use openssl::nid::Nid;
    use openssl::pkey::{PKey, Private};
    use openssl::sha::sha256;
    use openssl::sign::Signer;
    use serde_json::{Value, json};

    fn b64u(b: &[u8]) -> String {
        data_encoding::BASE64URL_NOPAD.encode(b)
    }
    fn head(major: u8, n: usize, out: &mut Vec<u8>) {
        if n < 24 {
            out.push(major << 5 | n as u8);
        } else if n < 256 {
            out.extend([major << 5 | 24, n as u8]);
        } else {
            out.extend([major << 5 | 25, (n >> 8) as u8, n as u8]);
        }
    }
    fn text(s: &str, out: &mut Vec<u8>) {
        head(3, s.len(), out);
        out.extend(s.as_bytes());
    }
    fn bytes(b: &[u8], out: &mut Vec<u8>) {
        head(2, b.len(), out);
        out.extend(b);
    }

    pub struct Key {
        pkey: PKey<Private>,
        x: Vec<u8>,
        y: Vec<u8>,
        id: Vec<u8>,
        user: String,
        counter: u32,
    }

    impl Key {
        pub fn new() -> Key {
            let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
            let ec = EcKey::generate(&group).unwrap();
            let mut ctx = BigNumContext::new().unwrap();
            let pt = ec.public_key().to_bytes(&group, PointConversionForm::UNCOMPRESSED, &mut ctx).unwrap();
            Key {
                x: pt[1..33].to_vec(),
                y: pt[33..65].to_vec(),
                pkey: PKey::from_ec_key(ec).unwrap(),
                id: (0..32u8).map(|i| i.wrapping_mul(7)).collect(),
                user: String::new(),
                counter: 0,
            }
        }

        fn cose(&self) -> Vec<u8> {
            // {1: 2 (EC2), 3: -7 (ES256), -1: 1 (P-256), -2: x, -3: y}
            let mut c = vec![0xA5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21];
            bytes(&self.x, &mut c);
            c.push(0x22);
            bytes(&self.y, &mut c);
            c
        }

        /// navigator.credentials.create, as auth.js's credJSON sends it.
        pub fn create(&mut self, options: &Value, origin: &str) -> Value {
            let rp = options["rp"]["id"].as_str().unwrap();
            self.user = options["user"]["id"].as_str().unwrap().to_string();
            let cdj = json!({"type": "webauthn.create", "challenge": options["challenge"], "origin": origin,
                             "crossOrigin": false})
            .to_string();
            let mut auth = sha256(rp.as_bytes()).to_vec();
            auth.push(0x45); // UP | UV | AT
            auth.extend(self.counter.to_be_bytes());
            auth.extend([0u8; 16]);
            auth.extend((self.id.len() as u16).to_be_bytes());
            auth.extend(&self.id);
            auth.extend(self.cose());
            let mut att = vec![0xA3];
            text("fmt", &mut att);
            text("none", &mut att);
            text("attStmt", &mut att);
            att.push(0xA0);
            text("authData", &mut att);
            bytes(&auth, &mut att);
            json!({"id": b64u(&self.id), "rawId": b64u(&self.id), "type": "public-key", "authenticatorAttachment": null,
                   "clientExtensionResults": {},
                   "response": {"clientDataJSON": b64u(cdj.as_bytes()), "attestationObject": b64u(&att),
                                "transports": ["internal", "hybrid"]}})
        }

        /// navigator.credentials.get
        pub fn get(&mut self, options: &Value, origin: &str) -> Value {
            let rp = options["rpId"].as_str().unwrap();
            self.counter += 1;
            let cdj = json!({"type": "webauthn.get", "challenge": options["challenge"], "origin": origin,
                             "crossOrigin": false})
            .to_string();
            let mut auth = sha256(rp.as_bytes()).to_vec();
            auth.push(0x05); // UP | UV
            auth.extend(self.counter.to_be_bytes());
            let mut signed = auth.clone();
            signed.extend(sha256(cdj.as_bytes()));
            let mut s = Signer::new(openssl::hash::MessageDigest::sha256(), &self.pkey).unwrap();
            s.update(&signed).unwrap();
            let sig = s.sign_to_vec().unwrap();
            json!({"id": b64u(&self.id), "rawId": b64u(&self.id), "type": "public-key", "authenticatorAttachment": "platform",
                   "clientExtensionResults": {},
                   "response": {"clientDataJSON": b64u(cdj.as_bytes()), "authenticatorData": b64u(&auth),
                                "signature": b64u(&sig), "userHandle": self.user}})
        }
    }
}

#[tokio::test]
async fn passkey_register_and_sign_in() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    let site = [("host", "wranglarr.lan"), ("origin", "https://wranglarr.lan"), ("x-forwarded-proto", "https")];
    let o = t.call("POST", "/api/auth/passkey/register-options", Some(json!({"password": PW})), &site).await.json();
    let mut key = soft::Key::new();
    let cred = key.create(&o["options"], "https://wranglarr.lan");
    let body = json!({"flow": o["flow"], "credential": cred, "name": "  laptop  "});
    let r = t.call("POST", "/api/auth/passkey/register", Some(body), &site).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let pk = r.json();
    assert_eq!((pk["name"].as_str(), pk["rp_id"].as_str()), (Some("laptop"), Some("wranglarr.lan")), "{pk}");
    let me = t.call("GET", "/api/auth/me", None, &site).await.json();
    assert_eq!(me["passkeys"].as_array().unwrap().len(), 1, "{me}");
    assert_eq!(me["passkeys"][0]["last_used"], Value::Null);
    let o = t.call("POST", "/api/auth/passkey/register-options", Some(json!({"password": PW})), &site).await.json();
    assert_eq!(o["options"]["excludeCredentials"][0]["id"], pk["id"], "the registered one is excluded");

    t.call("POST", "/api/auth/logout", None, &site).await;
    let r = t.call("GET", "/login", None, &site).await;
    assert!(r.text.contains("<div id=\"passkey-box\" >"), "passkeys offered");
    let o = t.call("GET", "/api/auth/passkey/login-options", None, &site).await.json();
    let cred = key.get(&o["options"], "https://wranglarr.lan");
    let r =
        t.call("POST", "/api/auth/passkey/login", Some(json!({"flow": o["flow"], "credential": cred})), &site).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let me = t.call("GET", "/api/auth/me", None, &site).await.json();
    assert_eq!(me["sessions"][0]["method"], "passkey");
    assert!(me["passkeys"][0]["last_used"].is_string());

    // the same assertion again: the challenge is gone
    let r =
        t.call("POST", "/api/auth/passkey/login", Some(json!({"flow": o["flow"], "credential": cred})), &site).await;
    assert_eq!(r.json()["error"], "that passkey request expired; try again");
    // an assertion made for another HTTPS service on this host (port 8443), relayed with its Host
    // header: the ceremony began at https://wranglarr.lan, so it can't finish anywhere else
    let o = t.call("GET", "/api/auth/passkey/login-options", None, &site).await.json();
    let cred = key.get(&o["options"], "https://wranglarr.lan:8443");
    let relay =
        [("host", "wranglarr.lan:8443"), ("origin", "https://wranglarr.lan:8443"), ("x-forwarded-proto", "https")];
    let r =
        t.call("POST", "/api/auth/passkey/login", Some(json!({"flow": o["flow"], "credential": cred})), &relay).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert!(r.json()["error"].as_str().unwrap().contains("isn't where the passkey request started"), "{}", r.text);
    // nor with the right headers: the signed client data says where the browser was
    let o = t.call("GET", "/api/auth/passkey/login-options", None, &site).await.json();
    let cred = key.get(&o["options"], "https://wranglarr.lan:8443");
    let r =
        t.call("POST", "/api/auth/passkey/login", Some(json!({"flow": o["flow"], "credential": cred})), &site).await;
    assert_eq!(r.status, 401, "{}", r.text);
    // a name under the rp id is another site too
    let sub = [("host", "a.wranglarr.lan"), ("origin", "https://a.wranglarr.lan"), ("x-forwarded-proto", "https")];
    let o = t.call("GET", "/api/auth/passkey/login-options", None, &site).await.json();
    let cred = key.get(&o["options"], "https://a.wranglarr.lan");
    let r = t.call("POST", "/api/auth/passkey/login", Some(json!({"flow": o["flow"], "credential": cred})), &sub).await;
    assert_eq!(r.status, 400, "{}", r.text);
    // the passkey is for wranglarr.lan, not another site
    let other = [("host", "other.lan"), ("origin", "https://other.lan"), ("x-forwarded-proto", "https")];
    let o = t.call("GET", "/api/auth/passkey/login-options", None, &other).await.json();
    let cred = key.get(&o["options"], "https://other.lan");
    let r =
        t.call("POST", "/api/auth/passkey/login", Some(json!({"flow": o["flow"], "credential": cred})), &other).await;
    assert_eq!(r.status, 401);

    let id = pk["id"].as_str().unwrap();
    let r = t.call("PATCH", &format!("/api/auth/passkeys/{id}"), Some(json!({"name": "phone"})), &site).await;
    assert_eq!(r.status, 200);
    assert_eq!(t.call("GET", "/api/auth/me", None, &site).await.json()["passkeys"][0]["name"], "phone");
    assert_eq!(t.call("DELETE", &format!("/api/auth/passkeys/{id}"), None, &site).await.status, 200);
    assert_eq!(t.call("GET", "/api/auth/me", None, &site).await.json()["passkeys"], json!([]));
}

#[tokio::test]
async fn a_new_password_signs_out_every_other_browser() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    let mut other = setup(Options::default()).await;
    other.router = t.router.clone();
    assert_eq!(other.post("/api/auth/login", json!({"username": "admin", "password": PW})).await.status, 200);
    let r = t.post("/api/auth/password", json!({"current": "nope", "new": "another long passphrase"})).await;
    assert_eq!((r.status.as_u16(), r.json()["error"].as_str()), (403, Some("the current password is wrong")));
    let r = t.post("/api/auth/password", json!({"current": PW, "new": "another long passphrase"})).await;
    assert_eq!(r.json(), json!({"ok": true, "ended": 1}), "{}", r.text);
    assert_eq!(other.get("/api/auth/me").await.status, 401, "the other browser is signed out");
    assert_eq!(t.get("/api/auth/me").await.status, 200, "this one stays");
}

#[tokio::test]
async fn retyping_the_password_shares_the_sign_in_limit() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    // a stolen session guessing its way to an API key
    for _ in 0..5 {
        let r = t.post("/api/auth/api-keys", json!({"name": "x", "password": "guess"})).await;
        assert_eq!(r.status, 403, "{}", r.text);
    }
    let r = t.post("/api/auth/api-keys", json!({"name": "x", "password": PW})).await;
    assert_eq!(r.status, 429, "{}", r.text);
    assert!(r.json()["error"].as_str().unwrap().starts_with("too many failed sign-ins"));
    assert_eq!(t.get("/api/auth/me").await.json()["api_keys"], json!([]));
}

#[tokio::test]
async fn an_arr_webhook_needs_no_session() {
    let mut t = setup(Options::default()).await;
    t.create_account().await;
    let mut raw = setup(Options::default()).await;
    raw.router = t.router.clone();
    // past the guard, to the token check (a cross-site POST without cookies is fine here)
    let r = raw
        .call("POST", "/api/hooks/radarr", Some(json!({"eventType": "Test"})), &[("origin", "http://elsewhere")])
        .await;
    assert_eq!((r.status.as_u16(), r.json()), (401, json!({"error": "unknown app or wrong credentials"})));
    // only POST, only there
    assert_eq!(raw.get("/api/hooks/radarr").await.json()["error"], "sign in first (or send an X-Api-Key header)");
    assert_eq!(raw.get("/api/keys").await.status, 401);
    assert_eq!(raw.get("/api/events/stream").await.status, 401);
}
