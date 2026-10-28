// SPDX-License-Identifier: GPL-3.0-or-later
//! The wizard's own small web server (not the main router): the page, the
//! static files, and /api/setup/*. With a token, only a browser that opened
//! `/?t=<token>` (which sets a cookie) gets in; anyone who can reach the port
//! could otherwise rewrite .env.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use super::env::{self, Answers, ENV_KEYS_SECRET, KEEP, VpnCheck, check_values, read_env, render_env};
use super::host::{self, Env, Via};
use super::input;
use super::scan::scan_backups;
use super::write::{AuthWriteReq, WriteReq};
use crate::error::{Error, Result};
use crate::web::{ApiError, ApiResult};
use crate::web::{Body, tpl};

pub const TOKEN_COOKIE: &str = "wranglarr_setup";

const LOCKED_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>wranglarr setup</title>
<link rel="stylesheet" href="/static/style.css"></head>
<body><main style="max-width:32rem;margin:4rem auto;padding:0 1rem">
<h1>wranglarr setup</h1>
<p>Open the link the wizard printed in its terminal. It ends in <code>?t=</code> and a code.</p>
<form method="get" action="/"><label>Or type the code
<input name="t" autocomplete="off" autofocus></label> <button type="submit">Open</button></form>
%s</main></body></html>"#;

/// One running wizard.
pub struct Wizard {
    /// The stack directory (resolved): .env lands here.
    pub target: PathBuf,
    /// `--auth`: only the app-logins step.
    pub auth_only: bool,
    pub token: Option<String>,
    pub env: Env,
}

type W = Arc<Wizard>;

impl Wizard {
    fn has_token(&self, v: Option<&str>) -> bool {
        match &self.token {
            None => true,
            Some(t) => v.is_some_and(|v| !v.is_empty() && crate::auth::ct_eq_str(v, t)),
        }
    }

    pub(super) fn env_path(&self) -> PathBuf {
        self.target.join(".env")
    }

    /// The .env as it is now, for KEEP (an unreadable one counts as empty here).
    fn env_loader(&self) -> impl FnMut() -> BTreeMap<String, String> + '_ {
        move || read_env(&self.env_path()).unwrap_or_default()
    }

    pub(super) fn unmask(&self, a: &mut Answers) {
        a.unmask(&mut self.env_loader());
    }

    /// The appdata folder as this process sees it (mount-aware).
    pub fn appdata_dir(&self, p: &str) -> Result<PathBuf> {
        match host::container_path(&self.env, &self.target, "appdata", p) {
            (Some(cp), _) => Ok(cp),
            (None, _) => Err(Error::invalid(format!(
                "{p} is not visible inside the setup container; add -v {p}:{} to docker run",
                self.env.mount("appdata").unwrap_or("")
            ))),
        }
    }
}

pub fn router(w: Wizard) -> Router {
    let w: W = Arc::new(w);
    Router::new()
        .route("/", get(page))
        .route("/static/{*path}", get(crate::web::static_file))
        .route("/api/setup/defaults", get(defaults))
        .route("/api/setup/check-path", post(check_path))
        .route("/api/setup/generate", post(generate))
        .route("/api/setup/check-vpn", post(check_vpn))
        .route("/api/setup/scan-backups", post(scan))
        .route("/api/setup/preview", post(preview))
        .route("/api/setup/preview-auth", post(preview_auth))
        .route("/api/setup/write-auth", post(write_auth))
        .route("/api/setup/write", post(write))
        .fallback(|| async { ApiError(Error::not_found("Not Found")) })
        .method_not_allowed_fallback(|| async {
            (StatusCode::METHOD_NOT_ALLOWED, axum::Json(json!({"error": "Method Not Allowed"})))
        })
        .layer(middleware::from_fn_with_state(w.clone(), gate))
        .with_state(w)
}

fn cookie<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .filter(|(k, _)| *k == name)
        .map(|(_, v)| v.trim_matches('"'))
        .next_back()
}

