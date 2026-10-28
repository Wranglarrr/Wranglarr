// SPDX-License-Identifier: GPL-3.0-or-later
//! What the wizard looks at on the machine: where a typed host path can be
//! inspected (the mounts), who can write it, which filesystem it is on (ZFS,
//! TrueNAS), the host's users and time zone, and the VPN value checks.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

/// Where the wizard runs, and what is mounted into it.
#[derive(Debug, Clone)]
pub struct Env {
    pub in_docker: bool,
    /// data_root / data_root_2 / appdata -> the mount point inside the container.
    pub mounts: BTreeMap<&'static str, String>,
    /// TZ, when set and not empty.
    pub tz: Option<String>,
    /// WRANGLARR_IMAGE: the image this wizard ran from (the release image when unset).
    pub image: String,
}

impl Env {
    /// The wizard's environment, from the process's (read once, where it starts).
    pub fn detect() -> Env {
        Env::read(crate::config::process_env)
    }

    /// `/.dockerenv` or WRANGLARR_IN_DOCKER=1; mounts from WRANGLARR_MOUNT_*;
    /// TZ and WRANGLARR_IMAGE, in the environment `env`.
    pub fn read(env: impl Fn(&str) -> Option<String>) -> Env {
        let var = |k: &str, d: &str| env(k).unwrap_or_else(|| d.to_string());
        Env {
            in_docker: Path::new("/.dockerenv").exists() || env("WRANGLARR_IN_DOCKER").as_deref() == Some("1"),
            mounts: BTreeMap::from([
                ("data_root", var("WRANGLARR_MOUNT_DATA", "/data")),
                ("data_root_2", var("WRANGLARR_MOUNT_DATA2", "/data2")),
                ("appdata", var("WRANGLARR_MOUNT_APPDATA", "/appdata")),
            ]),
            tz: env("TZ").filter(|t| !t.is_empty()),
            image: env("WRANGLARR_IMAGE").filter(|v| !v.is_empty()).unwrap_or_else(|| crate::IMAGE.into()),
        }
    }

    /// Running natively with nothing set: no mounts to map through.
    pub fn native() -> Env {
        Env { in_docker: false, ..Env::read(|_| None) }
    }

    pub fn mount(&self, key: &str) -> Option<&str> {
        self.mounts.get(key).map(String::as_str)
    }
}

/// How a typed path is inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Empty,
    /// relative to the stack dir: always visible
    Stack,
    /// running natively: the path is the path
    Host,
    Mount,
    Unmounted,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Empty => "empty",
            Via::Stack => "stack",
            Via::Host => "host",
            Via::Mount => "mount",
            Via::Unmounted => "unmounted",
        }
    }
}

/// Where inside this process the host path `value` for `key` can be inspected.
pub fn container_path(env: &Env, target: &Path, key: &str, value: &str) -> (Option<PathBuf>, Via) {
    let value = value.trim();
    if value.is_empty() {
        return (None, Via::Empty);
    }
    let p = tidy(value);
    if !p.is_absolute() {
        return (Some(resolve(&target.join(p))), Via::Stack);
    }
    if !env.in_docker {
        return (Some(p), Via::Host);
    }
    match env.mount(key).map(Path::new).filter(|m| m.exists()) {
        Some(m) => (Some(m.to_path_buf()), Via::Mount),
        None => (None, Via::Unmounted),
    }
}

// ---- typed paths ----------------------------------------------------------------

/// A typed path made plain: `~` and `~user` expanded, `.` parts and repeated or
/// trailing slashes dropped ("" is ".").
pub fn tidy(s: &str) -> PathBuf {
    let expanded = expand_home(s);
    let p: PathBuf = Path::new(&expanded).components().filter(|c| !matches!(c, Component::CurDir)).collect();
    if p.as_os_str().is_empty() { PathBuf::from(".") } else { p }
}

/// `~` (or `~user`) at the start of a path as that home folder; unchanged when
/// there is no such user.
fn expand_home(s: &str) -> String {
    let Some(rest) = s.strip_prefix('~') else { return s.to_string() };
    let (user, tail) = rest.find('/').map_or((rest, ""), |i| (&rest[..i], &rest[i..]));
    let home = if user.is_empty() {
        std::env::var("HOME").ok().or_else(|| passwd_home_of_uid(rustix::process::getuid().as_raw()))
    } else {
        passwd_home(user)
    };
    match home {
        Some(h) => {
            let h = if h.len() > 1 { h.trim_end_matches('/') } else { h.as_str() };
            let joined = format!("{h}{tail}");
            if joined.is_empty() { "/".into() } else { joined }
        }
        None => s.to_string(),
    }
}

