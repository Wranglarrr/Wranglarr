// SPDX-License-Identifier: GPL-3.0-or-later
//! wranglarr-apps-auth: set the apps' logins the way .env says. Run by scripts/init.sh
//! (the `wranglarr-apps-auth` one-shot in compose.yaml), never by the running wranglarr.
//!
//! APPS_AUTH (.env):
//! - `apps`: the same login (APPS_LOGIN_USER / APPS_LOGIN_PASSWORD) in every app.
//!   Arrs and Prowlarr ask for it everywhere, or not on the local network
//!   (APPS_AUTH_REQUIRED=disabledForLocalAddresses).
//! - `tunnel`: AuthTunnel: the apps have no login of their own (the arrs are set to External).
//!   Their web ports are published only by wranglarr-gate, which asks wranglarr
//!   whether you're signed in first.
//! - `manual`: leave the apps' logins alone (this does nothing).
//!
//! Done elsewhere in init:
//! - qBittorrent: login (and, for tunnel, the Docker-network whitelist) is written into
//!   qBittorrent.conf by scripts/patch_downloads.py while it's stopped: a new
//!   install only has a temporary password, so there is no API to log in to.
//! - NZBGet: the image applies NZBGET_USER / NZBGET_PASS from .env at every start.
//!
//! This checks both, and updates the qBittorrent / NZBGet entries in every arr when
//! their login changed. The apps' API keys never change.
//!
//! `--only apps`: the arrs, Prowlarr, Bazarr, SABnzbd (before the ports change);
//! `--only clients`: check qBittorrent and NZBGet, update the arrs' entries (after).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{self, AppConfig};
use crate::service::AppConfigExt;
use crate::service::Roles;
use crate::service::arr::ArrClient;
use crate::service::login::{LoginSource, Managed, Wanted, base, get, key, required_text, set_http};
use crate::setup::env::{password_problem, valid_username};
use crate::value::text;
use clap::{Parser, ValueEnum};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

pub use crate::applogins::REPORT;

fn say(msg: &str) {
    use std::io::Write;
    println!("apps-auth: {msg}");
    let _ = std::io::stdout().flush();
}

type R<T> = Result<T, String>;

/// The app's sign-in, when apps-auth sets it.
fn managed(a: &AppConfig) -> Option<&'static Managed> {
    a.def()?.login.as_ref()?.managed()
}

/// Its implementation name in the arrs, when it's a download client whose login comes from .env.
fn env_client(a: &AppConfig) -> Option<&'static str> {
    let d = a.def()?;
    (managed(a)?.source == LoginSource::Env).then_some(d.downloads?.arr_impl)
}

async fn arr_client(a: &AppConfig) -> R<ArrClient> {
    let key = crate::service::api_key(a).await.map_err(|e| e.to_string())?;
    ArrClient::new(&a.name, &a.kind, &a.url, &key, None).map_err(|e| e.to_string())
}

// ---- per app --------------------------------------------------------------------------------

