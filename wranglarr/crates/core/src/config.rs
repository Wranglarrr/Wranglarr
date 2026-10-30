// SPDX-License-Identifier: GPL-3.0-or-later
//! wranglarr.toml, with `${ENV}` expansion in every string.
//!
//! The environment is a parameter (a lookup), never read here: a command reads
//! the process environment where it starts ([`process_env`]) and tests pass
//! their own.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::shared::{SharedResource, parse_shared};

/// A value for a compose `.env` line that compose (and init.sh's envget) reads
/// back exactly. Compose expands `$` inside double quotes, so anything special
/// goes in single quotes.
pub fn env_quote(v: &str) -> std::result::Result<String, &'static str> {
    if v.contains('\n') || v.contains('\r') {
        return Err("can't contain a line break");
    }
    let special = |c: char| c.is_whitespace() || "#$'\"\\".contains(c);
    if !v.chars().any(special) {
        return Ok(v.to_string());
    }
    if !v.contains('\'') {
        return Ok(format!("'{v}'"));
    }
    if !v.chars().any(|c| "$\"\\".contains(c)) {
        return Ok(format!("\"{v}\""));
    }
    Err("can't contain both ' and one of $ \" \\")
}

static ENV_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("static regex"));

/// A variable of the environment a config is read in; None when it is unset.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The process environment, as the lookup a command starts with. As in the
/// LinuxServer.io images, `FILE__<NAME>=/run/secrets/x` sets `<NAME>` to that
/// file's contents (Docker and Compose secrets).
pub fn process_env(name: &str) -> Option<String> {
    with_files(name, &|n| std::env::var(n).ok())
}

/// `name` from the file `FILE__<name>` names (its trailing newlines dropped, as
/// a shell's `$(cat …)` does), else from `env`. The file wins, as in the
/// LinuxServer.io images: Compose passes `NAME: ${NAME}` as an empty string when
/// `.env` leaves it out. A file that can't be read is reported and skipped.
pub fn with_files(name: &str, env: Lookup) -> Option<String> {
    if !name.starts_with("FILE__")
        && let Some(path) = env(&format!("FILE__{name}")).map(|p| p.trim().to_string()).filter(|p| !p.is_empty())
    {
        match std::fs::read_to_string(&path) {
            Ok(text) => return Some(text.trim_end_matches(['\n', '\r']).to_string()),
            Err(e) => eprintln!("wranglarr: FILE__{name}: can't read {path}: {e}"),
        }
    }
    env(name)
}

fn expand(v: Value, env: Lookup) -> Value {
    match v {
        Value::String(s) => {
            Value::String(ENV_RE.replace_all(&s, |c: &regex::Captures| env(&c[1]).unwrap_or_default()).into_owned())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(|v| expand(v, env)).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, expand(v, env))).collect()),
        other => other,
    }
}

// ---- readers for TOML values, which may also come from `${ENV}` as strings ----

/// A string; a number or a bool as written in TOML.
fn s(o: &Map<String, Value>, k: &str, default: &str) -> String {
    match o.get(k) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Null) | None => default.to_string(),
        Some(v) => v.to_string(),
    }
}

fn int(o: &Map<String, Value>, k: &str, default: i64) -> Result<i64> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(default),
        Some(v @ Value::Number(_)) => {
            crate::value::as_whole(v).ok_or_else(|| Error::invalid(format!("{k}: not a whole number")))
        }
        Some(Value::String(v)) => {
            v.trim().parse().map_err(|_| Error::invalid(format!("{k}: {v:?} is not a whole number")))
        }
        Some(_) => Err(Error::invalid(format!("{k}: not a whole number"))),
    }
}

/// A TOML bool, a number (0 is false), or a yes/no word (from `${ENV}`, see
/// [`crate::value::flag_word`]); a string that isn't one is false. Missing or
/// anything else is the default.
pub fn flag(v: Option<&Value>, default: bool) -> bool {
    match v {
        Some(Value::String(s)) => crate::value::flag_word(s) == Some(true),
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => default,
    }
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                o => o.to_string(),
            })
            .collect(),
        _ => vec![],
    }
}

fn table<'a>(o: &'a Map<String, Value>, k: &str) -> std::borrow::Cow<'a, Map<String, Value>> {
    match o.get(k) {
        Some(Value::Object(t)) => std::borrow::Cow::Borrowed(t),
        _ => std::borrow::Cow::Owned(Map::new()),
    }
}

