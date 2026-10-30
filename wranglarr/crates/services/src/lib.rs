// SPDX-License-Identifier: GPL-3.0-or-later
//! The services wranglarr manages, one file each, and what it knows about them.
//!
//! A [`ServiceDef`] is static data: labels, image and ports, config files, copy
//! rules, credentials, proxy traits. Modules look a kind up here ([`get`]) and ask
//! what it *can do* (its [`Roles`], `arr`, `downloads`), never which kind it is.
//! Adding a service is a folder here plus a line in [`ALL`]; the app and the core
//! never name a kind (`tests/kinds.rs`). See wiki/Development.md, Services.

use serde::Serialize;
use serde_json::{Map, Value, json};

use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;

pub mod apps;
pub mod arr;
pub mod bazarr;
mod caps;
mod client;
pub mod downloads;
pub mod feed;
mod file_app;
pub mod image_tracks;
mod key_file;
pub mod link;
pub mod login;
pub mod nzbget;
pub mod qbittorrent;
pub mod sabnzbd;
pub mod seerr;

pub use apps::{AppConfigExt, api_key, credentials};
pub use arr::ArrDef;
pub use caps::{DownloadClient, Feed, Service};
pub use client::{AnyClient, client, client_blocking};
pub use key_file::{KeyFile, KeyFileError};
pub use wranglarr_core::files::{CopyRules, CopySpec, Payload};

/// Whether an API response advertises JSON, including structured suffix types.
pub(crate) fn is_json_type(headers: &reqwest::header::HeaderMap) -> bool {
    headers.get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| {
        let media = v.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        media == "application/json" || media.ends_with("+json")
    })
}

/// Every service, in the order the UI lists them.
pub static ALL: &[&ServiceDef] = &[
    &arr::radarr::DEF,
    &arr::sonarr::DEF,
    &arr::lidarr::DEF,
    &arr::readarr::DEF,
    &arr::prowlarr::DEF,
    &arr::whisparr::DEF,
    &bazarr::DEF,
    &qbittorrent::DEF,
    &sabnzbd::DEF,
    &nzbget::DEF,
    &seerr::DEF,
];

/// The service with this id (the `kind` in wranglarr.toml).
pub fn get(id: &str) -> Option<&'static ServiceDef> {
    ALL.iter().copied().find(|d| d.id == id)
}

/// Every service with all of `roles`.
pub fn with(roles: Roles) -> impl Iterator<Item = &'static ServiceDef> {
    ALL.iter().copied().filter(move |d| d.roles.contains(roles))
}

/// Whether `id` is a service with all of `roles` (false for unknown ids).
pub fn has(id: &str, roles: Roles) -> bool {
    get(id).is_some_and(|d| d.roles.contains(roles))
}

/// What a service is for. A kind can have several.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roles(u16);

impl Roles {
    /// One of the *arr family (the shared API, config.xml, Connect).
    pub const ARR: Roles = Roles(1);
    /// Manages a media library (an arr other than Prowlarr): takes download
    /// clients and indexers, has a calendar and a missing list.
    pub const LIBRARY: Roles = Roles(1 << 1);
    /// Manages indexers for the library arrs (Prowlarr).
    pub const INDEXERS: Roles = Roles(1 << 2);
    /// Subtitles for the library arrs (Bazarr).
    pub const SUBTITLES: Roles = Roles(1 << 3);
    /// A download client the arrs send to.
    pub const DOWNLOADS: Roles = Roles(1 << 4);
    /// A request front end (Seerr).
    pub const REQUESTS: Roles = Roles(1 << 5);

    pub const fn union(self, other: Roles) -> Roles {
        Roles(self.0 | other.0)
    }
    pub const fn contains(self, other: Roles) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Roles {
    type Output = Roles;
    fn bitor(self, rhs: Roles) -> Roles {
        self.union(rhs)
    }
}

/// The home page group a service's card goes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeGroup {
    Media,
    Downloads,
    Requests,
    Metadata,
}

impl HomeGroup {
    /// The home page groups, in order.
    pub const ALL: [HomeGroup; 4] = [HomeGroup::Media, HomeGroup::Downloads, HomeGroup::Requests, HomeGroup::Metadata];