/// The absolute path, `..` taken off as written, with links resolved as far as
/// the path exists; the part that doesn't exist yet is kept as it is.
pub fn resolve(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut plain = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                plain.pop();
            }
            Component::CurDir => {}
            c => plain.push(c),
        }
    }
    let mut missing = vec![];
    let mut base = plain.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            return missing.iter().rev().fold(real, |p, part| p.join(part));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                base = parent;
            }
            _ => return plain,
        }
    }
}

/// The mount `p` lives on, from /proc/self/mountinfo: {point, fstype, source}.
/// For ZFS the source is the dataset name (tank/media, boot-pool/ROOT/...).
pub fn mount_of(p: &Path) -> BTreeMap<&'static str, String> {
    let rp = resolve(p).to_string_lossy().into_owned();
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else { return BTreeMap::new() };
    mount_in(&rp, &text)
}

fn mount_in(rp: &str, mountinfo: &str) -> BTreeMap<&'static str, String> {
    let mut best: BTreeMap<&'static str, String> = BTreeMap::new();
    for ln in mountinfo.lines() {
        let (left, right) = ln.split_once(" - ").unwrap_or((ln, ""));
        let (f, r): (Vec<&str>, Vec<&str>) = (left.split_whitespace().collect(), right.split_whitespace().collect());
        if f.len() < 5 || r.len() < 2 {
            continue;
        }
        let point = f[4].replace("\\040", " ");
        let inside = rp == point || rp.starts_with(&format!("{}/", point.trim_end_matches('/')));
        if inside && point.len() >= best.get("point").map_or(0, String::len) {
            best = BTreeMap::from([("point", point), ("fstype", r[0].to_string()), ("source", r[1].to_string())]);
        }
    }
    best
}

fn sorted_dir(d: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    v.sort();
    Ok(v)
}

/// Folders under `root` (up to `depth` levels) that are a different filesystem
/// from it, e.g. ZFS child datasets. Hardlinks can't cross them.
pub fn split_filesystems(root: &Path, depth: usize, limit: usize) -> Vec<String> {
    let Ok(dev) = std::fs::metadata(root).map(|m| m.dev()) else { return vec![] };
    let (mut out, mut seen, mut level) = (vec![], 0, vec![root.to_path_buf()]);
    for _ in 0..depth {
        let mut next = vec![];
        for d in &level {
            let Ok(kids) = sorted_dir(d) else { continue };
            for k in kids {
                seen += 1;
                if seen > limit {
                    return out;
                }
                let Ok(lm) = std::fs::symlink_metadata(&k) else { continue };
                if lm.file_type().is_symlink() {
                    continue;
                }
                let Ok(m) = std::fs::metadata(&k) else { continue };
                if !m.is_dir() {
                    continue;
                }
                if m.dev() != dev {
                    out.push(k.strip_prefix(root).unwrap_or(&k).to_string_lossy().into_owned());
                } else {
                    next.push(k);
                }
            }
        }
        level = next;
    }
    out
}

/// Extra checks for the data drives: hardlink-breaking splits, ZFS/TrueNAS hints.
pub fn storage_notes(p: &Path, key: &str, uid: i64, gid: i64, writable: bool) -> Vec<Value> {
    let mut notes = vec![];
    let m = mount_of(p);
    let src = m.get("source").map(String::as_str).unwrap_or("");
    if m.get("fstype").map(String::as_str) == Some("zfs") {
        if src.starts_with("boot-pool/") {
            notes.push(json!({"cls": "bad", "msg": "this is on TrueNAS's boot pool, which is replaced on upgrades: use a dataset on a data pool (/mnt/<pool>/...)"}));
        } else if !writable && (uid, gid) != (568, 568) {
            notes.push(json!({"cls": "warn", "msg": format!("ZFS dataset {src}. On TrueNAS SCALE give the dataset the 'Apps' ACL preset and use PUID/PGID 568 (the apps user), or make your own user its owner")}));
        } else {
            notes.push(json!({"cls": "ok", "msg": format!("ZFS dataset {src}")}));
        }
    }
    if key.starts_with("data_root") {
        let split = split_filesystems(p, 2, 300);
        if !split.is_empty() {
            let shown = split.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
            notes.push(json!({"cls": "bad", "msg": format!(
                "separate filesystems inside this drive (ZFS child datasets or mounts): {shown}{}. Hardlinks can't cross it, so imports become full copies. Make downloads and media plain folders in one dataset",
                if split.len() > 8 { " …" } else { "" })}));
        }
    }
    notes
}

