// SPDX-License-Identifier: GPL-3.0-or-later
//! What the restore stack holds, worked out once from the manifest and the config:
//! the apps it runs, how each is started, the Postgres service and the `.env` values.
//! The renderers only format this.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use super::{COPY_ENV, StackEnv, defaults};
use crate::config::{AppConfig, Config, SuiteConfig};
use crate::manifest::{AppBackup, ContainerInfo, Manifest};
use crate::service::{AppConfigExt, Credentials, Roles};

static NON_UPPER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^A-Z0-9]+").expect("static regex"));
static HOSTNAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9_.-]*$").expect("static regex"));

/// The restore stack of one suite at one version.
pub(super) struct Stack<'a> {
    pub manifest: &'a Manifest,
    pub suite: &'a SuiteConfig,
    /// The backup version (its folder name).
    pub version: &'a str,
    /// The apps with something to restore, in the suite's order.
    pub apps: Vec<StackApp<'a>>,
    /// The Postgres service's name, which the apps' configs point at.
    pub pg_host: &'a str,
    /// The Postgres service, when there are dumps to restore.
    pub postgres: Option<Postgres>,
    /// (database, dump file), in the order they are restored.
    pub dumps: Vec<(String, String)>,
    /// The stack ran a shared VPN.
    pub vpn_used: bool,
    /// Apps whose image ran its own VPN tunnel.
    pub own_vpn: Vec<&'a str>,
    /// Apps that ran behind AuthTunnel with no login of their own.
    pub gated: Vec<&'a str>,
    /// `.env`'s values, in the order written.
    pub env: EnvVars,
    /// The data mounts' `.env` variables and their host folders ("" when unknown).
    pub mount_vars: EnvVars,
}

/// One app's service.
pub(super) struct StackApp<'a> {
    pub app: &'a AppConfig,
    /// Its entry in the manifest.
    pub entry: &'a AppBackup,
    /// It ran behind a VPN, so it only starts with the `downloads` profile.
    pub behind_vpn: bool,
    /// Its first database, which its environment points at.
    pub first_db: Option<String>,
    /// `host:container[/udp]`, as written under `ports:`.
    pub ports: Vec<String>,
    /// Its data mounts besides /config.
    pub binds: Vec<Bind>,
}

/// A data mount: the `.env` variable with the host folder, and where it goes.
pub(super) struct Bind {
    pub var: String,
    pub target: String,
    pub ro: bool,
}

/// The Postgres service.
pub(super) struct Postgres {
    /// The image to run when POSTGRES_IMAGE isn't set.
    pub image: String,
    /// Where the image keeps its data (moved in Postgres 18).
    pub datadir: &'static str,
}

/// `.env` values in the order they are written (a changed value keeps its place).
pub(super) type EnvVars = IndexMap<String, String>;

/// The `.env` variable for a data mount's host folder.
pub(super) fn data_var(target: &str) -> String {
    match target {
        "/data" => "DATA_ROOT".into(),
        "/data2" => "DATA_ROOT_2".into(),
        t => format!("MOUNT_{}", NON_UPPER.replace_all(&t.to_uppercase(), "_").trim_matches('_')),
    }
}

/// The `.env` variable that overrides an app's image.
pub(super) fn var_name(app: &str) -> String {
    format!("{}_IMAGE", NON_UPPER.replace_all(&app.to_uppercase(), "_"))
}

/// Where the app keeps its config.
pub(super) fn config_mount(kind: &str) -> &'static str {
    crate::service::get(kind).map_or("/config", |d| d.container.config_mount)
}

impl<'a> Stack<'a> {
    /// The stack for `suite` at the version in `manifest`; None when no app has a
    /// config snapshot or databases in it.
    pub fn plan(
        manifest: &'a Manifest,
        cfg: &'a Config,
        suite: &'a SuiteConfig,
        stack_env: &StackEnv,
    ) -> Option<Stack<'a>> {
        let apps: Vec<(&AppConfig, &AppBackup)> = suite
            .apps
            .iter()
            .filter_map(|a| manifest.apps.get(&a.name).filter(|e| e.restorable()).map(|e| (a, e)))
            .collect();
        if apps.is_empty() {
            return None;
        }
        let pg_host = if HOSTNAME.is_match(&cfg.postgres.host) { cfg.postgres.host.as_str() } else { "postgres" };
        let dumps: Vec<(String, String)> =
            apps.iter().flat_map(|(_, e)| &e.databases).map(|d| (d.name.clone(), d.file.clone())).collect();
        let vpn_used = cfg.vpn.is_some();
        let own_vpn: Vec<&str> = apps
            .iter()
            .filter(|(_, e)| e.container.as_ref().is_some_and(|c| c.vpn))
            .map(|(a, _)| a.name.as_str())
            .collect();
        // AuthTunnel: the apps have no login of their own (the gate was it), so the
        // restore publishes their web UIs on this machine only
        let gated: Vec<&str> = apps
            .iter()
            .filter(|(a, _)| stack_env.tunnel && !a.def().is_some_and(|d| d.proxy.own_login))
            .map(|(a, _)| a.name.as_str())
            .collect();

