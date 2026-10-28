// SPDX-License-Identifier: GPL-3.0-or-later
//! VPN status from gluetun's control server
//! (<https://github.com/qdm12/gluetun-wiki>), and what goes around the shared
//! tunnel when only some apps are in it.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::{AppConfig, VpnConfig};
use crate::service::AppConfigExt;
use crate::service::Roles;
use crate::value::{head, truthy};

const TTL: Duration = Duration::from_secs(30);

/// The host name in a URL, lowercased ("" when there is none).
pub fn url_host(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_lowercase)).unwrap_or_default()
}

pub struct Vpn {
    pub cfg: Option<VpnConfig>,
    cache: Mutex<Option<(Instant, Value)>>,
}

impl Vpn {
    pub fn new(cfg: Option<VpnConfig>) -> Vpn {
        Vpn { cfg, cache: Mutex::new(None) }
    }

    pub async fn status(&self, refresh: bool) -> Value {
        let Some(cfg) = &self.cfg else { return json!({"configured": false}) };
        if !refresh
            && let Some((at, v)) = self.cache.lock().ok().and_then(|c| c.clone())
            && at.elapsed() < TTL
        {
            return v;
        }
        let mut out = json!({"configured": true, "name": cfg.name, "reachable": false, "status": null,
                             "public_ip": null, "forwarded_port": null, "error": null});
        let base = cfg.control_url.trim_end_matches('/').to_string();
        let http = crate::http::http(Duration::from_secs(6));
        let get = |path: &str| {
            let mut r = http.get(format!("{base}{path}"));
            if !cfg.api_key.is_empty() {
                r = r.header("X-API-Key", &cfg.api_key);
            }
            r.send()
        };
        match get("/v1/vpn/status").await {
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
                out["error"] = json!("control server rejected the API key");
            }
            Ok(r) if !r.status().is_success() => {
                out["error"] = json!(format!(
                    "{} from {}/v1/vpn/status",
                    r.status(),
                    crate::http::safe_url(&base).trim_end_matches('/')
                ));
            }
            Ok(r) => match crate::http::response_json(r).await {
                Ok(v) => {
                    out["reachable"] = json!(true);
                    out["status"] = v.get("status").cloned().unwrap_or(Value::Null);
                    if let Ok(r) = get("/v1/publicip/ip").await
                        && let Ok(v) = crate::http::response_json(r).await
                    {
                        out["public_ip"] = truthy_or_null(v.get("public_ip"));
                    }
                    if let Ok(r) = get("/v1/portforward").await
                        && r.status() == reqwest::StatusCode::OK
                        && let Ok(v) = crate::http::response_json(r).await
                    {
                        out["forwarded_port"] = truthy_or_null(v.get("port"));
                    }
                }
                Err(e) => out["error"] = json!(head(&e.to_string(), 200)),
            },
            Err(e) => out["error"] = json!(head(&e.without_url().to_string(), 200)),
        }
        out["ok"] = json!(out["reachable"] == true && out["status"] == "running");
        if let Ok(mut c) = self.cache.lock() {
            *c = Some((Instant::now(), out.clone()));
        }
        out
    }
}

fn truthy_or_null(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Value::Null,
        Some(Value::String(s)) if s.is_empty() => Value::Null,
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => Value::Null,
        Some(v) => v.clone(),
    }
}

/// Is this app in the shared tunnel? Matched by app name or the host of its
/// URL (the compose service name, which is what WRANGLARR_VPN_APPS lists).
pub fn in_tunnel(vpn: Option<&VpnConfig>, app: &AppConfig) -> bool {
    match vpn {
        None => false,
        Some(VpnConfig { apps: None, .. }) => true,
        Some(VpnConfig { apps: Some(list), .. }) => list.contains(&app.name) || list.contains(&url_host(&app.url)),
    }
}

