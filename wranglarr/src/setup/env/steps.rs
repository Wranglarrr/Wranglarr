// SPDX-License-Identifier: GPL-3.0-or-later
//! The next steps shown at the end of the wizard, and the HOST_IP and default-login helpers.

use std::path::Path;

use serde_json::Value;

use super::super::host::{self, Env};
use super::Answers;
use super::auth::auth_steps;
use crate::error::{Error, Result};
use crate::service::{self, Roles, ServiceDef};

pub fn next_steps(a: &Answers, env: &Env, target: &Path, scan: &Value, restore_count: usize) -> Vec<String> {
    let ad = a.appdata.trim_end_matches('/');
    let mut steps = vec!["Review ./.env (it holds secrets; mode 600).".to_string()];
    if a.puid == 0 || a.pgid == 0 {
        steps.push("PUID/PGID is 0: every app will run as root and create root-owned files on your media. Set PUID/PGID in .env to a normal user (id -u / id -g) before running init.sh.".into());
    }
    let v = &a.vpn;
    if v.mode == "shared" && v.provider == "custom" && v.vpn_type == "wireguard" && v.private_key.is_empty() {
        steps.push(format!("Put your WireGuard config at {ad}/vpn/wireguard/wg0.conf."));
    }
    if v.mode == "shared" && v.vpn_type == "openvpn" && v.provider == "custom" {
        steps.push(format!(
            "Put your .ovpn at {ad}/vpn/custom.conf and set OPENVPN_CUSTOM_CONFIG=/gluetun/custom.conf in .env."
        ));
    }
    if v.mode == "per-app" {
        for s in &a.vpn_per_app {
            steps.push(format!("Put a WireGuard config for {s} at {ad}/{s}/wireguard/wg0.conf."));
        }
    }
    let head: String = a.data_root.chars().take(3).collect();
    if !a.data_root.starts_with('/') && !head.contains(':') {
        steps.push("DATA_ROOT should be an absolute host path (it is bind-mounted by Docker).".into());
    }
    if host::looks_truenas(env, target, &a.data_root) {
        steps.push("TrueNAS SCALE: in System > Shell (or over SSH), cd to the stack dataset and run: sudo ./scripts/init.sh  (it generates and patches the arr configs, creates the databases and starts everything). Use sudo docker compose ... for later commands. See the TrueNAS wiki page.".into());
    } else {
        steps.push("In the stack directory run: ./scripts/init.sh  (POSIX shell / WSL; it generates and patches the arr configs, creates the databases and starts everything).".into());
    }
    let host = if a.host_ip.trim().is_empty() { "<this host>" } else { a.host_ip.trim() };
    steps.push(format!("Open wranglarr at http://{}:{}{} and create its admin account. It asks for a one-time setup code from its log: docker compose logs wranglarr. Then, under Account, turn on two-factor or add a passkey.", crate::links::host_part(host), a.wranglarr_port, a.wranglarr_url_base.trim().trim_end_matches('/')));
    if restore_count > 0 {
        steps.push(format!(
            "wranglarr will show a pending restore plan ({restore_count} item(s)); run it from the banner once the apps are up."
        ));
    }
    let sqlite: Vec<&str> = scan
        .get("sqlite")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !sqlite.is_empty() {
        steps.push(format!("SQLite databases were found for: {}. Their config.xml is reused; after the first start, copy each database into Postgres (one command per app): see Importing-Existing-Apps in the docs (wiki/Importing-Existing-Apps.md).", sqlite.join(", ")));
    }
    steps.extend(auth_steps(a, false));
    let has = |x: &str| a.services.iter().any(|s| s == x);
    if has(service::nzbget::DEF.id) && a.apps_auth == "manual" {
        steps.push("NZBGet login is NZBGET_USER / NZBGET_PASS from .env.".into());
    }
    let picked = |r: Roles| a.services.iter().any(|s| service::has(s, r));
    let mut wired = vec![];
    if picked(Roles::DOWNLOADS) {
        wired.push("download clients into the arrs");
    }
    if picked(Roles::SUBTITLES) {
        wired.push("Sonarr/Radarr into Bazarr");
    }
    if picked(Roles::INDEXERS) {
        wired.push("the arrs into Prowlarr");
    }
    if !wired.is_empty() {
        steps.push(format!(
            "Nothing to wire by hand: on first start wranglarr connects {} (System > App connections).",
            wired.join(", ")
        ));
    }
    steps.push(if picked(Roles::INDEXERS) {
        "Trackers: add them in Prowlarr, or on wranglarr's Indexers page (Torznab/Newznab feeds, Jackett, NZBHydra2), and pick which arrs get them.".into()
    } else {
        "Trackers: add them on wranglarr's Indexers page (Torznab/Newznab feeds, Jackett, NZBHydra2, a Prowlarr) and pick which arrs get them.".into()
    });
    steps
}

/// A login service's default username (the wizard has fields for qBittorrent's
/// and NZBGet's logins).
pub(super) fn default_user(d: &ServiceDef) -> &'static str {
    match d.credentials {
        service::Credentials::UserPassword { default_user, .. } => default_user,
        service::Credentials::ApiKey { .. } => "",
    }
}

/// HOST_IP as written: an IP address (brackets dropped), or "" for init.sh to find.
pub(super) fn host_ip(v: &str) -> Result<String> {
    let v = v.trim().trim_matches(|c| c == '[' || c == ']');
    if v.is_empty() || v.parse::<std::net::IpAddr>().is_ok() {
        return Ok(v.to_string());
    }
    Err(Error::invalid(format!(
        "this machine's IP {v:?}: an IP address like 192.168.1.10, not a name or a port (blank: init.sh finds it)"
    )))
}

#[cfg(test)]
mod tests {
    use super::super::render_env;
    use super::*;

    #[test]
    fn host_ip_is_an_ip_address_or_blank() {
        assert_eq!(host_ip(" 192.168.1.10 ").expect("ip"), "192.168.1.10");
        assert_eq!(host_ip("[fd00::5]").expect("ipv6"), "fd00::5");
        assert_eq!(host_ip("").expect("blank: init.sh finds it"), "");
        for bad in ["nas.local", "wranglarr.example.com", "192.168.1.10:8787", "http://192.168.1.10"] {
            assert!(host_ip(bad).is_err(), "{bad}");
        }
        let a = Answers { host_ip: "192.168.1.10".into(), postgres_password: "x".into(), ..Default::default() };
        assert!(render_env(&a, "img").expect("env").contains("\nHOST_IP=192.168.1.10\n"));
    }
}
