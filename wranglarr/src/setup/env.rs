// SPDX-License-Identifier: GPL-3.0-or-later
//! The wizard's answers, their checks, and what they become: `.env`, the
//! backup preferences in a new wranglarr.toml, and the next steps shown at the end.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use chrono::Utc;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::host::{self, Env};
use super::input;
use crate::config::env_quote;
use crate::error::{Error, Result};
use crate::service::{self, Roles, ServiceDef};

/// The services the wizard offers, in order (the registry's, where it says so).
pub fn services() -> impl Iterator<Item = &'static ServiceDef> {
    service::ALL.iter().copied().filter(|d| d.wizard.is_some())
}

/// A wizard service's config file inside its folder (`config.xml`).
pub fn config_file(id: &str) -> Option<&'static str> {
    services().find(|d| d.id == id).and_then(|d| d.wizard).and_then(|w| w.config)
}

/// The wizard's services as the page gets them.
pub fn services_json() -> Value {
    Value::Array(
        services()
            .map(|d| {
                let w = d.wizard.as_ref();
                let mut v = json!({"id": d.id, "name": d.label, "group": w.map_or("", |w| w.group),
                                   "desc": d.description, "port": d.container.ui_port(), "db": d.database.is_some()});
                if let Some(c) = w.and_then(|w| w.config) {
                    v["config"] = json!(c);
                }
                if !d.container.vpn {
                    v["vpn"] = json!(false);
                }
                v
            })
            .collect(),
    )
}

pub const ENV_KEYS_SECRET: &[&str] = &[
    "POSTGRES_PASSWORD",
    "GLUETUN_API_KEY",
    "QBITTORRENT_PASSWORD",
    "WIREGUARD_PRIVATE_KEY",
    "WIREGUARD_PRESHARED_KEY",
    "VPN_PASSWORD",
    "NZBGET_PASS",
    "VPN_PIA_PASS",
    "APPS_LOGIN_PASSWORD",
];

/// Secrets already in .env never go back to the browser: it gets KEEP instead, and KEEP coming back
/// means "leave it as it is" (long enough for the wizard's 12-character password check).
pub const KEEP: &str = "•••••• unchanged ••••••";

/// What `wranglarr-setup --auth` may change in .env.
pub const AUTH_KEYS: &[&str] = &[
    "APPS_AUTH",
    "APPS_LOGIN_USER",
    "APPS_LOGIN_PASSWORD",
    "APPS_AUTH_REQUIRED",
    "AUTH_COOKIE_DOMAIN",
    "AUTH_LOGIN_URL",
    "QBITTORRENT_USER",
    "QBITTORRENT_PASSWORD",
    "NZBGET_USER",
    "NZBGET_PASS",
];

static HOST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$").expect("static regex"));
static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://[^/\s]+/?$").expect("static regex"));
static ENV_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([A-Za-z_][A-Za-z0-9_]*)=").expect("static regex"));
static HEAD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\[").expect("static regex"));
static TOML_KEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\w+)\s*=").expect("static regex"));

// ---- passwords ----------------------------------------------------------------------------
// the same rules as the login itself
pub use crate::auth::{password_problem, valid_username};

// ---- the answers ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VpnCheck {
    /// none | shared | per-app
    pub mode: String,
    /// shared tunnel: all | prowlarr (+ download clients) | downloads
    pub scope: String,
    pub vpn_type: String,
    pub provider: String,
    pub private_key: String,
    pub preshared_key: String,
    pub public_key: String,
    pub endpoint_ip: String,
    pub endpoint_port: String,
    pub addresses: String,
    pub lan_network: String,
    pub user: String,
    pub password: String,
}

impl Default for VpnCheck {
    fn default() -> Self {
        VpnCheck {
            mode: "none".into(),
            scope: "all".into(),
            vpn_type: "wireguard".into(),
            provider: "custom".into(),
            private_key: String::new(),
            preshared_key: String::new(),
            public_key: String::new(),
            endpoint_ip: String::new(),
            endpoint_port: "51820".into(),
            addresses: String::new(),
            lan_network: String::new(),
            user: String::new(),
            password: String::new(),
        }
    }
}