/// Configuration is user input: malformed sections must not silently become
/// empty/default settings. Environment-expanded scalar strings remain supported.
fn validate_shape(raw: &Map<String, Value>) -> Result<()> {
    fn fields(o: &Map<String, Value>, at: &str, flags: &[&str], lists: &[&str]) -> Result<()> {
        for key in flags {
            if let Some(v) = o.get(*key)
                && crate::value::as_flag(v).is_none()
            {
                return Err(Error::invalid(format!("{at}.{key}: expected a boolean or yes/no word")));
            }
        }
        for key in lists {
            if let Some(v) = o.get(*key)
                && !v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
            {
                return Err(Error::invalid(format!("{at}.{key}: expected an array of strings")));
            }
        }
        Ok(())
    }
    fn objects<'a>(o: &'a Map<String, Value>, key: &str) -> Result<Vec<&'a Map<String, Value>>> {
        match o.get(key) {
            None => Ok(vec![]),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_object().ok_or_else(|| Error::invalid(format!("{key}: expected an array of tables"))))
                .collect(),
            Some(_) => Err(Error::invalid(format!("{key}: expected an array of tables"))),
        }
    }
    for key in ["postgres", "storage", "server", "docker", "vpn", "updates", "connect", "shared"] {
        if raw.get(key).is_some_and(|v| !v.is_object()) {
            return Err(Error::invalid(format!("{key}: expected a table")));
        }
    }
    let mut suite_names = HashSet::new();
    let mut app_names = HashSet::new();
    for suite in objects(raw, "suite")? {
        let name = s_req(suite, "name", "suite")?;
        if !suite_names.insert(name) {
            return Err(Error::invalid("suite names must be unique"));
        }
        fields(suite, "suite", &["enabled"], &[])?;
        for app in objects(suite, "app")? {
            let name = s_req(app, "name", "app")?;
            if !app_names.insert(name) {
                return Err(Error::invalid("app names must be globally unique across all suites"));
            }
            fields(app, "app", &["enabled"], &["databases"])?;
        }
    }
    fields(&table(raw, "vpn"), "vpn", &[], &["apps"])?;
    fields(&table(raw, "connect"), "connect", &["auto"], &[])?;
    fields(
        &table(raw, "updates"),
        "updates",
        &["enabled", "auto_update", "backup_first", "include_postgres", "include_vpn"],
        &["extra", "exclude"],
    )?;
    Ok(())
}

// ---- the config ---------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AppConfig {
    pub name: String,
    pub kind: String,
    pub url: String,
    pub api_key: String,
    pub api_key_file: String,
    pub username: String,
    pub password: String,
    pub databases: Vec<String>,
    pub container: String,
    /// The app's /config mounted into wranglarr (bazarr + file-based apps).
    pub config_dir: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SuiteConfig {
    pub name: String,
    pub apps: Vec<AppConfig>,
    pub schedule: String,
    pub enabled: bool,
    pub keep_last: i64,
    pub keep_days: i64,
}

