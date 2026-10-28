// SPDX-License-Identifier: GPL-3.0-or-later
//! NZBGet: a Usenet client.

pub mod client;

pub use client::NzbgetClient;

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::login::{
    Login, LoginApi, LoginSource, LoginState, Managed, READ_TIMEOUT, SET_TIMEOUT, Wanted, base, http_err, set_http,
};
use crate::AnyClient;
use crate::AppConfigExt;
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;
use wranglarr_core::files::{PayloadRules, count, in_config, kv};

use super::{
    BackupMode, Container, CopyRules, Credentials, DownloadsDef, HomeGroup, Payload, Port, ProxyTraits, Roles,
    ServiceDef, Wizard,
};

static DOWNLOADS: DownloadsDef = DownloadsDef {
    payload: Payload { rules: payload_rules, items: payload_items },
    arr_impl: "Nzbget",
    categories: true,
    backup_holds: "the <b>queue</b> (each job's NZB and download state), NZBs waiting to be picked up, and the client's settings",
};

fn build(app: &AppConfig) -> Result<AnyClient> {
    let (u, p) = app.credentials_blocking();
    Ok(AnyClient::new(NzbgetClient::new(&app.name, &app.url, &u, &p, &app.config_dir)))
}

pub static DEF: ServiceDef = ServiceDef {
    id: "nzbget",
    label: "NZBGet",
    description: "Usenet client",
    roles: Roles::DOWNLOADS,
    home_group: HomeGroup::Downloads,
    color: "#3c9a3c",
    primary_count: "history",
    logo_paths: &["/img/favicon-256x256.png"],
    status_page: None,
    container: Container {
        image: "lscr.io/linuxserver/nzbget:latest",
        ports: &[Port::tcp(6789, 6789)],
        config_mount: "/config",
        // linuxserver NZBGet applies its login from these at launch (nzbget.conf keeps the default)
        env: &[("NZBGET_USER", "${NZBGET_USER}"), ("NZBGET_PASS", "${NZBGET_PASS}")],
        run_as_user: false,
        data: true,
        vpn: true,
    },
    copy: CopyRules {
        skip_dirs: &[],
        skip_files: &["nzbget.log*"],
        skip_sqlite: false,
        skip_files_in: &[],
        zip_layout: &[],
    },
    credentials: Credentials::UserPassword {
        env_user: "NZBGET_USER",
        env_pass: "NZBGET_PASS",
        default_user: "nzbget",
        default_pass: Some("tegbzn6789"),
        config_keys: Some(("ControlUsername", "ControlPassword")),
        basic_auth: true,
    },
    backup: BackupMode::ConfigDir,
    database: None,
    proxy: ProxyTraits {
        url_base: None,
        own_name: false,
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
        ready_path: "/",
        init_checks_username: true,
        api: &NzbgetLogin,
    })),
    feeds: &[],
    build,
};

/// The image applies NZBGET_USER / NZBGET_PASS at every start (HTTP basic auth), so
/// this only checks them.
struct NzbgetLogin;

/// Whether .env's login gets in.
async fn logs_in(c: &reqwest::Client, a: &AppConfig, timeout: std::time::Duration) -> Result<bool, String> {
    let (u, p) = crate::credentials(a).await;
    let r = c
        .get(format!("{}/jsonrpc/version", base(a)))
        .basic_auth(&u, Some(&p))
        .timeout(timeout)
        .send()
        .await
        .map_err(http_err)?;
    Ok(r.status() == StatusCode::OK)
}

#[async_trait]
impl LoginApi for NzbgetLogin {
    async fn read(&self, a: &AppConfig) -> Result<LoginState, String> {
        let ok = logs_in(&wranglarr_core::http::http(READ_TIMEOUT), a, READ_TIMEOUT).await?;
        Ok(LoginState {
            error: (!ok).then(|| "NZBGET_USER / NZBGET_PASS from .env don't log in".into()),
            ..LoginState::new("basic", crate::credentials(a).await.0)
        })
    }

    async fn set(&self, a: &AppConfig, want: Wanted<'_>) -> Result<String, String> {
        if !logs_in(&set_http(), a, SET_TIMEOUT).await? {
            return Err("NZBGET_USER / NZBGET_PASS from .env don't log in: recreate it so the image applies them (docker compose up -d --force-recreate nzbget)".into());
        }
        let u = crate::credentials(a).await.0;
        match want {
            Wanted::Tunnel => Ok(format!("logs in as {u}; the gate signs you in to it")),
            Wanted::Apps { user, .. } if u != user => Err(format!(
                "NZBGET_USER is {u:?}, not {user:?}: set NZBGET_USER and NZBGET_PASS in .env to the apps' login"
            )),
            Wanted::Apps { .. } => Ok(format!("basic login, user {u} (from .env)")),
        }
    }
}

// ---- backup: its torrents/NZBs, not its downloads -----------------------------------------

fn payload_rules(src: &Path) -> PayloadRules {
    let mut r = PayloadRules::default();
    let conf = kv(&src.join("nzbget.conf"), '=');
    let main = conf.get("MainDir").cloned().unwrap_or_else(|| "/config".into());
    let val: HashMap<&str, String> = ["DestDir", "InterDir", "TempDir", "NzbDir", "QueueDir"]
        .into_iter()
        .map(|k| (k, conf.get(k).cloned().unwrap_or_default().replace("${MainDir}", &main)))
        .collect();
    r.settings = val.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
    r.settings.insert("MainDir".into(), main);
    for (key, label) in
        [("DestDir", "completed downloads"), ("InterDir", "intermediate downloads"), ("TempDir", "temp files")]
    {
        match in_config(&val[key]) {
            Some(rel) if !rel.is_empty() => {
                r.skip.push(rel.clone());
                r.labels.insert(rel, label.into());
            }
            _ if !val[key].is_empty() => {
                r.outside.insert(label.into(), val[key].clone());
            }
            _ => {}
        }
    }
    for (key, label) in [("NzbDir", "incoming NZBs"), ("QueueDir", "queue (NZBs + state)")] {
        if !val[key].is_empty() && in_config(&val[key]).is_none() {
            r.outside.insert(label.into(), val[key].clone());
        }
    }
    r
}

fn payload_items(dest: &Path, rules: &PayloadRules) -> Vec<Value> {
    let mut items = vec![];
    for (key, label) in [("NzbDir", "incoming NZBs"), ("QueueDir", "queue files (NZBs + download state)")] {
        if let Some(rel) = in_config(&rules.settings.get(key).cloned().unwrap_or_default()) {
            let u = (!rel.is_empty()).then_some(rel.as_str());
            items.push(json!({"label": label, "count": count(dest, "*", u)}));
        }
    }
    items
}
