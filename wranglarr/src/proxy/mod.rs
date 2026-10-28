// SPDX-License-Identifier: GPL-3.0-or-later
//! Reverse proxy scaffolds: config for Caddy, nginx, Traefik, caddy-docker-proxy, Cloudflare
//! Tunnel or Nginx Proxy Manager that forwards this stack, as it is set up now.
//!
//! Each app (and wranglarr) is one of:
//!   public  reachable from anywhere, at its name or path
//!   lan     proxied, but only for addresses on your network (an IP allowlist in the proxy)
//!   off     not proxied: only `http://<this machine>:<port>`, as now
//!
//! wranglarr only writes the text; you paste it into your proxy. With AuthTunnel the
//! targets are wranglarr-gate's ports, so the apps stay behind wranglarr's sign-in.

mod notes;
mod plan;
mod render;
mod service;
mod settings;

pub use notes::Note;
pub use plan::{Entry, Plan, ProxyEnv, plan, public_urls};
pub use render::{Rendered, render};
pub use service::Proxy;
pub use settings::{AppExpose, ProxySettings, check};

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
/// How an app can be proxied.
pub const EXPOSE: &[&str] = &["public", "lan", "off"];
/// wranglarr itself, proxied like an app.
const WRANGLARR: &str = "wranglarr";
/// Apps whose UI follows a URL Base setting (served under /name without stripping it).
fn url_base(kind: &str) -> bool {
    crate::service::get(kind).is_some_and(|d| d.proxy.url_base.is_some())
}
/// Can't run under a path: always a name of their own (wranglarr too).
fn own_name(kind: &str) -> bool {
    kind == WRANGLARR || crate::service::get(kind).is_some_and(|d| d.proxy.own_name)
}
/// Own sign-in (Plex / Jellyfin / Emby accounts): never behind the gate.
fn ungated(kind: &str) -> bool {
    crate::service::get(kind).is_some_and(|d| d.proxy.own_login)
}
/// Where the app's URL Base setting is, for the note that asks for it.
fn url_base_where(kind: &str) -> &'static str {
    crate::service::get(kind).and_then(|d| d.proxy.url_base).unwrap_or(crate::service::ARR_URL_BASE)
}
