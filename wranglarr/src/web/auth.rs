// SPDX-License-Identifier: GPL-3.0-or-later
//! Login pages, the account page, /api/auth/*, and the guard in front of everything else.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};

use axum::Router;
use axum::extract::{ConnectInfo, FromRequestParts, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, patch, post};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use url::Url;

use super::{Body, Ctx, Page, Q, Who, header_str, page, render, rp_id};
use crate::app::App;
use crate::auth::{Auth, SESSION_COOKIE, SignIn};
use crate::deployment::TrustedProxies;
use crate::error::{Error, Result};
use crate::format::{quote, quote_path};
use crate::web::{ApiError, ApiResult};

pub const MFA_COOKIE: &str = "wranglarr_mfa";
/// Reachable without a session (the POSTs still need to come from wranglarr's own pages).
pub const PUBLIC: &[&str] = &[
    "/login",
    "/setup",
    "/api/auth/setup",
    "/api/auth/login",
    "/api/auth/login/totp",
    "/api/auth/passkey/login-options",
    "/api/auth/passkey/login",
    "/api/health",
    "/api/auth/gate",
];
const REFUSED: &str = "refused: this request didn't come from wranglarr's own pages";

// ---- request helpers -------------------------------------------------------------------

/// Apply the deployment's trust boundary before forwarded metadata can affect
/// CSRF, passkeys, cookies, or any page's request context.
fn filter_forwarded(h: &mut HeaderMap, peer: Option<IpAddr>, trusted: &TrustedProxies) {
    if !trusted.accepts(peer) {
        h.remove("x-forwarded-host");
        h.remove("x-forwarded-proto");
    }
}

/// Headers passed to these helpers have been filtered by the login guard.
pub fn is_https(h: &HeaderMap) -> bool {
    header_str(h, "x-forwarded-proto").split(',').next().unwrap_or("").trim() == "https"
}

/// This request's origin as wranglarr sees it: https when X-Forwarded-Proto says
/// so (else http), and X-Forwarded-Host's first entry (else Host), port and all;
/// the same headers [`rp_id`] reads. Passkey ceremonies are bound to it.
pub fn request_origin(h: &HeaderMap) -> String {
    let scheme = if is_https(h) { "https" } else { "http" };
    let fwd = header_str(h, "x-forwarded-host").split(',').next().unwrap_or("").trim();
    let host = if fwd.is_empty() { header_str(h, "host").trim() } else { fwd };
    format!("{scheme}://{host}")
}

/// The host names this request was addressed to (direct, or through a reverse proxy).
pub fn hosts(h: &HeaderMap) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    out.insert(header_str(h, "host").to_string());
    out.extend(header_str(h, "x-forwarded-host").split(',').map(|s| s.trim().to_string()));
    out.into_iter().filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect()
}

/// Where a page URL points: `host[:port]`, as a Host header writes it; None when
/// it isn't an http(s) URL.
fn authority(url: &str) -> Option<String> {
    let u = Url::parse(url).ok().filter(|u| matches!(u.scheme(), "http" | "https"))?;
    let host = u.host_str().filter(|h| !h.is_empty())?;
    Some(match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    })
}

/// The host name in a Host header value (`host[:port]`): lower case, IPv6 without
/// brackets; "" when there's none.
fn host_name(host_header: &str) -> String {
    crate::links::parse_url(&format!("http://{host_header}/"))
        .as_ref()
        .and_then(crate::links::url_host)
        .unwrap_or_default()
}

/// A state-changing request must come from a page of this wranglarr (CSRF).
/// Browsers always send Origin on POST/PUT/DELETE from scripts and forms.
pub fn same_origin(h: &HeaderMap) -> bool {
    let origin = header_str(h, "origin");
    let src = if origin.is_empty() { header_str(h, "referer") } else { origin };
    authority(src).is_some_and(|a| hosts(h).contains(&a))
}

