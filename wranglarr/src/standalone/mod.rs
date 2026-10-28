// SPDX-License-Identifier: GPL-3.0-or-later
//! Make every backup version a runnable stack on its own.
//!
//! Next to the manifest, each version gets:
//!
//! ```text
//! compose.yaml                      the suite's apps (+ Postgres), pinned to the images that were running
//! .env                              ids, timezone, Postgres login, data mounts (mode 600)
//! RESTORE.md                        how to bring it up with plain docker / podman
//! postgres/initdb/10-restore.sh     run by the stock postgres image on first start: recreates
//!                                   every database from the dumps in this folder
//! <app>/config/                     the app's /config, natural layout (see files.rs)
//! ```
//!
//! So if wranglarr is gone: copy the folder, `docker compose up -d`, done.

mod plan;
mod readme;
mod render;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::Value;

use crate::config::{Config, Lookup, SuiteConfig, env_quote};
use crate::docker::Docker;
use crate::manifest::{ContainerInfo, Manifest, Mount};
use crate::service::{self, Credentials, Port};
use plan::Stack;

/// What a restore stack takes from the running wranglarr's environment (part of
/// [`crate::deployment::Deployment`]).
#[derive(Clone)]
pub struct StackEnv {
    /// APPS_AUTH=tunnel (AuthTunnel): the apps had no login of their own.
    pub tunnel: bool,
    /// TZ (UTC when unset).
    pub tz: String,
    /// DATA_ROOT: the host folder a media app without binds of its own gets as /data.
    pub data_root: String,
    /// The apps' login variables (QBITTORRENT_USER, ...) that are set. They hold
    /// passwords, so this has no `Debug`.
    pub logins: BTreeMap<&'static str, String>,
}

impl StackEnv {
    /// The settings in the environment `env`.
    pub fn read(env: Lookup) -> StackEnv {
        let logins = service::ALL
            .iter()
            .filter_map(|d| match d.credentials {
                Credentials::UserPassword { env_user, env_pass, .. } => Some([env_user, env_pass]),
                _ => None,
            })
            .flatten()
            .filter_map(|k| env(k).map(|v| (k, v)))
            .collect();
        StackEnv {
            tunnel: env("APPS_AUTH").unwrap_or_default().trim().eq_ignore_ascii_case("tunnel"),
            tz: env("TZ").unwrap_or_else(|| "UTC".into()),
            data_root: env("DATA_ROOT").unwrap_or_default(),
            logins,
        }
    }

    /// An app's login variable, or `default` when it isn't set.
    fn login_or(&self, k: &str, default: &str) -> String {
        self.logins.get(k).map_or_else(|| default.to_string(), String::clone)
    }
}

/// The Docker network the restore stacks share.
const NETWORK: &str = "arrs-restore";
/// The container settings copied from the running apps into `.env`.
const COPY_ENV: [&str; 4] = ["PUID", "PGID", "TZ", "UMASK"];

/// The image and ports (`host:container[/udp]`, the web UI first) each kind gets
/// when the backup doesn't know better.
fn defaults(kind: &str) -> (&'static str, Vec<String>) {
    service::get(kind)
        .map_or(("", vec![]), |d| (d.container.image, d.container.ports.iter().map(Port::compose).collect()))
}

const VERSION_LABELS: [&str; 4] =
    ["org.opencontainers.image.version", "org.label-schema.version", "version", "build_version"];

/// The version an image says it is, from its labels ("" when it doesn't).
pub fn image_version(img: &Value) -> String {
    let labels = &img["Config"]["Labels"];
    for k in VERSION_LABELS {
        if let Some(v) = labels[k].as_str().filter(|v| !v.is_empty()) {
            if k == "build_version"
                && let Some((_, rest)) = v.split_once("version:-")
            {
                // linuxserver.io style
                return rest.split_whitespace().next().unwrap_or("").to_string();
            }
            return v.to_string();
        }
    }
    String::new()
}

