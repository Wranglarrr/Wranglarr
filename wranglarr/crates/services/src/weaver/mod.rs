// SPDX-License-Identifier: GPL-3.0-or-later
//! Weaver: a Usenet client (scryer-media/weaver). The arrs take it as an NZBGet
//! client: Weaver answers NZBGet's JSON-RPC API, with any username and one of its
//! own API keys as the password. Nothing sets that key from outside, so wranglarr
//! signs in with Weaver's login and creates one ([`Credentials::MintedKey`]).

pub mod client;

pub use client::WeaverClient;

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::login::{Login, LoginApi, LoginSource, LoginState, Managed, SET_TIMEOUT, Wanted, base};
use crate::AnyClient;
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;
use wranglarr_core::files::{PayloadRules, count};

use super::{
    BackupMode, Container, CopyRules, Credentials, DownloadsDef, HomeGroup, KeyFile, Payload, Port, ProxyTraits, Roles,
    ServiceDef, Wizard,
};

/// The file in Weaver's /config where wranglarr keeps the key it created, beside
/// the database that holds the key's hash: a backup or restore keeps them together.
pub const KEY_FILE: &str = "wranglarr-api-key.json";
/// The name the key has in Weaver's list of API keys.
pub const KEY_NAME: &str = "wranglarr";
/// The username the arrs send with the key (Weaver ignores it).
pub const RPC_USER: &str = crate::MINTED_KEY_USER;

static DOWNLOADS: DownloadsDef = DownloadsDef {
    payload: Payload { rules: payload_rules, items: payload_items },
    // Weaver's NZBGet API: the arrs' own NZBGet client talks to it
    arr_impl: "Nzbget",
    categories: true,
    backup_holds: "its <b>database</b> (the queue with each job's NZB, the history and the settings) and the API key wranglarr uses",
};

fn build(app: &AppConfig) -> Result<AnyClient> {
    Ok(AnyClient::new(WeaverClient::new(app)))
}

pub static DEF: ServiceDef = ServiceDef {
    id: "weaver",
    label: "Weaver",
    description: "Usenet client",
    roles: Roles::DOWNLOADS,
    home_group: HomeGroup::Downloads,
    color: "#4566b0",
    primary_count: "history",
    logo_paths: &["/favicon-adaptive.svg", "/apple-touch-icon-180.png"],
    status_page: None,
    container: Container {
        image: "ghcr.io/scryer-media/weaver:latest",
        ports: &[Port::tcp(9090, 9090)],
        config_mount: "/config",
        // the restored database has the login, the settings and the key's hash
        env: &[],
        run_as_user: false,
        tmpfs: &[],
        data: true,
        // no image with a VPN of its own: the shared tunnel only
        vpn: false,
    },
    copy: CopyRules {
        skip_dirs: &[],
        skip_files: &["disk-probe.json"],
        skip_sqlite: false,
        // the queue, history and settings, in WAL mode while Weaver runs
        sqlite_online: &["weaver.db"],
        skip_files_in: &[],
        zip_layout: &[],
    },
    credentials: Credentials::MintedKey {
        file: KeyFile::JsonPointer("/api_key"),
        env_user: "WEAVER_USER",
        env_pass: "WEAVER_PASS",
        default_user: "admin",
        mint: client::mint,
    },
    backup: BackupMode::ConfigDir,
    database: None,
    proxy: ProxyTraits {
        url_base: None,
        // its pages are served from / (`<base href="/">`)
        own_name: true,
        own_login: false,
        public_preset: false,
        host_check: false,
        public_note: None,
    },
    wizard: Some(Wizard { group: "download", config: None }),
    takes: None,
    arr: None,
    downloads: Some(&DOWNLOADS),
    login: Some(Login::Managed(Managed {
        source: LoginSource::Env,
        ready_path: "/readyz",
        init_checks_username: true,
        api: &WeaverLogin,
    })),
    feeds: &[],
    build,
};

/// The image creates its first login from WEAVER_USER / WEAVER_PASS
/// (`WEAVER_BOOTSTRAP_LOGIN_*`) on the first start only, so this only checks them.
/// With AuthTunnel the gate signs you in with wranglarr's key.
struct WeaverLogin;

/// Whether .env's login gets in. A session it opens is closed again.
async fn logs_in(c: &reqwest::Client, a: &AppConfig, timeout: Duration) -> Result<bool, String> {
    let (u, p) = crate::credentials(a).await;
    match client::sign_in(c, base(a), &u, &p, timeout).await? {
        Some(s) => {
            s.sign_out(c, timeout).await;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[async_trait]
impl LoginApi for WeaverLogin {
    async fn read(&self, a: &AppConfig) -> Result<LoginState, String> {
        // through the key, so reading opens no browser session
        let st = WeaverClient::new(a).login_status().await.map_err(|e| e.message().to_string())?;
        let user = text(&st["username"]);
        let want = crate::credentials(a).await.0;
        let error = if st["enabled"] != json!(true) {
            Some("Weaver has no login".to_string())
        } else if user != want {
            Some(format!("Weaver's login is {user:?}, not WEAVER_USER ({want:?})"))
        } else {
            None
        };
        Ok(LoginState { error, ..LoginState::new("form", user) })
    }

    async fn set(&self, a: &AppConfig, want: Wanted<'_>) -> Result<String, String> {
        let c = wranglarr_core::http::http(Duration::from_secs(30));
        if !logs_in(&c, a, SET_TIMEOUT).await? {
            return Err(
                "WEAVER_USER / WEAVER_PASS from .env don't log in: Weaver takes them only on its first start; \
                        change its login in Weaver (Settings → Security) to match"
                    .into(),
            );
        }
        let u = crate::credentials(a).await.0;
        match want {
            Wanted::Tunnel => Ok(format!("logs in as {u}; the gate signs you in to it")),
            Wanted::Apps { user, .. } if u != user => Err(format!(
                "WEAVER_USER is {u:?}, not {user:?}: set WEAVER_USER and WEAVER_PASS in .env to the apps' login, \
                 and Weaver's own login to the same"
            )),
            Wanted::Apps { .. } => Ok(format!("form login, user {u} (from .env)")),
        }
    }
}

fn text(v: &Value) -> String {
    wranglarr_core::value::text(v)
}

// ---- backup: its database; the downloads are outside /config ------------------------------

fn payload_rules(_src: &Path) -> PayloadRules {
    // Its folders are settings in the database, seeded from WEAVER_*_DIR on the
    // first start: wranglarr's stack puts them under /data/usenet.
    PayloadRules::default()
}

fn payload_items(dest: &Path, _rules: &PayloadRules) -> Vec<Value> {
    vec![json!({"label": "database (queue, history, settings)", "count": count(dest, "weaver.db", None)})]
}
