// SPDX-License-Identifier: GPL-3.0-or-later
//! Bazarr: subtitles for Sonarr and Radarr.

pub mod client;

pub use client::BazarrClient;

use async_trait::async_trait;
use serde_json::Value;

use crate::AnyClient;
use crate::AppConfigExt;
use crate::KeyFile;
use client::Body;
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;
use wranglarr_core::value::{int, text, truthy};

use super::link::{LinkHost, LinkOutcome, LinkSource, LinkState, LinkTarget, WireMode};
use super::login::{
    Login, LoginApi, LoginSource, LoginState, Managed, READ_TIMEOUT, SET_TIMEOUT, Wanted, base, get, http_err, json_of,
    key, raise_for_status, set_http, str_or_empty,
};
use super::{
    ARR_URL_BASE, BackupMode, Container, CopyRules, Credentials, Database, FeedKind, HomeGroup, Port, ProxyTraits,
    Roles, ServiceDef, Takes, Wizard,
};

fn build(app: &AppConfig) -> Result<AnyClient> {
    Ok(AnyClient::new(BazarrClient::new(&app.name, &app.url, &app.api_key_blocking()?, &app.config_dir)))
}

pub static DEF: ServiceDef = ServiceDef {
    id: "bazarr",
    label: "Bazarr",
    description: "Subtitles",
    roles: Roles::SUBTITLES,
    home_group: HomeGroup::Metadata,
    color: "#7b68ee",
    primary_count: "subtitles_series",
    logo_paths: &["/images/logo128.png"],
    status_page: Some("/system/status"),
    container: Container {
        image: "lscr.io/linuxserver/bazarr:latest",
        ports: &[Port::tcp(6767, 6767)],
        config_mount: "/config",
        env: &[],
        run_as_user: false,
        data: true,
        vpn: true,
    },
    copy: CopyRules {
        skip_dirs: &["log", "backup", "cache", "restore"],
        skip_files: &[],
        skip_sqlite: true,
        skip_files_in: &[],
        zip_layout: &[("config.yaml", "config/config.yaml"), ("bazarr.db", "db/bazarr.db")],
    },
    credentials: Credentials::ApiKey { file: KeyFile::YamlKey("apikey") },
    backup: BackupMode::ApiZip,
    database: Some(Database {
        env: &[
            ("POSTGRES_ENABLED", "true"),
            ("POSTGRES_HOST", "{pg_host}"),
            ("POSTGRES_PORT", "5432"),
            ("POSTGRES_DATABASE", "{db}"),
            ("POSTGRES_USERNAME", "${POSTGRES_USER}"),
            ("POSTGRES_PASSWORD", "${POSTGRES_PASSWORD}"),
            ("PGSSLCERT", "/tmp/postgresql.crt"),
        ],
    }),
    proxy: ProxyTraits {
        url_base: Some(ARR_URL_BASE),
        own_name: false,
        own_login: false,
        public_preset: true,
        host_check: false,
        public_note: None,
    },
    wizard: Some(Wizard { group: "media", config: Some("config/config.yaml") }),
    takes: Some(Takes { kinds: &["sonarr", "radarr"], one_each: true, after_setup: false }),
    arr: None,
    downloads: None,
    login: Some(Login::Managed(Managed {
        source: LoginSource::Api,
        ready_path: "/api/system/ping",
        init_checks_username: false,
        api: &BazarrLogin,
    })),
    feeds: &[FeedKind::Missing],
    build,
};

/// Its login lives in its settings (`auth.type`: None or form).
struct BazarrLogin;

/// `GET api/system/settings`.
async fn settings(
    c: &reqwest::Client,
    a: &AppConfig,
    key: &str,
    timeout: std::time::Duration,
) -> Result<Value, String> {
    let url = format!("{}/api/system/settings", base(a));
    let r = c.get(&url).header("X-API-KEY", key).timeout(timeout).send().await.map_err(http_err)?;
    if !r.status().is_success() {
        return Err(format!("{} from {url}", r.status()));
    }
    json_of(r).await
}

async fn api_key(a: &AppConfig) -> Result<String, String> {
    crate::api_key(a).await.map_err(|e| e.to_string())
}

#[async_trait]
impl LoginApi for BazarrLogin {
    async fn read(&self, a: &AppConfig) -> Result<LoginState, String> {
        let s = settings(&wranglarr_core::http::http(READ_TIMEOUT), a, &api_key(a).await?, READ_TIMEOUT).await?;
        let au = &s["auth"];
        let t = str_or_empty(&au["type"]).to_lowercase();
        let method = if t.is_empty() || t == "none" { "none" } else { t.as_str() };
        Ok(LoginState::new(method, str_or_empty(&au["username"])))
    }

