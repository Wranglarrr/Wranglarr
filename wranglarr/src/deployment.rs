// SPDX-License-Identifier: GPL-3.0-or-later
//! How this install is deployed: the settings compose's `.env` hands wranglarr
//! as environment variables. A command reads them once where it starts
//! ([`Deployment::read`] on [`config::process_env`]) and passes them to what
//! needs them; the rest of wranglarr never reads the process environment for
//! them, and tests build a [`Deployment`] of their own.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{self, Lookup};

/// The deployment settings, as read at start.
///
/// No `Debug`: the restore stack's part holds the apps' passwords.
#[derive(Clone)]
pub struct Deployment {
    /// WRANGLARR_CONFIG: where wranglarr.toml is (/config/wranglarr.toml).
    pub config_path: PathBuf,
    /// WRANGLARR_LOG: a tracing filter used instead of the configured log level.
    pub log_filter: Option<String>,
    /// WRANGLARR_IMAGE: wranglarr's image, for the commands it shows.
    pub image: String,
    /// WRANGLARR_HOST_BACKUPS: where the backup store is on the host, for
    /// messages; empty when unknown.
    pub host_backups: String,
    /// FORWARDED_ALLOW_IPS.
    pub trusted_proxies: TrustedProxies,
    /// The login.
    pub auth: crate::auth::Options,
    /// What the reverse-proxy config depends on.
    pub proxy: crate::proxy::ProxyEnv,
    /// How the apps' logins are set up.
    pub app_logins: crate::applogins::LoginsEnv,
    /// What a backup's restore stack takes from this one.
    pub restore_stack: crate::standalone::StackEnv,
    /// PUID:PGID: who backups and wranglarr's own folder are handed to when
    /// wranglarr runs as root on the host.
    pub stack_owner: crate::owner::StackOwner,
    /// UMASK, octal; empty leaves the process's.
    pub umask: String,
    /// WRANGLARR_PRE_RESTORE: where a restore keeps what it replaced when the
    /// folder next to the app's config isn't writable (/config/pre-restore).
    pub pre_restore: PathBuf,
    /// The updater's timing and its own container (WRANGLARR_UPDATE_*, HOSTNAME).
    pub updates: crate::updates::UpdatesEnv,
    /// WRANGLARR_VPN_PORT_POLL: how often qBittorrent's listening port is checked
    /// against the VPN's forwarded one (60 s; the test suite shrinks it).
    pub vpn_port_poll: std::time::Duration,
}

impl Deployment {
    /// The settings in the environment `env`.
    pub fn read(env: impl Fn(&str) -> Option<String>) -> Deployment {
        let env: Lookup = &env;
        Deployment {
            config_path: config::default_path(env),
            log_filter: env("WRANGLARR_LOG"),
            image: env("WRANGLARR_IMAGE").filter(|s| !s.is_empty()).unwrap_or_else(|| crate::IMAGE.into()),
            host_backups: env("WRANGLARR_HOST_BACKUPS").unwrap_or_default(),
            trusted_proxies: TrustedProxies::new(&env("FORWARDED_ALLOW_IPS").unwrap_or_else(|| "127.0.0.1".into())),
            auth: crate::auth::Options::read(env),
            proxy: crate::proxy::ProxyEnv::read(env),
            app_logins: crate::applogins::LoginsEnv::read(env),
            restore_stack: crate::standalone::StackEnv::read(env),
            stack_owner: crate::owner::StackOwner::read(env),
            umask: env("UMASK").unwrap_or_default(),
            pre_restore: env("WRANGLARR_PRE_RESTORE")
                .filter(|p| !p.is_empty())
                .map_or_else(|| crate::files::DEFAULT_PRE_RESTORE.into(), PathBuf::from),
            updates: crate::updates::UpdatesEnv::read(env),
            vpn_port_poll: env("WRANGLARR_VPN_PORT_POLL")
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|s| s.is_finite() && *s > 0.0)
                .map_or(crate::vpn::PORT_EVERY, std::time::Duration::from_secs_f64),
        }
    }
}

impl Default for Deployment {
    /// Nothing set: every default.
    fn default() -> Deployment {
        Deployment::read(|_| None)
    }
}

/// The proxies whose X-Forwarded-For/Host/Proto are believed: FORWARDED_ALLOW_IPS, a comma
/// list of addresses (default 127.0.0.1; `*` for any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedProxies(Arc<[String]>);