/// A change must come from the wizard's own page: its Origin (or, without one, its
/// Referer) is exactly this server, scheme, host and port as the browser addressed it.
/// The token cookie alone doesn't show that: SameSite=Strict still sends it from
/// another port on the same host, or a sibling name on the same site.
pub fn from_wizard_page(h: &HeaderMap) -> bool {
    let get = |name| h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("").trim();
    let src = match get(header::ORIGIN) {
        "" => get(header::REFERER),
        o => o,
    };
    let host = get(header::HOST);
    let Ok(sent) = url::Url::parse(src) else { return false };
    if host.is_empty() || !matches!(sent.scheme(), "http" | "https") {
        return false;
    }
    // the same parser normalises both (case, default ports, IDN), so only a real match compares equal
    url::Url::parse(&format!("{}://{host}/", sent.scheme())).is_ok_and(|me| {
        me.origin() == sent.origin() && me.path() == "/" && me.username().is_empty() && me.password().is_none()
    })
}

async fn gate(State(w): State<W>, req: Request, next: Next) -> Response {
    let p = req.uri().path();
    if !(p.starts_with("/static/") || p == "/" || w.has_token(cookie(req.headers(), TOKEN_COOKIE))) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "open the link the setup wizard printed in its terminal"})),
        )
            .into_response();
    }
    let safe = matches!(*req.method(), axum::http::Method::GET | axum::http::Method::HEAD);
    if !safe && !from_wizard_page(req.headers()) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({"error": "refused: this request didn't come from the setup wizard's own page"})),
        )
            .into_response();
    }
    next.run(req).await
}

fn locked(extra: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Html(LOCKED_PAGE.replace("%s", extra))).into_response()
}

async fn page(State(w): State<W>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap) -> Response {
    if let (Some(token), Some(t)) = (&w.token, q.get("t")) {
        if !w.has_token(Some(t.trim())) {
            return locked("<p class=\"err\">That code doesn't match.</p>");
        }
        let mut r = (StatusCode::SEE_OTHER, [(header::LOCATION, "/")]).into_response();
        if let Ok(v) = HeaderValue::from_str(&format!("{TOKEN_COOKIE}={token}; HttpOnly; Path=/; SameSite=strict")) {
            r.headers_mut().insert(header::SET_COOKIE, v);
        }
        return r;
    }
    if !w.has_token(cookie(&headers, TOKEN_COOKIE)) {
        return locked("");
    }
    let ctx = minijinja::context! { target => w.target.to_string_lossy(), auth_only => w.auth_only };
    match tpl::render("setup.html", ctx) {
        Ok(h) => Html(h).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Run blocking filesystem work off the async threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await?
}

async fn defaults(State(w): State<W>) -> ApiResult<axum::Json<Value>> {
    blocking(move || defaults_json(&w)).await.map(axum::Json).map_err(Into::into)
}

pub fn defaults_json(w: &Wizard) -> Result<Value> {
    let path = w.env_path();
    let env = read_env(&path)?;
    let mounts: BTreeMap<&str, bool> = if w.env.in_docker {
        w.env.mounts.iter().map(|(k, v)| (*k, Path::new(v).exists())).collect()
    } else {
        BTreeMap::new()
    };
    // Running as root (plain docker run, sudo on TrueNAS): never suggest running the apps as root.
    // A ZFS dataset under the data mount means TrueNAS SCALE, whose apps user is 568.
    let me = rustix::process::getuid().as_raw();
    let (mut uid, mut gid) = (me, rustix::process::getgid().as_raw());
    if me == 0 {
        let dm = if w.env.in_docker && mounts.get("data_root") == Some(&true) {
            host::mount_of(Path::new(w.env.mount("data_root").unwrap_or("")))
        } else {
            BTreeMap::new()
        };
        let zfs = dm.get("fstype").map(String::as_str) == Some("zfs")
            && !dm.get("source").map(String::as_str).unwrap_or("").starts_with('/');
        (uid, gid) = if zfs { (568, 568) } else { (1000, 1000) };
    }
    let shown: BTreeMap<String, String> = env
        .into_iter()
        .map(|(k, v)| {
            let v = if ENV_KEYS_SECRET.contains(&k.as_str()) && !v.is_empty() { KEEP.to_string() } else { v };
            (k, v)
        })
        .collect();
    let mut compose: Vec<String> = std::fs::read_dir(&w.target)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .filter(|n| n.starts_with("compose") && n.ends_with(".yaml"))
                .collect()
        })
        .unwrap_or_default();
    compose.sort();
    let tz = w.env.tz.clone().unwrap_or_else(|| if w.env.in_docker { "UTC".into() } else { host::host_tz() });
    Ok(json!({
        "target": w.target.to_string_lossy(),
        "existing_env": if shown.is_empty() { Value::Null } else { json!(shown) },
        "auth_only": w.auth_only,
        "env_exists": path.exists(),
        "in_docker": w.env.in_docker,
        "mounts": mounts,
        "host": {"hostname": host::hostname(), "uid": uid, "gid": gid, "root": me == 0,
                 "users": if w.env.in_docker { vec![] } else { host::host_users() },
                 "tz": tz, "compose_files": compose},
        "services": env::services_json(),
    }))
}