    pub fn key(self) -> &'static str {
        match self {
            HomeGroup::Media => "media",
            HomeGroup::Downloads => "downloads",
            HomeGroup::Requests => "requests",
            HomeGroup::Metadata => "metadata",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            HomeGroup::Media => "Media",
            HomeGroup::Downloads => "Downloads",
            HomeGroup::Requests => "Requests",
            HomeGroup::Metadata => "Indexers & subtitles",
        }
    }
}

/// A list under an app on the home page. The keys are persisted: each is a
/// switch in `home:settings`, and the page finds the list by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedKind {
    /// What's coming up (an arr's calendar).
    Upcoming,
    /// What's monitored, out and still missing (an arr's files, Bazarr's subtitles).
    Missing,
    /// Indexers, failing ones first (Prowlarr).
    Indexers,
    /// The latest requests (Seerr).
    Requests,
}

impl FeedKind {
    pub const ALL: [FeedKind; 4] = [FeedKind::Upcoming, FeedKind::Missing, FeedKind::Indexers, FeedKind::Requests];

    pub const fn key(self) -> &'static str {
        match self {
            FeedKind::Upcoming => "upcoming",
            FeedKind::Missing => "missing",
            FeedKind::Indexers => "indexers",
            FeedKind::Requests => "requests",
        }
    }
}

/// How a service is backed up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupMode {
    /// The app's own backup task makes a zip, restored through its API.
    ApiZip,
    /// No backup API: the config folder is copied, and restored with the
    /// container stopped.
    ConfigDir,
}

/// How wranglarr signs in to a service's API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Credentials {
    /// An API key: given, or read from its config file (`api_key_file`), where
    /// it is kept as `file` says.
    ApiKey { file: KeyFile },
    /// A username and password, from these `.env` variables. `default_pass`: the
    /// image's own default, which the restore stack falls back to.
    UserPassword {
        env_user: &'static str,
        env_pass: &'static str,
        default_user: &'static str,
        default_pass: Option<&'static str>,
        /// The `key=` lines of its config file (`api_key_file`) that hold the login,
        /// read when wranglarr.toml doesn't give it.
        config_keys: Option<(&'static str, &'static str)>,
        /// HTTP basic auth on every request, its images too (NZBGet).
        basic_auth: bool,
    },
}

/// How the restore stack points a service at Postgres: the environment it sets
/// on the service's container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Database {
    /// `(variable, value)`, in order. `{db}` and `{pg_host}` in a value become the
    /// database's name and the Postgres host.
    pub env: &'static [(&'static str, &'static str)],
}

impl Database {
    /// The Postgres keys live in the service's own config file, restored with it
    /// (the arrs' config.xml): no environment.
    pub const IN_CONFIG: Database = Database { env: &[] };

    /// The restore stack's environment lines (compose YAML, indented) pointing the
    /// app at database `db` on `pg_host`.
    pub fn compose_env(&self, db: &str, pg_host: &str) -> Vec<String> {
        self.env
            .iter()
            .map(|(name, value)| {
                let value = value.replace("{db}", db).replace("{pg_host}", pg_host);
                format!("      {name}: {}", compose_scalar(&value))
            })
            .collect()
    }
}

/// A compose value, quoted when YAML would read it as something other than a
/// string (a number, a boolean, null): the environment only takes strings.
fn compose_scalar(value: &str) -> std::borrow::Cow<'_, str> {
    let numeric = !value.is_empty() && value.chars().all(|c| c.is_ascii_digit());
    let keyword =
        ["true", "false", "yes", "no", "on", "off", "null", "~"].contains(&value.to_ascii_lowercase().as_str());
    if numeric || keyword || value.is_empty() { format!("{value:?}").into() } else { value.into() }
}

/// A published port: `host:container[/proto]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Port {
    pub host: u16,
    pub container: u16,
    pub udp: bool,
}

impl Port {
    pub const fn tcp(host: u16, container: u16) -> Port {
        Port { host, container, udp: false }
    }
    pub const fn udp(host: u16, container: u16) -> Port {
        Port { host, container, udp: true }
    }
    /// As compose writes it.
    pub fn compose(&self) -> String {
        format!("{}:{}{}", self.host, self.container, if self.udp { "/udp" } else { "" })
    }
}

/// The container a service runs in (the restore stack, the setup wizard).
#[derive(Clone, Copy, Debug)]
pub struct Container {
    /// The image when a backup doesn't say.
    pub image: &'static str,
    /// The first one is the web UI.
    pub ports: &'static [Port],
    /// Where the app keeps its config.
    pub config_mount: &'static str,
    /// Extra environment for the restore stack.
    pub env: &'static [(&'static str, &'static str)],
    /// No PUID/PGID support: run as that user, with an init.
    pub run_as_user: bool,
    /// Needs the shared `/data` mount (media and downloads).
    pub data: bool,
    /// Can run inside the VPN's network (the wizard's per-app tunnels).
    pub vpn: bool,
}

