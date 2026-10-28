// SPDX-License-Identifier: GPL-3.0-or-later
//! How the apps' logins are set up: read only.
//!
//! The logins are set at init, never by the running wranglarr (wiki/App-Logins.md):
//! APPS_AUTH in .env says how, `wranglarr-setup --auth` changes it, and
//! `./scripts/init.sh --auth` applies it (the wranglarr-apps-auth one-shot).
//! This reads each app's current login and that run's report, and says what looks wrong.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value, json};

use crate::app::App;
use crate::config::AppConfig;
use crate::links::Published;
use crate::service::AppConfigExt;
use crate::service::login::{Login, LoginSource, LoginState, Managed, required_text};
use crate::value::{head, text, truthy};

/// The init run's report, next to the state database (written by wranglarr-apps-auth).
pub const REPORT: &str = "apps-auth.json";
/// APPS_AUTH -> what it means, in the setup wizard's order.
pub const MODES: &[(&str, &str)] = &[
    ("apps", "one login in every app"),
    ("tunnel", "AuthTunnel: wranglarr signs you in to every app"),
    ("manual", "each app keeps its own login (wranglarr doesn't touch them)"),
];

pub fn mode_text(mode: &str) -> &'static str {
    MODES.iter().find(|(k, _)| *k == mode).map_or("", |(_, v)| *v)
}

/// What `.env` says about the apps' logins (part of [`crate::deployment::Deployment`]).
#[derive(Debug, Clone)]
pub struct LoginsEnv {
    /// APPS_AUTH: apps, tunnel or manual (the default, and for anything else).
    pub mode: String,
    /// APPS_LOGIN_USER, trimmed.
    pub user: String,
    /// APPS_AUTH_REQUIRED (enabled).
    pub required: String,
    /// AUTH_COOKIE_DOMAIN, as it is.
    pub cookie_domain: String,
    /// AUTH_LOGIN_URL, as it is.
    pub login_url: String,
}

impl LoginsEnv {
    /// The settings in the environment `env`.
    pub fn read(env: crate::config::Lookup) -> LoginsEnv {
        let mode = env("APPS_AUTH").unwrap_or_else(|| "manual".into()).trim().to_lowercase();
        LoginsEnv {
            mode: if MODES.iter().any(|(k, _)| *k == mode) { mode } else { "manual".into() },
            user: env("APPS_LOGIN_USER").unwrap_or_default().trim().to_string(),
            required: env("APPS_AUTH_REQUIRED").unwrap_or_else(|| "enabled".into()),
            cookie_domain: env("AUTH_COOKIE_DOMAIN").unwrap_or_default(),
            login_url: env("AUTH_LOGIN_URL").unwrap_or_default(),
        }
    }
}

/// The two commands that change the logins (shown on the home page), with
/// wranglarr's `image`.
pub fn rerun(image: &str) -> Vec<String> {
    vec![
        format!("docker run --rm -it -p 8788:8788 -v \"$PWD\":/stack {image} wranglarr-setup --auth"),
        "./scripts/init.sh --auth".into(),
    ]
}

/// The app's sign-in, when wranglarr reads and sets it.
fn managed(a: &AppConfig) -> Option<&'static Managed> {
    a.def()?.login.as_ref()?.managed()
}

/// The view of the service for the app logins: `app.app_logins()`.
pub struct AppLogins<'a>(&'a App);

impl App {
    pub fn app_logins(&self) -> AppLogins<'_> {
        AppLogins(self)
    }
}

/// What to say about one app's login.
pub fn warn(mode: &str, how: &Managed, row: &Map<String, Value>, port: &Published, user: &str) -> String {
    if row.get("error").is_some_and(truthy) {
        return String::new();
    }
    let m = row.get("method").and_then(Value::as_str).unwrap_or("");
    let login = row.get("login").is_some_and(truthy);
    if mode == "tunnel" {
        if how.source == LoginSource::Env {
            // it keeps .env's login: the gate gets in with it
            return String::new();
        }
        if !login && port.gate != Some(true) && port.port.is_some_and(|p| p != 0) {
            return "no login of its own, and its own port is published: anyone on the network can use it".into();
        }
        if login {
            return "still has its own login, so you sign in twice: re-run ./scripts/init.sh --auth".into();
        }
        return String::new();
    }
    if !login {
        return if m == "external" {
            "set to External (no login): the gate isn't in front of it any more, re-run ./scripts/init.sh --auth".into()
        } else {
            "no login: anyone who reaches it can use it".into()
        };
    }
    let username = row.get("username").and_then(Value::as_str).unwrap_or("");
    if mode == "apps" && !user.is_empty() && !username.is_empty() && username != user && !how.init_checks_username {
        return format!("its login is {username:?}, not {user:?}: re-run ./scripts/init.sh --auth");
    }
    String::new()
}

impl<'a> AppLogins<'a> {
    /// APPS_AUTH: apps, tunnel or manual (the default, and for anything else).
    pub fn mode(&self) -> String {
        self.env().mode.clone()
    }