impl VpnCheck {
    /// The string fields in declaration order (for check_values).
    fn strings(&self) -> [(&'static str, &str); 13] {
        [
            ("mode", &self.mode),
            ("scope", &self.scope),
            ("vpn_type", &self.vpn_type),
            ("provider", &self.provider),
            ("private_key", &self.private_key),
            ("preshared_key", &self.preshared_key),
            ("public_key", &self.public_key),
            ("endpoint_ip", &self.endpoint_ip),
            ("endpoint_port", &self.endpoint_port),
            ("addresses", &self.addresses),
            ("lan_network", &self.lan_network),
            ("user", &self.user),
            ("password", &self.password),
        ]
    }

    /// Put the .env value back wherever the browser sent KEEP.
    pub fn unmask(&mut self, env: &mut impl FnMut() -> BTreeMap<String, String>) {
        let fields: [(&mut String, &[&str]); 3] = [
            (&mut self.private_key, &["WIREGUARD_PRIVATE_KEY"]),
            (&mut self.preshared_key, &["WIREGUARD_PRESHARED_KEY"]),
            (&mut self.password, &["VPN_PASSWORD", "VPN_PIA_PASS"]),
        ];
        unmask_fields(fields, env);
    }
}

fn unmask_fields<const N: usize>(
    fields: [(&mut String, &[&str]); N],
    env: &mut impl FnMut() -> BTreeMap<String, String>,
) {
    let mut loaded: Option<BTreeMap<String, String>> = None;
    for (f, keys) in fields {
        if f == KEEP {
            let e = loaded.get_or_insert_with(&mut *env);
            *f = keys.iter().find_map(|k| e.get(*k).filter(|v| !v.is_empty())).cloned().unwrap_or_default();
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Answers {
    pub services: Vec<String>,
    pub tz: String,
    #[serde(deserialize_with = "input::whole_number")]
    pub puid: i64,
    #[serde(deserialize_with = "input::whole_number")]
    pub pgid: i64,
    pub umask: String,
    pub appdata: String,
    pub data_root: String,
    pub data_root_2: String,
    pub postgres_user: String,
    pub postgres_password: String,
    #[serde(deserialize_with = "input::whole_numbers")]
    pub ports: BTreeMap<String, i64>,
    #[serde(deserialize_with = "input::whole_number")]
    pub wranglarr_port: i64,
    pub gluetun_api_key: String,
    pub nzbget_user: String,
    pub nzbget_pass: String,
    pub qbittorrent_user: String,
    pub qbittorrent_password: String,
    /// app logins (wiki/App-Logins.md): apps | tunnel | manual
    pub apps_auth: String,
    pub apps_login_user: String,
    pub apps_login_password: String,
    pub apps_auth_required: String,
    pub auth_cookie_domain: String,
    pub auth_login_url: String,
    pub vpn: VpnCheck,
    pub vpn_per_app: Vec<String>,
    pub backup_cron: String,
    #[serde(deserialize_with = "input::whole_number")]
    pub keep_last: i64,
    #[serde(deserialize_with = "input::whole_number")]
    pub keep_days: i64,
}

impl Default for Answers {
    fn default() -> Self {
        Answers {
            services: vec![],
            tz: "UTC".into(),
            puid: 1000,
            pgid: 1000,
            umask: "002".into(),
            appdata: "./appdata".into(),
            data_root: "/mnt/data".into(),
            data_root_2: String::new(),
            postgres_user: "arr".into(),
            postgres_password: String::new(),
            ports: BTreeMap::new(),
            wranglarr_port: 8787,
            gluetun_api_key: String::new(),
            nzbget_user: default_user(&service::nzbget::DEF).into(),
            nzbget_pass: String::new(),
            qbittorrent_user: default_user(&service::qbittorrent::DEF).into(),
            qbittorrent_password: String::new(),
            apps_auth: "apps".into(),
            apps_login_user: "admin".into(),
            apps_login_password: String::new(),
            apps_auth_required: "enabled".into(),
            auth_cookie_domain: String::new(),
            auth_login_url: String::new(),
            vpn: VpnCheck::default(),
            vpn_per_app: vec![],
            backup_cron: "30 3 * * *".into(),
            keep_last: 14,
            keep_days: 60,
        }
    }
}

impl Answers {
    fn strings(&self) -> [(&'static str, &str); 19] {
        [
            ("tz", &self.tz),
            ("umask", &self.umask),
            ("appdata", &self.appdata),
            ("data_root", &self.data_root),
            ("data_root_2", &self.data_root_2),
            ("postgres_user", &self.postgres_user),
            ("postgres_password", &self.postgres_password),
            ("gluetun_api_key", &self.gluetun_api_key),
            ("nzbget_user", &self.nzbget_user),
            ("nzbget_pass", &self.nzbget_pass),
            ("qbittorrent_user", &self.qbittorrent_user),
            ("qbittorrent_password", &self.qbittorrent_password),
            ("apps_auth", &self.apps_auth),
            ("apps_login_user", &self.apps_login_user),
            ("apps_login_password", &self.apps_login_password),
            ("apps_auth_required", &self.apps_auth_required),
            ("auth_cookie_domain", &self.auth_cookie_domain),
            ("auth_login_url", &self.auth_login_url),
            ("backup_cron", &self.backup_cron),
        ]
    }

    /// Put the .env values back wherever the browser sent KEEP.
    pub fn unmask(&mut self, env: &mut impl FnMut() -> BTreeMap<String, String>) {
        let fields: [(&mut String, &[&str]); 5] = [
            (&mut self.postgres_password, &["POSTGRES_PASSWORD"]),
            (&mut self.gluetun_api_key, &["GLUETUN_API_KEY"]),
            (&mut self.nzbget_pass, &["NZBGET_PASS"]),
            (&mut self.qbittorrent_password, &["QBITTORRENT_PASSWORD"]),
            (&mut self.apps_login_password, &["APPS_LOGIN_PASSWORD"]),
        ];
        unmask_fields(fields, env);
        self.vpn.unmask(env);
    }
}

// ---- helpers ---------------------------------------------------------------------------------

/// The KEY=value lines of a .env (quotes around a value dropped).
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let (k, v) = line.split_once('=').unwrap_or((line, ""));
        let mut v = v.trim();
        let b = v.as_bytes();
        if b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'') {
            v = &v[1..v.len() - 1];
        }
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

/// `.env` read as [`parse_env`]; none yet is empty.
pub fn read_env(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    Ok(parse_env(&std::fs::read_to_string(path)?))
}

/// env_quote, or a 400 for a value .env can't hold.
pub fn q(v: &str) -> Result<String> {
    env_quote(v).map_err(|e| Error::invalid(format!("a value {e}")))
}

/// Name the field when a value can't be written to .env (q() alone can't say which).
pub fn check_values(a: &Answers) -> Result<()> {
    let all = a
        .strings()
        .into_iter()
        .map(|(f, v)| ("", f, v))
        .chain(a.vpn.strings().into_iter().map(|(f, v)| ("VPN ", f, v)));
    for (pre, f, v) in all {
        if let Err(e) = env_quote(v) {
            return Err(Error::invalid(format!("{pre}{}: {e}", f.replace('_', " "))));
        }
    }
    Ok(())
}

pub fn check_backup_prefs(a: &Answers) -> Result<()> {
    let cron = a.backup_cron.trim();
    if !cron.is_empty()
        && let Err(e) = crate::sched::parse(cron)
    {
        return Err(Error::invalid(format!("backup schedule {:?}: {e}", a.backup_cron)));
    }
    if a.keep_last < 0 || a.keep_days < 0 {
        return Err(Error::invalid("backup retention: 0 or more"));
    }
    Ok(())
}

/// The wizard's schedule goes on the first suite (media); the others keep their
/// staggered times unless the schedule is blank (no automatic backups at all).
/// Retention goes on every suite.
pub fn apply_backup_prefs(text: &str, a: &Answers) -> String {
    let cron = a.backup_cron.trim();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let heads: Vec<usize> = (0..lines.len()).filter(|&n| HEAD_RE.is_match(&lines[n])).collect();
    let suites: Vec<usize> = heads.iter().copied().filter(|&x| lines[x].trim() == "[[suite]]").collect();
    let mut out_of_order: Vec<(usize, Vec<String>)> = vec![]; // (index after which to insert, lines)
    for (si, &n) in suites.iter().enumerate() {
        let end = heads.iter().copied().find(|&h| h > n).unwrap_or(lines.len());
        let mut want: Vec<(&str, String)> =
            vec![("keep_last", a.keep_last.to_string()), ("keep_days", a.keep_days.to_string())];
        if si == 0 || cron.is_empty() {
            want.push(("schedule", crate::standalone::q(cron)));
        }
        let mut last_key = n;
        for (k, line) in lines.iter_mut().enumerate().take(end).skip(n + 1) {
            let Some(name) = TOML_KEY_RE.captures(line).map(|c| c[1].to_string()) else { continue };
            last_key = k;
            if let Some(i) = want.iter().position(|(w, _)| *w == name) {
                let (_, v) = want.remove(i);
                let note = if name == "schedule" && si == 0 { "   # from the setup wizard" } else { "" };
                *line = format!("{name} = {v}{note}");
            }
        }
        if !want.is_empty() {
            out_of_order.push((last_key, want.iter().map(|(k, v)| format!("{k} = {v}")).collect()));
        }
    }
    for (at, add) in out_of_order.into_iter().rev() {
        lines.splice(at + 1..at + 1, add);
    }
    lines.join("\n") + "\n"
}

// ---- app logins ----------------------------------------------------------------------------

pub fn check_auth(a: &Answers) -> Result<()> {
    if !["apps", "tunnel", "manual"].contains(&a.apps_auth.as_str()) {
        return Err(Error::invalid("app logins: pick one of the three"));
    }
    if a.apps_auth == "apps" {
        if !valid_username(a.apps_login_user.trim()) {
            return Err(Error::invalid("app login username: 1-64 letters, digits and . _ - @"));
        }
        if let Some(p) = password_problem(&a.apps_login_password, &a.apps_login_user) {
            return Err(Error::invalid(format!("app login password: {p}")));
        }
        if !["enabled", "disabledForLocalAddresses"].contains(&a.apps_auth_required.as_str()) {
            return Err(Error::invalid("app login: ask everywhere, or not on the local network"));
        }
    }
    if a.apps_auth == "tunnel" {
        let d = a.auth_cookie_domain.trim().trim_start_matches('.').to_lowercase();
        if !d.is_empty() && !HOST_RE.is_match(&d) {
            return Err(Error::invalid(format!("cookie domain {d:?}: a domain like example.com, no scheme or port")));
        }
        let u = a.auth_login_url.trim();
        if !u.is_empty() && !URL_RE.is_match(u) {
            return Err(Error::invalid(
                "wranglarr's address: like https://wranglarr.example.com, nothing after the host",
            ));
        }
        if !d.is_empty() && u.is_empty() {
            return Err(Error::invalid(
                "with a cookie domain, give wranglarr's address too (where the gate sends you to sign in)",
            ));
        }
    }
    Ok(())
}

/// Insertion-ordered KEY -> value pairs.
pub type Pairs = Vec<(&'static str, String)>;

fn set(p: &mut Pairs, k: &'static str, v: String) {
    match p.iter_mut().find(|(n, _)| *n == k) {
        Some(e) => e.1 = v,
        None => p.push((k, v)),
    }
}

pub fn get<'a>(p: &'a Pairs, k: &str) -> &'a str {
    p.iter().find(|(n, _)| *n == k).map_or("", |(_, v)| v.as_str())
}

fn or<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() { b } else { a }
}

/// The AUTH_KEYS values. apps: qBittorrent and NZBGet get the same login. tunnel: they keep
/// theirs (wranglarr, the arrs and the gate use it; you never type it) or get a random one.
pub fn auth_env(a: &Answers, old: &BTreeMap<String, String>) -> Result<Pairs> {
    let mut out: Pairs = vec![
        ("APPS_AUTH", a.apps_auth.clone()),
        ("APPS_LOGIN_USER", String::new()),
        ("APPS_LOGIN_PASSWORD", String::new()),
        ("APPS_AUTH_REQUIRED", "enabled".into()),
        ("AUTH_COOKIE_DOMAIN", String::new()),
        ("AUTH_LOGIN_URL", String::new()),
    ];
    let old = |k: &str| old.get(k).map_or("", String::as_str);
    let (mut qu, mut qp) =
        (or(&a.qbittorrent_user, default_user(&service::qbittorrent::DEF)).to_string(), a.qbittorrent_password.clone());
    let (mut nu, mut np) = (or(&a.nzbget_user, default_user(&service::nzbget::DEF)).to_string(), a.nzbget_pass.clone());
    if a.apps_auth == "apps" {
        let u = a.apps_login_user.trim().to_string();
        set(&mut out, "APPS_LOGIN_USER", u.clone());
        set(&mut out, "APPS_LOGIN_PASSWORD", a.apps_login_password.clone());
        set(&mut out, "APPS_AUTH_REQUIRED", a.apps_auth_required.clone());
        qu.clone_from(&u);
        nu = u;
        qp.clone_from(&a.apps_login_password);
        np.clone_from(&a.apps_login_password);
    } else if a.apps_auth == "tunnel" {
        set(&mut out, "AUTH_COOKIE_DOMAIN", a.auth_cookie_domain.trim().trim_start_matches('.').to_lowercase());
        set(&mut out, "AUTH_LOGIN_URL", a.auth_login_url.trim().trim_end_matches('/').to_string());
        qu = or(or(&qu, old("QBITTORRENT_USER")), default_user(&service::qbittorrent::DEF)).to_string();
        qp = if !qp.is_empty() {
            qp
        } else if !old("QBITTORRENT_PASSWORD").is_empty() {
            old("QBITTORRENT_PASSWORD").to_string()
        } else {
            crate::ids::random_token(18)
        };
        nu = or(or(&nu, old("NZBGET_USER")), default_user(&service::nzbget::DEF)).to_string();
        np = if !np.is_empty() {
            np
        } else if !old("NZBGET_PASS").is_empty() {
            old("NZBGET_PASS").to_string()
        } else {
            crate::ids::random_token(18)
        };
    }
    set(&mut out, "QBITTORRENT_USER", qu);
    set(&mut out, "QBITTORRENT_PASSWORD", qp);
    set(&mut out, "NZBGET_USER", nu);
    set(&mut out, "NZBGET_PASS", np);
    Ok(out)
}

/// Replace each key's last uncommented line in .env; append the ones that aren't there.
pub fn set_env_keys(text: &str, values: &Pairs) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut last: Vec<(&'static str, usize)> = vec![];
    for (i, line) in lines.iter().enumerate() {
        if let Some(c) = ENV_KEY_RE.captures(line)
            && let Some((k, _)) = values.iter().find(|(k, _)| *k == &c[1])
        {
            match last.iter_mut().find(|(n, _)| n == k) {
                Some(e) => e.1 = i,
                None => last.push((k, i)),
            }
        }
    }
    for (k, i) in &last {
        lines[*i] = format!("{k}={}", q(get(values, k))?);
    }
    let missing: Vec<&(&'static str, String)> =
        values.iter().filter(|(k, _)| !last.iter().any(|(n, _)| n == k)).collect();
    if !missing.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "# App logins (wranglarr-setup --auth, {}); applied by ./scripts/init.sh --auth",
            Utc::now().format("%Y-%m-%d")
        ));
        for (k, v) in missing {
            lines.push(format!("{k}={}", q(v)?));
        }
    }
    Ok(lines.join("\n") + "\n")
}