impl Container {
    /// The web UI's host port.
    pub fn ui_port(&self) -> u16 {
        self.ports.first().map_or(0, |p| p.host)
    }
}

/// The arrs' default copy rules.
pub const ARR_COPY: CopyRules = CopyRules {
    skip_dirs: &["logs", "Backups", "MediaCover", "UpdateLogs", "Sentry", "xdg"],
    skip_files: &[],
    skip_sqlite: true,
    skip_files_in: &[],
    zip_layout: &[],
};

/// How a reverse proxy and the AuthTunnel gate treat a service.
#[derive(Clone, Copy, Debug)]
pub struct ProxyTraits {
    /// The UI follows a URL Base setting (served under /name, not stripped),
    /// and where that setting is.
    pub url_base: Option<&'static str>,
    /// Can't run under a path: always a name of its own.
    pub own_name: bool,
    /// Its own sign-in (Plex / Jellyfin / Emby accounts): never behind the gate.
    pub own_login: bool,
    /// Public in the proxy page's "arrs public, the rest LAN" preset.
    pub public_preset: bool,
    /// Refuses Host names it doesn't know (SABnzbd): the proxy sends its own address.
    pub host_check: bool,
    /// A warning for the proxy page when it's made public.
    pub public_note: Option<&'static str>,
}

/// The arrs' URL Base setting.
pub const ARR_URL_BASE: &str = "Settings → General → URL Base";

/// Offered by the setup wizard.
#[derive(Clone, Copy, Debug)]
pub struct Wizard {
    /// The wizard's group (its own taxonomy: media / download / requests).
    pub group: &'static str,
    /// The live config the wizard looks for in `appdata/<id>/`, and pulls out of a
    /// legacy backup zip.
    pub config: Option<&'static str>,
}

/// What only download clients have.
#[derive(Clone, Copy, Debug)]
pub struct DownloadsDef {
    /// Its implementation name in the arrs' download client list.
    pub arr_impl: &'static str,
    /// Has categories that wranglarr creates (qBittorrent makes them on demand).
    pub categories: bool,
    /// What its backup holds (HTML, for the suite page).
    pub backup_holds: &'static str,
    pub payload: Payload,
}

/// The arrs a service links itself to (Connect): it stores their URL and API key.
#[derive(Clone, Copy, Debug)]
pub struct Takes {
    /// The kinds, in order.
    pub kinds: &'static [&'static str],
    /// Only the first app of each kind (Bazarr has one Sonarr and one Radarr slot).
    pub one_each: bool,
    /// Takes them only once its own setup is done (Seerr: a media server sign-in);
    /// until then its links wait, and Connect retries them every few minutes.
    pub after_setup: bool,
}

/// Everything wranglarr knows about one kind of service.
#[derive(Debug)]
pub struct ServiceDef {
    /// The persisted spelling: wranglarr.toml's `kind`, manifests, compose names.
    pub id: &'static str,
    pub label: &'static str,
    /// One line for the setup wizard.
    pub description: &'static str,
    pub roles: Roles,
    pub home_group: HomeGroup,
    /// Brand colour for its card.
    pub color: &'static str,
    /// The collection count shown as its headline (and compared between versions).
    pub primary_count: &'static str,
    /// Where the app serves its own logo.
    pub logo_paths: &'static [&'static str],
    /// The page in its UI that lists its health checks.
    pub status_page: Option<&'static str>,
    pub container: Container,
    pub copy: CopyRules,
    pub credentials: Credentials,
    pub backup: BackupMode,
    pub database: Option<Database>,
    pub proxy: ProxyTraits,
    pub wizard: Option<Wizard>,
    /// The arrs it links itself to (Bazarr, Seerr).
    pub takes: Option<Takes>,
    pub arr: Option<&'static ArrDef>,
    pub downloads: Option<&'static DownloadsDef>,
    /// Its sign-in, for APPS_AUTH.
    pub login: Option<login::Login>,
    /// Its home page lists, in order (its client's [`Feed`] makes them).
    pub feeds: &'static [FeedKind],
    /// Its API client for a configured app.
    pub build: fn(&AppConfig) -> Result<AnyClient>,
}

