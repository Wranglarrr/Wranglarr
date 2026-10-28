// SPDX-License-Identifier: GPL-3.0-or-later
//! The config text for each kind of proxy, from the plan. Each renderer gets the
//! entries that are proxied, the settings and the LAN ranges, and returns the
//! text, the file name to save it as, and how to use it.

mod caddy;
mod cloudflared;
mod nginx;
mod npm;
mod traefik;

use serde::Serialize;

use super::notes::notes;
use super::{Note, Plan, ProxyEnv, ProxySettings};

/// The private ranges: the LAN when the settings name none.
const PRIVATE: &[&str] = &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "127.0.0.0/8", "fc00::/7", "::1/128"];

/// The rendered config.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rendered {
    pub text: String,
    pub filename: &'static str,
    pub how: &'static str,
    pub plan: Vec<Plan>,
    pub notes: Vec<Note>,
}

/// The config for a plan in the settings' kind of proxy, with its notes.
pub fn render(plan: Vec<Plan>, s: &ProxySettings, env: &ProxyEnv) -> Rendered {
    let on: Vec<&Plan> = plan.iter().filter(|p| p.expose != "off" && !p.up.is_empty()).collect();
    let lan: Vec<String> = if s.lan.is_empty() {
        PRIVATE.iter().map(|r| r.to_string()).collect()
    } else {
        s.lan.split_whitespace().map(str::to_string).collect()
    };
    let (text, filename, how) = match s.kind.as_str() {
        "nginx" => nginx::nginx(&on, s, &lan),
        "traefik-file" => traefik::file(&on, s, &lan),
        "traefik-labels" => traefik::labels(&on, s, &lan),
        "caddy-labels" => caddy::labels(&on, s, &lan),
        "cloudflared" => cloudflared::cloudflared(&on, s, &lan),
        "npm" => npm::npm(&on, s, &lan),
        _ => caddy::caddyfile(&on, s, &lan),
    };
    let notes = notes(&plan, s, env);
    Rendered { text, filename, how, plan, notes }
}

/// The http:// address of an upstream.
fn url(up: &str) -> String {
    format!("http://{up}")
}

/// fqdn -> entries, in first-seen order, the root one (no path) first.
fn sites<'a>(on: &[&'a Plan]) -> Vec<(String, Vec<&'a Plan>)> {
    let mut out: Vec<(String, Vec<&Plan>)> = Vec::new();
    for p in on {
        match out.iter_mut().find(|(f, _)| *f == p.fqdn) {
            Some((_, v)) => v.push(p),
            None => out.push((p.fqdn.clone(), vec![p])),
        }
    }
    for (_, v) in &mut out {
        v.sort_by(|a, b| (!a.path.is_empty(), &a.path).cmp(&(!b.path.is_empty(), &b.path)));
    }
    out
}

/// target -> entries, in first-seen order.
fn by_target<'a>(on: &[&'a Plan]) -> Vec<(String, Vec<&'a Plan>)> {
    let mut out: Vec<(String, Vec<&Plan>)> = Vec::new();
    for p in on {
        match out.iter_mut().find(|(t, _)| *t == p.target) {
            Some((_, v)) => v.push(p),
            None => out.push((p.target.clone(), vec![p])),
        }
    }
    out
}

/// The comment that says who can reach an entry.
fn tag(p: &Plan) -> &'static str {
    if p.expose == "public" { "public" } else { "LAN only" }
}

/// What a renderer returns: the text, the file name, how to use it.
type Out = (String, &'static str, &'static str);
