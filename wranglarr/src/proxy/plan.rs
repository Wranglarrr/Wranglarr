// SPDX-License-Identifier: GPL-3.0-or-later
//! What each app becomes in the proxy: its name or path, where the proxy reaches it,
//! and whether it goes through wranglarr-gate.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{AppExpose, ProxySettings, own_name, ungated, url_base};
use crate::config::AppConfig;
use crate::links::{GATE, Published, host_part, parse_url, url_host};
use crate::service;

/// wranglarr or an app, as the proxy sees it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: String,
    /// Its port inside the stack's network.
    pub inner: u16,
    /// Its host name on the Docker network.
    pub service: String,
    /// The compose service its labels go on (the VPN's when it shares that network).
    pub compose_service: String,
}

impl Entry {
    pub fn wranglarr() -> Entry {
        Entry {
            name: "wranglarr".into(),
            kind: "wranglarr".into(),
            inner: 8787,
            service: "wranglarr".into(),
            compose_service: "wranglarr".into(),
        }
    }

    /// An app, from its config (the compose service: its host name, until Docker says otherwise).
    pub fn for_app(a: &AppConfig) -> Entry {
        let u = parse_url(&a.url);
        let service = u.as_ref().and_then(url_host).unwrap_or_else(|| a.name.clone());
        Entry {
            name: a.name.clone(),
            kind: a.kind.clone(),
            inner: u.and_then(|u| u.port()).filter(|p| *p != 0).unwrap_or(80),
            compose_service: service.clone(),
            service,
        }
    }
}

/// What an entry becomes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    pub name: String,
    pub kind: String,
    pub inner: u16,
    /// public | lan | off
    pub expose: String,
    /// Where it ended up: subdomain | path | direct (wranglarr: or domain).
    pub at: String,
    /// Local only.
    pub local: bool,
    pub sub: String,
    pub note: String,
    pub fqdn: String,
    pub path: String,
    pub strip: bool,
    pub sab: bool,
    pub host_up: String,
    pub docker_up: String,
    pub target: String,
    pub up: String,
}

/// The environment the output depends on (the logins setup, wranglarr's port).
#[derive(Debug, Clone, Default)]
pub struct ProxyEnv {
    /// APPS_AUTH, trimmed and lower-cased ("manual" when unset).
    pub apps_auth: String,
    /// AUTH_COOKIE_DOMAIN, trimmed and lower-cased.
    pub cookie_domain: String,
    /// AUTH_LOGIN_URL, trimmed.
    pub login_url: String,
    /// APPS_AUTH_REQUIRED as it is.
    pub required: Option<String>,
    /// WRANGLARR_PORT (8787).
    pub wranglarr_port: i64,
    /// HOST_IP: this machine's IP address on the network, found by setup ("" unknown).
    pub host_ip: String,
    /// WRANGLARR_URL_BASE: browser path prefix (blank at the root).
    pub url_base: String,
}

impl ProxyEnv {
    /// The settings in the environment `var` (part of [`crate::deployment::Deployment`]).
    pub fn read(var: crate::config::Lookup) -> ProxyEnv {
        ProxyEnv {
            apps_auth: var("APPS_AUTH").unwrap_or_else(|| "manual".into()).trim().to_lowercase(),
            cookie_domain: var("AUTH_COOKIE_DOMAIN").unwrap_or_default().trim().to_lowercase(),
            login_url: var("AUTH_LOGIN_URL").unwrap_or_default().trim().to_string(),
            required: var("APPS_AUTH_REQUIRED"),
            wranglarr_port: var("WRANGLARR_PORT").unwrap_or_else(|| "8787".into()).trim().parse().unwrap_or(8787),
            host_ip: var("HOST_IP").unwrap_or_default().trim().to_string(),
            url_base: var("WRANGLARR_URL_BASE").unwrap_or_default().trim().trim_end_matches('/').to_string(),
        }
    }
}

