// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy page's settings: what they hold, and checking a change to them.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{EXPOSE, KINDS};
use crate::error::{Error, Result};
use crate::links::HOST_RE;

/// The settings' keys, in the order they're checked.
const KEYS: &[&str] =
    &["kind", "style", "domain", "upstream", "upstream_host", "lan", "apps", "network", "entrypoint", "resolver"];
static LABEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$").expect("static regex"));
static NAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,64}$").expect("static regex"));
static SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s,]+").expect("static regex"));

/// How one app is proxied.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppExpose {
    /// public | lan | off
    pub expose: String,
    /// Its name (subdomain or path); blank: the app's name.
    pub sub: String,
}

/// The settings (state key `proxy:settings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub kind: String,
    /// subdomain | path
    pub style: String,
    pub domain: String,
    /// host | docker
    pub upstream: String,
    pub upstream_host: String,
    /// LAN ranges, space separated; blank: the private ranges.
    pub lan: String,
    pub apps: BTreeMap<String, AppExpose>,
    pub network: String,
    pub entrypoint: String,
    pub resolver: String,
}

impl Default for ProxySettings {
    fn default() -> Self {
        ProxySettings {
            kind: "caddy".into(),
            style: "subdomain".into(),
            domain: String::new(),
            upstream: "host".into(),
            upstream_host: String::new(),
            lan: String::new(),
            apps: BTreeMap::new(),
            network: "proxy".into(),
            entrypoint: "websecure".into(),
            resolver: "letsencrypt".into(),
        }
    }
}