#[derive(Deserialize)]
pub struct PathCheck {
    pub path: String,
    /// appdata | data_root | data_root_2 (decides which mount to inspect)
    #[serde(default)]
    pub key: String,
    #[serde(default = "thousand", deserialize_with = "input::whole_number")]
    pub uid: i64,
    #[serde(default = "thousand", deserialize_with = "input::whole_number")]
    pub gid: i64,
    #[serde(default, deserialize_with = "input::flag")]
    pub create: bool,
}

fn thousand() -> i64 {
    1000
}

async fn check_path(State(w): State<W>, Body(body): Body<PathCheck>) -> ApiResult<axum::Json<Value>> {
    blocking(move || Ok(check_path_json(&w, &body))).await.map(axum::Json).map_err(Into::into)
}

pub fn check_path_json(w: &Wizard, body: &PathCheck) -> Value {
    let key = if body.key.is_empty() { "appdata" } else { body.key.as_str() };
    let (p, how) = host::container_path(&w.env, &w.target, key, &body.path);
    let Some(p) = p else {
        if how == Via::Empty {
            return json!({"path": body.path, "skipped": true});
        }
        let mount = w.env.mount(key).unwrap_or("/mnt");
        return json!({"path": body.path, "exists": null, "unverified": true,
                      "note": format!("not visible from the setup container; to verify it add  -v {}:{mount}  to the docker run command", body.path)});
    };
    let mut res = json!({"path": body.path, "checked": p.to_string_lossy(), "via": how.as_str(),
                         "exists": p.exists(), "is_dir": p.is_dir(), "created": false});
    if !p.exists() && body.create {
        let made = std::fs::create_dir_all(&p).and_then(|()| {
            res["exists"] = json!(true);
            res["is_dir"] = json!(true);
            res["created"] = json!(true);
            match host::chown(&p, body.uid, body.gid) {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    res["note"] =
                        json!(format!("created, but could not chown to {}:{} (not root)", body.uid, body.gid));
                    Ok(())
                }
                other => other,
            }
        });
        if let Err(e) = made {
            res["error"] = json!(format!("{e}; create it on the host instead: mkdir -p {}", body.path));
            return res;
        }
    }
    if let Ok(st) = std::fs::metadata(&p) {
        let writable = host::can_access(&st, body.uid, body.gid, true);
        res["owner_uid"] = json!(st.uid());
        res["owner_gid"] = json!(st.gid());
        res["mode"] = json!(format!("0o{:o}", st.mode() & 0o777));
        res["readable"] = json!(host::can_access(&st, body.uid, body.gid, false));
        res["writable"] = json!(writable);
        if let Some((free, total)) = host::disk_usage(&p) {
            res["free_bytes"] = json!(free);
            res["total_bytes"] = json!(total);
        }
        res["mount"] = json!(host::ismount(&p));
        if st.is_dir() {
            res["notes"] = json!(host::storage_notes(&p, key, body.uid, body.gid, writable));
        }
    }
    res
}