/// A data drive under /mnt on a ZFS dataset, or the stack itself on one: TrueNAS SCALE.
pub fn looks_truenas(env: &Env, target: &Path, data_root: &str) -> bool {
    if !data_root.starts_with("/mnt/") {
        return false;
    }
    let (p, _) = container_path(env, target, "data_root", data_root);
    for q in [p.as_deref(), Some(target)] {
        let m = q.map(mount_of).unwrap_or_default();
        if m.get("fstype").map(String::as_str) == Some("zfs")
            && !m.get("source").map(String::as_str).unwrap_or("").starts_with('/')
        {
            return true;
        }
    }
    false
}

/// The machine's time zone, from its files; UTC when they don't say.
pub fn host_tz() -> String {
    if let Ok(t) = std::fs::read_to_string("/etc/timezone") {
        let t = t.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let t = target.to_string_lossy();
        if let Some((_, zone)) = t.split_once("zoneinfo/") {
            return zone.to_string();
        }
    }
    "UTC".into()
}

pub fn hostname() -> String {
    for f in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(h) = std::fs::read_to_string(f) {
            let h = h.trim();
            if !h.is_empty() {
                return h.to_string();
            }
        }
    }
    "localhost".into()
}

// ---- users and groups (/etc/passwd, /etc/group) ------------------------------------------

struct PwEntry {
    name: String,
    uid: u32,
    gid: u32,
    home: String,
}

fn passwd() -> Vec<PwEntry> {
    let text = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() < 7 || l.starts_with('#') {
                return None;
            }
            Some(PwEntry { name: f[0].into(), uid: f[2].parse().ok()?, gid: f[3].parse().ok()?, home: f[5].into() })
        })
        .collect()
}

struct GrEntry {
    name: String,
    gid: u32,
    members: Vec<String>,
}

fn groups() -> Vec<GrEntry> {
    let text = std::fs::read_to_string("/etc/group").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() < 4 || l.starts_with('#') {
                return None;
            }
            let members = f[3].split(',').filter(|m| !m.is_empty()).map(str::to_string).collect();
            Some(GrEntry { name: f[0].into(), gid: f[2].parse().ok()?, members })
        })
        .collect()
}

pub fn passwd_home(user: &str) -> Option<String> {
    passwd().into_iter().find(|p| p.name == user).map(|p| p.home)
}

pub fn passwd_home_of_uid(uid: u32) -> Option<String> {
    passwd().into_iter().find(|p| p.uid == uid).map(|p| p.home)
}

pub fn host_users() -> Vec<Value> {
    let me = rustix::process::getuid().as_raw();
    let grs = groups();
    let mut users: Vec<(u32, Value)> = passwd()
        .into_iter()
        .filter(|p| (1000..60000).contains(&p.uid) || p.uid == me)
        .map(|p| {
            let group = grs.iter().find(|g| g.gid == p.gid).map_or_else(|| p.gid.to_string(), |g| g.name.clone());
            (p.uid, json!({"name": p.name, "uid": p.uid, "gid": p.gid, "group": group}))
        })
        .collect();
    users.sort_by_key(|(uid, _)| *uid);
    users.into_iter().map(|(_, v)| v).collect()
}

/// Could uid:gid read (or write) something with this owner and mode?
pub fn can_access(meta: &std::fs::Metadata, uid: i64, gid: i64, write: bool) -> bool {
    if uid == 0 {
        return true;
    }
    let mode = meta.mode();
    let mut gids = vec![gid];
    if let Some(name) = passwd().into_iter().find(|p| i64::from(p.uid) == uid).map(|p| p.name) {
        gids.extend(groups().into_iter().filter(|g| g.members.contains(&name)).map(|g| i64::from(g.gid)));
    }
    let (w, r) = if i64::from(meta.uid()) == uid {
        (0o200, 0o400)
    } else if gids.contains(&i64::from(meta.gid())) {
        (0o020, 0o040)
    } else {
        (0o002, 0o004)
    };
    mode & (if write { w } else { r }) != 0
}

/// Whether `p` is a mount point: on another device than its parent, or `/`.
pub fn ismount(p: &Path) -> bool {
    let Ok(s1) = std::fs::symlink_metadata(p) else { return false };
    if s1.file_type().is_symlink() {
        return false;
    }
    let Ok(s2) = std::fs::symlink_metadata(resolve(&p.join(".."))) else { return false };
    s1.dev() != s2.dev() || s1.ino() == s2.ino()
}

/// (free, total) bytes of the filesystem `p` is on.
pub fn disk_usage(p: &Path) -> Option<(u64, u64)> {
    let s = rustix::fs::statvfs(p).ok()?;
    Some((s.f_bavail.saturating_mul(s.f_frsize), s.f_blocks.saturating_mul(s.f_frsize)))
}

