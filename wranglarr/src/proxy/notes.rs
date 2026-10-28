// SPDX-License-Identifier: GPL-3.0-or-later
//! The notes shown under the config: what to check or change for it to work.

use serde::Serialize;

use super::{Plan, ProxyEnv, ProxySettings, url_base_where};
use crate::links::{parse_url, url_host};
use crate::service;

/// A note under the config.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Note {
    /// "warn" or ""
    pub cls: &'static str,
    pub msg: String,
}

/// The notes for a plan: sign-in, certificates, networks and the apps' own settings.
pub(super) fn notes(plan: &[Plan], s: &ProxySettings, env: &ProxyEnv) -> Vec<Note> {
    let mode = env.apps_auth.as_str();
    let cd = env.cookie_domain.as_str();
    let lu = env.login_url.as_str();
    let on: Vec<&Plan> = plan.iter().filter(|p| p.expose != "off").collect();
    let any_public = on.iter().any(|p| p.expose == "public");
    let any_lan = on.iter().any(|p| p.expose == "lan");
    let mut n = Vec::new();
    // true: a warning; false: for information
    let mut push = |warn: bool, msg: String| n.push(Note { cls: if warn { "warn" } else { "" }, msg });
    if s.domain.is_empty() {
        push(true, "Fill in your domain: the output uses example.com until you do.".into());
    }
    let w = plan.iter().find(|p| p.kind == "wranglarr");
    if mode == "tunnel" && !on.is_empty() {
        if w.is_some_and(|w| w.expose == "off") {
            push(
                true,
                "AuthTunnel sends you to wranglarr's login page: proxy wranglarr too, at least as LAN only.".into(),
            );
        }
        if !s.domain.is_empty() && cd != s.domain && !(!cd.is_empty() && s.domain.ends_with(&format!(".{cd}"))) {
            let fqdn = w.map_or_else(|| format!("wranglarr.{}", s.domain), |w| w.fqdn.clone());
            push(
                true,
                format!(
                    "AuthTunnel: the sign-in cookie has to reach these names. Set the cookie domain to {} and \
                     wranglarr's address to https://{fqdn} with wranglarr-setup --auth, then ./scripts/init.sh \
                     --auth (Home → Logins shows how).",
                    s.domain
                ),
            );
        } else if let Some(w) = w.filter(|_| !lu.is_empty())
            && parse_url(lu).as_ref().and_then(url_host).as_deref() != Some(w.fqdn.as_str())
        {
            push(true, format!("wranglarr's address for the login page is {lu}, but here wranglarr is at {}.", w.fqdn));
        }
        push(
            false,
            "Requests that carry the app's API key go straight through the gate, so the arrs' mobile apps and \
             Prowlarr's links keep working; the app checks the key."
                .into(),
        );
    } else if mode == "manual" && any_public {
        push(
            true,
            "APPS_AUTH is manual: make sure every public app has a login of its own (Home → Logins shows which \
             have none), or use AuthTunnel."
                .into(),
        );
    } else if mode == "apps" && any_public && env.required.as_deref() == Some("disabledForLocalAddresses") {
        push(
            true,
            "The arrs skip their login for local addresses, and a request through your proxy can look local: set \
             it to ask everywhere (wranglarr-setup --auth)."
                .into(),
        );
    }
    if any_lan {
        push(
            false,
            "LAN only goes by the visitor's address as the proxy sees it. Behind another proxy or a CDN that's the \
             CDN's address: allow it only if your proxy trusts and reads X-Forwarded-For."
                .into(),
        );
    }
    if s.kind == "cloudflared" && any_lan {
        push(
            true,
            "A Cloudflare Tunnel is public by design: the LAN-only apps are left out. Put Cloudflare Access in \
             front instead if you want them through it."
                .into(),
        );
    }
    if s.style == "path" {
        let ub: Vec<String> = on
            .iter()
            .filter(|p| !p.path.is_empty() && !p.strip)
            .map(|p| format!("{} {} ({})", p.name, p.path, url_base_where(&p.kind)))
            .collect();
        if !ub.is_empty() {
            push(
                false,
                format!(
                    "Under a path, these apps need their URL Base set to the same path, and wranglarr.toml's url for \
                     them must end in it too (e.g. http://radarr:7878/radarr): {}",
                    ub.join(", ")
                ),
            );
        }
    }
    if on.iter().any(|p| p.sab) {
        push(
            false,
            "SABnzbd refuses host names it doesn't know; the config sends it its own address instead of yours.".into(),
        );
    }
    // each kind's warning once, in the registry's order
    for d in service::ALL {
        if let Some(note) = d.proxy.public_note
            && on.iter().any(|p| p.kind == d.id && p.expose == "public")
        {
            push(true, note.into());
        }
    }
    if on.iter().any(|p| p.kind == "wranglarr") {
        push(
            false,
            "Passkeys belong to the name you added them at: one added at an IP address won't work at the new domain. \
             Add another under Account once you're signed in there."
                .into(),
        );
    }
    if s.kind == "traefik-labels" || s.kind == "caddy-labels" {
        push(
            false,
            format!(
                "Save it next to compose.yaml as compose.proxy.yaml and add it to COMPOSE_FILE in .env \
                 (…:compose.proxy.yaml), then docker compose up -d. The proxy container must be on the {:?} network.",
                s.network
            ),
        );
    }
    if s.upstream == "docker" && ["caddy", "nginx", "traefik-file", "cloudflared"].contains(&s.kind.as_str()) {
        push(
            false,
            "The proxy container has to join the stack's network: docker network connect arrs <proxy container>."
                .into(),
        );
    }
    n
}
