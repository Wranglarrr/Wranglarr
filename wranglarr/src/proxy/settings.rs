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
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppExpose {
    /// Where it is: `subdomain` (its own name), `path` (under wranglarr's name) or
    /// `direct` (not proxied: this machine's address and its port); wranglarr itself:
    /// `subdomain`, `domain` (the domain itself), `path` (on the domain) or `direct`. "": the default (`style`).
    pub at: String,
    /// Local only: through the proxy, but only from the LAN ranges.
    pub local: bool,
    /// Its name (subdomain or path); blank: the app's name.
    pub sub: String,
}

impl Default for AppExpose {
    /// Not set: where `style` puts it, local only.
    fn default() -> Self {
        AppExpose { at: String::new(), local: true, sub: String::new() }
    }
}

impl AppExpose {
    /// public | lan | off, as the config renders it.
    pub fn expose(&self) -> &'static str {
        match (self.at.as_str(), self.local) {
            ("direct", _) => "off",
            (_, true) => "lan",
            _ => "public",
        }
    }

    /// From `{at, local, sub}`, or the older `{expose: public | lan | off, sub}`.
    fn from_json(v: &Map<String, Value>) -> std::result::Result<AppExpose, String> {
        let expose = match v.get("expose") {
            None | Some(Value::Null) => None,
            Some(Value::String(e)) if EXPOSE.contains(&e.as_str()) => Some(e.as_str()),
            Some(_) => return Err("public, lan or off".into()),
        };
        let at = match v.get("at") {
            None | Some(Value::Null) => if expose == Some("off") { "direct" } else { "" }.to_string(),
            Some(Value::String(a)) => a.trim().to_lowercase(),
            Some(_) => return Err("at: subdomain, path or direct".into()),
        };
        let local = match v.get("local") {
            None | Some(Value::Null) => expose != Some("public"),
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("local: true or false".into()),
        };
        Ok(AppExpose { at, local, sub: or_text(v.get("sub")).trim().to_lowercase() })
    }
}

impl<'de> Deserialize<'de> for AppExpose {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let m = Map::<String, Value>::deserialize(d)?;
        AppExpose::from_json(&m).map_err(serde::de::Error::custom)
    }
}

/// Where wranglarr can be, and where an app can be.
pub const AT_WRANGLARR: &[&str] = &["subdomain", "domain", "path", "direct"];
pub const AT_APP: &[&str] = &["subdomain", "path", "direct"];

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
            let a = AppExpose::from_json(v).map_err(|e| bad(format!("{n}: {e}")))?;
            let ats = if n == "wranglarr" { AT_WRANGLARR } else { AT_APP };
            if !a.at.is_empty() && !ats.contains(&a.at.as_str()) {
                return Err(bad(format!("{n}: at {:?}: {}", a.at, ats.join(", "))));
            }
            if !a.sub.is_empty() && !LABEL_RE.is_match(&a.sub) {
                return Err(bad(format!("{n}: the name {:?} is one DNS label (letters, digits, -)", a.sub)));
            }
            apps.insert(n.clone(), a);
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
        assert_eq!(s.apps["qbittorrent"], AppExpose { at: String::new(), local: true, sub: "qb".into() });
        // the older {expose} still reads; {at, local} is the newer form
        let s2 = check(
            &cur,
            &body(json!({"apps": {"radarr": {"expose": "off"}, "qbittorrent": {"expose": "public"},
                                  "wranglarr": {"at": "domain", "local": false}}})),
            &names,
        )
        .expect("good");
        assert_eq!((s2.apps["radarr"].at.as_str(), s2.apps["radarr"].expose()), ("direct", "off"));
        assert_eq!((s2.apps["qbittorrent"].at.as_str(), s2.apps["qbittorrent"].expose()), ("", "public"));
        assert_eq!((s2.apps["wranglarr"].at.as_str(), s2.apps["wranglarr"].expose()), ("domain", "public"));
        let saved: ProxySettings =
            serde_json::from_value(json!({"apps": {"radarr": {"expose": "lan", "sub": "movies"}}})).expect("old");
        assert_eq!(saved.apps["radarr"], AppExpose { at: String::new(), local: true, sub: "movies".into() });
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
            (json!({"apps": {"radarr": {"at": "domain"}}}), r#"radarr: at "domain": subdomain, path, direct"#),
            (
                json!({"apps": {"wranglarr": {"at": "moon"}}}),
                r#"wranglarr: at "moon": subdomain, domain, path, direct"#,
            ),
            (json!({"apps": {"radarr": {"local": "yes"}}}), "radarr: local: true or false"),
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