pub fn under(host: &str, domain: &str) -> bool {
    !host.is_empty() && !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// Where a sign-in may send the browser: a path on this wranglarr, or (AuthTunnel) back
/// to the app the gate sent you from, if the cookie reaches it.
///
/// Read the way the browser will (the WHATWG URL parser): it drops tabs and newlines and
/// reads `\` as `/`, so `/\t/evil.example/` would become `//evil.example/`. Neither ever
/// appears in a URL wranglarr makes, so a target with a control character or a backslash
/// is refused outright; an absolute one comes back as the parser normalised it.
pub fn safe_next(n: &str, auth: &Auth, me: &str) -> String {
    const HOME: &str = "/";
    if n.chars().any(|c| c.is_control() || c == '\\') {
        return HOME.into();
    }
    if n.starts_with('/') && !n.starts_with("//") {
        // can't leave this origin once the above is ruled out; checked anyway, as the browser sees it
        let base = Url::parse("http://wranglarr.invalid/").ok();
        let local = base.is_some_and(|b| b.join(n).is_ok_and(|u| u.origin() == b.origin()));
        return if local { n.to_string() } else { HOME.into() };
    }
    let o = auth.options();
    if o.tunnel
        && let Ok(u) = Url::parse(n)
        && matches!(u.scheme(), "http" | "https")
    {
        let h = crate::links::url_host(&u).unwrap_or_default();
        let me = me.to_lowercase();
        if !h.is_empty() && (h == me || (under(&h, &o.cookie_domain) && under(&me, &o.cookie_domain))) {
            return u.into();
        }
    }
    HOME.into()
}

/// Percent-decoding (a bad escape stays as it is).
pub fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The last value of a cookie in the Cookie header(s).
pub fn cookie(h: &HeaderMap, name: &str) -> String {
    let mut out = String::new();
    for v in h.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()) {
        for part in v.split(';') {
            if let Some((k, val)) = part.split_once('=')
                && k.trim() == name
            {
                let val = val.trim();
                out = val.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(val).to_string();
            }
        }
    }
    out
}

/// A Set-Cookie value, its attributes sorted by name.
fn set_cookie(
    name: &str,
    value: &str,
    max_age: Option<i64>,
    samesite: &str,
    secure: bool,
    domain: Option<&str>,
) -> String {
    let mut s = format!("{name}={value}");
    if let Some(d) = domain {
        s.push_str(&format!("; Domain={d}"));
    }
    s.push_str("; HttpOnly");
    if let Some(a) = max_age {
        s.push_str(&format!("; Max-Age={a}"));
    }
    s.push_str(&format!("; Path=/; SameSite={samesite}"));
    if secure {
        s.push_str("; Secure");
    }
    s
}

fn delete_cookie(name: &str, domain: Option<&str>) -> String {
    let d = domain.map(|d| format!("; Domain={d}")).unwrap_or_default();
    format!("{name}=\"\"{d}; expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0; Path=/; SameSite=lax")
}

fn add_cookie(r: &mut Response, c: &str) {
    if let Ok(v) = HeaderValue::from_str(c) {
        r.headers_mut().append(header::SET_COOKIE, v);
    }
}

/// What the login handlers need from a request.
pub struct AuthReq {
    pub headers: HeaderMap,
    pub peer: Option<IpAddr>,
    pub who: Who,
    /// FORWARDED_ALLOW_IPS, from the deployment.
    pub trusted: TrustedProxies,
}

impl FromRequestParts<App> for AuthReq {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(p: &mut Parts, app: &App) -> std::result::Result<AuthReq, Self::Rejection> {
        let peer = p.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        let trusted = app.deployment().trusted_proxies.clone();
        let mut headers = p.headers.clone();
        filter_forwarded(&mut headers, peer, &trusted);
        Ok(AuthReq { headers, peer, who: p.extensions.get::<Who>().cloned().unwrap_or_default(), trusted })
    }
}

impl AuthReq {
    /// The client's address. Behind a trusted proxy it is the last X-Forwarded-For
    /// hop that isn't a trusted proxy (the first hop when every proxy is trusted).
    pub fn ip(&self) -> String {
        self.trusted.client_ip(self.peer, header_str(&self.headers, "x-forwarded-for"))
    }
    pub fn https(&self) -> bool {
        is_https(&self.headers)
    }
    pub fn rp_id(&self) -> String {
        rp_id(&self.headers)
    }
    pub fn origin(&self) -> String {
        request_origin(&self.headers)
    }
    pub fn header(&self, name: &str) -> &str {
        header_str(&self.headers, name)
    }
    fn session(&self) -> Result<()> {
        if self.who.via != "session" {
            return Err(Error::Forbidden("this needs a signed-in browser, not an API key".into()));
        }
        Ok(())
    }
}

/// The cookie's Domain: AUTH_COOKIE_DOMAIN, when wranglarr is opened under it.
fn domain_for(auth: &Auth, r: &AuthReq) -> Option<String> {
    let d = auth.options().cookie_domain;
    under(&r.rp_id().to_lowercase(), &d).then_some(d)
}