impl TrustedProxies {
    /// From FORWARDED_ALLOW_IPS's value.
    pub fn new(list: &str) -> TrustedProxies {
        TrustedProxies(list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
    }

    fn any(&self) -> bool {
        self.0.iter().any(|t| t == "*")
    }

    fn trusts(&self, ip: &str) -> bool {
        self.0.iter().any(|t| t == "*" || t == ip)
    }

    /// Forwarded request metadata is meaningful only with a known, trusted
    /// immediate peer. A missing peer never becomes trusted, even with `*`.
    pub fn accepts(&self, peer: Option<IpAddr>) -> bool {
        peer.is_some_and(|ip| self.trusts(&ip.to_canonical().to_string()))
    }

    /// The client's address for a connection from `peer` with this
    /// X-Forwarded-For: behind a trusted proxy it is the last hop that isn't a
    /// trusted proxy (the first hop when every proxy is trusted); "?" with no peer.
    pub fn client_ip(&self, peer: Option<IpAddr>, forwarded_for: &str) -> String {
        let Some(peer) = peer else { return "?".into() };
        let peer = peer.to_canonical().to_string();
        if forwarded_for.is_empty() || !self.trusts(&peer) {
            return peer;
        }
        let hops: Vec<&str> = forwarded_for.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        if self.any() {
            return hops.first().map_or(peer, |h| h.to_string());
        }
        hops.iter().rev().find(|h| !self.trusts(h)).or(hops.first()).map_or(peer, |h| h.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn defaults() {
        let d = Deployment::default();
        assert_eq!(d.config_path, PathBuf::from("/config/wranglarr.toml"));
        assert_eq!(d.log_filter, None);
        assert_eq!(d.image, crate::IMAGE);
        assert_eq!(d.host_backups, "");
        assert_eq!(d.trusted_proxies, TrustedProxies::new("127.0.0.1"));
        assert!(d.auth.enabled && !d.auth.tunnel);
        assert_eq!(d.auth.port, "8787");
        assert_eq!(d.proxy.apps_auth, "manual");
        assert_eq!(d.proxy.wranglarr_port, 8787);
        assert_eq!(d.app_logins.mode, "manual");
        assert_eq!(d.app_logins.required, "enabled");
        assert_eq!(d.restore_stack.tz, "UTC");
        assert_eq!(d.stack_owner.ids(), None);
        assert_eq!(d.umask, "");
        assert_eq!(d.pre_restore, PathBuf::from("/config/pre-restore"));
        assert_eq!(d.updates.poll, std::time::Duration::from_secs(2));
        assert_eq!(d.updates.stable, std::time::Duration::from_secs(15));
        assert_eq!(d.updates.hostname, "");
    }

    #[test]
    fn read_from_the_environment() {
        let d = Deployment::read(env(&[
            ("WRANGLARR_CONFIG", "/etc/w.toml"),
            ("WRANGLARR_IMAGE", ""),
            ("WRANGLARR_AUTH", " Off "),
            ("APPS_AUTH", " Tunnel"),
            ("AUTH_COOKIE_DOMAIN", ".Example.com "),
            ("AUTH_LOGIN_URL", "https://login.example.com/ "),
            ("WRANGLARR_PORT", "9000"),
            ("FORWARDED_ALLOW_IPS", "10.0.0.1, *"),
            ("TZ", ""),
            ("PUID", " 1001 "),
            ("UMASK", "022"),
            ("WRANGLARR_PRE_RESTORE", "/data/keep"),
            ("WRANGLARR_UPDATE_POLL", "0.05"),
            ("WRANGLARR_UPDATE_STABLE", "-1"),
            ("HOSTNAME", "3f2a9c"),
        ]));
        assert_eq!(d.stack_owner.ids(), Some((1001, 1001)), "PGID defaults to PUID");
        assert_eq!(d.umask, "022");
        assert_eq!(d.pre_restore, PathBuf::from("/data/keep"));
        assert_eq!(d.updates.poll, std::time::Duration::from_millis(50));
        assert_eq!(d.updates.stable, std::time::Duration::from_secs(15), "a negative time is the default");
        assert_eq!(d.updates.hostname, "3f2a9c");
        assert_eq!(d.config_path, PathBuf::from("/etc/w.toml"));
        assert_eq!(d.image, crate::IMAGE, "empty is unset");
        assert!(!d.auth.enabled && d.auth.tunnel);
        assert_eq!(d.auth.cookie_domain, "example.com");
        assert_eq!(d.auth.login_url, "https://login.example.com");
        assert_eq!(d.auth.port, "9000");
        assert_eq!(d.proxy.apps_auth, "tunnel");
        assert_eq!(d.proxy.cookie_domain, ".example.com");
        assert_eq!(d.proxy.wranglarr_port, 9000);
        assert_eq!(d.app_logins.mode, "tunnel");
        assert!(d.restore_stack.tunnel);
        assert_eq!(d.restore_stack.tz, "", "set but empty stays empty");
        assert_eq!(d.trusted_proxies, TrustedProxies::new("10.0.0.1,*"));
    }

    #[test]
    fn client_ip_behind_a_local_proxy() {
        let local = TrustedProxies::new("127.0.0.1");
        let ip = |p: &TrustedProxies, peer: &str, xff: &str| p.client_ip(Some(peer.parse().expect("ip")), xff);
        assert_eq!(ip(&local, "10.0.0.5", "1.2.3.4"), "10.0.0.5", "not a trusted proxy");
        assert_eq!(ip(&local, "127.0.0.1", "1.2.3.4, 5.6.7.8"), "5.6.7.8");
        assert_eq!(ip(&local, "127.0.0.1", ""), "127.0.0.1");
        assert_eq!(ip(&local, "::ffff:127.0.0.1", "5.6.7.8"), "5.6.7.8", "a v4-mapped peer");
        assert_eq!(local.client_ip(None, "1.2.3.4"), "?");
        let any = TrustedProxies::new("*");
        assert_eq!(ip(&any, "10.0.0.5", "1.2.3.4, 5.6.7.8"), "1.2.3.4");
        let two = TrustedProxies::new("127.0.0.1, 5.6.7.8");
        assert_eq!(ip(&two, "127.0.0.1", "1.2.3.4, 5.6.7.8"), "1.2.3.4");
    }
}
