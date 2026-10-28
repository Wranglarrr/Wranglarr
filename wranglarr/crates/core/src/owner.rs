// SPDX-License-Identifier: GPL-3.0-or-later
//! Who owns the files wranglarr writes, and what it runs as.
//!
//! compose.yaml runs wranglarr as PUID:PGID with no capabilities; the one-shot
//! `wranglarr-init` service (`prepare_main`) hands its folders to that user
//! first. The rest covers anyone who runs it as root anyway:
//!
//! - restored app config gets the owner of the app's config folder, so the app
//!   can still save its settings;
//! - backups and wranglarr's own folder get PUID:PGID, but only when root in
//!   here is root on the host (under rootless Podman it's already your user).

use std::os::unix::fs::{MetadataExt, lchown};
use std::path::Path;
use std::sync::OnceLock;

use rustix::process::{getegid, geteuid, getgroups};

fn euid() -> u32 {
    geteuid().as_raw()
}

/// Root in here and root on the host (uid 0 isn't mapped to someone else).
pub fn is_host_root() -> bool {
    if euid() != 0 {
        return false;
    }
    if let Ok(map) = std::fs::read_to_string("/proc/self/uid_map") {
        for line in map.lines() {
            let mut f = line.split_whitespace();
            if let (Some(inside), Some(outside)) = (f.next(), f.next())
                && inside == "0"
            {
                return outside == "0";
            }
        }
    }
    true
}

/// The stack's user: PUID:PGID from `.env` (PGID defaults to PUID). A command
/// reads it once where it starts; what wranglarr writes is handed to this user
/// when wranglarr runs as root on the host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StackOwner(Option<(u32, u32)>);

impl StackOwner {
    /// PUID and PGID in `env`; none when PUID is unset, not a number or 0, or
    /// PGID is set but not a number.
    pub fn read(env: crate::config::Lookup) -> StackOwner {
        let id = |k: &str| env(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let ids = || -> Option<(u32, u32)> {
            let uid: u32 = id("PUID")?.parse().ok()?;
            let gid: u32 = match id("PGID") {
                Some(g) => g.parse().ok()?,
                None => uid,
            };
            (uid != 0).then_some((uid, gid))
        };
        StackOwner(ids())
    }

    /// uid:gid, when there is one.
    pub fn ids(&self) -> Option<(u32, u32)> {
        self.0
    }

    /// Hand something wranglarr wrote (a backup, its own state) to the stack's
    /// user, when wranglarr is root on the host.
    pub fn give(&self, path: &Path, recursive: bool) {
        let Some((uid, gid)) = self.0 else { return };
        if !is_host_root() || !path.exists() {
            return;
        }
        if recursive {
            chown_tree(path, uid, gid);
        } else if let Err(e) = lchown(path, Some(uid), Some(gid)) {
            tracing::debug!("chown {}: {e}", path.display());
        }
    }
}

/// Started as root with a PUID that isn't 0, as LinuxServer.io images are: hand
/// `dirs` to PUID:PGID, then run as that user from here on, with PGID, the
/// group of the Docker socket at `socket` and `DOCKER_GID` as its groups.
/// Returns the uid:gid it switched to; `None` when there was nothing to do.
///
/// Must run before any other thread starts: on Linux a thread changes only
/// its own user, so this refuses when there are others.
pub fn drop_root(
    owner: StackOwner,
    dirs: &[&Path],
    socket: &Path,
    docker_gid: Option<&str>,
) -> Result<Option<(u32, u32)>, String> {
    let Some((uid, gid)) = owner.ids() else { return Ok(None) };
    if euid() != 0 {
        return Ok(None);
    }
    let threads = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Threads:").map(|n| n.trim().to_string())));
    if threads.as_deref().is_some_and(|n| n != "1") {
        return Err(format!("can't switch to {uid}:{gid}: other threads are running"));
    }
    // a folder already handed over isn't walked again: backups can be large
    for d in dirs.iter().filter(|d| std::fs::metadata(d).is_ok_and(|m| (m.uid(), m.gid()) != (uid, gid))) {
        let n = chown_tree(d, uid, gid);
        if n > 0 {
            tracing::info!("{}: {n} entries handed to {uid}:{gid}", d.display());
        }
    }
    let groups = groups_for(gid, std::fs::metadata(socket).ok().map(|m| m.gid()), docker_gid);
    switch_to(uid, gid, &groups).map_err(|e| format!("can't switch to {uid}:{gid}: {e}"))?;
    Ok(Some((uid, gid)))
}

/// PGID first, then the Docker socket's group and `DOCKER_GID`. Group 0 is
/// required for sockets owned by the rootless engine's mapped root user.
fn groups_for(gid: u32, socket_gid: Option<u32>, docker_gid: Option<&str>) -> Vec<u32> {
    let mut out = vec![gid];
    let extra = socket_gid.into_iter().chain(docker_gid.and_then(|g| g.trim().parse().ok()));
    for g in extra {
        if !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn switch_to(uid: u32, gid: u32, groups: &[u32]) -> std::io::Result<()> {
    use rustix::process::{Gid, Uid};
    use rustix::thread::{set_thread_groups, set_thread_res_gid, set_thread_res_uid};
    let gids: Vec<Gid> = groups.iter().map(|g| Gid::from_raw(*g)).collect();
    set_thread_groups(&gids)?;
    let g = Gid::from_raw(gid);
    set_thread_res_gid(g, g, g)?;
    let u = Uid::from_raw(uid);
    set_thread_res_uid(u, u, u)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn switch_to(_uid: u32, _gid: u32, _groups: &[u32]) -> std::io::Result<()> {
    Err(std::io::Error::other("only on Linux"))
}

/// lchown `path` and everything under it to uid:gid. Returns how many changed.
pub fn chown_tree(path: &Path, uid: u32, gid: u32) -> usize {
    fn one(p: &Path, uid: u32, gid: u32, changed: &mut usize) {
        match std::fs::symlink_metadata(p) {
            Ok(m) if (m.uid(), m.gid()) != (uid, gid) => match lchown(p, Some(uid), Some(gid)) {
                Ok(()) => *changed += 1,
                Err(e) => tracing::debug!("chown {}: {e}", p.display()),
            },
            Ok(_) => {}
            Err(e) => tracing::debug!("chown {}: {e}", p.display()),
        }
    }
    fn walk(dir: &Path, uid: u32, gid: u32, changed: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            one(&p, uid, gid, changed);
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&p, uid, gid, changed);
            }
        }
    }
    let mut changed = 0;
    one(path, uid, gid, &mut changed);
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        walk(path, uid, gid, &mut changed);
    }
    changed
}