async fn blocking<T: Send + 'static>(app: &App, f: impl FnOnce(&App) -> Result<T> + Send + 'static) -> Result<T> {
    let a = app.clone();
    tokio::task::spawn_blocking(move || f(&a)).await?
}

async fn signed_in(app: &App, r: &AuthReq, method: &'static str, remember: bool) -> Result<Response> {
    let (ip, agent) = (r.ip(), r.header("user-agent").to_string());
    let (token, max_age) = blocking(app, move |a| a.auth().new_session(method, &ip, &agent, remember)).await?;
    // Lax in tunnel mode: a link to an app from elsewhere (a bookmark page, a chat) must carry it
    let samesite = if app.auth().options().tunnel { "lax" } else { "strict" };
    let mut resp = axum::Json(json!({"ok": true})).into_response();
    let domain = domain_for(app.auth(), r);
    add_cookie(&mut resp, &set_cookie(SESSION_COOKIE, &token, max_age, samesite, r.https(), domain.as_deref()));
    add_cookie(&mut resp, &delete_cookie(MFA_COOKIE, None));
    Ok(resp)
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(json!({"error": msg}))).into_response()
}

// ---- the guard ---------------------------------------------------------------------------

/// In front of everything: sets [`Who`] for signed-in requests (session cookie
/// or X-Api-Key), refuses cross-site changes, and sends everyone else to sign in.
pub async fn guard(State(app): State<App>, mut req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    filter_forwarded(req.headers_mut(), peer, &app.deployment().trusted_proxies);
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let h = req.headers();
    if !app.auth().enabled() {
        // no login, but still no cross-site changes: a browser always sends Origin (or Referer) with
        // them, while scripts send neither and keep working
        let cross = !header_str(h, "origin").is_empty() || !header_str(h, "referer").is_empty();
        if !safe && cross && !same_origin(h) {
            return json_error(StatusCode::FORBIDDEN, REFUSED);
        }
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    if path.starts_with("/static/") {
        return next.run(req).await;
    }
    // an arr's webhook: no session, signed with that app's own token (checked by the handler)
    if req.method() == Method::POST && path.starts_with("/api/hooks/") {
        return next.run(req).await;
    }
    let token = cookie(h, SESSION_COOKIE);
    let api_key = header_str(h, "x-api-key").to_string();
    // SQLite (and its busy timeout) off the async workers
    let t = token.clone();
    let id = match app.blocking(move |a| a.auth().identify(&t, &api_key)).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("auth: can't check the sign-in: {e}");
            return ApiError(e).into_response();
        }
    };
    let h = req.headers();
    if let Some((user, via)) = id.user {
        let sess = via == "session";
        if sess && !safe && !same_origin(h) {
            return json_error(StatusCode::FORBIDDEN, REFUSED);
        }
        let who = Who { user: Some(user), via, token: if sess { token } else { String::new() } };
        req.extensions_mut().insert(who);
        return next.run(req).await;
    }
    if PUBLIC.contains(&path.as_str()) {
        if !safe && !same_origin(h) {
            return json_error(StatusCode::FORBIDDEN, REFUSED);
        }
        return next.run(req).await;
    }
    if path.starts_with("/api/") {
        return json_error(StatusCode::UNAUTHORIZED, "sign in first (or send an X-Api-Key header)");
    }
    if !id.has_account {
        return Redirect::to("/setup").into_response();
    }
    // `next` is the decoded path, quoted again
    let next_q = if path == "/" { String::new() } else { format!("?next={}", quote_path(&unquote(&path))) };
    Redirect::to(&format!("/login{next_q}")).into_response()
}

// ---- routes --------------------------------------------------------------------------------

