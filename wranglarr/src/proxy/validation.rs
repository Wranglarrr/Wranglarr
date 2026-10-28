// SPDX-License-Identifier: GPL-3.0-or-later
//! Validate the resolved routing table, independently of Docker and DNS.

use std::collections::BTreeMap;

use super::{Entry, Plan, ProxyEnv, ProxySettings, own_name, plan};
use crate::error::{Error, Result};

use crate::url_base::ROOT_NAMESPACES as WRANGLARR_PATHS;

/// Capabilities of the generated configuration, rather than the proxy software in general.
struct Capabilities {
    local: bool,
    strip: bool,
    mixed_access: bool,
}

fn capabilities(kind: &str) -> Capabilities {
    Capabilities { local: kind != "cloudflared", strip: kind != "cloudflared", mixed_access: kind != "npm" }
}

/// Check placement using exactly the same resolved routes as links and renderers.
pub fn validate(entries: &[Entry], s: &ProxySettings) -> Result<()> {
    validate_plan(&plan(entries, s, "", &BTreeMap::new(), &ProxyEnv::default()), s)
}

pub(super) fn validate_plan(plan: &[Plan], s: &ProxySettings) -> Result<()> {
    let caps = capabilities(&s.kind);
    let on: Vec<_> = plan.iter().filter(|p| p.expose != "off").collect();
    for (i, p) in on.iter().enumerate() {
        if s.apps.get(&p.name).is_some_and(|a| a.at == "path") {
            if p.kind != "wranglarr" && own_name(&p.kind) {
                return Err(Error::invalid(format!(
                    "{} needs a subdomain of its own; it cannot use a wranglarr path",
                    p.name
                )));
            }
            if p.kind != "wranglarr" && s.apps.get("wranglarr").is_some_and(|a| a.at == "direct") {
                return Err(Error::invalid(format!("{}: proxy wranglarr before putting apps under its path", p.name)));
            }
        }
        if !p.path.is_empty() && WRANGLARR_PATHS.contains(&p.path.trim_start_matches('/')) {
            return Err(Error::invalid(format!("{}: path {} is reserved for wranglarr", p.name, p.path)));
        }
        if !caps.local && p.local {
            return Err(Error::invalid(format!(
                "{} cannot enforce local only for {}; use Direct IP or another proxy",
                s.kind, p.name
            )));
        }
        if !caps.strip && p.strip {
            return Err(Error::invalid(format!("{} cannot strip {} for {}; use a subdomain", s.kind, p.path, p.name)));
        }
        for other in &on[..i] {
            if !other.fqdn.eq_ignore_ascii_case(&p.fqdn) {
                continue;
            }
            if other.path == p.path {
                return Err(Error::invalid(format!(
                    "{} and {} both use https://{}{}/; choose distinct names or paths",
                    other.name, p.name, p.fqdn, p.path
                )));
            }
            if !caps.mixed_access && other.local != p.local {
                return Err(Error::invalid(format!(
                    "{} uses one access list per host: {} and {} at {} must have the same local-only setting, or use separate subdomains",
                    s.kind, other.name, p.name, p.fqdn
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::AppExpose;

    fn settings(kind: &str, root_local: bool, app_local: bool) -> ProxySettings {
        let app = |at: &str, local| AppExpose { at: at.into(), local, sub: String::new() };
        ProxySettings {
            kind: kind.into(),
            domain: "example.com".into(),
            apps: [("wranglarr".into(), app("subdomain", root_local)), ("radarr".into(), app("path", app_local))]
                .into(),
            ..Default::default()
        }
    }

    fn entries() -> Vec<Entry> {
        vec![
            Entry::wranglarr(),
            Entry {
                name: "radarr".into(),
                kind: "radarr".into(),
                inner: 7878,
                service: "radarr".into(),
                compose_service: "radarr".into(),
            },
        ]
    }

    #[test]
    fn routes_and_proxy_capabilities() {
        for kind in ["caddy", "nginx", "traefik-file", "traefik-labels", "caddy-labels"] {
            validate(&entries(), &settings(kind, false, true)).expect("per-route access");
        }
        for local in [true, false] {
            validate(&entries(), &settings("npm", local, local)).expect("same access on a host");
            assert!(validate(&entries(), &settings("npm", local, !local)).is_err());
        }
        validate(&entries(), &settings("cloudflared", false, false)).expect("public path with URL base");
        assert!(validate(&entries(), &settings("cloudflared", false, true)).is_err());
        let mut s = settings("cloudflared", false, false);
        let mut e = entries();
        e[1].kind = "qbittorrent".into();
        assert!(validate(&e, &s).expect_err("needs strip").to_string().contains("cannot strip"));
        s.kind = "caddy".into();
        s.apps.get_mut("radarr").expect("app").sub = "api".into();
        assert!(validate(&entries(), &s).expect_err("reserved").to_string().contains("reserved"));
        s.apps.get_mut("radarr").expect("app").at = "subdomain".into();
        s.apps.get_mut("radarr").expect("app").sub = "wranglarr".into();
        assert!(validate(&entries(), &s).expect_err("duplicate").to_string().contains("both use"));
        s.apps.get_mut("radarr").expect("app").at = "path".into();
        s.apps.get_mut("wranglarr").expect("app").at = "direct".into();
        assert!(validate(&entries(), &s).is_err());
        s.apps.get_mut("wranglarr").expect("app").at = "subdomain".into();
        e[1].kind = "seerr".into();
        assert!(validate(&e, &s).expect_err("own name").to_string().contains("subdomain"));
    }

    #[test]
    fn every_web_route_has_a_reserved_namespace() {
        let routes = [
            include_str!("../web/mod.rs"),
            include_str!("../web/auth.rs"),
            include_str!("../web/configs.rs"),
            include_str!("../web/connect.rs"),
            include_str!("../web/connections.rs"),
            include_str!("../web/events.rs"),
            include_str!("../web/home.rs"),
            include_str!("../web/keys.rs"),
            include_str!("../web/notify.rs"),
            include_str!("../web/proxy.rs"),
            include_str!("../web/suites.rs"),
            include_str!("../web/system.rs"),
            include_str!("../web/updates.rs"),
        ];
        for source in routes {
            // Ignore the test-only routers in web/mod.rs.
            for route in source.split("#[cfg(test)]").next().expect("source").split(".route(\"").skip(1) {
                let prefix =
                    route.split('"').next().expect("route").trim_start_matches('/').split('/').next().expect("prefix");
                assert!(
                    prefix.is_empty() || WRANGLARR_PATHS.contains(&prefix),
                    "reserve /{prefix} before adding this route"
                );
            }
        }
    }
}