/// `os.lchown`; -1 leaves that id as it is. A link is changed itself, never
/// what it points to.
pub fn chown(p: &Path, uid: i64, gid: i64) -> std::io::Result<()> {
    let id = |v: i64| -> std::io::Result<Option<u32>> {
        if v == -1 {
            return Ok(None);
        }
        u32::try_from(v).map(Some).map_err(|_| std::io::Error::from_raw_os_error(22))
    };
    std::os::unix::fs::lchown(p, id(uid)?, id(gid)?)
}

// ---- VPN values ---------------------------------------------------------------------------

static B64: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9+/]{43}=$").expect("static regex"));

/// `len(base64.b64decode(k, validate=True)) == 32`.
pub fn valid_wg_key(k: &str) -> bool {
    // 43 characters and the one "=" that completes the last group: exactly 32 bytes out
    // (more padding is "excess data after padding")
    B64.is_match(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wg_keys() {
        assert!(valid_wg_key(&format!("{}=", "A".repeat(43))));
        assert!(!valid_wg_key(&"A".repeat(44)));
        assert!(!valid_wg_key(&format!("{}==", "A".repeat(43))));
        assert!(!valid_wg_key(&format!("{}=", "A".repeat(42))));
        assert!(!valid_wg_key("not base64!"));
        assert!(!valid_wg_key(""));
    }

    #[test]
    fn mountinfo() {
        let mi = "22 1 0:21 / / rw - ext4 /dev/sda1 rw\n\
                  30 22 0:40 / /mnt/tank rw - zfs tank rw\n\
                  31 30 0:41 / /mnt/tank/media\\040x rw - zfs tank/media rw\n";
        let m = mount_in("/mnt/tank/media x/movies", mi);
        assert_eq!(m.get("source").map(String::as_str), Some("tank/media"));
        assert_eq!(mount_in("/mnt/tankx", mi).get("point").map(String::as_str), Some("/"));
    }

    #[test]
    fn paths() {
        let d = tempfile::tempdir().expect("tmp");
        let target = resolve(d.path());
        let native = Env::native();
        assert_eq!(container_path(&native, &target, "appdata", "  "), (None, Via::Empty));
        assert_eq!(
            container_path(&native, &target, "appdata", "./appdata/"),
            (Some(target.join("appdata")), Via::Stack)
        );
        assert_eq!(container_path(&native, &target, "data_root", "/mnt//x/"), (Some("/mnt/x".into()), Via::Host));
        let mut docker = native.clone();
        docker.in_docker = true;
        docker.mounts.insert("data_root", target.to_string_lossy().into_owned());
        docker.mounts.insert("appdata", "/nonexistent-mount".into());
        assert_eq!(container_path(&docker, &target, "data_root", "/mnt/x"), (Some(target.clone()), Via::Mount));
        assert_eq!(container_path(&docker, &target, "appdata", "/srv/appdata"), (None, Via::Unmounted));
        assert!(ismount(Path::new("/")));
        assert!(!ismount(&target.join("nope")));
        assert!(disk_usage(&target).is_some());
    }

    #[test]
    fn typed_paths() {
        assert_eq!(tidy("/mnt//data/./x/"), PathBuf::from("/mnt/data/x"));
        assert_eq!(tidy("./appdata"), PathBuf::from("appdata"));
        assert_eq!(tidy(""), PathBuf::from("."));
        assert_eq!(tidy("~nosuchuser-wranglarr/x"), PathBuf::from("~nosuchuser-wranglarr/x"));
        let d = tempfile::tempdir().expect("tmp");
        let real = resolve(d.path());
        std::fs::create_dir(real.join("target")).expect("mkdir");
        std::os::unix::fs::symlink(real.join("target"), real.join("link")).expect("link");
        assert_eq!(resolve(&real.join("link/x/../y")), real.join("target/y"));
        assert_eq!(resolve(&real.join("a/./b/..")), real.join("a"));
    }

    #[test]
    fn access_bits() {
        let d = tempfile::tempdir().expect("tmp");
        let f = d.path().join("f");
        std::fs::write(&f, "").expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        let m = std::fs::metadata(&f).expect("meta");
        let (u, g) = (i64::from(m.uid()), i64::from(m.gid()));
        assert!(can_access(&m, u, g, true) && can_access(&m, u, g, false));
        assert!(can_access(&m, 0, 0, true), "root");
        assert!(!can_access(&m, 59999, 59999, false), "others get nothing");
    }
}
