// SPDX-License-Identifier: GPL-3.0-or-later
//! Reverse proxy scaffolds: config for Caddy, nginx, Traefik, caddy-docker-proxy, Cloudflare
//! Tunnel or Nginx Proxy Manager that forwards this stack, as it is set up now.
//!
//! Each app (and wranglarr) is at one of:
//!   subdomain  its own name, `https://radarr.example.com`
//!   path       under wranglarr's name, `https://wranglarr.example.com/radarr` (its
//!              certificate and sign-in cookie)
//!   direct     not proxied: only `http://<this machine>:<port>`
//!
//! Wranglarr can also use `path` on the domain itself, e.g. `/wranglarr`;
//! app paths are siblings (`/radarr`). The prefix is preserved upstream.
//!
//! Proxied entries are either public or local only (an IP allowlist of the LAN ranges in the
//! proxy). The renderers see that as `expose`: public, lan or off.
//!
//! wranglarr only writes the text; you paste it into your proxy. With AuthTunnel the
//! targets are wranglarr-gate's ports, so the apps stay behind wranglarr's sign-in.

mod notes;
mod plan;
mod render;
mod service;
mod settings;
mod validation;

pub use notes::Note;
pub use plan::{Entry, Plan, ProxyEnv, placement, plan, proxy_urls, public_urls, under_domain};
pub use render::{Rendered, render};
pub use service::Proxy;
pub use settings::{AT_APP, AT_WRANGLARR, AppExpose, ProxySettings, check};
pub use validation::validate;

/// Where the settings are kept in the state db.
pub const KEY: &str = "proxy:settings";
/// The proxies, in the order the page offers them.
pub const KINDS: &[(&str, &str)] = &[
    ("caddy", "Caddy (Caddyfile)"),
    ("nginx", "nginx"),
    ("traefik-file", "Traefik (file provider)"),
    ("traefik-labels", "Traefik (Docker labels)"),
    ("caddy-labels", "caddy-docker-proxy (Docker labels)"),
    ("cloudflared", "Cloudflare Tunnel (cloudflared)"),
    ("npm", "Nginx Proxy Manager"),
];
/// How an app could be proxied, in settings saved before `at` and `local`.
pub const EXPOSE: &[&str] = &["public", "lan", "off"];
/// wranglarr itself, proxied like an app.
const WRANGLARR: &str = "wranglarr";
/// Apps whose UI follows a URL Base setting (served under /name without stripping it).
fn url_base(kind: &str) -> bool {
    kind == WRANGLARR || crate::service::get(kind).is_some_and(|d| d.proxy.url_base.is_some())
}
/// Apps that cannot run under a path: always a hostname of their own.
pub(crate) fn own_name(kind: &str) -> bool {
    crate::service::get(kind).is_some_and(|d| d.proxy.own_name)
}
/// Own sign-in (Plex / Jellyfin / Emby accounts): never behind the gate.
pub(crate) fn ungated(kind: &str) -> bool {
    crate::service::get(kind).is_some_and(|d| d.proxy.own_login)
}
/// Where the app's URL Base setting is, for the note that asks for it.
fn url_base_where(kind: &str) -> &'static str {
    crate::service::get(kind).and_then(|d| d.proxy.url_base).unwrap_or(crate::service::ARR_URL_BASE)
}