fn indexer_url(item: &Value) -> String {
    for f in item["fields"].as_array().into_iter().flatten() {
        if matches!(f["name"].as_str(), Some("baseUrl" | "feedUrl" | "apiUrl")) && truthy(&f["value"]) {
            return match &f["value"] {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
        }
    }
    String::new()
}

fn enabled(item: &Value) -> bool {
    ["enable", "enableRss", "enableAutomaticSearch", "enableInteractiveSearch"].iter().any(|k| truthy(&item[k]))
}

/// Which side of a partial tunnel `app` is on, when [`bypass`] has anything to
/// check for it: then the caller fetches the app's indexers.
pub fn bypass_side(vpn: Option<&VpnConfig>, app: &AppConfig, apps: &[&AppConfig]) -> Option<bool> {
    let v = vpn?;
    v.apps.as_ref()?;
    let prowlarrs: Vec<&&AppConfig> = apps.iter().filter(|a| a.has(Roles::INDEXERS) && in_tunnel(vpn, a)).collect();
    let outside: Vec<&&AppConfig> = apps.iter().filter(|a| a.has(Roles::LIBRARY) && !in_tunnel(vpn, a)).collect();
    if prowlarrs.is_empty() || outside.is_empty() {
        return None;
    }
    if outside.iter().any(|a| a.name == app.name) {
        Some(false)
    } else if prowlarrs.iter().any(|a| a.name == app.name) {
        Some(true)
    } else {
        None
    }
}

/// A Prowlarr indexer whose grabs go to the indexer itself because Redirect is
/// ticked, so turning it off helps. Prowlarr always redirects Usenet grabs (its
/// download endpoint forces it, and refuses Redirect off for them), and ignores
/// the box for torrent indexers that can't redirect (the box is greyed out).
fn redirects_by_choice(ix: &Value) -> bool {
    truthy(&ix["enable"])
        && truthy(&ix["redirect"])
        && ix["protocol"].as_str() != Some("usenet")
        && ix.get("supportsRedirect").is_none_or(truthy)
}

/// Health lines (the arr health shape) for indexer traffic that skips the
/// tunnel. Only when the tunnel is partial on purpose: Prowlarr in it, an arr
/// outside. The arr then reaches its indexers only through Prowlarr, unless
/// it has indexers of its own or a Prowlarr indexer redirects grabs to the
/// indexer itself. `indexers`: the app's `/indexer` list.
pub fn bypass(vpn: Option<&VpnConfig>, app: &AppConfig, apps: &[&AppConfig], indexers: &[Value]) -> Vec<Value> {
    let Some(inside) = bypass_side(vpn, app, apps) else { return vec![] };
    let Some(v) = vpn else { return vec![] };
    let mut inside_hosts: HashSet<String> =
        apps.iter().filter(|a| in_tunnel(vpn, a)).map(|a| url_host(&a.url)).collect();
    inside_hosts.extend(v.apps.iter().flatten().cloned());
    let name = |ix: &Value| ix["name"].as_str().map(str::to_string).unwrap_or_else(|| "unnamed".into());
    let mut out = vec![];
    for ix in indexers {
        if !inside {
            if !enabled(ix) {
                continue;
            }
            let host = url_host(&indexer_url(ix));
            if !inside_hosts.contains(&host) {
                let at = if host.is_empty() { "no address".to_string() } else { host };
                out.push(json!({"type": "warning", "source": "wranglarr-vpn", "message": format!(
                    "VPN: {} reaches the indexer '{}' ({at}) itself, outside the tunnel. Add it in Prowlarr instead, or put the arrs in the tunnel too.",
                    app.name, name(ix))}));
            }
        } else if redirects_by_choice(ix) {
            out.push(json!({"type": "warning", "source": "wranglarr-vpn", "message": format!(
                "VPN: Prowlarr's '{}' has Redirect on, so the arrs outside the tunnel fetch its .torrent themselves. Turn Redirect off for it in Prowlarr.",
                name(ix))}));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, kind: &str, url: &str) -> AppConfig {
        let text = format!(
            "[[suite]]\nname = \"s\"\n[[suite.app]]\nname = \"{name}\"\nkind = \"{kind}\"\nurl = \"{url}\"\napi_key = \"k\"\n"
        );
        crate::service::apps::parse(&text, |_| None).expect("cfg").suites[0].apps[0].clone()
    }

    fn vpn(apps: Option<Vec<&str>>) -> VpnConfig {
        VpnConfig {
            control_url: "http://vpn:8000".into(),
            api_key: String::new(),
            name: "vpn".into(),
            container: "arrs-vpn".into(),
            apps: apps.map(|a| a.into_iter().map(String::from).collect()),
        }
    }

    #[test]
    fn tunnel_membership() {
        let r = app("radarr", "radarr", "http://radarr:7878");
        assert!(!in_tunnel(None, &r));
        assert!(in_tunnel(Some(&vpn(None)), &r));
        assert!(in_tunnel(Some(&vpn(Some(vec!["radarr"]))), &r));
        assert!(!in_tunnel(Some(&vpn(Some(vec!["prowlarr"]))), &r));
    }

    #[test]
    fn partial_tunnel_warnings() {
        let p = app("prowlarr", "prowlarr", "http://prowlarr:9696");
        let r = app("radarr", "radarr", "http://radarr:7878");
        let v = vpn(Some(vec!["prowlarr", "qbittorrent"]));
        let all = [&p, &r];
        let own =
            json!({"name": "own", "enable": true, "fields": [{"name": "baseUrl", "value": "http://tracker.example"}]});
        let via =
            json!({"name": "via", "enable": true, "fields": [{"name": "baseUrl", "value": "http://prowlarr:9696/1/"}]});
        let w = bypass(Some(&v), &r, &all, &[own, via]);
        assert_eq!(w.len(), 1);
        assert!(w[0]["message"].as_str().unwrap_or("").contains("'own' (tracker.example)"));
        let red =
            json!({"name": "red", "enable": true, "redirect": true, "protocol": "torrent", "supportsRedirect": true});
        assert_eq!(bypass(Some(&v), &p, &all, &[red]).len(), 1);
        // Prowlarr redirects Usenet grabs whatever the box says, and ignores it where it's greyed out
        let nzb = json!({"name": "NZBgeek", "enable": true, "redirect": true, "protocol": "usenet", "supportsRedirect": true});
        let greyed =
            json!({"name": "grey", "enable": true, "redirect": true, "protocol": "torrent", "supportsRedirect": false});
        assert!(bypass(Some(&v), &p, &all, &[nzb, greyed]).is_empty());
        assert!(bypass(Some(&vpn(None)), &r, &all, &[]).is_empty());
    }
}
