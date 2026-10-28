// SPDX-License-Identifier: GPL-3.0-or-later
//! Where each app opens in the browser (the home page's posters), and each app's own logo.
//!
//! A link comes from, in order:
//!   1. a per-app address set on the home page (Links), e.g. `https://radarr.example.com`
//!   2. the domain pattern, e.g. https://{name}.home.example.com
//!   3. when wranglarr itself was opened through the reverse proxy (at its domain or a
//!      name under it): the app's name there, e.g. `https://radarr.example.com`, or for an
//!      app without one, this machine's local address and the app's port (local only).
//!      The domain and names come from the Reverse proxy page; without a domain there,
//!      AuthTunnel's `AUTH_COOKIE_DOMAIN` with a name each (`{name}.<domain>`). The local
//!      address is the Links panel's, else the Reverse proxy page's "this machine's address"
//!   4. otherwise: the host the browser used to reach wranglarr (or the Links panel's
//!      address) plus the port Docker publishes the app on, e.g. http://192.168.1.10:7878

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use url::{Host, Url};

use crate::app::App;
use crate::config::AppConfig;
use crate::error::{Error, Result};
use crate::service::AppConfigExt;
use crate::service::Credentials;

pub const KEY: &str = "links:settings";
const PORTS_TTL: Duration = Duration::from_secs(60);
/// AuthTunnel's nginx (scripts/authtunnel.sh).
pub const GATE: &str = "wranglarr-gate";
const LOGO_TTL: Duration = Duration::from_secs(6 * 3600);
/// A logo this big or bigger isn't one (and isn't read any further).
const LOGO_MAX: usize = 512_000;

/// Each app serves its own logo; wranglarr passes it through rather than shipping copies.
fn logo_paths(kind: &str) -> &'static [&'static str] {
    crate::service::get(kind).map_or(&[], |d| d.logo_paths)
}

pub static HOST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*$",
    )
    .expect("static regex")
});

// ---- addresses ----------------------------------------------------------------------

/// A URL's host: lower case, an IPv6 address without its brackets; None when it has none.
pub fn url_host(u: &Url) -> Option<String> {
    match u.host()? {
        Host::Domain("") => None,
        Host::Domain(d) => Some(d.to_lowercase()),
        Host::Ipv4(ip) => Some(ip.to_string()),
        Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

/// An app's address, parsed; None when it isn't a URL.
pub fn parse_url(u: &str) -> Option<Url> {
    Url::parse(u.trim()).ok()
}

/// A domain pattern filled in for one app: `{name}` and `{kind}` are replaced, and
/// `{{` and `}}` stand for a brace.
pub fn format_pattern(p: &str, name: &str, kind: &str) -> std::result::Result<String, String> {
    let mut out = String::new();
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '}' => return Err("a } without its {: write }} for a brace".into()),
            '{' => {
                let mut field = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    field.push(c);
                }
                if !closed {
                    return Err("a { without its }: write {{ for a brace".into());
                }
                match field.as_str() {
                    "name" => out.push_str(name),
                    "kind" => out.push_str(kind),
                    other => return Err(format!("unknown placeholder {{{other}}}: use {{name}} or {{kind}}")),
                }
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

fn is_ip(h: &str) -> bool {
    h.parse::<IpAddr>().is_ok()
}

pub fn check_host(h: &str) -> Result<String> {
    let h = h.trim().trim_matches(|c| c == '[' || c == ']');
    if h.is_empty() || is_ip(h) {
        return Ok(h.to_string());
    }
    if !HOST_RE.is_match(h) {
        return Err(Error::invalid(format!("host: {h:?} isn't a host name or IP address (no scheme or port here)")));
    }
    Ok(h.to_string())
}

pub fn check_url(u: &str, what: &str) -> Result<String> {
    let u = u.trim();
    if u.is_empty() {
        return Ok(String::new());
    }
    let ok = parse_url(u).is_some_and(|p| matches!(p.scheme(), "http" | "https") && url_host(&p).is_some())
        && !u.chars().any(char::is_whitespace);
    if !ok {
        return Err(Error::invalid(format!("{what}: {u:?} isn't an http:// or https:// address")));
    }
    Ok(u.to_string())
}

pub fn check_pattern(p: &str) -> Result<String> {
    let p = p.trim();
    if p.is_empty() {
        return Ok(String::new());
    }
    if !p.contains("{name}") && !p.contains("{kind}") {
        return Err(Error::invalid(
            "domain pattern: put {name} (or {kind}) where the app's name goes, e.g. https://{name}.home.example.com",
        ));
    }
    // filled in for a sample app, to check it makes a URL
    let sample = crate::service::ALL[0].id;
    let filled = format_pattern(p, sample, sample).map_err(|e| Error::invalid(format!("domain pattern: {e}")))?;
    check_url(&filled, "domain pattern")?;
    Ok(p.to_string())
}

pub fn host_part(host: &str) -> String {
    if host.contains(':') { format!("[{host}]") } else { host.to_string() }
}

fn null_default<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> std::result::Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

// ---- types ------------------------------------------------------------------------------

/// The Links settings (state key `links:settings`); also the PUT body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkSettings {
    /// "port" or "domain"
    pub mode: String,
    /// "http" or "https" (port mode)
    pub scheme: String,
    /// A fixed host for port mode; blank: the one the browser used.
    pub host: String,
    pub pattern: String,
    /// app -> address set by hand
    #[serde(deserialize_with = "null_default")]
    pub apps: BTreeMap<String, String>,
}

impl Default for LinkSettings {
    fn default() -> Self {
        LinkSettings {
            mode: "port".into(),
            scheme: "http".into(),
            host: String::new(),
            pattern: String::new(),
            apps: BTreeMap::new(),
        }
    }
}

/// Where Docker publishes an app's port on the host.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Published {
    pub port: Option<u16>,
    /// Bound to 127.0.0.1 (or ::1) only.
    pub loopback: bool,
    pub how: String,
    /// Published by wranglarr-gate (AuthTunnel) instead of the app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<bool>,
}

/// Where an app opens.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Link {
    pub url: Option<String>,
    /// "set" | "domain" | "proxy" | "port" | "none"
    pub how: &'static str,
    pub note: String,
}