    async fn set(&self, a: &AppConfig, want: Wanted<'_>) -> Result<String, String> {
        let b = base(a);
        let api = api_key(a).await?;
        let data: Vec<(&str, &str)> = match want {
            Wanted::Tunnel => vec![("settings-auth-type", "None")],
            Wanted::Apps { user, password, .. } => {
                vec![
                    ("settings-auth-type", "form"),
                    ("settings-auth-username", user),
                    ("settings-auth-password", password),
                ]
            }
        };
        let c = set_http();
        let r = c
            .post(format!("{b}/api/system/settings"))
            .header("X-API-KEY", &api)
            .form(&data)
            .send()
            .await
            .map_err(http_err)?;
        raise_for_status(&r)?;
        let Wanted::Apps { user, password, .. } = want else {
            let s = settings(&c, a, &api, SET_TIMEOUT).await?;
            let t = get(key(&s, "auth")?, "type");
            if !(t.is_null() || t.as_str() == Some("None") || t.as_str() == Some("")) {
                return Err(format!("login is still {}", wranglarr_core::value::text(&t)));
            }
            return Ok("no login of its own, the gate signs you in".into());
        };
        let r = c
            .post(format!("{b}/api/system/account"))
            .query(&[("action", "login")])
            .form(&[("username", user), ("password", password)])
            .timeout(SET_TIMEOUT)
            .send()
            .await
            .map_err(http_err)?;
        if r.status().as_u16() != 204 {
            return Err(format!("saved, but signing in with it got {}", r.status().as_u16()));
        }
        Ok(format!("login page, user {user}: signed in with it"))
    }
}

// ---- Connect: one Sonarr and one Radarr, in its settings ----------------------------------

/// Validate the settings sections before attempting to change connections.
async fn current_settings(c: &BazarrClient) -> wranglarr_core::error::Result<Value> {
    let s = c.req(reqwest::Method::GET, "system/settings", &[], Body::None, None).await?;
    if !s.is_object() || !s["general"].is_object() {
        return Err(wranglarr_core::error::Error::upstream(format!(
            "{}: system/settings: expected settings with a general object from the Bazarr API",
            c.name
        )));
    }
    Ok(s)
}

fn validate_link_settings(s: &Value, kind: &str) -> wranglarr_core::error::Result<()> {
    let enabled = &s["general"][format!("use_{kind}").as_str()];
    if enabled.is_null() || (truthy(enabled) && !s[kind].is_object()) {
        return Err(wranglarr_core::error::Error::upstream(format!(
            "Bazarr system/settings: missing {kind} connection settings"
        )));
    }
    Ok(())
}

#[async_trait]
impl LinkSource for BazarrClient {
    async fn link_state(&self, to: &LinkTarget<'_>) -> wranglarr_core::error::Result<LinkOutcome> {
        let s = current_settings(self).await?;
        let kind = to.app.kind.as_str();
        validate_link_settings(&s, kind)?;
        let sec = &s[kind];
        if !truthy(&s["general"][format!("use_{kind}").as_str()]) {
            return Ok(LinkOutcome::new(LinkState::Missing, ""));
        }
        if sec["ip"].as_str() != Some(to.host.as_str()) || int(&sec["port"]) != i64::from(to.port) {
            return Ok(LinkOutcome::new(
                LinkState::Differs,
                format!("points at {}:{}", text(&sec["ip"]), text(&sec["port"])),
            ));
        }
        if sec["apikey"].as_str() != Some(crate::api_key(to.app).await?.as_str()) {
            return Ok(LinkOutcome::new(LinkState::Differs, "different API key"));
        }
        Ok(LinkOutcome::new(LinkState::Connected, ""))
    }

    async fn wire(
        &self,
        to: &LinkTarget<'_>,
        mode: WireMode,
        _: &dyn LinkHost,
    ) -> wranglarr_core::error::Result<LinkOutcome> {
        let kind = to.app.kind.as_str();
        let s = current_settings(self).await?;
        validate_link_settings(&s, kind)?;
        if mode == WireMode::New && truthy(&s["general"][format!("use_{kind}").as_str()]) {
            let sec = &s[kind];
            return Ok(LinkOutcome::new(
                LinkState::Kept,
                format!("already uses {}:{}", text(&sec["ip"]), text(&sec["port"])),
            ));
        }
        let (host, port) = (&to.host, to.port);
        let form: Vec<(String, String)> = vec![
            (format!("settings-general-use_{kind}"), "true".into()),
            (format!("settings-{kind}-ip"), host.clone()),
            (format!("settings-{kind}-port"), port.to_string()),
            (format!("settings-{kind}-base_url"), if to.base.is_empty() { "/".into() } else { to.base.clone() }),
            (format!("settings-{kind}-ssl"), if to.ssl { "true" } else { "false" }.into()),
            (format!("settings-{kind}-apikey"), crate::api_key(to.app).await?),
        ];
        self.req(reqwest::Method::POST, "system/settings", &[], Body::Form(&form), None).await?;
        Ok(LinkOutcome::new(LinkState::Connected, format!("{host}:{port}")))
    }
}

#[cfg(test)]
mod connection_settings_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_settings_cannot_be_treated_as_a_new_connection() {
        assert!(validate_link_settings(&json!({"general": {}}), "radarr").is_err());
        assert!(validate_link_settings(&json!({"general": {"use_radarr": true}}), "radarr").is_err());
        assert!(validate_link_settings(&json!({"general": {"use_radarr": false}}), "radarr").is_ok());
        assert!(validate_link_settings(&json!({"general": {"use_radarr": true}, "radarr": {}}), "radarr").is_ok());
    }
}
