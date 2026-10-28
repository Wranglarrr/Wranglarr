// SPDX-License-Identifier: GPL-3.0-or-later
//! The apps' own sign-in: how wranglarr reads it (the home page's App logins) and
//! how `wranglarr-apps-auth` sets it at init (APPS_AUTH in .env). Each service that
//! has one implements [`LoginApi`] in its own file; its [`ServiceDef`](super::ServiceDef)
//! says which, in [`Login`].
//!
//! Messages here end up in apps-auth.json and on the home page, so they never carry
//! a URL (an API key can be in its query).

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use wranglarr_core::config::AppConfig;

/// What a service's sign-in is, as far as APPS_AUTH goes.
#[derive(Clone, Copy, Debug)]
pub enum Login {
    /// wranglarr reads it, and apps-auth sets it.
    Managed(Managed),
    /// The app's own accounts, which wranglarr leaves alone. `accounts` says whose
    /// (Seerr: "media server accounts").
    Own { accounts: &'static str },
}

impl Login {
    pub fn managed(&self) -> Option<&Managed> {
        match self {
            Login::Managed(m) => Some(m),
            Login::Own { .. } => None,
        }
    }
}

/// A sign-in wranglarr reads and sets.
#[derive(Clone, Copy)]
pub struct Managed {
    pub source: LoginSource,
    /// apps-auth waits until this path answers (anything below 500).
    pub ready_path: &'static str,
    /// apps-auth itself fails when its username isn't APPS_LOGIN_USER, so the page
    /// shows that failure rather than a warning of its own.
    pub init_checks_username: bool,
    pub api: &'static dyn LoginApi,
}

impl fmt::Debug for Managed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Managed")
            .field("source", &self.source)
            .field("ready_path", &self.ready_path)
            .field("init_checks_username", &self.init_checks_username)
            .finish_non_exhaustive()
    }
}

/// Where the login comes from, which also orders the apps-auth run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginSource {
    /// apps-auth sets it through the app's API (`--only apps`, before the ports change).
    Api,
    /// The credentials in .env, applied by init or by the image. apps-auth checks
    /// them, then updates the app's entry in every arr (`--only clients`, after the
    /// ports change). With AuthTunnel the app keeps this login: the gate gets in
    /// with it, or through a whitelisted network.
    Env,
}

/// What APPS_AUTH asks for (`manual` never gets this far).
#[derive(Clone, Copy, Debug)]
pub enum Wanted<'a> {
    /// AuthTunnel: no login of its own; the gate signs you in.
    Tunnel,
    /// The same login in every app. `required` is APPS_AUTH_REQUIRED (the arrs').
    Apps { user: &'a str, password: &'a str, required: &'a str },
}

/// An app's login as it is now, as the home page shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LoginState {
    /// `none`, `external`, `forms`, `form`, `basic`, … (`?` when it couldn't be read).
    pub method: String,
    /// The arrs' authenticationRequired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<String>,
    pub username: String,
    /// The app answered, but .env's credentials didn't get in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// qBittorrent: whitelisted networks get in without the login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bypass: Option<bool>,
}

impl LoginState {
    pub fn new(method: impl Into<String>, username: impl Into<String>) -> LoginState {
        LoginState { method: method.into(), username: username.into(), ..LoginState::default() }
    }
}

/// The API side of a sign-in.
#[async_trait]
pub trait LoginApi: Send + Sync {
    /// The login as it is now.
    async fn read(&self, app: &AppConfig) -> Result<LoginState, String>;
    /// Make it what APPS_AUTH says, check that it took, and say what it is now.
    async fn set(&self, app: &AppConfig, want: Wanted<'_>) -> Result<String, String>;
}

/// APPS_AUTH_REQUIRED -> what it means.
pub const REQUIRED: &[(&str, &str)] =
    &[("enabled", "everywhere"), ("disabledForLocalAddresses", "not on the local network")];

pub fn required_text(r: &str) -> Option<&'static str> {
    REQUIRED.iter().find(|(k, _)| *k == r).map(|(_, v)| *v)
}

// ---- shared plumbing for the implementations -------------------------------------------------

/// How long reading a login may take (the home page waits for every app).
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// apps-auth's per-request limit for the calls that change or check a login.
pub const SET_TIMEOUT: Duration = Duration::from_secs(15);

/// apps-auth's HTTP client.
pub fn set_http() -> reqwest::Client {
    wranglarr_core::http::http(Duration::from_secs(30))
}

pub fn base(a: &AppConfig) -> &str {
    a.url.trim_end_matches('/')
}

/// The message for a failed request, without the URL (it can hold an API key).
pub fn http_err(e: reqwest::Error) -> String {
    wranglarr_core::http::reason(&e)
}

/// An error for a status that isn't a success. Only for URLs without a key in them.
pub fn raise_for_status(r: &reqwest::Response) -> Result<(), String> {
    let s = r.status();
    if s.is_success() {
        return Ok(());
    }
    Err(format!("{s} from {}", r.url()))
}

pub async fn json_of(r: reqwest::Response) -> Result<Value, String> {
    let text = wranglarr_core::http::response_text(r).await.map_err(|e| e.describe(wranglarr_core::http::reason))?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// A field the answer must have.
pub fn key<'a>(v: &'a Value, k: &str) -> Result<&'a Value, String> {
    v.get(k).ok_or_else(|| format!("no {k:?} in the answer"))
}

/// A field, null when missing.
pub fn get(v: &Value, k: &str) -> Value {
    v.get(k).cloned().unwrap_or(Value::Null)
}

/// The value as text, or "" when it isn't set (null, false, 0, empty).
pub fn str_or_empty(v: &Value) -> String {
    if wranglarr_core::value::truthy(v) { wranglarr_core::value::text(v) } else { String::new() }
}

/// The cookies a response sets: (name, value).
pub fn set_cookies(r: &reqwest::Response) -> Vec<(String, String)> {
    r.headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next()?.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::LoginState;

    #[test]
    fn login_state_shape() {
        let arr = LoginState { required: Some("enabled".into()), ..LoginState::new("forms", "bob") };
        assert_eq!(
            serde_json::to_string(&arr).expect("json"),
            r#"{"method":"forms","required":"enabled","username":"bob"}"#
        );
        let qb = LoginState { bypass: Some(true), ..LoginState::new("form", "admin") };
        assert_eq!(
            serde_json::to_value(&qb).expect("json"),
            json!({"method": "form", "username": "admin", "bypass": true})
        );
        let failed = LoginState { error: Some("down".into()), ..LoginState::new("?", "") };
        assert_eq!(
            serde_json::to_value(&failed).expect("json"),
            json!({"method": "?", "username": "", "error": "down"})
        );
    }
}