/// wranglarr's own repository: where problems with the built-in apps go.
pub const WRANGLARR_REPO: &str = "Wranglarrr/Wranglarr";

/// Where problems with a kind of app are reported: a GitHub repository's issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportTo {
    /// `owner/repo`.
    pub repo: String,
    /// Looked after by wranglarr (the built-ins, and later the curated lariats);
    /// otherwise its own publisher answers for it.
    pub curated: bool,
}

impl ReportTo {
    /// wranglarr's own issues.
    pub fn wranglarr() -> ReportTo {
        ReportTo { repo: WRANGLARR_REPO.into(), curated: true }
    }
}

impl ServiceDef {
    /// Where its problems are reported: the built-ins all go to wranglarr. A lariat
    /// will name its own source here.
    pub fn report_to(&self) -> ReportTo {
        ReportTo::wranglarr()
    }

    /// How a copy of its config folder is made.
    pub fn copy_spec(&self) -> CopySpec {
        CopySpec { rules: self.copy, payload: self.downloads.map(|d| d.payload) }
    }
    pub fn has(&self, roles: Roles) -> bool {
        self.roles.contains(roles)
    }
    /// Its icon, shipped in static/icons/.
    pub fn icon(&self) -> String {
        format!("/static/icons/{}.svg", self.id)
    }
    /// What the front end knows about it.
    pub fn view(&self) -> ServiceView {
        ServiceView {
            id: self.id,
            label: self.label,
            icon: self.icon(),
            color: self.color,
            group: self.home_group.key(),
            primary: self.primary_count,
            status_page: self.status_page,
            downloads: self.has(Roles::DOWNLOADS),
            arr: self.has(Roles::ARR),
            library: self.has(Roles::LIBRARY),
            url_base: self.proxy.url_base.is_some(),
            public_preset: self.proxy.public_preset,
            backup_holds: self.downloads.map(|d| d.backup_holds),
        }
    }
}

/// A service as the pages see it (the `SERVICES` global, `static/*.js`): the
/// fields they use, under the names they use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceView {
    pub id: &'static str,
    pub label: &'static str,
    /// Its icon's URL.
    pub icon: String,
    pub color: &'static str,
    /// Its [`HomeGroup`]'s key.
    pub group: &'static str,
    /// Its headline collection count.
    pub primary: &'static str,
    pub status_page: Option<&'static str>,
    pub downloads: bool,
    pub arr: bool,
    pub library: bool,
    /// Follows a URL Base setting.
    pub url_base: bool,
    pub public_preset: bool,
    pub backup_holds: Option<&'static str>,
}

/// Every service for the front end, by id (the `SERVICES` global).
pub fn views() -> ServiceViews {
    ServiceViews(ALL.iter().map(|d| d.view()).collect())
}

/// The services' views keyed by id, in [`ALL`]'s order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceViews(Vec<ServiceView>);

impl Serialize for ServiceViews {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_map(self.0.iter().map(|v| (v.id, v)))
    }
}

/// A label table as a JSON object.
pub fn labels_json(table: &[(&str, &str)]) -> Value {
    Value::Object(table.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<Map<_, _>>())
}

/// The ids, comma-separated (for messages).
pub fn ids() -> String {
    ALL.iter().map(|d| d.id).collect::<Vec<_>>().join(", ")
}

/// Labels for the collection counts an app reports (shared across kinds).
pub const COLLECTION_LABELS: &[(&str, &str)] = &[
    ("movies", "movies"),
    ("with_file", "with files"),
    ("missing", "missing"),
    ("size_on_disk", "on disk"),
    ("series", "series"),
    ("episodes", "episodes"),
    ("episode_files", "episode files"),
    ("artists", "artists"),
    ("albums", "albums"),
    ("tracks", "tracks"),
    ("track_files", "track files"),
    ("authors", "authors"),
    ("books", "books"),
    ("book_files", "book files"),
    ("indexers", "indexers"),
    ("applications", "apps"),
    ("subtitles_movies", "movies"),
    ("subtitles_series", "series"),
    ("wanted_episodes", "wanted episodes"),
    ("wanted_movies", "wanted movies"),
    ("torrents", "torrents"),
    ("downloading", "downloading"),
    ("seeding", "seeding"),
    ("paused", "paused"),
    ("queue", "queued"),
    ("history", "in history"),
    ("requests", "requests"),
    ("pending", "pending"),
    ("available", "available"),
];