/// What each entry becomes, for these settings. The proxy reaches published ports at the
/// settings' host, else this machine's address (HOST_IP), else `browser_host` (the
/// host the page was opened at).
pub fn plan(
    entries: &[Entry],
    s: &ProxySettings,
    browser_host: &str,
    ports: &BTreeMap<String, Published>,
    env: &ProxyEnv,
) -> Vec<Plan> {
    let tunnel = env.apps_auth == "tunnel";
    let host = [s.upstream_host.as_str(), env.host_ip.as_str(), browser_host]
        .into_iter()
        .find(|h| !h.is_empty())
        .unwrap_or("127.0.0.1");
    let domain = if s.domain.is_empty() { "example.com" } else { &s.domain };
    // wranglarr's own name: the apps under a path share it
    let w = placement(s, "wranglarr", "wranglarr");
    let wsub = if w.sub.is_empty() { "wranglarr".to_string() } else { w.sub.clone() };
    let wfqdn = match w.at.as_str() {
        "domain" | "path" => Some(domain.to_string()),
        "direct" => None,
        _ => Some(format!("{wsub}.{domain}")),
    };
    let mut out = Vec::new();
    for e in entries {
        let cfg = placement(s, &e.name, &e.kind);
        let sub = if cfg.sub.is_empty() { e.name.clone() } else { cfg.sub.clone() };
        let mut p = Plan {
            name: e.name.clone(),
            kind: e.kind.clone(),
            inner: e.inner,
            expose: cfg.expose().to_string(),
            at: cfg.at.clone(),
            local: cfg.local,
            fqdn: format!("{sub}.{domain}"),
            path: String::new(),
            sub,
            note: String::new(),
            strip: false,
            sab: service::get(&e.kind).is_some_and(|d| d.proxy.host_check),
            host_up: String::new(),
            docker_up: String::new(),
            target: String::new(),
            up: String::new(),
        };
        if e.kind == "wranglarr" {
            if cfg.at == "domain" || cfg.at == "path" {
                p.fqdn = domain.to_string();
            }
            if cfg.at == "path" {
                p.path = format!("/{}", p.sub);
            }
        } else if cfg.at == "path" {
            match &wfqdn {
                _ if own_name(&e.kind) => p.note = format!("{} doesn't run under a path: it gets its own name", e.name),
                Some(w) => {
                    p.path = format!("/{}", p.sub);
                    p.fqdn = w.clone();
                }
                None => p.note = "wranglarr isn't proxied, so there's no name to put it under: it gets its own".into(),
            }
            if p.path.is_empty() {
                p.at = "subdomain".into();
            }
        }
        p.strip = !p.path.is_empty() && !url_base(&e.kind);
        // where the proxy sends it
        if e.kind == "wranglarr" {
            p.host_up = format!("{}:{}", host_part(host), env.wranglarr_port);
            p.docker_up = "wranglarr:8787".into();
            p.target = "wranglarr".into();
        } else {
            let pubd = ports.get(&e.name).cloned().unwrap_or_default();
            if let Some(n) = pubd.port.filter(|n| *n != 0) {
                p.host_up = format!("{}:{n}", host_part(host));
            }
            if pubd.loopback && !["127.0.0.1", "localhost", "::1"].contains(&host) {
                p.note = "published on 127.0.0.1 only: the proxy has to run on this machine (use 127.0.0.1)".into();
            }
            if tunnel && !ungated(&e.kind) {
                p.docker_up = format!("{GATE}:{}", e.inner);
                p.target = GATE.into();
            } else {
                p.docker_up = format!("{}:{}", host_part(&e.service), e.inner);
                p.target = e.compose_service.clone();
            }
            if s.upstream == "host" && p.host_up.is_empty() {
                p.note = "not published on this machine: use “the proxy is a container on the stack's network”".into();
            }
        }
        p.up = if s.upstream == "docker" { p.docker_up.clone() } else { p.host_up.clone() };
        out.push(p);
    }
    out
}

/// Where `name` (of `kind`) is: as saved, the default where nothing is.
pub fn placement(s: &ProxySettings, name: &str, kind: &str) -> AppExpose {
    let mut c = s.apps.get(name).cloned().unwrap_or_default();
    if c.at.is_empty() {
        c.at = default_at(&s.style, kind).into();
    }
    c
}

/// Where `style` (subdomain | path, from before each app had its own) puts an app:
/// a path each is under wranglarr's name now, not the bare domain.
pub fn default_at(style: &str, kind: &str) -> &'static str {
    if style == "path" && kind != "wranglarr" { "path" } else { "subdomain" }
}