/// Give `path` (and its contents) the owner of `reference`: restored config
/// gets the owner of the app's config folder. Works under rootless Podman too.
pub fn like(path: &Path, reference: &Path) {
    if euid() != 0 {
        return;
    }
    let Ok(m) = std::fs::metadata(reference) else { return };
    if m.uid() == 0 && m.gid() == 0 {
        return;
    }
    chown_tree(path, m.uid(), m.gid());
}

/// Use the stack's UMASK (`raw`, octal; compose's default is 002, group-writable)
/// for what wranglarr creates. Empty leaves the process's umask as it is.
pub fn apply_umask(raw: &str) {
    if raw.trim().is_empty() {
        return;
    }
    match u32::from_str_radix(raw.trim(), 8) {
        Ok(m) => {
            rustix::process::umask(rustix::fs::Mode::from_bits_truncate(m & 0o777));
        }
        Err(_) => tracing::warn!("UMASK={raw:?} is not an octal number; ignored"),
    }
}

/// How wranglarr is running, for the System page and the log.
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct Process {
    pub user: String,
    pub root: bool,
    pub root_on_host: bool,
}

static PROCESS: OnceLock<Process> = OnceLock::new();

pub fn process() -> Process {
    PROCESS.get().cloned().unwrap_or_default()
}

pub fn describe() -> Process {
    let (uid, gid) = (euid(), getegid().as_raw());
    let extra: Vec<String> = if uid != 0 {
        getgroups()
            .unwrap_or_default()
            .into_iter()
            .map(|g| g.as_raw())
            .filter(|g| *g != gid)
            .map(|g| g.to_string())
            .collect()
    } else {
        vec![]
    };
    let p = Process {
        user: format!("{uid}:{gid}")
            + &if extra.is_empty() { String::new() } else { format!(" (+ group {})", extra.join(", ")) },
        root: uid == 0,
        root_on_host: is_host_root(),
    };
    if p.root {
        root_warning(p.root_on_host);
    } else {
        tracing::info!("running as {}", p.user);
    }
    let _ = PROCESS.set(p.clone());
    p
}

/// A boxed block in amber, for warnings that must stand out in `docker logs`.
/// Plain text when NO_COLOR is set.
pub fn amber(lines: &[&str]) -> String {
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let bar = format!("+{}+", "-".repeat(width + 2));
    let mut out = vec![bar.clone()];
    out.extend(lines.iter().map(|l| format!("| {l:<width$} |")));
    out.push(bar);
    let text = out.join("\n");
    if std::env::var_os("NO_COLOR").is_some() { text } else { format!("\x1b[1;38;5;214m{text}\x1b[0m") }
}