pub fn auth_steps(a: &Answers, auth_only: bool) -> Vec<String> {
    let mut s = vec![];
    if auth_only {
        s.push("Apply it, in the stack directory: ./scripts/init.sh --auth  (it sets each app's login, and starts or removes wranglarr-gate for AuthTunnel).".to_string());
    }
    if a.apps_auth == "apps" {
        s.push(format!("Every app's login is {:?} with the password you chose. init.sh sets it in each app and signs in with it to check; the API keys don't change.", a.apps_login_user.trim()));
    } else if a.apps_auth == "tunnel" {
        let where_ = if a.auth_cookie_domain.trim().is_empty() {
            "sign in to wranglarr at the same address you open the apps at".to_string()
        } else {
            format!("sign in at {} and the cookie covers *.{}", a.auth_login_url.trim(), a.auth_cookie_domain.trim())
        };
        s.push(format!("AuthTunnel: the apps' ports now belong to wranglarr-gate, which asks for your wranglarr sign-in; {where_}. Point your reverse proxy at the same ports as before (wiki: Reverse-Proxy)."));
    } else {
        s.push(
            "Manual: wranglarr leaves the apps' logins alone; each app asks you to create one on first visit.".into(),
        );
        if a.services.iter().any(|x| x == service::qbittorrent::DEF.id) && a.qbittorrent_password.is_empty() {
            s.push("qBittorrent prints a temporary admin password on first start: docker compose logs qbittorrent. Set a permanent one in its UI, then put it in .env as QBITTORRENT_PASSWORD and run: docker compose up -d wranglarr.".into());
        }
    }
    s
}

