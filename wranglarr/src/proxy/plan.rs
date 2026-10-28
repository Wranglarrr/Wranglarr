// SPDX-License-Identifier: GPL-3.0-or-later
//! What each app becomes in the proxy: its name or path, where the proxy reaches it,
//! and whether it goes through wranglarr-gate.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{ProxySettings, own_name, ungated, url_base};
use crate::config::AppConfig;
use crate::links::{GATE, Published, parse_url, url_host};
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
    pub expose: String,
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
        }
    }
}

/// What each entry becomes, for these settings. `browser_host` (the host the page was
/// opened at) is where the proxy reaches published ports when the settings name no host.
pub fn plan(
    entries: &[Entry],
    s: &ProxySettings,
    browser_host: &str,
    ports: &BTreeMap<String, Published>,
    env: &ProxyEnv,
) -> Vec<Plan> {
    let tunnel = env.apps_auth == "tunnel";
    let host = [s.upstream_host.as_str(), browser_host].into_iter().find(|h| !h.is_empty()).unwrap_or("127.0.0.1");
    let domain = if s.domain.is_empty() { "example.com" } else { &s.domain };
    let mut out = Vec::new();
    for e in entries {
        let cfg = s.apps.get(&e.name).cloned().unwrap_or_default();
        let expose = if cfg.expose.is_empty() { "lan".to_string() } else { cfg.expose };
        let sub = if cfg.sub.is_empty() { e.name.clone() } else { cfg.sub };
        let subdomain = s.style == "subdomain";
        let mut p = Plan {
            name: e.name.clone(),
            kind: e.kind.clone(),
            inner: e.inner,
            expose,
            fqdn: if subdomain { format!("{sub}.{domain}") } else { domain.to_string() },
            path: if subdomain || e.kind == "wranglarr" { String::new() } else { format!("/{sub}") },
            sub,
            note: String::new(),
            strip: false,
            sab: service::get(&e.kind).is_some_and(|d| d.proxy.host_check),
            host_up: String::new(),
            docker_up: String::new(),
            target: String::new(),
            up: String::new(),
        };
        if s.style == "path" && own_name(&e.kind) {
            p.path = String::new();
            p.fqdn = format!("{}.{domain}", p.sub); // wants a name of its own
            p.note = format!("{} doesn't run under a path: it gets its own name", e.name);
        }
        p.strip = !p.path.is_empty() && !url_base(&e.kind);
        // where the proxy sends it
        if e.kind == "wranglarr" {
            p.host_up = format!("{host}:{}", env.wranglarr_port);
            p.docker_up = "wranglarr:8787".into();
            p.target = "wranglarr".into();
        } else {
            let pubd = ports.get(&e.name).cloned().unwrap_or_default();
            if let Some(n) = pubd.port.filter(|n| *n != 0) {
                p.host_up = format!("{host}:{n}");
            }
            if pubd.loopback && !["127.0.0.1", "localhost", "::1"].contains(&host) {
                p.note = "published on 127.0.0.1 only: the proxy has to run on this machine (use 127.0.0.1)".into();
            }
            if tunnel && !ungated(&e.kind) {
                p.docker_up = format!("{GATE}:{}", e.inner);
                p.target = GATE.into();
            } else {
                p.docker_up = format!("{}:{}", e.service, e.inner);
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

/// Whether `host` is the proxy's domain or a name under it.
pub fn under_domain(s: &ProxySettings, host: &str) -> bool {
    let domain = s.domain.trim().trim_end_matches('.').to_lowercase();
    let host = host.trim().trim_end_matches('.').to_lowercase();
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// app -> its address through the proxy (`https://radarr.example.com/`), for a browser
/// that opened wranglarr at `browser_host`, for each proxied app. None unless that's the
/// proxy's domain or a name under it (so wranglarr itself came through the proxy): by IP
/// and port, the apps open by IP and port too.
pub fn public_urls(s: &ProxySettings, apps: &[&AppConfig], browser_host: &str) -> Option<BTreeMap<String, String>> {
    if !under_domain(s, browser_host) {
        return None;
    }
    let entries: Vec<Entry> = apps.iter().map(|a| Entry::for_app(a)).collect();
    plan(&entries, s, "", &BTreeMap::new(), &ProxyEnv::default())
        .into_iter()
        .filter(|p| p.expose != "off")
        .map(|p| (p.name, format!("https://{}{}/", p.fqdn, p.path)))
        .collect::<BTreeMap<_, _>>()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: &str, inner: u16) -> Entry {
        Entry { name: name.into(), kind: kind.into(), inner, service: name.into(), compose_service: name.into() }
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
        assert_eq!((p[1].fqdn.as_str(), p[1].path.as_str(), p[1].strip), ("home.lan", "/radarr", false));
        assert_eq!(p[1].note, "published on 127.0.0.1 only: the proxy has to run on this machine (use 127.0.0.1)");
        assert_eq!((p[1].docker_up.as_str(), p[1].target.as_str()), ("wranglarr-gate:7878", "wranglarr-gate"));
        assert_eq!((p[2].fqdn.as_str(), p[2].target.as_str()), ("seerr.home.lan", "seerr"), "seerr: never gated");
        assert_eq!(p[2].note, "seerr doesn't run under a path: it gets its own name");
        assert!(p[3].strip && p[3].up.is_empty());
        assert_eq!(p[3].note, "not published on this machine: use “the proxy is a container on the stack's network”");
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
        s.apps.insert("sab".into(), super::super::AppExpose { expose: "off".into(), sub: String::new() });
        s.apps.insert("radarr".into(), super::super::AppExpose { expose: "public".into(), sub: "movies".into() });
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
        let u = public_urls(&s, &refs, "example.com").expect("through the proxy");
        assert_eq!(u.get("radarr").map(String::as_str), Some("https://example.com/movies/"));
        assert_eq!(u.get("seerr").map(String::as_str), Some("https://seerr.example.com/"));
        // no domain on the Reverse proxy page: nothing to go on
        let blank = ProxySettings::default();
        assert!(public_urls(&blank, &refs, "wranglarr.example.com").is_none());
    }
}