    fn env(&self) -> &'a LoginsEnv {
        &self.0.deployment().app_logins
    }

    /// The apps it can read a login from, each once.
    pub fn apps(&self) -> Vec<&'a AppConfig> {
        let mut seen = HashSet::new();
        self.0.cfg().all_apps().filter(|a| managed(a).is_some() && seen.insert(a.name.clone())).collect()
    }

    /// The last init run's report; {} when there is none.
    pub fn report(&self) -> Map<String, Value> {
        let dir = self.0.cfg().state_db.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        std::fs::read_to_string(dir.join(REPORT))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    pub async fn status(&self) -> Value {
        let mode = self.mode();
        let rep = self.report();
        let user = self.env().user.clone();
        let ports: BTreeMap<String, Published> =
            if mode == "tunnel" { self.0.links().published(false).await } else { BTreeMap::new() };
        let apps = self.apps();
        let reads = futures::future::join_all(apps.iter().map(|a| read(a))).await;
        let mut rows = Vec::new();
        for (a, got) in apps.iter().zip(reads) {
            let mut row = Map::new();
            row.insert("app".into(), json!(a.name));
            row.insert("kind".into(), json!(a.kind));
            let got = got.unwrap_or_else(|e| LoginState { error: Some(head(&e, 200)), ..LoginState::new("?", "") });
            if let Ok(Value::Object(m)) = serde_json::to_value(&got) {
                row.extend(m);
            }
            let m = row.get("method").and_then(Value::as_str);
            let login = !matches!(m, Some("none" | "external" | "?") | None);
            row.insert("login".into(), json!(login));
            let p = ports.get(&a.name).cloned().unwrap_or_default();
            row.insert("gate".into(), json!(p.gate == Some(true)));
            let Some(login) = managed(a) else { continue };
            let mut w = warn(&mode, login, &row, &p, &user);
            if rep.get("mode").and_then(Value::as_str) == Some(mode.as_str())
                && let Some(r) = rep.get("apps").and_then(|x| x.get(&a.name)).filter(|r| truthy(r))
                && !r.get("ok").is_some_and(truthy)
                && w.is_empty()
            {
                w = format!("the last init run couldn't set it: {}", text(r.get("detail").unwrap_or(&Value::Null)));
            }
            row.insert("warn".into(), json!(w));
            rows.push(Value::Object(row));
        }
        // the apps whose users have accounts of their own (Seerr: the media server's)
        for a in self.0.cfg().all_apps() {
            if let Some(Login::Own { accounts }) = a.def().and_then(|d| d.login) {
                rows.push(json!({"app": a.name, "kind": a.kind, "method": "own", "username": accounts,
                                 "login": true, "gate": false, "warn": ""}));
            }
        }
        let req = self.env().required.clone();
        let apps_mode = mode == "apps";
        let tunnel = mode == "tunnel";
        let last_run = if rep.is_empty() {
            Value::Null
        } else {
            json!({"mode": rep.get("mode"), "at": rep.get("at"), "username": rep.get("username")})
        };
        json!({
            "mode": mode, "mode_text": mode_text(&mode), "username": user,
            "required": if apps_mode { req.clone() } else { String::new() },
            "required_text": if apps_mode { required_text(&req).unwrap_or("") } else { "" },
            "cookie_domain": if tunnel { self.env().cookie_domain.clone() } else { String::new() },
            "login_url": if tunnel { self.env().login_url.clone() } else { String::new() },
            "wranglarr_auth": self.0.auth_enabled(),
            "last_run": last_run,
            "rerun": rerun(&self.0.deployment().image),
            "apps": rows,
        })
    }
}

/// The app's login as it is now: method, username (and a few extras).
async fn read(a: &AppConfig) -> std::result::Result<LoginState, String> {
    match managed(a) {
        Some(login) => login.api.read(a).await,
        None => Err(format!("{:?} has no login wranglarr reads", a.kind)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    fn login(kind: &str) -> &'static Managed {
        crate::service::get(kind).and_then(|d| d.login.as_ref()).and_then(Login::managed).expect(kind)
    }

    #[test]
    fn warns_about_logins() {
        let open = Published { port: Some(7878), ..Default::default() };
        let gated = Published { port: Some(7878), gate: Some(true), ..Default::default() };
        let none = row(json!({"method": "none", "login": false}));
        let forms = row(json!({"method": "forms", "login": true, "username": "bob"}));
        assert_eq!(
            warn("tunnel", login("radarr"), &none, &open, ""),
            "no login of its own, and its own port is published: anyone on the network can use it"
        );
        assert_eq!(warn("tunnel", login("radarr"), &none, &gated, ""), "");
        assert_eq!(warn("tunnel", login("qbittorrent"), &forms, &open, ""), "");
        assert_eq!(
            warn("tunnel", login("radarr"), &forms, &gated, ""),
            "still has its own login, so you sign in twice: re-run ./scripts/init.sh --auth"
        );
        assert_eq!(warn("manual", login("radarr"), &none, &open, ""), "no login: anyone who reaches it can use it");
        let ext = row(json!({"method": "external", "login": false}));
        assert!(warn("apps", login("radarr"), &ext, &open, "").starts_with("set to External (no login)"));
        assert_eq!(
            warn("apps", login("radarr"), &forms, &open, "admin"),
            r#"its login is "bob", not "admin": re-run ./scripts/init.sh --auth"#
        );
        assert_eq!(warn("apps", login("nzbget"), &forms, &open, "admin"), "");
        assert_eq!(warn("manual", login("radarr"), &forms, &open, "admin"), "");
        let failed = row(json!({"method": "?", "login": false, "error": "down"}));
        assert_eq!(warn("manual", login("radarr"), &failed, &open, ""), "");
    }

    #[test]
    fn rerun_names_the_image() {
        let r = rerun("example/wranglarr:1");
        assert!(r[0].contains("example/wranglarr:1 wranglarr-setup --auth") && r[1] == "./scripts/init.sh --auth");
        assert_eq!(mode_text("tunnel"), "AuthTunnel: wranglarr signs you in to every app");
    }
}