#[derive(Deserialize)]
pub struct Generate {
    #[serde(default = "password")]
    pub kind: String,
}

fn password() -> String {
    "password".into()
}

async fn generate(Body(body): Body<Generate>) -> ApiResult<axum::Json<Value>> {
    let value = if body.kind == "apikey" { api_key() } else { crate::ids::random_token(24) };
    Ok(axum::Json(json!({"value": value})))
}

/// A new API key: 32 random hex digits, the form the arrs use.
fn api_key() -> String {
    let mut b = [0u8; 16];
    rand::fill(&mut b);
    hex::encode(b)
}

async fn check_vpn(State(w): State<W>, Body(mut body): Body<VpnCheck>) -> ApiResult<axum::Json<Value>> {
    blocking(move || {
        body.unmask(&mut w.env_loader());
        Ok(check_vpn_json(&body))
    })
    .await
    .map(axum::Json)
    .map_err(Into::into)
}

pub fn check_vpn_json(b: &VpnCheck) -> Value {
    let (mut errors, mut warnings): (Vec<String>, Vec<String>) = (vec![], vec![]);
    if b.mode == "none" {
        return json!({"errors": [], "warnings": []});
    }
    if !b.lan_network.is_empty() {
        for n in b.lan_network.split(',') {
            if !wranglarr_core::net::is_network(n.trim()) {
                errors.push(format!("LAN network {:?} is not a CIDR", n.trim()));
            }
        }
    }
    if b.mode == "shared" {
        if b.vpn_type == "wireguard" {
            if b.provider == "custom" {
                if b.private_key.is_empty() {
                    warnings.push("no private key: you must drop a wg0.conf at appdata/vpn/wireguard/wg0.conf".into());
                } else {
                    for (label, k) in [("private", &b.private_key), ("public", &b.public_key)] {
                        if !host::valid_wg_key(k) {
                            errors.push(format!("WireGuard {label} key is not a valid 32-byte base64 key"));
                        }
                    }
                    if !b.preshared_key.is_empty() && !host::valid_wg_key(&b.preshared_key) {
                        errors.push("WireGuard preshared key is invalid".into());
                    }
                    if b.endpoint_ip.parse::<std::net::IpAddr>().is_err() {
                        errors.push("endpoint must be an IP address (gluetun does not resolve hostnames)".into());
                    }
                    if b.addresses.is_empty() {
                        errors.push("WIREGUARD_ADDRESSES (your tunnel address, e.g. 10.2.0.2/32) is required".into());
                    }
                }
            } else {
                if b.private_key.is_empty() {
                    errors.push(format!("{}: WireGuard private key is required", b.provider));
                }
                if ["protonvpn", "private internet access"].contains(&b.provider.as_str())
                    && (b.user.is_empty() || b.password.is_empty())
                {
                    warnings.push(format!("{}: account username/password also needed for port forwarding", b.provider));
                }
            }
        } else if b.provider != "custom" && (b.user.is_empty() || b.password.is_empty()) {
            errors.push("OpenVPN with a provider needs the account username and password".into());
        }
    }
    json!({"errors": errors, "warnings": warnings})
}

#[derive(Deserialize)]
pub struct Scan {
    #[serde(default = "appdata")]
    pub appdata: String,
}

fn appdata() -> String {
    "./appdata".into()
}

async fn scan(State(w): State<W>, Body(body): Body<Scan>) -> ApiResult<axum::Json<Value>> {
    blocking(move || Ok(scan_backups(&w.appdata_dir(&body.appdata)?))).await.map(axum::Json).map_err(Into::into)
}