pub fn routes() -> Router<App> {
    Router::new()
        .route("/setup", get(setup_page))
        .route("/login", get(login_page))
        .route("/account", get(account_page))
        .route("/api/auth/gate", get(api_gate))
        .route("/api/auth/setup", post(api_setup))
        .route("/api/auth/login", post(api_login))
        .route("/api/auth/login/totp", post(api_login_totp))
        .route("/api/auth/passkey/login-options", get(api_pk_login_options))
        .route("/api/auth/passkey/login", post(api_pk_login))
        .route("/api/auth/logout", post(api_logout))
        .route("/api/auth/me", get(api_me))
        .route("/api/auth/password", post(api_password))
        .route("/api/auth/totp/begin", post(api_totp_begin))
        .route("/api/auth/totp/enable", post(api_totp_enable))
        .route("/api/auth/totp/disable", post(api_totp_disable))
        .route("/api/auth/recovery", post(api_recovery))
        .route("/api/auth/passkey/register-options", post(api_pk_reg_options))
        .route("/api/auth/passkey/register", post(api_pk_register))
        .route("/api/auth/passkeys/{pid}", patch(api_pk_rename).delete(api_pk_delete))
        .route("/api/auth/api-keys", post(api_key_create))
        .route("/api/auth/api-keys/{kid}", delete(api_key_revoke))
        .route("/api/auth/sessions/{sid}", delete(api_session_end))
        .route("/api/auth/sessions/end-others", post(api_sessions_end_others))
}

#[derive(Deserialize)]
struct SetupIn {
    code: String,
    #[serde(default = "admin")]
    username: String,
    password: String,
}

fn admin() -> String {
    "admin".into()
}

#[derive(Deserialize)]
struct LoginIn {
    username: String,
    password: String,
    #[serde(default)]
    remember: bool,
}

#[derive(Deserialize)]
struct CodeIn {
    code: String,
}

#[derive(Deserialize)]
struct PasswordIn {
    password: String,
}

#[derive(Deserialize)]
struct ChangePasswordIn {
    current: String,
    new: String,
}

#[derive(Deserialize)]
struct PasskeyIn {
    flow: String,
    credential: Map<String, Value>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    remember: bool,
}

#[derive(Deserialize, Default)]
struct NameIn {
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
}

// ---- pages ---------------------------------------------------------------------------------

async fn setup_page(State(app): State<App>) -> ApiResult<Response> {
    // "" once an account exists
    let code = blocking(&app, |a| a.auth().ensure_setup_code()).await?;
    if code.is_empty() {
        return Ok(Redirect::to("/login").into_response());
    }
    Ok(render("auth.html", &json!({"mode": "setup"}))?.into_response())
}

async fn login_page(State(app): State<App>, r: AuthReq, q: Q) -> ApiResult<Response> {
    let Some(acct) = blocking(&app, |a| a.auth().account()).await? else {
        return Ok(Redirect::to("/setup").into_response());
    };
    let next = safe_next(q.get("next").unwrap_or("/"), app.auth(), &r.rp_id());
    if r.who.user.is_some() {
        return Ok(Redirect::to(&next).into_response());
    }
    let has_pk = !crate::auth::passkey::rows(&acct).is_empty();
    Ok(render("auth.html", &json!({"mode": "login", "next": next, "has_passkeys": has_pk}))?.into_response())
}

async fn account_page(State(app): State<App>, ctx: Ctx) -> ApiResult<Response> {
    if !app.auth().enabled() {
        return Ok(Redirect::to("/system").into_response());
    }
    let p: Page =
        page(&app, &ctx, "account.html", "account", json!({"rp_id": ctx.host, "https": is_https(&ctx.headers)}))
            .await
            .map_err(Into::into);
    Ok(p?.into_response())
}

// ---- AuthTunnel: wranglarr-gate's auth_request ------------------------------------------------

/// The API key of the app wranglarr-gate fronts as `name` (by name, else by kind).
pub async fn gate_key(app: &App, name: &str) -> String {
    let a = app.cfg().all_apps().find(|a| a.name == name).or_else(|| app.cfg().all_apps().find(|a| a.kind == name));
    match a {
        Some(a) => crate::service::api_key(a).await.unwrap_or_default(),
        None => String::new(),
    }
}