/// A field as text, "" when it isn't set (null, false, 0, empty).
fn or_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(v) if !crate::value::truthy(v) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// Merge `body` into `current` and check it; a bad value is a 400 with the reason.
/// `names`: wranglarr and every app (other names are dropped).
pub fn check(current: &ProxySettings, body: &Map<String, Value>, names: &[String]) -> Result<ProxySettings> {
    let bad = |m: String| Error::invalid(m);
    // what isn't sent keeps its saved value
    let mut s = match serde_json::to_value(current)? {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    for (k, v) in body {
        if KEYS.contains(&k.as_str()) && !v.is_null() {
            s.insert(k.clone(), v.clone());
        }
    }
    for k in KEYS {
        let ok = if *k == "apps" { s[*k].is_object() } else { s[*k].is_string() };
        if !ok {
            return Err(bad(format!("{k}: {}", if *k == "apps" { "an object" } else { "text" })));
        }
    }
    let text = |k: &str| s[k].as_str().unwrap_or("").to_string();
    let kind = text("kind");
    if !KINDS.iter().any(|(k, _)| *k == kind) {
        return Err(bad("pick a proxy".into()));
    }
    let style = text("style");
    if style != "subdomain" && style != "path" {
        return Err(bad("style: subdomain or path".into()));
    }
    let domain = text("domain").trim().to_lowercase().trim_end_matches('.').to_string();
    if !domain.is_empty() && !HOST_RE.is_match(&domain) {
        return Err(bad(format!("domain {domain:?}: a name like example.com, no scheme, port or path")));
    }
    let upstream = text("upstream");
    if upstream != "host" && upstream != "docker" {
        return Err(bad("upstream: host or docker".into()));
    }
    let upstream_host = text("upstream_host").trim().to_string();
    if !upstream_host.is_empty() && !(HOST_RE.is_match(&upstream_host) || upstream_host.parse::<IpAddr>().is_ok()) {
        return Err(bad(format!("proxy reaches the apps at {upstream_host:?}: an IP address or host name")));
    }
    let lan = text("lan");
    let ranges: Vec<&str> = SPLIT_RE.split(&lan).filter(|r| !r.is_empty()).collect();
    for r in &ranges {
        if !wranglarr_core::net::is_network(r) {
            return Err(bad(format!("LAN range {r:?}: like 192.168.1.0/24")));
        }
    }
    let defaults = ProxySettings::default();
    let named = |k: &str, d: &str| -> Result<String> {
        let v = text(k);
        let v = if v.is_empty() { d.to_string() } else { v }.trim().to_string();
        if !NAME_RE.is_match(&v) {
            return Err(bad(format!("{k}: letters, digits and . _ -")));
        }
        Ok(v)
    };
    let network = named("network", &defaults.network)?;
    let entrypoint = named("entrypoint", &defaults.entrypoint)?;
    let resolver = named("resolver", &defaults.resolver)?;
    let mut apps = BTreeMap::new();
    if let Some(m) = s["apps"].as_object() {
        for (n, v) in m {
            let Some(v) = v.as_object() else { continue };
            if !names.contains(n) {
                continue;
            }
            let ex = match v.get("expose") {
                None => "lan".to_string(),
                Some(Value::String(e)) => e.clone(),
                Some(_) => String::new(),
            };
            let sub = or_text(v.get("sub")).trim().to_lowercase();
            if !EXPOSE.contains(&ex.as_str()) {
                return Err(bad(format!("{n}: public, lan or off")));
            }
            if !sub.is_empty() && !LABEL_RE.is_match(&sub) {
                return Err(bad(format!("{n}: the name {sub:?} is one DNS label (letters, digits, -)")));
            }
            apps.insert(n.clone(), AppExpose { expose: ex, sub });
        }
    }
    Ok(ProxySettings {
        kind,
        style,
        domain,
        upstream,
        upstream_host,
        lan: ranges.join(" "),
        apps,
        network,
        entrypoint,
        resolver,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checks_settings() {
        let names = vec!["wranglarr".to_string(), "radarr".into(), "qbittorrent".into()];
        let cur = ProxySettings::default();
        let body = |v: Value| v.as_object().cloned().unwrap_or_default();
        let s = check(
            &cur,
            &body(json!({"domain": " Example.COM. ", "lan": "192.168.1.0/24, 10.0.0.0/8", "network": "",
                         "apps": {"qbittorrent": {"expose": "lan", "sub": " QB "}, "nope": {"expose": "public"},
                                  "radarr": "x"}})),
            &names,
        )
        .expect("good");
        assert_eq!(s.domain, "example.com");
        assert_eq!(s.lan, "192.168.1.0/24 10.0.0.0/8");
        assert_eq!(s.network, "proxy", "blank: the default");
        assert_eq!(s.apps.len(), 1);
        assert_eq!(s.apps["qbittorrent"], AppExpose { expose: "lan".into(), sub: "qb".into() });
        let cases = [
            (json!({"kind": "apache"}), "pick a proxy"),
            (json!({"kind": 3}), "kind: text"),
            (json!({"apps": []}), "apps: an object"),
            (json!({"style": "sideways"}), "style: subdomain or path"),
            (
                json!({"domain": "https://example.com/x"}),
                r#"domain "https://example.com/x": a name like example.com, no scheme, port or path"#,
            ),
            (json!({"upstream": "moon"}), "upstream: host or docker"),
            (json!({"upstream_host": "a b"}), r#"proxy reaches the apps at "a b": an IP address or host name"#),
            (json!({"lan": "not-a-range"}), r#"LAN range "not-a-range": like 192.168.1.0/24"#),
            (json!({"entrypoint": "a b"}), "entrypoint: letters, digits and . _ -"),
            (json!({"apps": {"radarr": {"expose": "everyone"}}}), "radarr: public, lan or off"),
            (
                json!({"apps": {"radarr": {"sub": "a.b"}}}),
                r#"radarr: the name "a.b" is one DNS label (letters, digits, -)"#,
            ),
        ];
        for (b, msg) in cases {
            let e = check(&cur, &body(b.clone()), &names).expect_err(&b.to_string());
            assert!(matches!(e, Error::Invalid(_)));
            assert_eq!(e.to_string(), msg, "{b}");
        }
        // null keeps the saved value
        let s = check(&s, &body(json!({"domain": null, "kind": "npm"})), &names).expect("null");
        assert_eq!((s.domain.as_str(), s.kind.as_str()), ("example.com", "npm"));
    }
}