// ---- env rendering ---------------------------------------------------------------------------

/// The shared tunnel's add-on per scope (compose.vpn.yaml alone = the download clients).
fn vpn_scope_file(scope: &str) -> &'static str {
    match scope {
        "prowlarr" => ":compose.vpn-prowlarr.yaml",
        "downloads" => "",
        _ => ":compose.vpn-all.yaml",
    }
}

/// The `.env` for these answers; `image` is wranglarr's own (the one the wizard ran from).
pub fn render_env(a: &Answers, image: &str) -> Result<String> {
    let mut l: Vec<String> = vec![];
    let line = |k: &str, v: &str| -> Result<String> { Ok(format!("{k}={}", q(v)?)) };
    l.push(format!("# Generated by wranglarr setup {} on {}", crate::VERSION, Utc::now().format("%Y-%m-%d %H:%MZ")));
    l.push("# See .env.example for every option.\n".into());
    l.extend([
        line("TZ", &a.tz)?,
        format!("PUID={}", a.puid),
        format!("PGID={}", a.pgid),
        format!("UMASK={}", a.umask),
        String::new(),
    ]);
    l.push("# wranglarr's image: the one this wizard ran from (wranglarr:local = one you built)".into());
    l.extend([line("WRANGLARR_IMAGE", image)?, String::new()]);
    l.push("# Services (compose profiles)".into());
    // written unquoted: only the wizard's own service ids (no line break, no "=")
    if let Some(bad) = a.services.iter().find(|s| !services().any(|k| k.id == s.as_str())) {
        return Err(Error::invalid(format!("services: {bad:?} isn't a service the wizard sets up")));
    }
    l.push(format!("COMPOSE_PROFILES={}", a.services.join(",")));
    if a.vpn.mode == "shared" {
        l.push(format!("COMPOSE_FILE=compose.yaml:compose.vpn.yaml{}", vpn_scope_file(&a.vpn.scope)));
    } else if a.vpn.mode == "per-app" {
        l.push("COMPOSE_FILE=compose.yaml:compose.vpn-per-app.yaml".into());
    }
    l.extend([
        String::new(),
        "# Storage".into(),
        line("APPDATA", &a.appdata)?,
        line("DATA_ROOT", &a.data_root)?,
        line("DATA_ROOT_2", &a.data_root_2)?,
        String::new(),
    ]);
    l.extend([
        "# Postgres".into(),
        "POSTGRES_IMAGE=postgres:17".into(),
        line("POSTGRES_USER", &a.postgres_user)?,
        line("POSTGRES_PASSWORD", &a.postgres_password)?,
        "POSTGRES_HOST=postgres".into(),
        "POSTGRES_PORT=5432".into(),
        String::new(),
    ]);
    l.push("# Published ports".into());
    for s in services() {
        l.push(format!(
            "{}_PORT={}",
            s.id.to_uppercase(),
            a.ports.get(s.id).copied().unwrap_or(i64::from(s.container.ui_port()))
        ));
    }
    l.extend([format!("WRANGLARR_PORT={}", a.wranglarr_port), String::new()]);
    l.extend([
        "# wranglarr -> gluetun control server".into(),
        line("GLUETUN_API_KEY", &a.gluetun_api_key)?,
        String::new(),
    ]);
    let ae = auth_env(a, &BTreeMap::new())?;
    l.extend([
        "# App logins (wiki/App-Logins.md), applied by ./scripts/init.sh; change them with".to_string(),
        "# wranglarr-setup --auth, then ./scripts/init.sh --auth.".into(),
        "#   apps   = this login in every app;  tunnel = AuthTunnel, wranglarr signs you in to every app;".into(),
        "#   manual = each app keeps its own".into(),
    ]);
    for k in &AUTH_KEYS[..6] {
        l.push(line(k, get(&ae, k))?);
    }
    l.push(String::new());
    l.extend([
        "# NZBGet web login (the image applies it at every start)".into(),
        line("NZBGET_USER", get(&ae, "NZBGET_USER"))?,
        line("NZBGET_PASS", get(&ae, "NZBGET_PASS"))?,
        String::new(),
    ]);
    let qb_note = if a.apps_auth != "manual" {
        ": init writes it into qBittorrent.conf"
    } else {
        ": set the password in qBittorrent's UI first (it starts with a temporary one, printed in its logs), then fill it in here"
    };
    l.extend([
        format!("# qBittorrent web login{qb_note}"),
        line("QBITTORRENT_USER", get(&ae, "QBITTORRENT_USER"))?,
        line("QBITTORRENT_PASSWORD", get(&ae, "QBITTORRENT_PASSWORD"))?,
        String::new(),
    ]);
    let v = &a.vpn;
    l.push("# VPN, shared tunnel (compose.vpn.yaml)".into());
    l.extend([
        line("VPN_SERVICE_PROVIDER", &v.provider)?,
        format!("VPN_TYPE={}", v.vpn_type),
        line("WIREGUARD_PRIVATE_KEY", &v.private_key)?,
        line("WIREGUARD_PRESHARED_KEY", &v.preshared_key)?,
        line("WIREGUARD_ADDRESSES", &v.addresses)?,
        line("WIREGUARD_PUBLIC_KEY", &v.public_key)?,
        line("WIREGUARD_ENDPOINT_IP", &v.endpoint_ip)?,
        line("WIREGUARD_ENDPOINT_PORT", or(&v.endpoint_port, "51820"))?,
    ]);
    l.extend(
        ["WIREGUARD_MTU=1320", "SERVER_COUNTRIES=", "SERVER_CITIES=", "SERVER_HOSTNAMES=", "SERVER_REGIONS="]
            .map(String::from),
    );
    l.extend([line("VPN_USER", &v.user)?, line("VPN_PASSWORD", &v.password)?, "VPN_PORT_FORWARDING=off".into()]);
    l.extend(
        ["VPN_PORT_FORWARDING_PROVIDER=", "OPENVPN_PROTOCOL=udp", "OPENVPN_CUSTOM_CONFIG=", "OPENVPN_CIPHERS="]
            .map(String::from),
    );
    l.push(line("VPN_LAN_NETWORK", &v.lan_network)?);
    l.extend(
        [
            "DNS_UPSTREAM_RESOLVER_TYPE=dot",
            "DNS_UPSTREAM_RESOLVERS=cloudflare",
            "BLOCK_MALICIOUS=on",
            "HEALTH_VPN_DURATION_INITIAL=30s",
            "UPDATER_PERIOD=24h",
            "",
        ]
        .map(String::from),
    );
    l.push("# VPN, per-app tunnels (compose.vpn-per-app.yaml)".into());
    for s in services().filter(|s| s.container.vpn) {
        let on = a.vpn_per_app.iter().any(|x| x == s.id);
        l.push(format!("{}_VPN={}", s.id.to_uppercase(), if on { "true" } else { "false" }));
    }
    l.extend(
        [
            "VPN_PROVIDER=generic",
            "VPN_CONF=wg0",
            "VPN_HEALTHCHECK_ENABLED=true",
            "VPN_AUTO_PORT_FORWARD=false",
            "VPN_EXPOSE_PORTS_ON_LAN=",
            "VPN_PORT_REDIRECTS=",
            "VPN_NAMESERVERS=",
            "VPN_LAN_LEAK_ENABLED=false",
        ]
        .map(String::from),
    );
    let pia = v.provider == "pia";
    l.push(line("VPN_PIA_USER", if pia { &v.user } else { "" })?);
    l.push(line("VPN_PIA_PASS", if pia { &v.password } else { "" })?);
    l.extend(
        [
            "VPN_PIA_PREFERRED_REGION=",
            "VPN_PIA_DIP_TOKEN=",
            "VPN_PIA_PORT_FORWARD_PERSIST=false",
            "PRIVOXY_ENABLED=false",
            "UNBOUND_ENABLED=false",
            "UNBOUND_NAMESERVERS=",
            "",
        ]
        .map(String::from),
    );
    Ok(l.join("\n") + "\n")
}