impl SuiteConfig {
    pub fn app(&self, name: &str) -> Option<&AppConfig> {
        self.apps.iter().find(|a| a.name == name)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PostgresConfig {
    pub host: String,
    pub port: i64,
    pub user: String,
    pub password: String,
    /// For updates; "" = not managed.
    pub container: String,
}

impl PostgresConfig {
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("PGHOST", self.host.clone()),
            ("PGPORT", self.port.to_string()),
            ("PGUSER", self.user.clone()),
            ("PGPASSWORD", self.password.clone()),
        ]
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VpnConfig {
    /// gluetun control server, e.g. http://vpn:8000
    pub control_url: String,
    pub api_key: String,
    pub name: String,
    pub container: String,
    /// The apps in the shared tunnel (service names), from WRANGLARR_VPN_APPS or
    /// `[vpn] apps`; None = not known.
    pub apps: Option<Vec<String>>,
}

/// Container image updates. UI changes are stored in the state db and win.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct UpdatesConfig {
    pub enabled: bool,
    pub check_schedule: String,
    pub auto_update: bool,
    pub auto_schedule: String,
    pub backup_first: bool,
    /// Pre-update backups each suite keeps, apart from its own retention: the
    /// newest `pre_update_keep`, and older ones in a run of updates closer than
    /// `pre_update_settle_days` apart (see `store::PreUpdateKeep`).
    pub pre_update_keep: i64,
    pub pre_update_settle_days: i64,
    pub health_timeout: i64,
    /// Update wranglarr itself on `self_schedule` (off: only when someone presses Update now).
    pub self_update: bool,
    pub self_schedule: String,
    pub include_postgres: bool,
    pub include_vpn: bool,
    pub extra: Vec<String>,
    pub exclude: Vec<String>,
    #[serde(default)]
    pub tracks: BTreeMap<String, String>,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        UpdatesConfig {
            enabled: true,
            check_schedule: "0 */6 * * *".into(),
            auto_update: false,
            auto_schedule: "0 5 * * *".into(),
            backup_first: true,
            pre_update_keep: crate::store::PreUpdateKeep::DEFAULT.keep,
            pre_update_settle_days: crate::store::PreUpdateKeep::DEFAULT.settle_days,
            health_timeout: 300,
            self_update: false,
            self_schedule: "30 5 * * *".into(),
            include_postgres: true,
            include_vpn: true,
            extra: vec![],
            exclude: vec![],
            tracks: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectConfig {
    /// At startup, connect what was never connected before.
    pub auto: bool,
}

/// `[events]`: the apps tell wranglarr when their status changes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EventsConfig {
    /// Keep a `wranglarr · live status` webhook in every arr and Prowlarr.
    pub enabled: bool,
    /// How the apps reach wranglarr; "" = `http://wranglarr:<server.port>`.
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub suites: Vec<SuiteConfig>,
    pub postgres: PostgresConfig,
    pub storage_root: PathBuf,
    pub state_db: PathBuf,
    pub bind: String,
    pub port: u16,
    pub log_level: String,
    pub docker_socket: String,
    pub shared: Vec<SharedResource>,
    pub vpn: Option<VpnConfig>,
    pub updates: UpdatesConfig,
    pub connect: ConnectConfig,
    pub events: EventsConfig,
}

impl Config {
    pub fn suite(&self, name: &str) -> Option<&SuiteConfig> {
        self.suites.iter().find(|s| s.name == name)
    }
    pub fn all_apps(&self) -> impl Iterator<Item = &AppConfig> {
        self.suites.iter().flat_map(|s| s.apps.iter())
    }
    pub fn app(&self, name: &str) -> Option<&AppConfig> {
        self.all_apps().find(|a| a.name == name)
    }
}

/// `WRANGLARR_ENABLED_APPS` (= COMPOSE_PROFILES): None when unset or empty.
pub fn parse_enabled_env(value: Option<&str>) -> Option<HashSet<String>> {
    let v = value.unwrap_or("").trim();
    if v.is_empty() {
        return None;
    }
    Some(v.split(|c: char| c == ',' || c.is_whitespace()).filter(|x| !x.is_empty()).map(str::to_string).collect())
}

fn vpn_apps(vpn_raw: &Map<String, Value>, env: Lookup) -> Option<Vec<String>> {
    if let Some(env) = env("WRANGLARR_VPN_APPS") {
        return Some(env.split(',').map(str::trim).filter(|a| !a.is_empty()).map(str::to_string).collect());
    }
    match vpn_raw.get("apps") {
        Some(Value::Array(_)) => Some(str_list(vpn_raw.get("apps"))),
        _ => None,
    }
}

/// Where wranglarr.toml is: WRANGLARR_CONFIG, else /config/wranglarr.toml.
pub fn default_path(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    env("WRANGLARR_CONFIG").map_or_else(|| "/config/wranglarr.toml".into(), PathBuf::from)
}

/// The app kinds a config may use: the service registry (wranglarr-services).
pub trait Kinds {
    fn knows(&self, kind: &str) -> bool;
    /// Every kind, comma-separated (for the error).
    fn ids(&self) -> String;
}

/// The config file at `path`, read in the environment `env`.
pub fn load_with(path: &Path, kinds: &dyn Kinds, env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
    parse_with(&text, kinds, env)
}

/// A config's text, read in the environment `env`: the `${VAR}`s in its
/// strings, WRANGLARR_ENABLED_APPS and WRANGLARR_VPN_APPS.
pub fn parse_with(text: &str, kinds: &dyn Kinds, env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    let env: Lookup = &env;
    let raw: toml::Table = toml::from_str(text).map_err(|e| Error::invalid(format!("wranglarr.toml: {e}")))?;
    let raw = match expand(serde_json::to_value(raw)?, env) {
        Value::Object(o) => o,
        _ => Map::new(),
    };
    validate_shape(&raw)?;

    // WRANGLARR_ENABLED_APPS (= COMPOSE_PROFILES in the compose stack) narrows
    // the suites to the apps that are actually running.
    let enabled_set = parse_enabled_env(env("WRANGLARR_ENABLED_APPS").as_deref());

    let mut suites = vec![];
    for st in raw.get("suite").and_then(Value::as_array).into_iter().flatten() {
        let Some(st) = st.as_object() else { continue };
        let mut apps = vec![];
        for a in st.get("app").and_then(Value::as_array).into_iter().flatten() {
            let Some(a) = a.as_object() else { continue };
            let app = AppConfig {
                name: s_req(a, "name", "app")?,
                kind: s_req(a, "kind", "app")?,
                url: s_req(a, "url", "app")?,
                api_key: s(a, "api_key", ""),
                api_key_file: s(a, "api_key_file", ""),
                username: s(a, "username", ""),
                password: s(a, "password", ""),
                databases: str_list(a.get("databases")),
                container: s(a, "container", ""),
                config_dir: s(a, "config_dir", ""),
                enabled: flag(a.get("enabled"), true),
            };
            if !kinds.knows(&app.kind) {
                return Err(Error::invalid(format!(
                    "app {:?}: unsupported kind {:?} (one of {})",
                    app.name,
                    app.kind,
                    kinds.ids()
                )));
            }
            if !app.enabled || enabled_set.as_ref().is_some_and(|e| !e.contains(&app.name)) {
                continue;
            }
            apps.push(app);
        }
        if apps.is_empty() {
            continue;
        }
        suites.push(SuiteConfig {
            name: s_req(st, "name", "suite")?,
            apps,
            schedule: s(st, "schedule", ""),
            enabled: flag(st.get("enabled"), true),
            keep_last: int(st, "keep_last", 14)?,
            keep_days: int(st, "keep_days", 0)?,
        });
    }
    if suites.is_empty() {
        return Err(Error::invalid("config defines no [[suite]] with enabled apps"));
    }
    let mut seen = HashSet::new();
    if !suites.iter().all(|s| seen.insert(s.name.clone())) {
        return Err(Error::invalid("suite names must be unique"));
    }

    for suite in &suites {
        if !crate::store::valid_name(&suite.name) {
            return Err(Error::invalid("suite name must be a single visible path component"));
        }
        let mut apps = HashSet::new();
        for app in &suite.apps {
            if !crate::store::valid_name(&app.name) {
                return Err(Error::invalid("app name must be a single visible path component"));
            }
            if !apps.insert(&app.name) {
                return Err(Error::invalid("app names must be unique within a suite"));
            }
            if app.databases.iter().any(|db| !crate::store::valid_name(db)) {
                return Err(Error::invalid("database name must be a single visible path component"));
            }
        }
    }

    let pg = table(&raw, "postgres");
    let st = table(&raw, "storage");
    let srv = table(&raw, "server");
    let vpn_raw = table(&raw, "vpn");
    let mut vpn = (!s(&vpn_raw, "control_url", "").is_empty()).then(|| VpnConfig {
        control_url: s(&vpn_raw, "control_url", ""),
        api_key: s(&vpn_raw, "api_key", ""),
        name: s(&vpn_raw, "name", "vpn"),
        container: s(&vpn_raw, "container", "arrs-vpn"),
        apps: vpn_apps(&vpn_raw, env),
    });
    if let Some(e) = &enabled_set
        && vpn.is_some()
        && !e.contains("vpn")
        && !e.contains(&s(&vpn_raw, "profile", "vpn"))
    {
        vpn = None;
    }
    let port = int(&srv, "port", 8787)?;
    Ok(Config {
        suites,
        postgres: PostgresConfig {
            host: s(&pg, "host", "postgres"),
            port: int(&pg, "port", 5432)?,
            user: s(&pg, "user", ""),
            password: s(&pg, "password", ""),
            container: s(&pg, "container", "arrs-postgres"),
        },
        storage_root: s(&st, "root", "/backups").into(),
        state_db: s(&st, "state_db", "/config/wranglarr.db").into(),
        bind: s(&srv, "bind", "0.0.0.0"),
        port: u16::try_from(port).map_err(|_| Error::invalid(format!("server.port: {port} is not a port")))?,
        log_level: s(&srv, "log_level", "info"),
        docker_socket: s(&table(&raw, "docker"), "socket", ""),
        shared: parse_shared(&table(&raw, "shared"))?,
        vpn,
        updates: updates(&table(&raw, "updates"))?,
        connect: ConnectConfig { auto: flag(table(&raw, "connect").get("auto"), true) },
        events: EventsConfig {
            enabled: flag(table(&raw, "events").get("enabled"), true),
            url: s(&table(&raw, "events"), "url", "").trim_end_matches('/').to_string(),
        },
    })
}

fn s_req(o: &Map<String, Value>, k: &str, what: &str) -> Result<String> {
    match o.get(k) {
        Some(Value::String(v)) if !v.trim().is_empty() => Ok(v.clone()),
        Some(_) => Err(Error::invalid(format!("{what}: '{k}' must be a nonempty string"))),
        _ => Err(Error::invalid(format!("{what}: '{k}' is required"))),
    }
}

fn updates(u: &Map<String, Value>) -> Result<UpdatesConfig> {
    let d = UpdatesConfig::default();
    let tracks: BTreeMap<String, String> = match u.get("tracks") {
        None => BTreeMap::new(),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|_| Error::invalid("updates.tracks must map container names to image tags"))?,
    };
    for (container, tag) in &tracks {
        if !crate::store::valid_name(container) || !crate::docker::valid_tag(tag) {
            return Err(Error::invalid(format!("updates.tracks: invalid container or tag for {container:?}")));
        }
    }
    Ok(UpdatesConfig {
        enabled: flag(u.get("enabled"), d.enabled),
        check_schedule: s(u, "check_schedule", &d.check_schedule),
        auto_update: flag(u.get("auto_update"), d.auto_update),
        auto_schedule: s(u, "auto_schedule", &d.auto_schedule),
        backup_first: flag(u.get("backup_first"), d.backup_first),
        pre_update_keep: int(u, "pre_update_keep", d.pre_update_keep)?,
        pre_update_settle_days: int(u, "pre_update_settle_days", d.pre_update_settle_days)?,
        health_timeout: int(u, "health_timeout", d.health_timeout)?,
        self_update: flag(u.get("self_update"), d.self_update),
        self_schedule: s(u, "self_schedule", &d.self_schedule),
        include_postgres: flag(u.get("include_postgres"), d.include_postgres),
        include_vpn: flag(u.get("include_vpn"), d.include_vpn),
        extra: str_list(u.get("extra")),
        exclude: str_list(u.get("exclude")),
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_secrets_stand_in_for_unset_variables() {
        let dir = tempfile::tempdir().expect("tmp");
        let secret = dir.path().join("pg");
        std::fs::write(&secret, "s3cret\n\n").expect("write");
        let path = secret.display().to_string();
        let missing = dir.path().join("nope").display().to_string();
        let env = |k: &str| match k {
            "FILE__POSTGRES_PASSWORD" | "FILE__SET" => Some(path.clone()),
            "FILE__GONE" => Some(missing.clone()),
            "SET" => Some("plain".into()),
            _ => None,
        };
        assert_eq!(with_files("POSTGRES_PASSWORD", &env).as_deref(), Some("s3cret"), "trailing newlines dropped");
        assert_eq!(
            with_files("SET", &env).as_deref(),
            Some("s3cret"),
            "the file wins, as in the LinuxServer.io images"
        );
        assert_eq!(with_files("GONE", &env), None, "an unreadable file is skipped");
        assert_eq!(with_files("OTHER", &env), None);
        let v: Value = toml::from_str("p = \"${POSTGRES_PASSWORD}\"").expect("toml");
        assert_eq!(expand(v, &|n| with_files(n, &env))["p"], "s3cret", "in wranglarr.toml too");
    }

    struct Some_;
    impl Kinds for Some_ {
        fn knows(&self, kind: &str) -> bool {
            ["radarr", "sonarr", "qbittorrent"].contains(&kind)
        }
        fn ids(&self) -> String {
            "radarr, sonarr, qbittorrent".into()
        }
    }

    fn parse(text: &str) -> Result<Config> {
        parse_with(text, &Some_, |_| None)
    }

    const MIN: &str = r#"
[storage]
root = "/b"
[[suite]]
name = "media"
keep_last = "7"
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "http://radarr:7878"
api_key = "k"
enabled = "yes"
"#;

    #[test]
    fn parses_minimal() {
        let c = parse(MIN).expect("parses");
        assert_eq!(c.suites[0].keep_last, 7);
        assert_eq!(c.suites[0].apps[0].name, "radarr");
        assert_eq!(c.port, 8787);
        assert_eq!(c.storage_root, PathBuf::from("/b"));
        assert!(c.vpn.is_none());
        assert_eq!(c.updates, UpdatesConfig::default());
    }

    #[test]
    fn rejects_unknown_kind() {
        let e = parse(&MIN.replace("kind = \"radarr\"", "kind = \"plex\"")).expect_err("bad kind");
        assert!(e.to_string().contains("unsupported kind"), "{e}");
    }

    #[test]
    fn no_suites() {
        let e = parse(&MIN.replace("enabled = \"yes\"", "enabled = false")).expect_err("no apps");
        assert_eq!(e.to_string(), "config defines no [[suite]] with enabled apps");
    }

    #[test]
    fn read_in_its_environment() {
        let text = MIN.replace("api_key = \"k\"", "api_key = \"${RADARR_KEY}\"")
            + "[vpn]\ncontrol_url = \"http://vpn:8000\"\napps = [\"qbit\"]\n";
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };

        let c = parse_with(&text, &Some_, env(&[("RADARR_KEY", "secret")])).expect("parses");
        assert_eq!(c.suites[0].apps[0].api_key, "secret");
        assert_eq!(c.vpn.expect("vpn").apps, Some(vec!["qbit".to_string()]));

        let c = parse(&text).expect("parses");
        assert_eq!(c.suites[0].apps[0].api_key, "", "unset is empty");

        let c = parse_with(&text, &Some_, env(&[("WRANGLARR_VPN_APPS", "a, b,")])).expect("parses");
        assert_eq!(c.vpn.expect("vpn").apps, Some(vec!["a".to_string(), "b".to_string()]));

        let c = parse_with(&text, &Some_, env(&[("WRANGLARR_ENABLED_APPS", "radarr")])).expect("parses");
        assert!(c.vpn.is_none(), "the vpn isn't running");

        assert_eq!(default_path(|_| None), PathBuf::from("/config/wranglarr.toml"));
        assert_eq!(default_path(env(&[("WRANGLARR_CONFIG", "/x.toml")])), PathBuf::from("/x.toml"));
    }

    #[test]
    fn quoting() {
        assert_eq!(env_quote("plain").as_deref(), Ok("plain"));
        assert_eq!(env_quote("p$word").as_deref(), Ok("'p$word'"));
        assert_eq!(env_quote("it's").as_deref(), Ok("\"it's\""));
        assert_eq!(env_quote("").as_deref(), Ok(""));
        assert!(env_quote("it's $5").is_err());
        assert!(env_quote("a\nb").is_err());
    }
    #[test]
    fn app_identity_is_unique_across_suites_even_when_filtered() {
        let duplicate = format!(
            "{MIN}\n[[suite]]\nname = \"other\"\n[[suite.app]]\nname = \"radarr\"\nkind = \"radarr\"\nurl = \"http://other\"\nenabled = false\n"
        );
        let e = parse(&duplicate).expect_err("duplicate identity");
        assert!(e.to_string().contains("globally unique"), "{e}");
        assert!(parse_with(&duplicate, &Some_, |_| Some("sonarr".into())).is_err());
    }

    #[test]
    fn malformed_configuration_cannot_silently_default() {
        for malformed in [
            MIN.replace("keep_last = \"7\"", "keep_last = 7.5"),
            MIN.replace("enabled = \"yes\"", "enabled = \"perhaps\""),
            MIN.replace("name = \"radarr\"", "name = 42"),
            MIN.replace("url = \"http://radarr:7878\"", "url = []"),
            MIN.replace("[[suite.app]]", "app = \"radarr\"\n[other]"),
            format!("{MIN}\n[server]\nport = 8787.5\n"),
            format!("{MIN}\n[updates]\nbackup_first = \"typo\"\n"),
            format!("{MIN}\n[updates]\nextra = \"container\"\n"),
            format!("{MIN}\n[updates]\nexclude = [1]\n"),
            format!("{MIN}\n[vpn]\napps = \"qbit\"\n"),
            format!("server = false\n{MIN}"),
            "suite = [1]".into(),
            "suite = false".into(),
        ] {
            assert!(parse(&malformed).is_err(), "accepted {malformed}");
        }
        assert!(parse(&MIN.replace("keep_last = \"7\"", "keep_last = 7.0")).is_ok());
    }

    #[test]
    fn unsafe_storage_names_are_rejected() {
        for bad in ["../escape", "/absolute", "a/b", ".hidden"] {
            assert!(parse(&MIN.replace(r#"name = "media""#, &format!("name = {bad:?}"))).is_err());
            assert!(parse(&MIN.replace(r#"name = "radarr""#, &format!("name = {bad:?}"))).is_err());
        }
        let duplicate = format!(
            r#"{MIN}
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "http://r"
"#
        );
        assert!(parse(&duplicate).is_err());
    }
}