fn root_warning(on_host: bool) {
    let mut lines: Vec<&str> = if on_host {
        vec!["WARNING: wranglarr is running as ROOT ON THE HOST.", ""]
    } else {
        vec![
            "WARNING: wranglarr is running as root in its container.",
            "(Root in here is your own user on the host under rootless Podman/Docker,",
            "so this is less severe, but still more than wranglarr needs.)",
            "",
        ]
    };
    lines.extend([
        "It doesn't need root. Running it as root exposes you to more:",
        "",
        "- Any bug or compromised dependency in wranglarr can read and change",
        "  every file it can see, as root: all app configs and API keys, the",
        "  backups (database dumps, the Postgres password), your /data mounts.",
        "- wranglarr can change its own files while it runs.",
        "- The Docker socket is root on the host whatever user wranglarr runs",
        "  as. Running without root keeps everything else from being root too.",
        "- If anyone gets past wranglarr's login, or it's turned off",
        "  (WRANGLARR_AUTH=off), they get all of that as root. Keep two-factor",
        "  or a passkey on, and never forward wranglarr's port to the internet.",
        "",
        "Fix: set PUID/PGID in .env to a normal user (never 0), keep",
        "  user: \"${PUID}:${PGID}\" and group_add: [\"${DOCKER_GID}\"]",
        "on the wranglarr service in compose.yaml, run ./scripts/init.sh",
        "(it sets DOCKER_GID), then: docker compose up -d wranglarr",
        "",
        "Details and a service user for your OS: wiki/Security.md",
    ]);
    eprintln!("{}", amber(&lines));
}

/// `wranglarr-prepare`: the compose service wranglarr-init. Runs as root with
/// only the chown capabilities and no network, hands wranglarr's own folders
/// to PUID:PGID, and exits, so the long-running wranglarr never needs root.
pub fn prepare_main(args: &[String]) -> i32 {
    let parse = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).map(|v| v.trim().parse::<u32>());
    let uid = match parse("PUID") {
        None => 1000,
        Some(Ok(u)) => u,
        Some(Err(_)) => {
            eprintln!("wranglarr-init: PUID/PGID must be numbers");
            return 1;
        }
    };
    let gid = match parse("PGID") {
        None => uid,
        Some(Ok(g)) => g,
        Some(Err(_)) => {
            eprintln!("wranglarr-init: PUID/PGID must be numbers");
            return 1;
        }
    };
    if uid == 0 || gid == 0 {
        eprintln!(
            "wranglarr-init: \n{}",
            amber(&[
                "WARNING: PUID/PGID is 0, so wranglarr and every app run as ROOT.",
                "",
                "Files they create on your media are root-owned, and a bug or a",
                "compromised app can change anything it can see.",
                "",
                "Fix: set PUID/PGID in .env to a normal user (id -u / id -g, or",
                "568:568 on TrueNAS), then: docker compose up -d",
                "Details and a service user for your OS: wiki/Security.md",
            ])
        );
    }
    if euid() != 0 {
        println!("wranglarr-init: not root ({}), nothing to hand over", euid());
        return 0;
    }
    let dirs: Vec<String> = if args.is_empty() { vec!["/config".into(), "/backups".into()] } else { args.to_vec() };
    for d in dirs {
        let p = Path::new(&d);
        if !p.exists() {
            continue;
        }
        let n = chown_tree(p, uid, gid);
        let what = if n > 0 { format!("{n} entries") } else { "already".into() };
        println!("wranglarr-init: {d}: {what} handed to {uid}:{gid}");
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn amber_box() {
        let s = super::amber(&["ab", "abcd"]);
        assert!(s.contains("| ab   |") && s.contains("+------+"), "{s}");
    }

    #[test]
    fn stack_owner_from_the_environment() {
        let owner = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            super::StackOwner::read(&|k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())).ids()
        };
        assert_eq!(owner(&[("PUID", "1000"), ("PGID", "100")]), Some((1000, 100)));
        assert_eq!(owner(&[("PUID", "1000"), ("PGID", " ")]), Some((1000, 1000)));
        assert_eq!(owner(&[("PUID", "0")]), None, "root is no owner");
        assert_eq!(owner(&[("PUID", "1000"), ("PGID", "staff")]), None);
        assert_eq!(owner(&[]), None);
    }

    #[test]
    fn groups_after_dropping_root() {
        assert_eq!(super::groups_for(1000, Some(998), Some("998")), vec![1000, 998]);
        assert_eq!(
            super::groups_for(1000, Some(0), Some(" 963 ")),
            vec![1000, 0, 963],
            "keep the socket's mapped group"
        );
        assert_eq!(super::groups_for(1000, Some(0), Some("0")), vec![1000, 0]);
        assert_eq!(super::groups_for(1000, None, Some("0")), vec![1000, 0]);
        assert_eq!(super::groups_for(100, None, Some("docker")), vec![100]);
    }
}