/// Labels for the configuration scope an app reports (shared across kinds).
pub const SCOPE_LABELS: &[(&str, &str)] = &[
    ("indexer", "indexers"),
    ("downloadclient", "download clients"),
    ("qualityprofile", "quality profiles"),
    ("customformat", "custom formats"),
    ("importlist", "import lists"),
    ("notification", "notifications"),
    ("tag", "tags"),
    ("rootfolder", "root folders"),
    ("applications", "applications"),
    ("indexerproxy", "proxies"),
    ("providers", "providers"),
    ("languages_profiles", "language profiles"),
    ("media_server", "media server"),
    ("radarr", "Radarr"),
    ("sonarr", "Sonarr"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_lower_case() {
        let mut seen = std::collections::HashSet::new();
        for d in ALL {
            assert!(seen.insert(d.id), "{} twice", d.id);
            assert_eq!(d.id, d.id.to_ascii_lowercase());
            assert!(!d.container.ports.is_empty(), "{} has no UI port", d.id);
        }
    }

    #[test]
    fn families_are_consistent() {
        for d in ALL {
            assert_eq!(d.arr.is_some(), d.has(Roles::ARR), "{}", d.id);
            assert_eq!(d.downloads.is_some(), d.has(Roles::DOWNLOADS), "{}", d.id);
            if d.has(Roles::LIBRARY) {
                assert!(d.has(Roles::ARR), "{}: a library is an arr", d.id);
            }
            if let Credentials::UserPassword { .. } = d.credentials {
                assert_eq!(d.backup, BackupMode::ConfigDir, "{}", d.id);
            }
        }
        assert_eq!(with(Roles::ARR).count(), 6);
        assert_eq!(with(Roles::LIBRARY).count(), 5);
        assert_eq!(with(Roles::DOWNLOADS).count(), 3);
    }

    #[test]
    fn the_pages_view_of_a_service() {
        // the names and order static/*.js reads
        let all = serde_json::to_string(&views()).expect("json");
        assert!(
            all.starts_with(r#"{"radarr":{"id":"radarr","label":"Radarr","icon":"/static/icons/radarr.svg","#),
            "{all}"
        );
        let qb = serde_json::to_value(get("qbittorrent").expect("qb").view()).expect("json");
        let keys: Vec<&str> = qb.as_object().expect("object").keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "id",
                "label",
                "icon",
                "color",
                "group",
                "primary",
                "status_page",
                "downloads",
                "arr",
                "library",
                "url_base",
                "public_preset",
                "backup_holds"
            ]
        );
        assert_eq!(
            (qb["group"].as_str(), qb["downloads"].as_bool(), qb["arr"].as_bool()),
            (Some("downloads"), Some(true), Some(false))
        );
        assert!(qb["backup_holds"].is_string());
        let radarr = serde_json::to_value(get("radarr").expect("radarr").view()).expect("json");
        assert_eq!(radarr["backup_holds"], Value::Null);
        assert_eq!(serde_json::to_value(views()).expect("json").as_object().map(Map::len), Some(ALL.len()));
    }

    #[test]
    fn database_environment_for_the_restore_stack() {
        let env = |id: &str| get(id).and_then(|d| d.database).expect("a database").compose_env("app_main", "pg");
        assert_eq!(
            env("seerr"),
            [
                "      DB_TYPE: postgres",
                "      DB_HOST: pg",
                r#"      DB_PORT: "5432""#,
                "      DB_USER: ${POSTGRES_USER}",
                "      DB_PASS: ${POSTGRES_PASSWORD}",
                "      DB_NAME: app_main",
            ]
        );
        assert_eq!(
            env("bazarr"),
            [
                r#"      POSTGRES_ENABLED: "true""#,
                "      POSTGRES_HOST: pg",
                r#"      POSTGRES_PORT: "5432""#,
                "      POSTGRES_DATABASE: app_main",
                "      POSTGRES_USERNAME: ${POSTGRES_USER}",
                "      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}",
                "      PGSSLCERT: /tmp/postgresql.crt",
            ]
        );
        assert!(env("radarr").is_empty(), "the arrs keep it in config.xml");
        assert_eq!(compose_scalar("2024"), r#""2024""#);
        assert_eq!(compose_scalar("Off"), r#""Off""#);
    }
}
