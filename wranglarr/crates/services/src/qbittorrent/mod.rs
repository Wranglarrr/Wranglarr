// SPDX-License-Identifier: GPL-3.0-or-later
//! qBittorrent: the torrent client.

pub mod client;

pub use client::QbittorrentClient;

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::login::{
    Login, LoginApi, LoginSource, LoginState, Managed, READ_TIMEOUT, SET_TIMEOUT, Wanted, base, get, http_err, json_of,
    set_cookies, set_http, str_or_empty,
};
use crate::AnyClient;
use crate::AppConfigExt;
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;
use wranglarr_core::files::{PayloadRules, collect_files, count, has_part, in_config, kv};

use super::{
    BackupMode, Container, CopyRules, Credentials, DownloadsDef, HomeGroup, Payload, Port, ProxyTraits, Roles,
    ServiceDef, Wizard,
};

static DOWNLOADS: DownloadsDef = DownloadsDef {
    payload: Payload { rules: payload_rules, items: payload_items },
    arr_impl: "QBittorrent",
    categories: false,
    backup_holds: "the <b>.torrent file</b> of every torrent plus its resume data (progress, save path, category, tags), and the client's settings",
};

fn build(app: &AppConfig) -> Result<AnyClient> {
    let (u, p) = app.credentials_blocking();
    Ok(AnyClient::new(QbittorrentClient::new(&app.name, &app.url, &u, &p, &app.config_dir)))
}

pub static DEF: ServiceDef = ServiceDef {
    id: "qbittorrent",
    label: "qBittorrent",
    description: "Torrent client",
    roles: Roles::DOWNLOADS,
    home_group: HomeGroup::Downloads,
    color: "#2f67ba",
    primary_count: "torrents",
    logo_paths: &["/images/qbittorrent-tray.svg"],
    status_page: None,
    container: Container {
        image: "lscr.io/linuxserver/qbittorrent:latest",
        ports: &[Port::tcp(8090, 8090), Port::tcp(6881, 6881), Port::udp(6881, 6881)],
        config_mount: "/config",
        env: &[("WEBUI_PORT", "8090"), ("TORRENTING_PORT", "6881")],
        run_as_user: false,
        data: true,
        vpn: true,
    },
    copy: CopyRules {
        skip_dirs: &["logs", "GeoDB"],
        skip_files: &["lockfile"],
        skip_sqlite: false,
        skip_files_in: &[],
        zip_layout: &[],
    },
    credentials: Credentials::UserPassword {
        env_user: "QBITTORRENT_USER",
        env_pass: "QBITTORRENT_PASSWORD",
        default_user: "admin",
        default_pass: None,
        config_keys: None,
        basic_auth: false,
    },
    backup: BackupMode::ConfigDir,
    database: None,
    proxy: ProxyTraits {
        url_base: None,
        own_name: false,
        own_login: false,
        public_preset: false,
        host_check: false,
        public_note: Some(
            "qBittorrent public: keep its login (or AuthTunnel), and leave its brute-force ban on (Options → Web UI).",
        ),
    },
    wizard: Some(Wizard { group: "download", config: None }),
    takes: None,
    arr: None,
    downloads: Some(&DOWNLOADS),
    login: Some(Login::Managed(Managed {
        source: LoginSource::Env,
        ready_path: "/",
        init_checks_username: false,
        api: &QbittorrentLogin,
    })),
    feeds: &[],
    build,
};

/// init writes its login into qBittorrent.conf (and, for AuthTunnel, whitelists the
/// gate's network), so this only checks it.
struct QbittorrentLogin;