/// Whether `host` is the proxy's domain or a name under it.
pub fn under_domain(s: &ProxySettings, host: &str) -> bool {
    let domain = s.domain.trim().trim_end_matches('.').to_lowercase();
    let host = host.trim().trim_end_matches('.').to_lowercase();
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// app -> its address through the proxy (`https://radarr.example.com/`), for each
/// proxied app; empty when the settings name no domain.
pub fn proxy_urls(s: &ProxySettings, apps: &[&AppConfig]) -> BTreeMap<String, String> {
    if s.domain.trim().is_empty() {
        return BTreeMap::new();
    }
    let entries: Vec<Entry> = apps.iter().map(|a| Entry::for_app(a)).collect();
    plan(&entries, s, "", &BTreeMap::new(), &ProxyEnv::default())
        .into_iter()
        .filter(|p| p.expose != "off")
        .map(|p| (p.name, format!("https://{}{}/", p.fqdn, p.path)))
        .collect()
}

/// [`proxy_urls`], for a browser that opened wranglarr at `browser_host`. None unless
/// that's the proxy's domain or a name under it (so wranglarr itself came through the
/// proxy): by IP and port, the apps open by IP and port too.
pub fn public_urls(s: &ProxySettings, apps: &[&AppConfig], browser_host: &str) -> Option<BTreeMap<String, String>> {
    under_domain(s, browser_host).then(|| proxy_urls(s, apps))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: &str, inner: u16) -> Entry {
        Entry { name: name.into(), kind: kind.into(), inner, service: name.into(), compose_service: name.into() }
    }

    #[test]
    fn ipv6_upstreams_are_bracketed() {
        let entries = [Entry::wranglarr(), entry("radarr", "radarr", 7878)];
        let ports = [("radarr".into(), Published { port: Some(17878), ..Default::default() })].into();
        let s = ProxySettings::default();
        let env = ProxyEnv { host_ip: "fd00::5".into(), wranglarr_port: 8787, ..Default::default() };
        let p = plan(&entries, &s, "", &ports, &env);
        assert_eq!(p[0].up, "[fd00::5]:8787");
        assert_eq!(p[1].up, "[fd00::5]:17878");
        let s = ProxySettings { upstream_host: "::1".into(), ..s };
        assert_eq!(plan(&entries, &s, "", &ports, &env)[1].up, "[::1]:17878");
        let app = AppConfig {
            name: "remote".into(),
            kind: "radarr".into(),
            url: "http://[fd00::6]:7878".into(),
            ..Default::default()
        };
        let s = ProxySettings { upstream: "docker".into(), ..s };
        assert_eq!(plan(&[Entry::for_app(&app)], &s, "", &ports, &env)[0].up, "[fd00::6]:7878");
    }

    #[test]
    fn wranglarr_on_a_domain_path_has_sibling_app_paths() {
        let entries = [Entry::wranglarr(), entry("radarr", "radarr", 7878)];
        let at = |sub: &str| AppExpose { at: "path".into(), local: false, sub: sub.into() };
        let s = ProxySettings {
            domain: "myhouse.com".into(),
            upstream: "docker".into(),
            apps: [("wranglarr".into(), at("w")), ("radarr".into(), at("radarr"))].into(),
            ..Default::default()
        };
        let p = plan(&entries, &s, "myhouse.com", &BTreeMap::new(), &ProxyEnv::default());
        assert_eq!((p[0].fqdn.as_str(), p[0].path.as_str(), p[0].strip), ("myhouse.com", "/w", false));
        assert_eq!((p[1].fqdn.as_str(), p[1].path.as_str(), p[1].strip), ("myhouse.com", "/radarr", false));
        super::super::validate(&entries, &s).expect("wranglarr may use a path");
        let app = AppConfig {
            name: "radarr".into(),
            kind: "radarr".into(),
            url: "http://radarr:7878".into(),
            ..Default::default()
        };
        assert_eq!(public_urls(&s, &[&app], "myhouse.com").expect("proxied")["radarr"], "https://myhouse.com/radarr/");
    }

    #[test]
    fn plans_paths_and_names() {
        let entries = [
            entry("wranglarr", "wranglarr", 8787),
            entry("radarr", "radarr", 7878),
            entry("seerr", "seerr", 5055),
            entry("qb", "qbittorrent", 8090),
        ];
        let mut ports = BTreeMap::new();
        ports.insert("radarr".to_string(), Published { port: Some(7878), loopback: true, ..Default::default() });
        ports.insert("seerr".to_string(), Published { port: Some(5055), ..Default::default() });
        let s = ProxySettings { style: "path".into(), domain: "home.lan".into(), ..Default::default() };
        let env = ProxyEnv { apps_auth: "tunnel".into(), wranglarr_port: 9000, ..Default::default() };
        let p = plan(&entries, &s, "10.0.0.2", &ports, &env);
        assert_eq!(
            (p[0].fqdn.as_str(), p[0].path.as_str(), p[0].up.as_str()),
            ("wranglarr.home.lan", "", "10.0.0.2:9000")
        );
        assert_eq!((p[1].fqdn.as_str(), p[1].path.as_str(), p[1].strip), ("wranglarr.home.lan", "/radarr", false));
        assert_eq!(p[1].note, "published on 127.0.0.1 only: the proxy has to run on this machine (use 127.0.0.1)");
        assert_eq!((p[1].docker_up.as_str(), p[1].target.as_str()), ("wranglarr-gate:7878", "wranglarr-gate"));
        assert_eq!((p[2].fqdn.as_str(), p[2].target.as_str()), ("seerr.home.lan", "seerr"), "seerr: never gated");
        assert_eq!(p[2].note, "seerr doesn't run under a path: it gets its own name");
        assert!(p[3].strip && p[3].up.is_empty());
        assert_eq!(p[3].note, "not published on this machine: use “the proxy is a container on the stack's network”");
    }

    #[test]
    fn each_app_at_a_name_a_path_under_wranglarr_or_direct() {
        use super::super::AppExpose;
        let entries = [
            entry("wranglarr", "wranglarr", 8787),
            entry("radarr", "radarr", 7878),
            entry("sab", "sabnzbd", 8080),
            entry("qb", "qbittorrent", 8090),
            entry("seerr", "seerr", 5055),
        ];
        let at = |at: &str, local: bool| AppExpose { at: at.into(), local, sub: String::new() };
        let mut s = ProxySettings { domain: "example.com".into(), ..Default::default() };
        s.apps.insert("wranglarr".into(), at("subdomain", false));
        s.apps.insert("radarr".into(), at("subdomain", false));
        s.apps.insert("sab".into(), at("path", true));
        s.apps.insert("qb".into(), at("direct", true));
        s.apps.insert("seerr".into(), at("path", false));
        let env = ProxyEnv::default();
        let ports: BTreeMap<String, Published> =
            entries.iter().map(|e| (e.name.clone(), Published { port: Some(e.inner), ..Default::default() })).collect();
        let p = plan(&entries, &s, "10.0.0.2", &ports, &env);
        let got = |i: usize| (p[i].fqdn.as_str(), p[i].path.as_str(), p[i].expose.as_str(), p[i].at.as_str());
        assert_eq!(got(0), ("wranglarr.example.com", "", "public", "subdomain"));
        assert_eq!(got(1), ("radarr.example.com", "", "public", "subdomain"));
        assert_eq!(got(2), ("wranglarr.example.com", "/sab", "lan", "path"), "under wranglarr's name, local only");
        assert_eq!(got(3).2, "off", "direct: not proxied");
        assert_eq!(got(4), ("seerr.example.com", "", "public", "subdomain"));
        assert_eq!(p[4].note, "seerr doesn't run under a path: it gets its own name");
        // wranglarr at the domain itself: the paths go there
        s.apps.insert("wranglarr".into(), at("domain", false));
        let p = plan(&entries, &s, "", &BTreeMap::new(), &env);
        assert_eq!(
            (p[0].fqdn.as_str(), p[2].fqdn.as_str(), p[2].path.as_str()),
            ("example.com", "example.com", "/sab")
        );
        // wranglarr not proxied: nothing to put a path under
        s.apps.insert("wranglarr".into(), at("direct", false));
        let p = plan(&entries, &s, "", &ports, &env);
        assert_eq!((p[2].fqdn.as_str(), p[2].path.as_str(), p[2].at.as_str()), ("sab.example.com", "", "subdomain"));
        assert!(p[2].note.starts_with("wranglarr isn't proxied"), "{}", p[2].note);
    }

    #[test]
    fn public_urls_only_when_wranglarr_came_through_the_proxy() {
        let cfg = |name: &str, kind: &str, url: &str| AppConfig {
            name: name.into(),
            kind: kind.into(),
            url: url.into(),
            ..Default::default()
        };
        let apps = [
            cfg("radarr", "radarr", "http://radarr:7878"),
            cfg("sab", "sabnzbd", "http://sabnzbd:8080"),
            cfg("seerr", "seerr", "http://seerr:5055"),
        ];
        let refs: Vec<&AppConfig> = apps.iter().collect();
        let mut s = ProxySettings { domain: "example.com".into(), ..Default::default() };
        s.apps.insert("sab".into(), super::super::AppExpose { at: "direct".into(), local: false, sub: String::new() });
        s.apps
            .insert("radarr".into(), super::super::AppExpose { at: String::new(), local: false, sub: "movies".into() });
        let u = public_urls(&s, &refs, "Wranglarr.Example.com").expect("through the proxy");
        assert_eq!(u.get("radarr").map(String::as_str), Some("https://movies.example.com/"));
        assert_eq!(u.get("seerr").map(String::as_str), Some("https://seerr.example.com/"), "unset: lan, proxied");
        assert!(!u.contains_key("sab"), "not proxied");
        assert_eq!(public_urls(&s, &refs, "example.com.").map(|u| u.len()), Some(2), "the domain itself counts");
        for other in ["192.168.1.10", "notexample.com", "example.com.evil.net", ""] {
            assert!(public_urls(&s, &refs, other).is_none(), "{other}");
        }
        // a path each; Seerr still gets a name of its own
        s.style = "path".into();
        let u = public_urls(&s, &refs, "wranglarr.example.com").expect("through the proxy");
        assert_eq!(u.get("radarr").map(String::as_str), Some("https://wranglarr.example.com/movies/"));
        assert_eq!(u.get("seerr").map(String::as_str), Some("https://seerr.example.com/"));
        // no domain on the Reverse proxy page: nothing to go on
        let blank = ProxySettings::default();
        assert!(public_urls(&blank, &refs, "wranglarr.example.com").is_none());
    }
}
