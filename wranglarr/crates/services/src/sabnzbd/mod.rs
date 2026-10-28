// SPDX-License-Identifier: GPL-3.0-or-later
//! SABnzbd: a Usenet client. Listens on 8080 in its container, 8085 on the host.

pub mod client;

pub use client::SabnzbdClient;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::login::{
    Login, LoginApi, LoginSource, LoginState, Managed, READ_TIMEOUT, SET_TIMEOUT, Wanted, base, get, http_err, json_of,
    key, set_cookies, set_http, str_or_empty,
};
use crate::AnyClient;
use crate::AppConfigExt;
use crate::KeyFile;
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;
use wranglarr_core::files::{PayloadRules, collect_files, count, has_part, in_config, kv};

use super::{
    BackupMode, Container, CopyRules, Credentials, DownloadsDef, HomeGroup, Payload, Port, ProxyTraits, Roles,
    ServiceDef, Wizard,
};

static DOWNLOADS: DownloadsDef = DownloadsDef {
    payload: Payload { rules: payload_rules, items: payload_items },
    arr_impl: "Sabnzbd",
    categories: true,
    backup_holds: "the <b>queue and history</b>, a <b>copy of every .nzb</b> added (the <code>nzb_backup_dir</code> folder), and the client's settings",
};

fn build(app: &AppConfig) -> Result<AnyClient> {
    Ok(AnyClient::new(SabnzbdClient::new(&app.name, &app.url, &app.api_key_blocking()?, &app.config_dir)))
}

pub static DEF: ServiceDef = ServiceDef {
    id: "sabnzbd",
    label: "SABnzbd",
    description: "Usenet client",
    roles: Roles::DOWNLOADS,
    home_group: HomeGroup::Downloads,
    color: "#f5c400",
    primary_count: "history",
    logo_paths: &["/staticcfg/images/logo-small.svg"],
    status_page: None,
    container: Container {
        image: "lscr.io/linuxserver/sabnzbd:latest",
        ports: &[Port::tcp(8085, 8080)],
        config_mount: "/config",
        env: &[],
        run_as_user: false,
        data: true,
        vpn: true,
    },
    copy: CopyRules { skip_dirs: &["logs"], skip_files: &[], skip_sqlite: false, skip_files_in: &[], zip_layout: &[] },
    credentials: Credentials::ApiKey { file: KeyFile::IniKey("api_key") },
    backup: BackupMode::ConfigDir,
    database: None,
    proxy: ProxyTraits {
        url_base: Some("Config → General → URL Base (url_base)"),
        own_name: false,
        own_login: false,
        public_preset: false,
        host_check: true,
        public_note: None,
    },
    wizard: Some(Wizard { group: "download", config: None }),
    takes: None,
    arr: None,
    downloads: Some(&DOWNLOADS),
    login: Some(Login::Managed(Managed {
        source: LoginSource::Api,
        ready_path: "/",
        init_checks_username: false,
        api: &SabnzbdLogin,
    })),
    feeds: &[],
    build,
};

/// Its login is `misc.username` / `misc.password` in its config.
struct SabnzbdLogin;

/// One call to its API (`mode` plus `params`).
async fn api(
    c: &reqwest::Client,
    a: &AppConfig,
    apikey: &str,
    mode: &str,
    params: &[(&str, &str)],
) -> Result<Value, String> {
    let mut q: Vec<(&str, &str)> = vec![("mode", mode)];
    q.extend_from_slice(params);
    q.extend([("output", "json"), ("apikey", apikey)]);
    let r = c.get(format!("{}/api", base(a))).query(&q).send().await.map_err(http_err)?;
    // not the client's error: its message carries the URL, API key included
    if r.status().as_u16() >= 400 {
        return Err(format!("SABnzbd answered {} to {mode}", r.status().as_u16()));
    }
    json_of(r).await
}

/// `config.misc`.
async fn misc(c: &reqwest::Client, a: &AppConfig, apikey: &str) -> Result<Value, String> {
    let cfg = api(c, a, apikey, "get_config", &[("section", "misc")]).await?;
    Ok(key(key(&cfg, "config")?, "misc")?.clone())
}