/// 204: let the request through to the app. 401: not signed in; X-Wranglarr-Login
/// says where to send the browser (the gate answers with a redirect there).
async fn api_gate(State(app): State<App>, r: AuthReq) -> ApiResult<Response> {
    let o = app.auth().options();
    if !o.tunnel {
        return Err(Error::not_found("AuthTunnel is off (APPS_AUTH isn't tunnel)").into());
    }
    if !o.enabled || r.who.via == "session" {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    // an API client (mobile app, script) with that app's own API key: through, but only if the
    // key is the real one, or anyone could walk past the gate with apikey=x
    let sent: Vec<&str> =
        [r.header("x-wranglarr-key"), r.header("x-wranglarr-key-arg")].into_iter().filter(|k| !k.is_empty()).collect();
    if !sent.is_empty() {
        let real = gate_key(&app, r.header("x-wranglarr-app")).await;
        if !real.is_empty() && sent.iter().any(|s| crate::auth::ct_eq_str(s, &real)) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
    }
    let first = |k: &str| r.header(k).split(',').next().unwrap_or("").trim().to_string();
    let mut proto = first("x-original-proto").to_lowercase();
    if proto.is_empty() {
        proto = "http".into();
    }
    if proto != "http" && proto != "https" {
        proto = "http".into();
    }
    let host = first("x-original-host");
    let mut uri = r.header("x-original-uri").to_string();
    if !uri.starts_with('/') {
        uri = "/".into();
    }
    let name = host_name(&host);
    // AUTH_LOGIN_URL is wranglarr on the reverse proxy: its cookie reaches only names under
    // AUTH_COOKIE_DOMAIN. An app opened another way (by IP and port) signs in at wranglarr
    // on that same address instead, so both ways in keep working.
    let via_proxy = o.cookie_domain.is_empty() || name.is_empty() || under(&name, &o.cookie_domain);
    let base = if !o.login_url.is_empty() && via_proxy {
        o.login_url.clone()
    } else if !name.is_empty() {
        let n = if name.contains(':') { format!("[{name}]") } else { name };
        format!("{proto}://{n}:{}", o.port)
    } else {
        String::new()
    };
    let nxt = if host.is_empty() { "/".to_string() } else { format!("{proto}://{host}{uri}") };
    let mut resp = StatusCode::UNAUTHORIZED.into_response();
    if !base.is_empty()
        && let Ok(v) = HeaderValue::from_str(&format!("{base}/login?next={}", quote(&nxt)))
    {
        resp.headers_mut().insert("x-wranglarr-login", v);
    }
    Ok(resp)
}

// ---- sign-in ---------------------------------------------------------------------------------

async fn api_setup(State(app): State<App>, r: AuthReq, Body(b): Body<SetupIn>) -> ApiResult<Response> {
    let ip = r.ip();
    blocking(&app, move |a| {
        let _attempt = a.auth().admit(&ip)?;
        a.auth().create_account(&b.code, &b.username, &b.password).inspect_err(|e| {
            if matches!(e, Error::Forbidden(_)) {
                a.auth().failed(&ip);
            }
        })
    })
    .await?;
    signed_in(&app, &r, "password", false).await.map_err(Into::into)
}

async fn api_login(State(app): State<App>, r: AuthReq, Body(b): Body<LoginIn>) -> ApiResult<Response> {
    let ip = r.ip();
    let res = blocking(&app, move |a| a.auth().login_password(&ip, &b.username, &b.password, b.remember)).await?;
    match res {
        SignIn::Pending(token) => {
            let mut resp = axum::Json(json!({"mfa": true})).into_response();
            add_cookie(&mut resp, &set_cookie(MFA_COOKIE, &token, Some(300), "strict", r.https(), None));
            Ok(resp)
        }
        SignIn::Done { method, remember } => signed_in(&app, &r, method, remember).await.map_err(Into::into),
    }
}

async fn api_login_totp(State(app): State<App>, r: AuthReq, Body(b): Body<CodeIn>) -> ApiResult<Response> {
    let (ip, pending) = (r.ip(), cookie(&r.headers, MFA_COOKIE));
    match blocking(&app, move |a| a.auth().login_second_factor(&ip, &pending, &b.code)).await? {
        SignIn::Done { method, remember } => signed_in(&app, &r, method, remember).await.map_err(Into::into),
        SignIn::Pending(_) => Err(Error::Unauthorized("that sign-in expired; start again".into()).into()),
    }
}

async fn api_pk_login_options(State(app): State<App>, r: AuthReq) -> ApiResult<axum::Json<Value>> {
    let (rp, here, ip) = (r.rp_id(), r.origin(), r.ip());
    blocking(&app, move |a| a.auth().passkey_login_options(&rp, &here, &ip)).await.map(axum::Json).map_err(Into::into)
}

async fn api_pk_login(State(app): State<App>, r: AuthReq, Body(b): Body<PasskeyIn>) -> ApiResult<Response> {
    let (ip, sent, here) = (r.ip(), r.header("origin").to_string(), r.origin());
    let remember = b.remember;
    blocking(&app, move |a| a.auth().passkey_login(&ip, &b.flow, &Value::Object(b.credential), &sent, &here)).await?;
    signed_in(&app, &r, "passkey", remember).await.map_err(Into::into)
}

async fn api_logout(State(app): State<App>, r: AuthReq) -> ApiResult<Response> {
    let token = r.who.token.clone();
    blocking(&app, move |a| a.auth().end_session(&token)).await?;
    let mut resp = axum::Json(json!({"ok": true})).into_response();
    add_cookie(&mut resp, &delete_cookie(SESSION_COOKIE, None));
    if let Some(d) = domain_for(app.auth(), &r) {
        add_cookie(&mut resp, &delete_cookie(SESSION_COOKIE, Some(&d)));
    }
    Ok(resp)
}

// ---- the account -------------------------------------------------------------------------------

type J = ApiResult<axum::Json<Value>>;

fn ok() -> J {
    Ok(axum::Json(json!({"ok": true})))
}

async fn api_me(State(app): State<App>, r: AuthReq) -> J {
    if !app.auth().enabled() {
        return Ok(axum::Json(json!({"enabled": false})));
    }
    let (token, rp, https) = (r.who.token.clone(), r.rp_id(), r.https());
    let out = blocking(&app, move |a| {
        let mut out = json!({"enabled": true});
        if let (Some(o), Value::Object(s)) = (out.as_object_mut(), a.auth().summary()?) {
            o.extend(s);
            o.insert("sessions".into(), json!(a.auth().sessions(&token)?));
            o.insert("api_keys".into(), json!(a.auth().api_keys()?));
            o.insert("rp_id".into(), json!(rp));
            o.insert("https".into(), json!(https));
        }
        Ok(out)
    })
    .await?;
    Ok(axum::Json(out))
}

/// A new password: every other session ends (`ended` says how many).
async fn api_password(State(app): State<App>, r: AuthReq, Body(b): Body<ChangePasswordIn>) -> J {
    r.session()?;
    let (ip, keep) = (r.ip(), r.who.token.clone());
    let ended = blocking(&app, move |a| a.auth().change_password(&ip, &b.current, &b.new, &keep)).await?;
    Ok(axum::Json(json!({"ok": true, "ended": ended})))
}

async fn api_totp_begin(State(app): State<App>, r: AuthReq, Body(b): Body<PasswordIn>) -> J {
    r.session()?;
    let ip = r.ip();
    blocking(&app, move |a| a.auth().totp_begin(&a.auth().reconfirm(&ip, &b.password)?))
        .await
        .map(axum::Json)
        .map_err(Into::into)
}

async fn api_totp_enable(State(app): State<App>, r: AuthReq, Body(b): Body<CodeIn>) -> J {
    r.session()?;
    let codes = blocking(&app, move |a| a.auth().totp_enable(&b.code)).await?;
    Ok(axum::Json(json!({"recovery": codes})))
}

async fn api_totp_disable(State(app): State<App>, r: AuthReq, Body(b): Body<PasswordIn>) -> J {
    r.session()?;
    let ip = r.ip();
    blocking(&app, move |a| a.auth().totp_disable(&a.auth().reconfirm(&ip, &b.password)?)).await?;
    ok()
}

async fn api_recovery(State(app): State<App>, r: AuthReq, Body(b): Body<PasswordIn>) -> J {
    r.session()?;
    let ip = r.ip();
    let codes = blocking(&app, move |a| a.auth().new_recovery(&a.auth().reconfirm(&ip, &b.password)?)).await?;
    Ok(axum::Json(json!({"recovery": codes})))
}

async fn api_pk_reg_options(State(app): State<App>, r: AuthReq, Body(b): Body<PasswordIn>) -> J {
    r.session()?;
    let (rp, here, ip) = (r.rp_id(), r.origin(), r.ip());
    blocking(&app, move |a| {
        let ok = a.auth().reconfirm(&ip, &b.password)?;
        a.auth().passkey_register_options(&ok, &rp, &here, &ip)
    })
    .await
    .map(axum::Json)
    .map_err(Into::into)
}

async fn api_pk_register(State(app): State<App>, r: AuthReq, Body(b): Body<PasskeyIn>) -> J {
    r.session()?;
    let (sent, here) = (r.header("origin").to_string(), r.origin());
    blocking(&app, move |a| a.auth().passkey_register(&b.flow, &Value::Object(b.credential), &b.name, &sent, &here))
        .await
        .map(axum::Json)
        .map_err(Into::into)
}

async fn api_pk_rename(State(app): State<App>, r: AuthReq, Path(pid): Path<String>, Body(b): Body<NameIn>) -> J {
    r.session()?;
    blocking(&app, move |a| a.auth().passkey_rename(&pid, &b.name)).await?;
    ok()
}

async fn api_pk_delete(State(app): State<App>, r: AuthReq, Path(pid): Path<String>) -> J {
    r.session()?;
    blocking(&app, move |a| a.auth().passkey_delete(&pid)).await?;
    ok()
}

async fn api_key_create(State(app): State<App>, r: AuthReq, Body(b): Body<NameIn>) -> J {
    r.session()?;
    let ip = r.ip();
    blocking(&app, move |a| a.auth().create_api_key(&a.auth().reconfirm(&ip, &b.password)?, &b.name))
        .await
        .map(axum::Json)
        .map_err(Into::into)
}

async fn api_key_revoke(State(app): State<App>, r: AuthReq, Path(kid): Path<String>) -> J {
    r.session()?;
    blocking(&app, move |a| a.auth().revoke_api_key(&kid)).await?;
    ok()
}

async fn api_session_end(State(app): State<App>, r: AuthReq, Path(sid): Path<String>) -> J {
    r.session()?;
    blocking(&app, move |a| a.auth().end_session_by_id(&sid)).await?;
    ok()
}

async fn api_sessions_end_others(State(app): State<App>, r: AuthReq) -> J {
    r.session()?;
    let token = r.who.token.clone();
    let ended = blocking(&app, move |a| a.auth().end_other_sessions(&token)).await?;
    Ok(axum::Json(json!({"ended": ended})))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::auth::Options;
    use crate::state::State;

    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn forwarded_origin_metadata_is_filtered_at_the_peer_boundary() {
        let original = hm(&[
            ("host", "wranglarr.lan:8787"),
            ("x-forwarded-host", "public.example"),
            ("x-forwarded-proto", "https"),
            ("origin", "https://public.example"),
        ]);
        for (allow, peer, accepted) in [
            ("127.0.0.1", Some("127.0.0.1"), true),
            ("127.0.0.1", Some("::ffff:127.0.0.1"), true),
            ("127.0.0.1", Some("10.0.0.5"), false),
            ("*", None, false),
            ("*", Some("10.0.0.5"), true),
            ("", Some("127.0.0.1"), false),
        ] {
            let mut h = original.clone();
            filter_forwarded(&mut h, peer.map(|p| p.parse().unwrap()), &TrustedProxies::new(allow));
            assert_eq!(same_origin(&h), accepted);
            assert_eq!(is_https(&h), accepted);
            assert_eq!(rp_id(&h), if accepted { "public.example" } else { "wranglarr.lan" });
            assert_eq!(
                request_origin(&h),
                if accepted { "https://public.example" } else { "http://wranglarr.lan:8787" }
            );
        }
    }

    #[test]
    fn origin_checks() {
        let h = hm(&[("host", "wranglarr.lan:8787"), ("origin", "http://wranglarr.lan:8787")]);
        assert!(same_origin(&h));
        let h = hm(&[("host", "127.0.0.1:8787"), ("origin", "http://evil.example")]);
        assert!(!same_origin(&h));
        let h = hm(&[("host", "127.0.0.1:8787")]);
        assert!(!same_origin(&h), "no Origin, no Referer");
        let h = hm(&[("host", "a:1"), ("referer", "http://a:1/suites/x?y")]);
        assert!(same_origin(&h), "Referer when there's no Origin");
        let h = hm(&[
            ("host", "10.0.0.2:8787"),
            ("x-forwarded-host", "Wr.Example.com, other"),
            ("origin", "https://wr.example.com"),
        ]);
        assert!(same_origin(&h), "through a reverse proxy");
        let h = hm(&[("host", "a:1"), ("origin", "null")]);
        assert!(!same_origin(&h));
    }

    #[test]
    fn rp_ids() {
        assert_eq!(rp_id(&hm(&[("host", "Wranglarr.LAN:8787")])), "wranglarr.lan");
        assert_eq!(rp_id(&hm(&[("host", "x:1"), ("x-forwarded-host", "wr.example.com, y")])), "wr.example.com");
        assert_eq!(rp_id(&hm(&[("host", "[::1]:8787")])), "::1");
        assert!(is_https(&hm(&[("x-forwarded-proto", "https, http")])));
        assert!(!is_https(&hm(&[])));
    }

    #[test]
    fn next_targets() {
        let a = Auth::with_state(State::memory().unwrap(), Options::default()).unwrap();
        assert_eq!(safe_next("/suites/x?y=1", &a, "wr"), "/suites/x?y=1");
        for bad in [
            "//evil.example/",
            "/\\evil",
            "https://evil.example/",
            "javascript:alert(1)",
            // the browser drops tabs and newlines: these would all be //evil.example/
            "/\t/evil.example/",
            "/\n/evil.example/",
            "/\r\n/evil.example/",
            "\t//evil.example/",
            " //evil.example/",
            "/\u{7f}/evil.example/",
            "/\u{85}/evil.example/",
            "",
        ] {
            assert_eq!(safe_next(bad, &a, "wr"), "/", "{bad}");
        }
        assert_eq!(safe_next("https://wr/", &a, "wr"), "/", "absolute only in tunnel mode");
        a.set_options(Options { tunnel: true, cookie_domain: "example.com".into(), ..Options::default() });
        let me = "wranglarr.example.com";
        for (n, want) in [
            ("https://radarr.example.com/movie/1", "https://radarr.example.com/movie/1"),
            ("https://evil.example/", "/"),
            ("https://example.com.evil.net/", "/"),
            ("//evil.example/", "/"),
            ("https://sub.radarr.example.com/", "https://sub.radarr.example.com/"),
            ("javascript:alert(1)", "/"),
            ("https:\\\\evil.example", "/"),
            ("https://user@radarr.example.com:7878/", "https://user@radarr.example.com:7878/"),
            ("HTTPS://Radarr.Example.com/movie/1", "https://radarr.example.com/movie/1"),
            ("https://radarr.example.com\t.evil.net/", "/"),
            ("https://radarr.example.com/\nx", "/"),
            ("https://evil.net\\@radarr.example.com/", "/"),
            ("https://evil.net%2F@radarr.example.com/", "https://evil.net%2F@radarr.example.com/"),
            ("ftp://radarr.example.com/", "/"),
        ] {
            assert_eq!(safe_next(n, &a, me), want, "{n}");
        }
        assert_eq!(
            safe_next("https://radarr.example.com/", &a, "10.0.0.2"),
            "/",
            "wranglarr itself not under the domain"
        );
    }

    #[test]
    fn reads_urls() {
        assert_eq!(authority("https://A.b:8787/x").as_deref(), Some("a.b:8787"));
        assert_eq!(authority("http://[::1]:8080/").as_deref(), Some("[::1]:8080"));
        assert_eq!(authority("//a/b"), None);
        assert_eq!(authority("foo"), None);
        assert_eq!(authority("javascript:alert(1)"), None);
        assert_eq!(host_name("[::1]:80"), "::1");
        assert_eq!(host_name("Radarr.Example.com:7878"), "radarr.example.com");
        assert_eq!(host_name(""), "");
        assert_eq!(unquote("/x%20y/caf%C3%A9%zz%2"), "/x y/café%zz%2");
    }

    #[test]
    fn cookies() {
        let h = hm(&[("cookie", "a=1; wranglarr_session=abc"), ("cookie", "wranglarr_session=\"def\"")]);
        assert_eq!(cookie(&h, SESSION_COOKIE), "def");
        assert_eq!(cookie(&h, "nope"), "");
        assert_eq!(
            set_cookie("s", "t", Some(60), "lax", true, Some("example.com")),
            "s=t; Domain=example.com; HttpOnly; Max-Age=60; Path=/; SameSite=lax; Secure"
        );
        assert_eq!(set_cookie("s", "t", None, "strict", false, None), "s=t; HttpOnly; Path=/; SameSite=strict");
    }

    #[test]
    fn client_ip_behind_a_local_proxy() {
        let r = |peer: &str, xff: &str| AuthReq {
            headers: if xff.is_empty() { HeaderMap::new() } else { hm(&[("x-forwarded-for", xff)]) },
            peer: Some(peer.parse().unwrap()),
            who: Who::default(),
            trusted: TrustedProxies::new("127.0.0.1"),
        };
        assert_eq!(r("10.0.0.5", "1.2.3.4").ip(), "10.0.0.5", "not a trusted proxy");
        assert_eq!(r("127.0.0.1", "1.2.3.4, 5.6.7.8").ip(), "5.6.7.8");
        assert_eq!(r("127.0.0.1", "").ip(), "127.0.0.1");
    }
}