/// A logo: its bytes and content type.
pub type Logo = (Bytes, String);

/// The caches (published ports, logos), kept on the service.
#[derive(Default)]
pub struct LinkCache {
    ports: Mutex<Option<(Instant, BTreeMap<String, Published>)>>,
    /// app -> (fetched at, (body, content type) or None when it has none)
    logos: Mutex<HashMap<String, (Instant, Option<Logo>)>>,
    /// app -> held while its logo is being fetched, so requests that arrive
    /// meanwhile wait for that one instead of each downloading it (one per
    /// configured app at most)
    logo_fetches: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Every app once, in config order (an app in two suites counts once).
pub fn unique_apps<'a>(apps: impl Iterator<Item = &'a AppConfig>) -> Vec<&'a AppConfig> {
    let mut seen = HashSet::new();
    apps.filter(|a| seen.insert(a.name.clone())).collect()
}

/// wranglarr was opened through the reverse proxy: each proxied app's address there, and
/// this machine's local address for the others ("" when it isn't known).
#[derive(Debug, Clone, Default)]
pub struct Through {
    pub urls: BTreeMap<String, String>,
    pub local: String,
}

/// The links, from the settings, the published ports and, when wranglarr was opened
/// through the reverse proxy (port mode), the apps' addresses there (`through`).
pub fn resolve_with(
    s: &LinkSettings,
    apps: &[&AppConfig],
    through: Option<&Through>,
    ports: &BTreeMap<String, Published>,
    browser_host: &str,
) -> BTreeMap<String, Link> {
    let host = [s.host.as_str(), browser_host].into_iter().find(|h| !h.is_empty()).unwrap_or("localhost");
    let mut out = BTreeMap::new();
    for a in apps {
        let link = if let Some(u) = s.apps.get(&a.name) {
            Link { url: Some(u.clone()), how: "set", note: "set on this page".into() }
        } else if s.mode == "domain" {
            let url = format_pattern(&s.pattern, &a.name, &a.kind).unwrap_or_else(|_| s.pattern.clone());
            Link { url: Some(url), how: "domain", note: "from the domain pattern".into() }
        } else if let Some(t) = through {
            match t.urls.get(&a.name) {
                Some(u) => Link { url: Some(u.clone()), how: "proxy", note: "its name on the reverse proxy".into() },
                None if t.local.is_empty() => Link {
                    url: None,
                    how: "none",
                    note: "local only, not on the reverse proxy: put this machine's address under Address to link it"
                        .into(),
                },
                None => {
                    let mut l = port_link(s, a, ports, &t.local);
                    if l.url.is_some() {
                        l.note = format!("local only, not on the reverse proxy ({})", l.note);
                    }
                    l
                }
            }
        } else {
            port_link(s, a, ports, host)
        };
        out.insert(a.name.clone(), link);
    }
    out
}