        let mut env = base_env(&apps, &stack_env.tz);
        let postgres = (!dumps.is_empty()).then(|| {
            env.insert("POSTGRES_USER".into(), cfg.postgres.user.clone());
            env.insert("POSTGRES_PASSWORD".into(), cfg.postgres.password.clone());
            postgres(manifest)
        });
        let mut mount_vars = EnvVars::new();
        let apps = apps
            .into_iter()
            .map(|(a, e)| {
                if let Some(Credentials::UserPassword {
                    env_user,
                    env_pass,
                    default_user,
                    default_pass: Some(pass),
                    ..
                }) = a.def().map(|d| d.credentials)
                {
                    env.entry(env_user.into()).or_insert_with(|| stack_env.login_or(env_user, default_user));
                    env.entry(env_pass.into()).or_insert_with(|| stack_env.login_or(env_pass, pass));
                }
                let info = e.container.as_ref();
                let binds = binds(a, info, &stack_env.data_root);
                for b in &binds {
                    mount_vars.entry(b.bind.var.clone()).or_insert_with(|| b.host.clone());
                }
                StackApp {
                    app: a,
                    entry: e,
                    behind_vpn: a.has(Roles::DOWNLOADS) && (vpn_used || own_vpn.contains(&a.name.as_str())),
                    first_db: e.databases.first().map(|d| d.name.clone()),
                    ports: ports(a, info, gated.contains(&a.name.as_str())),
                    binds: binds.into_iter().map(|b| b.bind).collect(),
                }
            })
            .collect();
        Some(Stack {
            manifest,
            suite,
            version: &manifest.version,
            apps,
            pg_host,
            postgres,
            dumps,
            vpn_used,
            own_vpn,
            gated,
            env,
            mount_vars,
        })
    }
}

/// The ids, timezone and umask: the defaults (wranglarr's own timezone `tz`),
/// then what the first app that had each one ran with.
fn base_env(apps: &[(&AppConfig, &AppBackup)], tz: &str) -> EnvVars {
    let mut env = EnvVars::new();
    env.insert("TZ".into(), tz.into());
    env.insert("PUID".into(), "1000".into());
    env.insert("PGID".into(), "1000".into());
    env.insert("UMASK".into(), "002".into());
    let mut seen = EnvVars::new();
    for info in apps.iter().filter_map(|(_, e)| e.container.as_ref()) {
        for (k, v) in &info.env {
            seen.entry(k.clone()).or_insert_with(|| v.clone()); // first app that had it wins
        }
    }
    for k in COPY_ENV {
        if let Some(v) = seen.get(k) {
            env.insert(k.to_string(), v.clone());
        }
    }
    env
}

/// The Postgres image (the one that ran, else the major version's) and its data folder.
fn postgres(manifest: &Manifest) -> Postgres {
    let pinfo = manifest.postgres_container.as_ref().and_then(Option::as_ref);
    let pv = manifest.postgres_version.as_deref().unwrap_or("");
    let major = match (if pv.is_empty() { "17" } else { pv }).split('.').next().unwrap_or("") {
        "" => "17",
        m => m,
    };
    let image =
        pinfo.and_then(ContainerInfo::image_ref).map(str::to_string).unwrap_or_else(|| format!("postgres:{major}"));
    let datadir = if major.parse::<u32>().is_ok_and(|m| m >= 18) && major.bytes().all(|b| b.is_ascii_digit()) {
        "/var/lib/postgresql"
    } else {
        "/var/lib/postgresql/data"
    };
    Postgres { image, datadir }
}

/// The ports the app published (else its defaults). Behind AuthTunnel its web UI
/// is published on 127.0.0.1 only.
fn ports(a: &AppConfig, info: Option<&ContainerInfo>, gated: bool) -> Vec<String> {
    let ports: Vec<String> =
        info.map(|i| &i.ports).filter(|p| !p.is_empty()).cloned().unwrap_or_else(|| defaults(&a.kind).1);
    if !gated {
        return ports;
    }
    let ui = defaults(&a.kind).1.into_iter().next().unwrap_or_default(); // "7878:7878": host:container
    let cport = ui.split(':').nth(1).unwrap_or("");
    let mut p = vec![format!("127.0.0.1:{ui}")];
    p.extend(ports.into_iter().filter(|x| x.rsplit(':').next() != Some(cport)));
    p
}

/// A data mount with the host folder it had.
struct HostBind {
    bind: Bind,
    host: String,
}

/// The app's bind mounts besides /config; a media app without any gets /data
/// from DATA_ROOT (`data_root`).
fn binds(a: &AppConfig, info: Option<&ContainerInfo>, data_root: &str) -> Vec<HostBind> {
    let cm = config_mount(&a.kind);
    let text = |s: &Option<String>| s.clone().unwrap_or_default();
    let mut binds: Vec<(String, String, bool)> = info
        .into_iter()
        .flat_map(|i| &i.mounts)
        .filter(|m| m.target.as_deref().unwrap_or("") != cm)
        .map(|m| (text(&m.source), text(&m.target), m.ro))
        .collect();
    let media = a.def().is_some_and(|d| d.container.data);
    if binds.is_empty() && media {
        let host = if data_root.starts_with('/') { data_root.to_string() } else { String::new() };
        binds.push((host, "/data".into(), false));
    }
    binds
        .into_iter()
        .map(|(host, target, ro)| HostBind { bind: Bind { var: data_var(&target), target, ro }, host })
        .collect()
}
