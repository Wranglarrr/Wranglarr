// SPDX-License-Identifier: GPL-3.0-or-later
//! Seerr: requests for Radarr and Sonarr, signed in with Plex / Jellyfin / Emby.

pub mod client;

pub use client::SeerrClient;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use crate::AnyClient;
use crate::AppConfigExt;
use crate::KeyFile;
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;
use wranglarr_core::value::{int, text, truthy};

use super::link::{LinkHost, LinkOutcome, LinkSource, LinkState, LinkTarget, WireMode};
use super::login::Login;
use super::{
    BackupMode, Container, CopyRules, Credentials, Database, FeedKind, HomeGroup, Port, ProxyTraits, Roles, ServiceDef,
    Takes, Wizard,
};

fn build(app: &AppConfig) -> Result<AnyClient> {
    Ok(AnyClient::new(SeerrClient::new(&app.name, &app.url, &app.api_key_blocking()?, &app.config_dir)))
}

pub static DEF: ServiceDef = ServiceDef {
    id: "seerr",
    label: "Seerr",
    description: "Requests for Radarr / Sonarr (Plex, Jellyfin, Emby)",
    roles: Roles::REQUESTS,
    home_group: HomeGroup::Requests,
    color: "#7c5cff",
    primary_count: "requests",
    logo_paths: &["/os_icon.svg", "/android-chrome-512x512.png"],
    status_page: None,
    container: Container {
        image: "ghcr.io/seerr-team/seerr:latest",
        ports: &[Port::tcp(5055, 5055)],
        config_mount: "/app/config",
        env: &[],
        // no PUID of its own
        run_as_user: true,
        data: false,
        vpn: false,
    },
    // requests live in Postgres; db/ only holds an unused (or, on SQLite, live) database file
    copy: CopyRules {
        skip_dirs: &["logs", "cache"],
        skip_files: &["*.sqlite3", "*.sqlite3-shm", "*.sqlite3-wal"],
        skip_sqlite: true,
        skip_files_in: &[],
        zip_layout: &[],
    },
    credentials: Credentials::ApiKey { file: KeyFile::JsonPointer("/main/apiKey") },
    backup: BackupMode::ConfigDir,
    database: Some(Database {
        env: &[
            ("DB_TYPE", "postgres"),
            ("DB_HOST", "{pg_host}"),
            ("DB_PORT", "5432"),
            ("DB_USER", "${POSTGRES_USER}"),
            ("DB_PASS", "${POSTGRES_PASSWORD}"),
            ("DB_NAME", "{db}"),
        ],
    }),
    proxy: ProxyTraits {
        url_base: None,
        own_name: true,
        own_login: true,
        public_preset: false,
        host_check: false,
        public_note: None,
    },
    wizard: Some(Wizard { group: "requests", config: None }),
    takes: Some(Takes { kinds: &["radarr", "sonarr"], one_each: false, after_setup: true }),
    arr: None,
    downloads: None,
    login: Some(Login::Own { accounts: "media server accounts" }),
    feeds: &[FeedKind::Requests],
    build,
};

// ---- Connect: its Radarr and Sonarr servers ----------------------------------------------

/// Its entry for the linked app: same name, or the same host.
async fn find(c: &SeerrClient, to: &LinkTarget<'_>) -> Result<Option<Value>> {
    for r in c.servers(&to.app.kind).await? {
        let h = r["hostname"].as_str();
        if r["name"].as_str() == Some(to.app.name.as_str())
            || h == Some(to.host.as_str())
            || (h.is_some() && h == to.url_host.as_deref())
        {
            return Ok(Some(r));
        }
    }
    Ok(None)
}

fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[async_trait]
impl LinkSource for SeerrClient {
    async fn accepts_links(&self) -> bool {
        self.initialized().await
    }

    async fn link_state(&self, to: &LinkTarget<'_>) -> Result<LinkOutcome> {
        if !self.initialized().await {
            return Ok(LinkOutcome::new(
                LinkState::Waiting,
                "Seerr isn't set up yet: open it and sign in with Plex, Jellyfin or Emby",
            ));
        }
        let Some(cur) = find(self, to).await? else { return Ok(LinkOutcome::new(LinkState::Missing, "")) };
        if cur["hostname"].as_str() != Some(to.host.as_str()) || int(&cur["port"]) != i64::from(to.port) {
            return Ok(LinkOutcome::new(
                LinkState::Differs,
                format!("points at {}:{}", text(&cur["hostname"]), text(&cur["port"])),
            ));
        }
        if truthy(&cur["apiKey"]) && cur["apiKey"].as_str() != Some(crate::api_key(to.app).await?.as_str()) {
            return Ok(LinkOutcome::new(LinkState::Differs, "different API key"));
        }
        Ok(LinkOutcome::new(
            LinkState::Connected,
            format!("'{}', {}, {}", text(&cur["name"]), text(&cur["activeProfileName"]), text(&cur["activeDirectory"])),
        ))
    }