/// `host` plus the port Docker publishes `a` on.
fn port_link(s: &LinkSettings, a: &AppConfig, ports: &BTreeMap<String, Published>, host: &str) -> Link {
    let Some(p) = ports.get(&a.name).filter(|p| p.port.is_some_and(|n| n != 0)) else {
        return Link { url: None, how: "none", note: "not published on the host: set an address for it".into() };
    };
    // the app's URL base, if any
    let base = parse_url(&a.url).map(|u| u.path().to_string()).unwrap_or_default();
    let base = base.trim_end_matches('/');
    let mut note = p.how.clone();
    if p.loopback && !["localhost", "127.0.0.1", "::1"].contains(&host) {
        note = "published on 127.0.0.1 only: opens on the host itself, not from other machines".into();
    }
    let port = p.port.unwrap_or_default();
    Link { url: Some(format!("{}://{}:{port}{base}/", s.scheme, host_part(host))), how: "port", note }
}

/// This machine's local address for links through the proxy: the Links panel's, else
/// the Reverse proxy page's "this machine's address", when it is one (not loopback, not
/// a name on the proxy itself).
fn local_address(s: &LinkSettings, page: &crate::proxy::ProxySettings, proxy: &crate::proxy::ProxySettings) -> String {
    if !s.host.is_empty() {
        return s.host.clone();
    }
    let h = page.upstream_host.trim();
    let loopback = h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) || h.eq_ignore_ascii_case("localhost");
    if h.is_empty() || loopback || crate::proxy::under_domain(proxy, h) { String::new() } else { h.to_string() }
}

/// The proxy the links follow: the Reverse proxy page's, or, when it names no domain,
/// AuthTunnel's cookie domain (`cookie_domain`, empty unless APPS_AUTH=tunnel), whose
/// apps the wizard puts at a name each: `https://radarr.example.com`.
fn proxy_for_links(page: crate::proxy::ProxySettings, cookie_domain: &str) -> crate::proxy::ProxySettings {
    if !page.domain.trim().is_empty() || cookie_domain.is_empty() {
        return page;
    }
    crate::proxy::ProxySettings { domain: cookie_domain.to_string(), ..Default::default() }
}

/// The first host binding of `<inner>/tcp` in a container's inspect.
fn bind(info: &Value, inner: u16) -> Option<Published> {
    let binds = info["NetworkSettings"]["Ports"][format!("{inner}/tcp")].as_array()?;
    binds.iter().find_map(|b| {
        let hp = b["HostPort"].as_str().filter(|p| !p.is_empty())?;
        let ip = b["HostIp"].as_str().unwrap_or("");
        Some(Published {
            port: Some(hp.trim().parse().ok()?),
            loopback: ip == "127.0.0.1" || ip == "::1",
            how: String::new(),
            gate: None,
        })
    })
}

/// The view of the service for links: `app.links()`.
pub struct Links<'a>(&'a App);

impl App {
    pub fn links(&self) -> Links<'_> {
        Links(self)
    }
}