/// The preview: secret values as dots.
pub fn mask_secrets(text: &str) -> String {
    let masked: Vec<String> = text
        .split('\n')
        .map(|line| {
            for k in ENV_KEYS_SECRET {
                if let Some(v) = line.strip_prefix(k).and_then(|r| r.strip_prefix('='))
                    && !v.is_empty()
                {
                    return format!("{k}=••••••");
                }
            }
            line.to_string()
        })
        .collect();
    masked.join("\n")
}

// ---- next steps ------------------------------------------------------------------------------

pub fn next_steps(a: &Answers, env: &Env, target: &Path, scan: &Value, restore_count: usize) -> Vec<String> {
    let ad = a.appdata.trim_end_matches('/');
    let mut steps = vec!["Review ./.env (it holds secrets; mode 600).".to_string()];
    if a.puid == 0 || a.pgid == 0 {
        steps.push("PUID/PGID is 0: every app will run as root and create root-owned files on your media. Set PUID/PGID in .env to a normal user (id -u / id -g) before running init.sh.".into());
    }
    let v = &a.vpn;
    if v.mode == "shared" && v.provider == "custom" && v.vpn_type == "wireguard" && v.private_key.is_empty() {
        steps.push(format!("Put your WireGuard config at {ad}/vpn/wireguard/wg0.conf."));
    }
    if v.mode == "shared" && v.vpn_type == "openvpn" && v.provider == "custom" {
        steps.push(format!(
            "Put your .ovpn at {ad}/vpn/custom.conf and set OPENVPN_CUSTOM_CONFIG=/gluetun/custom.conf in .env."
        ));
    }
    if v.mode == "per-app" {
        for s in &a.vpn_per_app {
            steps.push(format!("Put a WireGuard config for {s} at {ad}/{s}/wireguard/wg0.conf."));
        }
    }
    let head: String = a.data_root.chars().take(3).collect();
    if !a.data_root.starts_with('/') && !head.contains(':') {
        steps.push("DATA_ROOT should be an absolute host path (it is bind-mounted by Docker).".into());
    }
    if host::looks_truenas(env, target, &a.data_root) {
        steps.push("TrueNAS SCALE: in System > Shell (or over SSH), cd to the stack dataset and run: sudo ./scripts/init.sh  (it generates and patches the arr configs, creates the databases and starts everything). Use sudo docker compose ... for later commands. See the TrueNAS wiki page.".into());
    } else {
        steps.push("In the stack directory run: ./scripts/init.sh  (POSIX shell / WSL; it generates and patches the arr configs, creates the databases and starts everything).".into());
    }
    steps.push(format!("Open wranglarr at http://<this host>:{} and create its admin account. It asks for a one-time setup code from its log: docker compose logs wranglarr. Then, under Account, turn on two-factor or add a passkey.", a.wranglarr_port));
    if restore_count > 0 {
        steps.push(format!(
            "wranglarr will show a pending restore plan ({restore_count} item(s)); run it from the banner once the apps are up."
        ));
    }
    let sqlite: Vec<&str> = scan
        .get("sqlite")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !sqlite.is_empty() {
        steps.push(format!("SQLite databases were found for: {}. Their config.xml is reused; after the first start, copy each database into Postgres (one command per app): see Importing-Existing-Apps in the docs (wiki/Importing-Existing-Apps.md).", sqlite.join(", ")));
    }
    steps.extend(auth_steps(a, false));
    let has = |x: &str| a.services.iter().any(|s| s == x);
    if has(service::nzbget::DEF.id) && a.apps_auth == "manual" {
        steps.push("NZBGet login is NZBGET_USER / NZBGET_PASS from .env.".into());
    }
    let picked = |r: Roles| a.services.iter().any(|s| service::has(s, r));
    let mut wired = vec![];
    if picked(Roles::DOWNLOADS) {
        wired.push("download clients into the arrs");
    }
    if picked(Roles::SUBTITLES) {
        wired.push("Sonarr/Radarr into Bazarr");
    }
    if picked(Roles::INDEXERS) {
        wired.push("the arrs into Prowlarr");
    }
    if !wired.is_empty() {
        steps.push(format!(
            "Nothing to wire by hand: on first start wranglarr connects {} (System > App connections).",
            wired.join(", ")
        ));
    }
    steps.push(if picked(Roles::INDEXERS) {
        "Trackers: add them in Prowlarr, or on wranglarr's Indexers page (Torznab/Newznab feeds, Jackett, NZBHydra2), and pick which arrs get them.".into()
    } else {
        "Trackers: add them on wranglarr's Indexers page (Torznab/Newznab feeds, Jackett, NZBHydra2, a Prowlarr) and pick which arrs get them.".into()
    });
    steps
}