    async fn wire(&self, to: &LinkTarget<'_>, mode: WireMode, links: &dyn LinkHost) -> Result<LinkOutcome> {
        let dst = to.app;
        // Seerr's API spells the arr kind: settings/radarr, settings/sonarr
        let kind = dst.kind.as_str();
        if !self.initialized().await {
            return Ok(LinkOutcome::new(
                LinkState::Waiting,
                "Seerr isn't set up yet: open it and sign in with Plex, Jellyfin or Emby; wranglarr adds it after",
            ));
        }
        let (host, port) = (&to.host, to.port);
        let cur = find(self, to).await?;
        if let Some(cur) = &cur
            && mode == WireMode::New
        {
            return Ok(LinkOutcome::new(LinkState::Kept, format!("already has '{}'", text(&cur["name"]))));
        }
        let conn = obj([
            ("hostname", json!(host)),
            ("port", json!(port)),
            ("apiKey", json!(crate::api_key(dst).await?)),
            ("useSsl", json!(to.ssl)),
            ("baseUrl", json!(to.base)),
        ]);
        let t = self.test_server(kind, &Value::Object(conn.clone())).await?; // also proves Seerr can reach it
        let profiles = t["profiles"].as_array().cloned().unwrap_or_default();
        let roots = t["rootFolders"].as_array().cloned().unwrap_or_default();
        let (Some(first_prof), Some(first_root)) = (profiles.first(), roots.first()) else {
            return Ok(LinkOutcome::new(
                LinkState::Failed,
                if profiles.is_empty() {
                    format!("{} has no quality profiles", dst.name)
                } else {
                    format!(
                        "{} has no root folder yet: add one (Settings → Media Management), then Connect apps",
                        dst.name
                    )
                },
            ));
        };
        let none = json!({});
        let cur_v = cur.as_ref().unwrap_or(&none);
        let has = |k: &str| cur_v.get(k).is_some();
        // keep what the user picked in Seerr; otherwise the first profile and root folder
        let prof = profiles
            .iter()
            .find(|p| cur.is_some() && p["id"] == cur_v["activeProfileId"] && has("activeProfileId"))
            .unwrap_or(first_prof)
            .clone();
        let root = roots
            .iter()
            .find(|r| cur.is_some() && r["path"] == cur_v["activeDirectory"] && has("activeDirectory"))
            .unwrap_or(first_root)["path"]
            .clone();
        let others: Vec<Value> =
            self.servers(kind).await?.into_iter().filter(|r| cur.is_none() || r.get("id") != cur_v.get("id")).collect();
        let is4k = dst.name.to_lowercase().contains("4k");
        let get_or = |k: &str, d: Value| cur_v.get(k).cloned().unwrap_or(d);
        let mut body: Map<String, Value> = cur_v.as_object().cloned().unwrap_or_default();
        body.extend(conn);
        let default_flag = !others.iter().any(|r| truthy(&r["isDefault"]) && r["is4k"] == json!(is4k));
        let external = match cur_v.get("externalUrl").filter(|v| truthy(v)) {
            Some(v) => v.clone(),
            None => json!(links.public_url(dst).await),
        };
        body.extend(obj([
            ("name", json!(dst.name)),
            ("activeProfileId", prof["id"].clone()),
            ("activeProfileName", prof["name"].clone()),
            ("activeDirectory", root.clone()),
            ("is4k", json!(is4k)),
            ("isDefault", get_or("isDefault", json!(default_flag))),
            ("externalUrl", external),
            ("syncEnabled", get_or("syncEnabled", json!(true))),
            ("preventSearch", get_or("preventSearch", json!(false))),
            ("tags", get_or("tags", json!([]))),
        ]));
        let mut setdefault = |k: &str, v: Value| {
            body.entry(k.to_string()).or_insert(v);
        };
        if kind == "radarr" {
            setdefault("minimumAvailability", json!("released"));
        } else {
            setdefault("enableSeasonFolders", json!(true));
            setdefault("seriesType", json!("standard"));
            setdefault("animeSeriesType", json!("anime"));
            setdefault("activeAnimeProfileId", prof["id"].clone());
            setdefault("activeAnimeProfileName", prof["name"].clone());
            setdefault("activeAnimeDirectory", root.clone());
            setdefault("animeTags", json!([]));
            // Sonarr v3 only
            if let Some(lang) = t["languageProfiles"].as_array().and_then(|l| l.first()) {
                setdefault("activeLanguageProfileId", lang["id"].clone());
                setdefault("activeAnimeLanguageProfileId", lang["id"].clone());
            }
        }
        body.shift_remove("id");
        let sid = cur.as_ref().map(|c| int(&c["id"]));
        self.save_server(kind, &Value::Object(body), sid).await?;
        Ok(LinkOutcome::new(
            if cur.is_some() { LinkState::Updated } else { LinkState::Created },
            format!("'{}' → {host}:{port}, {}, {}", dst.name, text(&prof["name"]), text(&root)),
        ))
    }
}