async fn api_key(a: &AppConfig) -> Result<String, String> {
    crate::api_key(a).await.map_err(|e| e.to_string())
}

#[async_trait]
impl LoginApi for SabnzbdLogin {
    async fn read(&self, a: &AppConfig) -> Result<LoginState, String> {
        let m = misc(&wranglarr_core::http::http(READ_TIMEOUT), a, &api_key(a).await?).await?;
        let u = str_or_empty(&m["username"]);
        Ok(LoginState::new(if u.is_empty() { "none" } else { "form" }, u))
    }

    async fn set(&self, a: &AppConfig, want: Wanted<'_>) -> Result<String, String> {
        let k = api_key(a).await?;
        let c = set_http();
        let (u, p) = match want {
            Wanted::Tunnel => ("", ""),
            Wanted::Apps { user, password, .. } => (user, password),
        };
        for (keyword, value) in [("username", u), ("password", p)] {
            api(&c, a, &k, "set_config", &[("section", "misc"), ("keyword", keyword), ("value", value)]).await?;
        }
        let Wanted::Apps { user, password, .. } = want else {
            if wranglarr_core::value::truthy(&get(&misc(&c, a, &k).await?, "username")) {
                return Err("still has a username".into());
            }
            return Ok("no login of its own, the gate signs you in".into());
        };
        let r = c
            .post(format!("{}/login/", base(a)))
            .form(&[("username", user), ("password", password)])
            .timeout(SET_TIMEOUT)
            .send()
            .await
            .map_err(http_err)?;
        if !set_cookies(&r).iter().any(|(k, _)| k == "login_cookie") {
            return Err("saved, but signing in with it didn't work".into());
        }
        Ok(format!("login page, user {user}: signed in with it"))
    }
}

// ---- backup: its torrents/NZBs, not its downloads -----------------------------------------

fn payload_rules(src: &Path) -> PayloadRules {
    let mut r = PayloadRules::default();
    let unq = |s: &str| s.trim_matches('"').to_string();
    let ini = kv(&src.join("sabnzbd.ini"), '=');
    let get = |k: &str| ini.get(k).cloned().unwrap_or_default();
    for key in ["download_dir", "complete_dir", "nzb_backup_dir", "dirscan_dir", "admin_dir"] {
        r.settings.insert(key.into(), unq(&get(key)));
    }
    for (key, label) in
        [("complete_dir", "completed downloads"), ("download_dir", "incomplete downloads (job folders)")]
    {
        match in_config(&get(key)) {
            Some(rel) if !rel.is_empty() => {
                if key == "download_dir" {
                    r.admin_only.push(rel.clone())
                } else {
                    r.skip.push(rel.clone())
                }
                r.labels.insert(rel, label.into());
            }
            _ if !unq(&get(key)).is_empty() => {
                r.outside.insert(label.into(), unq(&get(key)));
            }
            _ => {}
        }
    }
    r
}

fn payload_items(dest: &Path, rules: &PayloadRules) -> Vec<Value> {
    let mut items = vec![];
    if !rules.admin_only.is_empty() {
        let mut all = vec![];
        collect_files(dest, &mut all);
        let jobs: HashSet<PathBuf> =
            all.iter().filter(|p| has_part(p, "__ADMIN__")).filter_map(|p| p.parent().map(Path::to_path_buf)).collect();
        items.push(json!({"label": "queued jobs with their NZB", "count": jobs.len()}));
    }
    let s = |k: &str| rules.settings.get(k).cloned().unwrap_or_default();
    if let Some(b) = in_config(&s("nzb_backup_dir")).filter(|x| !x.is_empty()) {
        items.push(
            json!({"label": format!("NZB copies in {b}/ (nzb_backup_dir)"), "count": count(dest, "*.nzb*", Some(&b))}),
        );
    }
    if let Some(sc) = in_config(&s("dirscan_dir")).filter(|x| !x.is_empty()) {
        items.push(json!({"label": format!("NZBs waiting in the watched folder {sc}/"), "count": count(dest, "*.nzb*", Some(&sc))}));
    }
    items.push(json!({"label": "history database", "count": count(dest, "history*.db", None)}));
    items
}