/// What the running container looked like, for pinning the restore stack.
/// Best effort: None when it can't be told, never an error.
pub async fn container_info(docker: Option<&Docker>, name: &str) -> Option<ContainerInfo> {
    let d = docker.filter(|_| !name.is_empty())?;
    let c = d.inspect(name).await.ok()?;
    let image_id = c["Image"].as_str().unwrap_or("").to_string();
    let img = d.image(&image_id).await.unwrap_or(Value::Null);
    let env: Vec<(String, String)> = c["Config"]["Env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_str()?.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();
    let env_get = |k: &str| env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let mut ports = vec![];
    if let Some(pb) = c["HostConfig"]["PortBindings"].as_object() {
        for (cport, binds) in pb {
            let (num, proto) = cport.split_once('/').unwrap_or((cport, ""));
            for b in binds.as_array().into_iter().flatten() {
                if let Some(hp) = b["HostPort"].as_str().filter(|h| !h.is_empty()) {
                    let tail = if !proto.is_empty() && proto != "tcp" { format!("/{proto}") } else { String::new() };
                    ports.push(format!("{}{hp}:{num}{tail}", host_ip(b["HostIp"].as_str().unwrap_or(""))));
                }
            }
        }
    }
    let text = |v: &Value| v.as_str().map(str::to_string);
    let mounts = c["Mounts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["Type"] == "bind")
        .map(|m| Mount {
            source: text(&m["Source"]),
            target: text(&m["Destination"]),
            ro: !m["RW"].as_bool().unwrap_or(true),
            ..Mount::default()
        })
        .collect();
    Some(ContainerInfo {
        image: Some(crate::docker::image_reference(&c).to_string()),
        pinned: img["RepoDigests"].get(0).and_then(text),
        version: image_version(&img),
        env: COPY_ENV.iter().filter_map(|k| env_get(k).map(|v| (k.to_string(), v))).collect(),
        ports,
        mounts,
        network_mode: c["HostConfig"]["NetworkMode"].as_str().unwrap_or("").to_string(),
        // a hotio image with its own tunnel (compose.vpn-per-app.yaml)
        vpn: env_get("VPN_ENABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
        image_id,
        extra: serde_json::Map::new(),
    })
}

/// The `ip:` prefix of a published port, so a port bound to one address (say
/// 127.0.0.1) isn't opened to every interface in the restore stack. Empty for
/// all interfaces.
fn host_ip(ip: &str) -> String {
    match ip {
        "" | "0.0.0.0" | "::" => String::new(),
        ip if ip.contains(':') => format!("[{ip}]:"),
        ip => format!("{ip}:"),
    }
}

/// `v` as a double-quoted string with JSON's escapes, which YAML and TOML read
/// as the same string.
pub fn q(v: &str) -> String {
    Value::String(v.to_string()).to_string()
}

/// The image an app ran (its digest when pinned), else its kind's default.
fn image_ref<'a>(kind: &str, info: Option<&'a ContainerInfo>) -> &'a str {
    info.and_then(ContainerInfo::image_ref).unwrap_or(defaults(kind).0)
}

/// An app's image: `${VAR:-<the image that ran, else the default>}`.
fn image(kind: &str, info: Option<&ContainerInfo>, var: &str) -> String {
    format!("${{{var}:-{}}}", image_ref(kind, info))
}

/// A `.env` line, quoted the way compose reads it back.
fn env_line(k: &str, v: &str) -> String {
    match env_quote(v) {
        Ok(q) => format!("{k}={q}"),
        // a hand-written .env can hold what compose can't read back
        Err(e) => format!("# {k} {e}: set it by hand\n{k}="),
    }
}

/// Write the restore stack into `vdir`; the files written, relative to it.
pub fn write(
    vdir: &Path,
    manifest: &Manifest,
    cfg: &Config,
    suite: &SuiteConfig,
    env: &StackEnv,
) -> std::io::Result<Vec<String>> {
    let Some(stack) = Stack::plan(manifest, cfg, suite, env) else { return Ok(vec![]) };
    std::fs::write(vdir.join("compose.yaml"), render::compose(&stack))?;
    let mut written = vec!["compose.yaml".to_string()];

    write_private(vdir, ".env", &render::env_file(&stack))?;
    written.push(".env".into());

    if stack.postgres.is_some() {
        let init = vdir.join("postgres").join("initdb");
        std::fs::create_dir_all(&init)?;
        let p = init.join("10-restore.sh");
        std::fs::write(&p, render::initdb(&stack))?;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
        written.push("postgres/initdb/10-restore.sh".into());
    }

    std::fs::write(vdir.join("RESTORE.md"), readme::readme(&stack))?;
    written.push("RESTORE.md".into());
    Ok(written)
}

/// Write `name` in `dir` readable by its owner only (it holds the Postgres password).
/// The atomic write's file is private from creation; write-then-chmod would expose
/// the file briefly under a permissive process umask.
fn write_private(dir: &Path, name: &str, text: &str) -> std::io::Result<()> {
    crate::files::write_atomic(&dir.join(name), text.as_bytes(), None)
}

#[cfg(test)]
mod tests {
    use super::plan::{data_var, var_name};
    use super::*;
    use serde_json::json;

    #[test]
    fn helpers() {
        assert_eq!(q("a\"b"), r#""a\"b""#);
        assert_eq!(q("\u{e9}\n"), "\"\u{e9}\\n\"");
        assert_eq!(data_var("/data"), "DATA_ROOT");
        assert_eq!(data_var("/mnt/media 2"), "MOUNT_MNT_MEDIA_2");
        assert_eq!(var_name("radarr-4k"), "RADARR_4K_IMAGE");
        assert_eq!(env_line("A", "x y"), "A='x y'");
        assert!(env_line("A", "a'$").starts_with("# A can't contain both"));
        let img = json!({"Config": {"Labels": {"build_version": "Linuxserver.io version:- 5.1.0.1 Build-date:- x"}}});
        assert_eq!(image_version(&img), "5.1.0.1");
        assert_eq!(host_ip(""), "");
        assert_eq!(host_ip("0.0.0.0"), "");
        assert_eq!(host_ip("::"), "");
        assert_eq!(host_ip("127.0.0.1"), "127.0.0.1:");
        assert_eq!(host_ip("::1"), "[::1]:");
    }

    #[test]
    fn stack_env() {
        let vars = [("NZBGET_USER", "me"), ("APPS_AUTH", " TUNNEL "), ("UNRELATED", "x")];
        let e = StackEnv::read(&|k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string()));
        assert!(e.tunnel);
        assert_eq!((e.tz.as_str(), e.data_root.as_str()), ("UTC", ""));
        assert_eq!(e.logins.keys().copied().collect::<Vec<_>>(), ["NZBGET_USER"]);
        assert_eq!(e.login_or("NZBGET_USER", "someone"), "me");
        assert_eq!(e.login_or("NZBGET_PASSWORD", "default"), "default");
    }

    #[test]
    fn writes_a_stack() {
        let t = tempfile::tempdir().expect("tmp");
        let cfg = crate::service::apps::parse(
            "[postgres]\nhost = \"postgres\"\nuser = \"arr\"\npassword = \"pw\"\n\
             [[suite]]\nname = \"Media One\"\n\
             [[suite.app]]\nname = \"radarr\"\nkind = \"radarr\"\nurl = \"http://radarr:7878\"\napi_key = \"k\"\ndatabases = [\"radarr-main\"]\n\
             [[suite.app]]\nname = \"qbit\"\nkind = \"qbittorrent\"\nurl = \"http://qbit:8090\"\n",
            |_| None,
        )
        .expect("cfg");
        let m = json!({"version": "20260101T000000Z", "created": "2026-01-01T00:00:00+00:00", "postgres_version": "18.1",
            "apps": {
                "radarr": {"version": "5.0", "databases": [{"name": "radarr-main", "file": "radarr/radarr-main.dump"}],
                           "config_snapshot": {"files": 3},
                           "container": {"image": "lscr.io/linuxserver/radarr:latest", "env": {"PUID": "1001", "TZ": "Europe/Oslo"},
                                         "ports": ["7878:7878"], "mounts": [{"source": "/srv/media", "target": "/data", "ro": false}]}},
                "qbit": {"config_snapshot": {"files": 1}, "databases": []}}});
        let m: Manifest = serde_json::from_value(m).expect("manifest");
        let w = write(t.path(), &m, &cfg, &cfg.suites[0], &StackEnv::read(&|_| None)).expect("write");
        assert_eq!(w, ["compose.yaml", ".env", "postgres/initdb/10-restore.sh", "RESTORE.md"]);
        let compose = std::fs::read_to_string(t.path().join("compose.yaml")).expect("read");
        assert!(compose.contains("name: arrs-restore-media-one"));
        assert!(compose.contains("      - ./postgres/data:/var/lib/postgresql           #"));
        assert!(compose.contains("      - ${DATA_ROOT:?set DATA_ROOT in .env}:/data"));
        assert!(compose.contains("      WEBUI_PORT: \"8090\""));
        assert_eq!(std::fs::metadata(t.path().join(".env")).expect("metadata").permissions().mode() & 0o777, 0o600);
        let env = std::fs::read_to_string(t.path().join(".env")).expect("read");
        assert!(
            env.contains(
                "\nTZ=Europe/Oslo\nPUID=1001\nPGID=1000\nUMASK=002\nPOSTGRES_USER=arr\nPOSTGRES_PASSWORD=pw\n"
            ),
            "{env}"
        );
        assert!(env.contains("DATA_ROOT=/srv/media"));
        let sh = std::fs::read_to_string(t.path().join("postgres/initdb/10-restore.sh")).expect("read");
        assert!(sh.contains("restore \"radarr-main\" \"radarr/radarr-main.dump\""));
        let md = std::fs::read_to_string(t.path().join("RESTORE.md")).expect("read");
        assert!(md.contains(
            "| radarr | 5.0 | `lscr.io/linuxserver/radarr:latest` | `radarr/config/` (3 files) | `radarr-main` |"
        ));
    }
}