async fn wait_up(a: &AppConfig, timeout: f64) -> R<()> {
    let end = Instant::now() + Duration::from_secs_f64(timeout.clamp(0.0, 1e9));
    let path = managed(a).map_or("/", |m| m.ready_path);
    let c = set_http();
    loop {
        if let Ok(r) = c.get(format!("{}{path}", base(a))).timeout(Duration::from_secs(5)).send().await
            && r.status().as_u16() < 500
        {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!("not reachable at {} after {} s", a.url, timeout as i64));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

enum Stop {
    /// An arr's API failed: reported, then on to the next arr.
    Arr(String),
    /// Anything else ends the whole client.
    Other(String),
}

/// qBittorrent's or NZBGet's login changed: update its entry in every arr and Prowlarr (they log in with it).
pub async fn fix_client_entries(cfg_apps: &[AppConfig], client: &AppConfig, wait: f64) -> R<Vec<String>> {
    let imp = env_client(client).unwrap_or("");
    let (u, p) = crate::service::credentials(client).await;
    let mut out = vec![];
    for a in cfg_apps {
        if !a.has(Roles::ARR) {
            continue;
        }
        if let Err(e) = wait_up(a, wait).await {
            out.push(format!("{}: couldn't update its {} entry: {e}", a.name, client.name));
            continue;
        }
        let c = arr_client(a).await?;
        let res: Result<(), Stop> = async {
            let list = c.get("downloadclient").await.map_err(|e| Stop::Arr(e.to_string()))?;
            for mut dc in list.as_array().cloned().unwrap_or_default() {
                let fields = dc.get("fields").and_then(Value::as_array).cloned().unwrap_or_default();
                let f = |n: &str| -> Result<Value, Stop> {
                    for x in &fields {
                        if key(x, "name").map_err(Stop::Other)?.as_str() == Some(n) {
                            return Ok(get(x, "value"));
                        }
                    }
                    Ok(Value::Null)
                };
                let (fu, fp) = (f("username")?, f("password")?);
                if dc.get("implementation").and_then(Value::as_str) != Some(imp) {
                    continue;
                }
                if fu.as_str() == Some(u.as_str()) && !(fp.is_null() || fp.as_str() == Some("")) {
                    // the arr never shows the stored password; a test call tells whether it still works
                    if c.post("downloadclient/test", &dc).await.is_ok() {
                        continue;
                    }
                }
                let Some(Value::Array(fs)) = dc.get_mut("fields") else {
                    return Err(Stop::Other("no \"fields\" in the download client".into()));
                };
                for x in fs.iter_mut() {
                    match x.get("name").and_then(Value::as_str) {
                        Some("username") => x["value"] = json!(u),
                        Some("password") => x["value"] = json!(p),
                        _ => {}
                    }
                }
                let id = text(key(&dc, "id").map_err(Stop::Other)?);
                c.put(&format!("downloadclient/{id}"), &dc).await.map_err(|e| Stop::Arr(e.to_string()))?;
                out.push(format!("{}: updated its {} login", a.name, client.name));
            }
            Ok(())
        }
        .await;
        match res {
            Ok(()) => {}
            Err(Stop::Arr(e)) => out.push(format!("{}: couldn't update its {} entry: {e}", a.name, client.name)),
            Err(Stop::Other(e)) => return Err(e),
        }
    }
    for line in &out {
        say(line);
    }
    Ok(out)
}

// ---- main ---------------------------------------------------------------------------------

const PROG: &str = "wranglarr-apps-auth";

/// Set the apps' logins the way .env says. Run by scripts/init.sh (the
/// `wranglarr-apps-auth` one-shot in compose.yaml), never by the running wranglarr.
#[derive(Parser, Debug)]
struct Args {
    /// One half (init.sh orders them around port changes)
    #[arg(long, value_enum)]
    only: Option<Half>,
    /// Seconds to wait for each app to answer
    #[arg(long, default_value_t = 300.0, value_parser = seconds)]
    wait: f64,
}

/// `--only`: which apps a run sets.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    /// The arrs, Prowlarr, Bazarr, SABnzbd
    Apps,
    /// qBittorrent and NZBGet, and the arrs' entries for them
    Clients,
}

/// A number of seconds: 0 or more.
fn seconds(s: &str) -> R<f64> {
    match s.trim().parse::<f64>() {
        Ok(n) if n.is_finite() && n >= 0.0 => Ok(n),
        _ => Err("a number of seconds, 0 or more".into()),
    }
}

/// apps-auth.json.
#[derive(Serialize)]
struct Report {
    mode: String,
    username: String,
    required: String,
    at: String,
    apps: Map<String, Value>,
}

/// `wranglarr apps-auth` / `wranglarr-apps-auth`.
pub fn main(args: &[String]) -> i32 {
    let (only, wait) = match crate::cli::parse::<Args>(PROG, args) {
        Ok(a) => (a.only, a.wait),
        Err(code) => return code,
    };
    let var = |k: &str, d: &str| config::process_env(k).unwrap_or_else(|| d.to_string());
    let mode = var("APPS_AUTH", "manual").trim().to_lowercase();
    let user = var("APPS_LOGIN_USER", "admin").trim().to_string();
    let pw = var("APPS_LOGIN_PASSWORD", "");
    let required = Some(var("APPS_AUTH_REQUIRED", "enabled").trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "enabled".into());
    if !["apps", "tunnel", "manual"].contains(&mode.as_str()) {
        say(&format!("APPS_AUTH={mode:?}: use apps, tunnel or manual"));
        return 2;
    }
    if mode == "manual" {
        say("APPS_AUTH=manual: the apps' logins are left as they are");
        return 0;
    }
    if mode == "apps" {
        if !valid_username(&user) {
            say("APPS_LOGIN_USER: 1-64 letters, digits and . _ - @");
            return 2;
        }
        if let Some(prob) = password_problem(&pw, &user) {
            say(&format!("APPS_LOGIN_PASSWORD: {prob}"));
            return 2;
        }
        if required_text(&required).is_none() {
            say("APPS_AUTH_REQUIRED: enabled or disabledForLocalAddresses");
            return 2;
        }
    }
    let path = config::default_path(config::process_env);
    let cfg = match crate::service::apps::load(&path, config::process_env) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("apps-auth: {e}");
            return 1;
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("apps-auth: {e}");
            return 1;
        }
    };
    let report_path = cfg.state_db.parent().unwrap_or(Path::new("")).join(REPORT);
    let run = Run { mode, user, pw, required, only, wait, report_path };
    rt.block_on(run.go(&cfg))
}