async fn preview(State(w): State<W>, Body(mut a): Body<Answers>) -> ApiResult<axum::Json<Value>> {
    blocking(move || {
        w.unmask(&mut a);
        check_values(&a)?;
        Ok(json!({"env": env::mask_secrets(&render_env(&a, &w.env.image)?)}))
    })
    .await
    .map(axum::Json)
    .map_err(Into::into)
}

async fn preview_auth(State(w): State<W>, Body(a): Body<Answers>) -> ApiResult<axum::Json<Value>> {
    blocking(move || w.preview_auth(a)).await.map(axum::Json).map_err(Into::into)
}

async fn write_auth(State(w): State<W>, Body(req): Body<AuthWriteReq>) -> ApiResult<axum::Json<Value>> {
    blocking(move || w.write_auth(req.answers)).await.map(axum::Json).map_err(Into::into)
}

async fn write(State(w): State<W>, Body(req): Body<WriteReq>) -> ApiResult<axum::Json<Value>> {
    blocking(move || w.write(req)).await.map(axum::Json).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_need_the_wizards_own_origin() {
        let h = |pairs: &[(&'static str, &'static str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.insert(*k, HeaderValue::from_static(v));
            }
            h
        };
        let host = ("host", "192.168.1.5:8000");
        for ok in [
            h(&[host, ("origin", "http://192.168.1.5:8000")]),
            h(&[host, ("origin", "HTTP://192.168.1.5:8000")]),
            h(&[host, ("referer", "http://192.168.1.5:8000/?step=3")]),
            h(&[("host", "Setup.LAN"), ("origin", "http://setup.lan:80")]),
        ] {
            assert!(from_wizard_page(&ok), "{ok:?}");
        }
        for bad in [
            h(&[host]),
            h(&[host, ("origin", "null")]),
            // another port on the same host: SameSite=Strict still sends the cookie from there
            h(&[host, ("origin", "http://192.168.1.5:7878")]),
            h(&[host, ("origin", "http://192.168.1.5")]),
            h(&[host, ("origin", "http://evil.example")]),
            h(&[host, ("referer", "http://192.168.1.5:7878/")]),
            h(&[("origin", "http://192.168.1.5:8000")]),
            h(&[("host", "192.168.1.5:8000/x"), ("origin", "http://192.168.1.5:8000")]),
            h(&[("host", "a@192.168.1.5:8000"), ("origin", "http://192.168.1.5:8000")]),
            h(&[host, ("origin", "file://192.168.1.5:8000")]),
        ] {
            assert!(!from_wizard_page(&bad), "{bad:?}");
        }
    }

    #[test]
    fn cookies() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; wranglarr_setup=tok; b=2"));
        assert_eq!(cookie(&h, TOKEN_COOKIE), Some("tok"));
        assert_eq!(cookie(&h, "c"), None);
    }

    #[test]
    fn vpn_checks() {
        let b = VpnCheck { mode: "shared".into(), lan_network: "10.0.0.0/8, lan".into(), ..Default::default() };
        let r = check_vpn_json(&b);
        assert_eq!(r["errors"], json!([r#"LAN network "lan" is not a CIDR"#]));
        assert_eq!(
            r["warnings"],
            json!(["no private key: you must drop a wg0.conf at appdata/vpn/wireguard/wg0.conf"])
        );
        let b = VpnCheck { mode: "shared".into(), private_key: "x".into(), ..Default::default() };
        assert_eq!(check_vpn_json(&b)["errors"].as_array().map(Vec::len), Some(4));
        let b = VpnCheck {
            mode: "shared".into(),
            vpn_type: "openvpn".into(),
            provider: "mullvad".into(),
            ..Default::default()
        };
        assert_eq!(
            check_vpn_json(&b)["errors"],
            json!(["OpenVPN with a provider needs the account username and password"])
        );
    }
}