impl<'a> Links<'a> {
    /// Every app once, in config order.
    pub fn apps(&self) -> Vec<&'a AppConfig> {
        unique_apps(self.0.cfg().all_apps())
    }

    /// The app called `name`; a 404 otherwise.
    pub fn app(&self, name: &str) -> Result<&'a AppConfig> {
        self.0.cfg().app(name).ok_or_else(|| Error::not_found(format!("no app {name:?}")))
    }

    // ---- settings ----------------------------------------------------------------
    pub async fn settings(&self) -> LinkSettings {
        self.0.state().get::<LinkSettings>(KEY).await.ok().flatten().unwrap_or_default()
    }

    /// Check and save; a bad value is a 400 with the reason.
    pub async fn save(&self, body: &LinkSettings) -> Result<LinkSettings> {
        let mode = body.mode.as_str();
        if mode != "port" && mode != "domain" {
            return Err(Error::invalid("mode: port or domain"));
        }
        if body.scheme != "http" && body.scheme != "https" {
            return Err(Error::invalid("scheme: http or https"));
        }
        let names: HashSet<&str> = self.apps().iter().map(|a| a.name.as_str()).collect();
        let mut apps = BTreeMap::new();
        for (name, url) in &body.apps {
            if !names.contains(name.as_str()) {
                return Err(Error::invalid(format!("no app called {name:?}")));
            }
            let u = check_url(url, name)?;
            if !u.is_empty() {
                apps.insert(name.clone(), u);
            }
        }
        let pattern = check_pattern(&body.pattern)?;
        if mode == "domain" && pattern.is_empty() {
            return Err(Error::invalid(
                "domain pattern: needed for 'a domain per app', e.g. https://{name}.home.example.com",
            ));
        }
        let s = LinkSettings {
            mode: mode.into(),
            scheme: body.scheme.clone(),
            host: check_host(&body.host)?,
            pattern,
            apps,
        };
        self.0.state().put(KEY, &s).await?;
        Ok(s)
    }

    pub async fn reset(&self) -> Result<()> {
        self.0.state().delete(KEY).await
    }

    // ---- published ports ---------------------------------------------------------
    /// app -> where Docker publishes it (cached for a minute).
    pub async fn published(&self, refresh: bool) -> BTreeMap<String, Published> {
        if let Ok(g) = self.0.link_cache().ports.lock()
            && let Some((at, cached)) = &*g
            && !cached.is_empty()
            && !refresh
            && at.elapsed() < PORTS_TTL
        {
            return cached.clone();
        }
        let docker_up = self.0.docker_ok().await;
        let apps = self.apps();
        let found = futures::future::join_all(apps.iter().map(|a| self.published_one(a, docker_up))).await;
        let out: BTreeMap<String, Published> = apps.iter().map(|a| a.name.clone()).zip(found).collect();
        if let Ok(mut g) = self.0.link_cache().ports.lock() {
            *g = Some((Instant::now(), out.clone()));
        }
        out
    }

    async fn published_one(&self, a: &AppConfig, docker_up: bool) -> Published {
        let inner = parse_url(&a.url).and_then(|u| u.port_or_known_default()).filter(|p| *p != 0).unwrap_or(80);
        let guess = Published {
            port: Some(inner),
            loopback: false,
            how: "guessed: the app's own port (no Docker socket to ask)".into(),
            gate: None,
        };
        let Some(d) = self.0.docker().filter(|_| docker_up) else { return guess };
        let container = if a.container.is_empty() { &a.name } else { &a.container };
        let info = match d.inspect(container).await {
            Ok(info) => {
                let mode = info["HostConfig"]["NetworkMode"].as_str().unwrap_or("").to_string();
                match mode.strip_prefix("container:") {
                    // sharing the VPN container's network: its ports
                    Some(other) => d.inspect(other).await,
                    None => Ok(info),
                }
            }
            Err(e) => Err(e),
        };
        let info = match info {
            Ok(i) => i,
            Err(e) => return Published { how: format!("guessed: Docker couldn't find the container ({e})"), ..guess },
        };
        if let Some(own) = bind(&info, inner) {
            return Published { how: "published port".into(), gate: Some(false), ..own };
        }
        // AuthTunnel: wranglarr-gate publishes the app's port instead, on the same number inside
        if let Ok(g) = d.inspect(GATE).await
            && let Some(gate) = bind(&g, inner)
        {
            return Published { how: "through wranglarr-gate".into(), gate: Some(true), ..gate };
        }
        Published { port: None, loopback: false, how: "not published on the host".into(), gate: None }
    }

    // ---- the links -----------------------------------------------------------------
    /// app -> where it opens, for a browser that reached wranglarr at `browser_host`.
    pub async fn resolve(&self, browser_host: &str) -> BTreeMap<String, Link> {
        let s = self.settings().await;
        let apps = self.apps();
        if s.mode != "port" {
            return resolve_with(&s, &apps, None, &BTreeMap::new(), browser_host);
        }
        let page = self.0.proxy().settings().await;
        let proxy = proxy_for_links(page.clone(), &self.0.auth().options().cookie_domain);
        let through = crate::proxy::public_urls(&proxy, &apps, browser_host)
            .map(|urls| Through { urls, local: local_address(&s, &page, &proxy) });
        resolve_with(&s, &apps, through.as_ref(), &self.published(false).await, browser_host)
    }

    // ---- logos -----------------------------------------------------------------------
    /// The app's own logo (body, content type), asked from the app and cached;
    /// None when it has none or doesn't answer. 404 for an unknown app.
    pub async fn logo(&self, name: &str) -> Result<Option<Logo>> {
        let a = self.app(name)?;
        let cached = || {
            let g = self.0.link_cache().logos.lock().ok()?;
            g.get(name).filter(|(at, _)| at.elapsed() < LOGO_TTL).map(|(_, hit)| hit.clone())
        };
        if let Some(hit) = cached() {
            return Ok(hit);
        }
        let fetch = {
            let mut g = self.0.link_cache().logo_fetches.lock().map_err(|_| Error::internal("logo lock poisoned"))?;
            g.entry(a.name.clone()).or_default().clone()
        };
        let _one_at_a_time = fetch.lock().await;
        if let Some(hit) = cached() {
            return Ok(hit); // fetched while this one waited
        }
        let basic_auth =
            a.def().is_some_and(|d| matches!(d.credentials, Credentials::UserPassword { basic_auth: true, .. }));
        let auth = if basic_auth {
            // it wants its login even for images
            let (u, p) = crate::service::credentials(a).await;
            (!u.is_empty()).then_some((u, p))
        } else {
            None
        };
        let http = crate::http::http(Duration::from_secs(5));
        let base = a.url.trim_end_matches('/');
        let mut found = None;
        for path in logo_paths(&a.kind) {
            let mut rb = http.get(format!("{base}{path}"));
            if let Some((u, p)) = &auth {
                rb = rb.basic_auth(u, Some(p));
            }
            let r = match rb.send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("logo {name}: {e}");
                    return Ok(None); // don't cache: the app may just be starting
                }
            };
            let status = r.status().as_u16();
            let ctype = r
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if status != 200 || (ctype != "image/svg+xml" && ctype != "image/png") {
                continue;
            }
            match crate::http::read_at_most(r, LOGO_MAX - 1).await {
                Ok(Some(body)) => {
                    found = Some((body, ctype));
                    break;
                }
                Ok(None) => tracing::debug!("logo {name}: {path} is {LOGO_MAX} bytes or more"),
                Err(e) => {
                    tracing::debug!("logo {name}: {e}");
                    return Ok(None);
                }
            }
        }
        if let Ok(mut g) = self.0.link_cache().logos.lock() {
            g.insert(name.to_string(), (Instant::now(), found.clone()));
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, kind: &str, url: &str) -> AppConfig {
        AppConfig { name: name.into(), kind: kind.into(), url: url.into(), enabled: true, ..Default::default() }
    }

    fn port(p: u16, loopback: bool) -> Published {
        Published { port: Some(p), loopback, how: "published port".into(), gate: Some(false) }
    }

    #[test]
    fn reads_addresses() {
        let u = parse_url(" HTTP://User@Radarr:7878/radarr/?x#y").expect("ok");
        assert_eq!(
            (u.scheme(), url_host(&u).as_deref(), u.port(), u.path()),
            ("http", Some("radarr"), Some(7878), "/radarr/")
        );
        let u = parse_url("http://[::1]:8/").expect("ok");
        assert_eq!((url_host(&u).as_deref(), u.port()), (Some("::1"), Some(8)));
        assert_eq!(parse_url("https://sonarr").and_then(|u| u.port_or_known_default()), Some(443));
        assert!(parse_url("http://").is_none());
        assert!(parse_url("http://[::1").is_none());
        let u = parse_url("radarr:7878").expect("a scheme of its own");
        assert_eq!((u.scheme(), url_host(&u)), ("radarr", None));
    }

    #[test]
    fn checks_hosts_urls_patterns() {
        assert_eq!(check_host(" [::1] ").expect("ip"), "::1");
        assert_eq!(check_host("01.2.3.4").expect("matches the host pattern"), "01.2.3.4");
        assert_eq!(check_host("").expect("blank"), "");
        for bad in ["a_b", "-a.com", "a.com.", "x:80"] {
            let e = check_host(bad).expect_err(bad);
            assert_eq!(
                e.to_string(),
                format!("host: {bad:?} isn't a host name or IP address (no scheme or port here)")
            );
        }
        assert_eq!(check_url(" HTTP://X ", "w").expect("ok"), "HTTP://X");
        assert_eq!(check_url("http://[::1]:8/", "w").expect("ok"), "http://[::1]:8/");
        for bad in ["http://", "http://a b", "http://u@:80", "ftp://x", "http://[::1"] {
            let e = check_url(bad, "w").expect_err(bad);
            assert_eq!(e.to_string(), format!("w: {bad:?} isn't an http:// or https:// address"));
        }
        assert_eq!(check_pattern("https://{name}.x.com").expect("ok"), "https://{name}.x.com");
        assert_eq!(check_pattern("https://{{a}}{name}").expect("escaped braces"), "https://{{a}}{name}");
        let cases = [
            ("ftp://{name}", r#"domain pattern: "ftp://radarr" isn't an http:// or https:// address"#),
            ("https://{name}.{other}", "domain pattern: unknown placeholder {other}: use {name} or {kind}"),
            ("https://{}{name}", "domain pattern: unknown placeholder {}: use {name} or {kind}"),
            ("https://{name}}", "domain pattern: a } without its {: write }} for a brace"),
            ("https://{name}{", "domain pattern: a { without its }: write {{ for a brace"),
            (
                "https://{name",
                "domain pattern: put {name} (or {kind}) where the app's name goes, e.g. https://{name}.home.example.com",
            ),
        ];
        for (p, msg) in cases {
            assert_eq!(check_pattern(p).expect_err(p).to_string(), msg, "{p}");
        }
    }

    #[test]
    fn resolves_port_mode() {
        let apps = [
            app("radarr", "radarr", "http://radarr:7878"),
            app("sonarr", "sonarr", "http://sonarr:8989/sonarr/"),
            app("sab", "sabnzbd", "http://sabnzbd:8080"),
            app("qb", "qbittorrent", "http://qbittorrent:8090"),
        ];
        let refs: Vec<&AppConfig> = apps.iter().collect();
        let mut ports = BTreeMap::new();
        ports.insert("radarr".to_string(), port(7878, false));
        ports.insert("sonarr".to_string(), port(18989, true));
        ports.insert(
            "qb".to_string(),
            Published { port: None, how: "not published on the host".into(), ..Default::default() },
        );
        let s = LinkSettings::default();
        let l = resolve_with(&s, &refs, None, &ports, "192.168.1.10");
        assert_eq!(
            l["radarr"],
            Link { url: Some("http://192.168.1.10:7878/".into()), how: "port", note: "published port".into() }
        );
        assert_eq!(l["sonarr"].url.as_deref(), Some("http://192.168.1.10:18989/sonarr/"));
        assert_eq!(l["sonarr"].note, "published on 127.0.0.1 only: opens on the host itself, not from other machines");
        for n in ["sab", "qb"] {
            assert_eq!(
                l[n],
                Link { url: None, how: "none", note: "not published on the host: set an address for it".into() }
            );
        }
        // a fixed host (IPv6 in brackets), https, and one set by hand
        let s = LinkSettings {
            scheme: "https".into(),
            host: "::1".into(),
            apps: [("qb".to_string(), "https://qb.example.com".to_string())].into(),
            ..Default::default()
        };
        let l = resolve_with(&s, &refs, None, &ports, "ignored");
        assert_eq!(l["radarr"].url.as_deref(), Some("https://[::1]:7878/"));
        assert_eq!(l["sonarr"].note, "published port", "loopback is fine from the host itself");
        assert_eq!(
            l["qb"],
            Link { url: Some("https://qb.example.com".into()), how: "set", note: "set on this page".into() }
        );
        // no host at all
        let l = resolve_with(&LinkSettings::default(), &refs, None, &ports, "");
        assert_eq!(l["radarr"].url.as_deref(), Some("http://localhost:7878/"));
    }

    #[test]
    fn through_the_proxy_names_first_then_this_machine_local_only() {
        let apps = [app("radarr", "radarr", "http://radarr:7878"), app("qb", "qbittorrent", "http://qb:8090")];
        let refs: Vec<&AppConfig> = apps.iter().collect();
        let ports: BTreeMap<String, Published> =
            [("radarr".to_string(), port(7878, false)), ("qb".to_string(), port(8090, false))].into();
        let urls: BTreeMap<String, String> = [("radarr".to_string(), "https://radarr.example.com/".to_string())].into();
        let through = Through { urls, local: "192.168.1.10".into() };
        let l = resolve_with(&LinkSettings::default(), &refs, Some(&through), &ports, "wranglarr.example.com");
        assert_eq!(
            l["radarr"],
            Link {
                url: Some("https://radarr.example.com/".into()),
                how: "proxy",
                note: "its name on the reverse proxy".into()
            }
        );
        // no name on the proxy: this machine's address and its port, never wranglarr's name
        assert_eq!(
            l["qb"],
            Link {
                url: Some("http://192.168.1.10:8090/".into()),
                how: "port",
                note: "local only, not on the reverse proxy (published port)".into()
            }
        );
        // ... and without a local address to use, no link rather than a wrong one
        let unknown = Through { local: String::new(), ..through.clone() };
        let l = resolve_with(&LinkSettings::default(), &refs, Some(&unknown), &ports, "wranglarr.example.com");
        assert_eq!((l["qb"].url.as_ref(), l["qb"].how), (None, "none"));
        assert!(l["qb"].note.starts_with("local only"), "{}", l["qb"].note);
        // a per-app address and the domain pattern still come first
        let set = LinkSettings {
            apps: [("radarr".to_string(), "http://10.0.0.5:7878".to_string())].into(),
            ..Default::default()
        };
        assert_eq!(resolve_with(&set, &refs, Some(&through), &ports, "x")["radarr"].how, "set");
    }

    #[test]
    fn the_local_address_for_links_through_the_proxy() {
        use crate::proxy::ProxySettings;
        let proxy = ProxySettings { domain: "example.com".into(), ..Default::default() };
        let page = |h: &str| ProxySettings { upstream_host: h.into(), ..Default::default() };
        let fixed = LinkSettings { host: "nas.lan".into(), ..Default::default() };
        assert_eq!(local_address(&fixed, &page("192.168.1.10"), &proxy), "nas.lan", "the Links panel's first");
        let blank = LinkSettings::default();
        assert_eq!(local_address(&blank, &page("192.168.1.10"), &proxy), "192.168.1.10");
        for not_local in ["", "127.0.0.1", "localhost", "::1", "wranglarr.example.com"] {
            assert_eq!(local_address(&blank, &page(not_local), &proxy), "", "{not_local}");
        }
    }

    #[test]
    fn authtunnels_domain_stands_in_for_an_unset_proxy_page() {
        use crate::proxy::ProxySettings;
        let p = proxy_for_links(ProxySettings::default(), "example.com");
        assert_eq!((p.domain.as_str(), p.style.as_str()), ("example.com", "subdomain"));
        let page = ProxySettings { domain: "home.net".into(), style: "path".into(), ..Default::default() };
        assert_eq!(proxy_for_links(page.clone(), "example.com"), page, "the page's own domain comes first");
        assert_eq!(proxy_for_links(ProxySettings::default(), ""), ProxySettings::default(), "no tunnel: nothing");
    }

    #[test]
    fn resolves_domain_mode() {
        let apps = [app("radarr4k", "radarr", "http://radarr4k:7878"), app("qb", "qbittorrent", "http://qb:8090")];
        let refs: Vec<&AppConfig> = apps.iter().collect();
        let s = LinkSettings {
            mode: "domain".into(),
            pattern: "https://{name}.home.example.com/{kind}".into(),
            apps: [("qb".to_string(), "http://192.168.1.20:8080".to_string())].into(),
            ..Default::default()
        };
        let l = resolve_with(&s, &refs, None, &BTreeMap::new(), "h");
        assert_eq!(
            l["radarr4k"],
            Link {
                url: Some("https://radarr4k.home.example.com/radarr".into()),
                how: "domain",
                note: "from the domain pattern".into()
            }
        );
        assert_eq!(l["qb"].how, "set");
    }

    #[test]
    fn reads_bindings() {
        let info = serde_json::json!({"NetworkSettings": {"Ports": {"7878/tcp": [
            {"HostIp": "127.0.0.1", "HostPort": ""}, {"HostIp": "127.0.0.1", "HostPort": "17878"}]}}});
        assert_eq!(
            bind(&info, 7878),
            Some(Published { port: Some(17878), loopback: true, how: String::new(), gate: None })
        );
        assert_eq!(bind(&info, 80), None);
    }
}