/// Sign in with .env's login and read its preferences; None when the login is refused.
async fn preferences(c: &reqwest::Client, a: &AppConfig, timeout: Duration) -> Result<Option<Value>, String> {
    let (u, p) = crate::credentials(a).await;
    let b = base(a);
    let r = c
        .post(format!("{b}/api/v2/auth/login"))
        .header("Referer", b)
        .form(&[("username", u.as_str()), ("password", p.as_str())])
        .timeout(timeout)
        .send()
        .await
        .map_err(http_err)?;
    let status = r.status().as_u16();
    let cookies = set_cookies(&r).iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
    let text = wranglarr_core::http::response_text(r).await.map_err(|e| e.describe(wranglarr_core::http::reason))?;
    if !(status == 200 || status == 204) || !["", "Ok."].contains(&text.trim()) {
        return Ok(None);
    }
    let mut rq = c.get(format!("{b}/api/v2/app/preferences")).timeout(timeout);
    if !cookies.is_empty() {
        rq = rq.header(reqwest::header::COOKIE, cookies);
    }
    json_of(rq.send().await.map_err(http_err)?).await.map(Some)
}

#[async_trait]
impl LoginApi for QbittorrentLogin {
    async fn read(&self, a: &AppConfig) -> Result<LoginState, String> {
        let Some(pr) = preferences(&wranglarr_core::http::http(READ_TIMEOUT), a, READ_TIMEOUT).await? else {
            return Ok(LoginState {
                error: Some("QBITTORRENT_USER / QBITTORRENT_PASSWORD from .env don't log in".into()),
                ..LoginState::new("?", crate::credentials(a).await.0)
            });
        };
        Ok(LoginState {
            bypass: Some(wranglarr_core::value::truthy(&pr["bypass_auth_subnet_whitelist_enabled"])),
            ..LoginState::new("form", str_or_empty(&pr["web_ui_username"]))
        })
    }

    async fn set(&self, a: &AppConfig, want: Wanted<'_>) -> Result<String, String> {
        let Some(pr) = preferences(&set_http(), a, SET_TIMEOUT).await? else {
            return Err("QBITTORRENT_USER / QBITTORRENT_PASSWORD from .env don't log in (init writes them into qBittorrent.conf; was QBITTORRENT_PASSWORD empty?)".into());
        };
        let u = crate::credentials(a).await.0;
        if let Wanted::Tunnel = want {
            if !wranglarr_core::value::truthy(&get(&pr, "bypass_auth_subnet_whitelist_enabled")) {
                return Err(
                    "the gate's network isn't whitelisted, so qBittorrent would ask for its own login too".into()
                );
            }
            return Ok(format!("logs in as {u}; the gate's network gets in without it"));
        }
        let name = str_or_empty(&get(&pr, "web_ui_username"));
        Ok(format!("login page, user {}: signed in with it", if name.is_empty() { u } else { name }))
    }
}

// ---- backup: its torrents/NZBs, not its downloads -----------------------------------------

fn payload_rules(src: &Path) -> PayloadRules {
    let mut r = PayloadRules::default();
    for conf in [src.join("qBittorrent/qBittorrent.conf"), src.join("config/qBittorrent.conf")] {
        if conf.exists() {
            let c = kv(&conf, '=');
            let pick =
                |a: &str, b: &str| c.get(a).filter(|v| !v.is_empty()).or_else(|| c.get(b)).cloned().unwrap_or_default();
            let save = pick("Session\\DefaultSavePath", "Downloads\\SavePath");
            let temp = pick("Session\\TempPath", "Downloads\\TempPath");
            r.settings.insert("save_path".into(), save.clone());
            r.settings.insert("temp_path".into(), temp.clone());
            for (v, label) in [(save, "downloaded data"), (temp, "incomplete downloads")] {
                match in_config(&v) {
                    Some(rel) if !rel.is_empty() => {
                        r.skip.push(rel.clone());
                        r.labels.insert(rel, label.into());
                    }
                    _ if !v.is_empty() => {
                        r.outside.insert(label.into(), v);
                    }
                    _ => {}
                }
            }
            break;
        }
    }
    r
}

fn payload_items(dest: &Path, _: &PayloadRules) -> Vec<Value> {
    let mut items = vec![];
    let mut all = vec![];
    collect_files(dest, &mut all);
    let torrents =
        all.iter().filter(|p| p.extension().is_some_and(|e| e == "torrent") && has_part(p, "BT_backup")).count();
    items.push(json!({"label": ".torrent files", "count": torrents}));
    items.push(
        json!({"label": "resume files (progress, save path, category)", "count": count(dest, "*.fastresume", None)}),
    );
    items
}