/// A login service's default username (the wizard has fields for qBittorrent's
/// and NZBGet's logins).
fn default_user(d: &ServiceDef) -> &'static str {
    match d.credentials {
        service::Credentials::UserPassword { default_user, .. } => default_user,
        service::Credentials::ApiKey { .. } => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_parse() {
        let e = parse_env("# c\nA=1\n B = 'x y' \nC=\"q\"\nD='\nnoeq\nA=2\n");
        assert_eq!(e.get("A").map(String::as_str), Some("2"));
        assert_eq!(e.get("B").map(String::as_str), Some("x y"));
        assert_eq!(e.get("C").map(String::as_str), Some("q"));
        assert_eq!(e.get("D").map(String::as_str), Some("'"));
        assert!(!e.contains_key("noeq"));
    }

    #[test]
    fn values_name_the_field() {
        let a = Answers { postgres_password: "pa$$'word".into(), ..Default::default() };
        assert_eq!(
            check_values(&a).expect_err("bad").to_string(),
            "postgres password: can't contain both ' and one of $ \" \\"
        );
        let mut a = Answers::default();
        a.vpn.lan_network = "a\nb".into();
        assert_eq!(check_values(&a).expect_err("bad").to_string(), "VPN lan network: can't contain a line break");
    }

    #[test]
    fn services_are_the_wizards_own() {
        let mut a = Answers { services: vec!["radarr".into(), "seerr".into()], ..Default::default() };
        assert!(render_env(&a, crate::IMAGE).expect("renders").contains("\nCOMPOSE_PROFILES=radarr,seerr\n"));
        for bad in ["radarr\nAPPS_AUTH_REQUIRED=disabled", "radarr,tools", "", "Radarr"] {
            a.services = vec!["sonarr".into(), bad.into()];
            assert_eq!(
                render_env(&a, crate::IMAGE).expect_err(bad).to_string(),
                format!("services: {bad:?} isn't a service the wizard sets up")
            );
        }
    }

    #[test]
    fn backup_prefs() {
        let a = Answers { backup_cron: "nonsense".into(), ..Default::default() };
        assert!(check_backup_prefs(&a).expect_err("bad").to_string().starts_with(r#"backup schedule "nonsense": "#));
        let a = Answers { keep_days: -1, ..Default::default() };
        assert_eq!(check_backup_prefs(&a).expect_err("bad").to_string(), "backup retention: 0 or more");
        assert!(check_backup_prefs(&Answers { backup_cron: " ".into(), ..Default::default() }).is_ok());
        let text = "[server]\nport = 1\n[[suite]]\nname = \"a\"\nschedule = \"x\"  # c\nkeep_last = 3\n  [[suite.app]]\n\
                    name = \"r\"\n[[suite]]\nname = \"b\"\nschedule = \"y\"\n";
        let a = Answers { backup_cron: "0 2 * * *".into(), keep_last: 5, keep_days: 9, ..Default::default() };
        assert_eq!(
            apply_backup_prefs(text, &a),
            "[server]\nport = 1\n[[suite]]\nname = \"a\"\nschedule = \"0 2 * * *\"   # from the setup wizard\nkeep_last = 5\n\
             keep_days = 9\n  [[suite.app]]\nname = \"r\"\n[[suite]]\nname = \"b\"\nschedule = \"y\"\nkeep_last = 5\nkeep_days = 9\n"
        );
        let a = Answers { backup_cron: String::new(), ..Default::default() };
        assert!(apply_backup_prefs(text, &a).contains("name = \"b\"\nschedule = \"\"\n"));
    }

    #[test]
    fn logins() {
        let a = Answers { apps_login_password: "short".into(), ..Default::default() };
        assert_eq!(check_auth(&a).expect_err("bad").to_string(), "app login password: use at least 12 characters");
        let a = Answers { apps_login_password: "a-long-Enough-pass".into(), ..Default::default() };
        assert!(check_auth(&a).is_ok());
        let a = Answers { apps_auth: "tunnel".into(), auth_cookie_domain: ".Example.COM".into(), ..Default::default() };
        assert!(check_auth(&a).expect_err("bad").to_string().starts_with("with a cookie domain"));
        let a = Answers { apps_auth: "tunnel".into(), auth_cookie_domain: "http://x".into(), ..Default::default() };
        assert_eq!(
            check_auth(&a).expect_err("bad").to_string(),
            r#"cookie domain "http://x": a domain like example.com, no scheme or port"#
        );
        let a = Answers {
            apps_auth: "tunnel".into(),
            auth_cookie_domain: ".Example.COM".into(),
            auth_login_url: "https://w.example.com/".into(),
            ..Default::default()
        };
        let e = auth_env(&a, &BTreeMap::new()).expect("env");
        assert_eq!(get(&e, "AUTH_COOKIE_DOMAIN"), "example.com");
        assert_eq!(get(&e, "AUTH_LOGIN_URL"), "https://w.example.com");
        assert_eq!(get(&e, "QBITTORRENT_PASSWORD").len(), 24, "a random one");
        let old = BTreeMap::from([("NZBGET_PASS".to_string(), "kept".to_string())]);
        assert_eq!(get(&auth_env(&a, &old).expect("env"), "NZBGET_PASS"), "kept");
    }

    #[test]
    fn env_keys_replaced() {
        let vals: Pairs = vec![("A", "x y".into()), ("B", "2".into())];
        let out = set_env_keys("A=1\n# A=0\nA=3\nC=4", &vals).expect("ok");
        assert!(out.starts_with("A=1\n# A=0\nA='x y'\nC=4\n\n# App logins (wranglarr-setup --auth, "), "{out}");
        assert!(out.ends_with("); applied by ./scripts/init.sh --auth\nB=2\n"), "{out}");
    }

    #[test]
    fn masking() {
        assert_eq!(
            mask_secrets("POSTGRES_PASSWORD=x\nPOSTGRES_USER=arr\nNZBGET_PASS=\n"),
            "POSTGRES_PASSWORD=••••••\nPOSTGRES_USER=arr\nNZBGET_PASS=\n"
        );
    }
}