struct Run {
    mode: String,
    user: String,
    pw: String,
    required: String,
    /// One half, or both when None.
    only: Option<Half>,
    wait: f64,
    report_path: PathBuf,
}

impl Run {
    async fn one(&self, a: &AppConfig) -> R<String> {
        wait_up(a, self.wait).await?;
        let want = if self.mode == "tunnel" {
            Wanted::Tunnel
        } else {
            Wanted::Apps { user: &self.user, password: &self.pw, required: &self.required }
        };
        let login = managed(a).ok_or_else(|| format!("{:?} has no login wranglarr sets", a.kind))?;
        login.api.set(a, want).await
    }

    async fn go(&self, cfg: &config::Config) -> i32 {
        let mut apps: Vec<AppConfig> = vec![];
        for a in cfg.all_apps() {
            if !apps.iter().any(|x| x.name == a.name) {
                apps.push(a.clone());
            }
        }
        let source = |a: &&AppConfig| managed(a).map(|m| m.source);
        let first = apps.iter().filter(|a| source(a) == Some(LoginSource::Api));
        let clients = apps.iter().filter(|a| source(a) == Some(LoginSource::Env));
        let mut todo: Vec<&AppConfig> = vec![];
        if self.only != Some(Half::Clients) {
            todo.extend(first);
        }
        if self.only != Some(Half::Apps) {
            todo.extend(clients);
        }
        let names = todo.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
        let login = if self.mode == "apps" { format!(", login {:?}", self.user) } else { String::new() };
        say(&format!("mode {}{login}: {}", self.mode, if names.is_empty() { "no apps" } else { &names }));

        // the last run's results stay for the apps this run leaves out, in the same mode
        let old: Value = std::fs::read_to_string(&self.report_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let mut results = match &old["apps"] {
            Value::Object(o) if old["mode"].as_str() == Some(self.mode.as_str()) => o.clone(),
            _ => Map::new(),
        };
        let entry = |ok: bool, detail: &str| json!({"ok": ok, "detail": detail});
        let mut failed = 0;
        for a in todo {
            let res: R<()> = async {
                let msg = self.one(a).await?;
                results.insert(a.name.clone(), entry(true, &msg));
                say(&format!("{}: {msg}", a.name));
                if env_client(a).is_some() {
                    let bad: Vec<String> = fix_client_entries(&apps, a, self.wait)
                        .await?
                        .into_iter()
                        .filter(|x| x.contains("couldn't"))
                        .collect();
                    if !bad.is_empty() {
                        return Err(bad.join("; "));
                    }
                }
                Ok(())
            }
            .await;
            if let Err(e) = res {
                failed += 1;
                results.insert(a.name.clone(), entry(false, &e));
                say(&format!("{}: FAILED: {e}", a.name));
            }
        }
        let apps_mode = self.mode == "apps";
        let report = Report {
            mode: self.mode.clone(),
            username: if apps_mode { self.user.clone() } else { String::new() },
            required: if apps_mode { self.required.clone() } else { String::new() },
            at: crate::time::now_iso(),
            apps: results,
        };
        let written = serde_json::to_string_pretty(&report)
            .map_err(std::io::Error::other)
            .and_then(|text| std::fs::write(&self.report_path, text));
        if let Err(e) = written {
            say(&format!("couldn't write {}: {e}", self.report_path.display()));
        }
        if failed == 0 {
            say("done");
        } else {
            say(&format!("{failed} app(s) failed; fix the cause and re-run: ./scripts/init.sh --auth"));
        }
        i32::from(failed > 0)
    }
}
