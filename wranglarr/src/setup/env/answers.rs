// SPDX-License-Identifier: GPL-3.0-or-later
//! The wizard's answers as the page sends them, and putting kept secrets back.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::super::input;
use super::KEEP;
use super::steps::default_user;
use crate::service;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VpnCheck {
    /// none | shared | per-app
    pub mode: String,
    /// shared tunnel: all | prowlarr (+ download clients) | downloads
    pub scope: String,
    pub vpn_type: String,
    pub provider: String,
    pub private_key: String,
    pub preshared_key: String,
    pub public_key: String,
    pub endpoint_ip: String,
    pub endpoint_port: String,
    pub addresses: String,
    pub lan_network: String,
    pub user: String,
    pub password: String,
}

impl Default for VpnCheck {
    fn default() -> Self {
        VpnCheck {
            mode: "none".into(),
            scope: "all".into(),
            vpn_type: "wireguard".into(),
            provider: "custom".into(),
            private_key: String::new(),
            preshared_key: String::new(),
            public_key: String::new(),
            endpoint_ip: String::new(),
            endpoint_port: "51820".into(),
            addresses: String::new(),
            lan_network: String::new(),
            user: String::new(),
            password: String::new(),
        }
    }
}

impl VpnCheck {
    /// The string fields in declaration order (for check_values).
    pub(super) fn strings(&self) -> [(&'static str, &str); 13] {
        [
            ("mode", &self.mode),
            ("scope", &self.scope),
            ("vpn_type", &self.vpn_type),
            ("provider", &self.provider),
            ("private_key", &self.private_key),
            ("preshared_key", &self.preshared_key),
            ("public_key", &self.public_key),
            ("endpoint_ip", &self.endpoint_ip),
            ("endpoint_port", &self.endpoint_port),
            ("addresses", &self.addresses),
            ("lan_network", &self.lan_network),
            ("user", &self.user),
            ("password", &self.password),
        ]
    }

    /// Put the .env value back wherever the browser sent KEEP.
    pub fn unmask(&mut self, env: &mut impl FnMut() -> BTreeMap<String, String>) {
        let fields: [(&mut String, &[&str]); 3] = [
            (&mut self.private_key, &["WIREGUARD_PRIVATE_KEY"]),
            (&mut self.preshared_key, &["WIREGUARD_PRESHARED_KEY"]),
            (&mut self.password, &["VPN_PASSWORD", "VPN_PIA_PASS"]),
        ];
        unmask_fields(fields, env);
    }
}

fn unmask_fields<const N: usize>(
    fields: [(&mut String, &[&str]); N],
    env: &mut impl FnMut() -> BTreeMap<String, String>,
) {
    let mut loaded: Option<BTreeMap<String, String>> = None;
    for (f, keys) in fields {
        if f == KEEP {
            let e = loaded.get_or_insert_with(&mut *env);
            *f = keys.iter().find_map(|k| e.get(*k).filter(|v| !v.is_empty())).cloned().unwrap_or_default();
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Answers {
    pub services: Vec<String>,
    pub tz: String,
    #[serde(deserialize_with = "input::whole_number")]
    pub puid: i64,
    #[serde(deserialize_with = "input::whole_number")]
    pub pgid: i64,
    pub umask: String,
    pub appdata: String,
    pub data_root: String,
    pub data_root_2: String,
    pub postgres_user: String,
    pub postgres_password: String,
    #[serde(deserialize_with = "input::whole_numbers")]
    pub ports: BTreeMap<String, i64>,
    #[serde(deserialize_with = "input::whole_number")]
    pub wranglarr_port: i64,
    pub wranglarr_url_base: String,
    /// This machine's IP address on the network (HOST_IP); blank: init.sh finds it.
    pub host_ip: String,
    pub gluetun_api_key: String,
    pub nzbget_user: String,
    pub nzbget_pass: String,
    pub qbittorrent_user: String,
    pub qbittorrent_password: String,
    /// app logins (wiki/App-Logins.md): apps | tunnel | manual
    pub apps_auth: String,
    pub apps_login_user: String,
    pub apps_login_password: String,
    pub apps_auth_required: String,
    pub auth_cookie_domain: String,
    pub auth_login_url: String,
    pub vpn: VpnCheck,
    pub vpn_per_app: Vec<String>,
    pub backup_cron: String,
    #[serde(deserialize_with = "input::whole_number")]
    pub keep_last: i64,
    #[serde(deserialize_with = "input::whole_number")]
    pub keep_days: i64,
}

impl Default for Answers {
    fn default() -> Self {
        Answers {
            services: vec![],
            tz: "UTC".into(),
            puid: 1000,
            pgid: 1000,
            umask: "002".into(),
            appdata: "./appdata".into(),
            data_root: "/mnt/data".into(),
            data_root_2: String::new(),
            postgres_user: "arr".into(),
            postgres_password: String::new(),
            ports: BTreeMap::new(),
            wranglarr_port: 8787,
            wranglarr_url_base: String::new(),
            host_ip: String::new(),
            gluetun_api_key: String::new(),
            nzbget_user: default_user(&service::nzbget::DEF).into(),
            nzbget_pass: String::new(),
            qbittorrent_user: default_user(&service::qbittorrent::DEF).into(),
            qbittorrent_password: String::new(),
            apps_auth: "apps".into(),
            apps_login_user: "admin".into(),
            apps_login_password: String::new(),
            apps_auth_required: "enabled".into(),
            auth_cookie_domain: String::new(),
            auth_login_url: String::new(),
            vpn: VpnCheck::default(),
            vpn_per_app: vec![],
            backup_cron: "30 3 * * *".into(),
            keep_last: 14,
            keep_days: 60,
        }
    }
}

impl Answers {
    pub(super) fn strings(&self) -> [(&'static str, &str); 20] {
        [
            ("tz", &self.tz),
            ("umask", &self.umask),
            ("appdata", &self.appdata),
            ("data_root", &self.data_root),
            ("data_root_2", &self.data_root_2),
            ("postgres_user", &self.postgres_user),
            ("postgres_password", &self.postgres_password),
            ("gluetun_api_key", &self.gluetun_api_key),
            ("nzbget_user", &self.nzbget_user),
            ("nzbget_pass", &self.nzbget_pass),
            ("qbittorrent_user", &self.qbittorrent_user),
            ("qbittorrent_password", &self.qbittorrent_password),
            ("apps_auth", &self.apps_auth),
            ("apps_login_user", &self.apps_login_user),
            ("apps_login_password", &self.apps_login_password),
            ("apps_auth_required", &self.apps_auth_required),
            ("auth_cookie_domain", &self.auth_cookie_domain),
            ("auth_login_url", &self.auth_login_url),
            ("backup_cron", &self.backup_cron),
            ("wranglarr_url_base", &self.wranglarr_url_base),
        ]
    }

    /// Put the .env values back wherever the browser sent KEEP.
    pub fn unmask(&mut self, env: &mut impl FnMut() -> BTreeMap<String, String>) {
        let fields: [(&mut String, &[&str]); 5] = [
            (&mut self.postgres_password, &["POSTGRES_PASSWORD"]),
            (&mut self.gluetun_api_key, &["GLUETUN_API_KEY"]),
            (&mut self.nzbget_pass, &["NZBGET_PASS"]),
            (&mut self.qbittorrent_password, &["QBITTORRENT_PASSWORD"]),
            (&mut self.apps_login_password, &["APPS_LOGIN_PASSWORD"]),
        ];
        unmask_fields(fields, env);
        self.vpn.unmask(env);
    }
}
