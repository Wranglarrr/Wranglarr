// SPDX-License-Identifier: GPL-3.0-or-later
//! App config directories, kept in their natural format.
//!
//! Every version holds a plain copy of each app's /config (minus logs, caches
//! and live database files) at `<app>/config/`, plus `<app>/config.sha256` in
//! `sha256sum` format. That directory can be mounted straight back into the
//! app's container as /config: no wranglarr needed.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use glob::Pattern;
use serde::Serialize;
use serde_json::{Value, json};

use crate::owner;

mod dir;
mod restore;
mod snapshot;
pub use dir::{Dir, NewFile, Owner};
pub use restore::{
    DEFAULT_PRE_RESTORE, restore_config, restore_sqlite, set_pre_restore_dir, sqlite_in, sqlite_restore_blocker,
    validate_restore_source,
};
pub use snapshot::{Snapshot, snapshot_config, verify_snapshot};

const COMMON_SKIP_DIRS: &[&str] = &["__pycache__", "ipc-socket", ".cache"];
const COMMON_SKIP_FILES: &[&str] = &["*.pid", "*.lock", "*.log", "*.log.[0-9]*", "*-journal", "*.tmp"];
const SQLITE: &[&str] = &["*.db", "*.db-shm", "*.db-wal"];

/// What to leave out when copying a service's config folder, and how its own
/// backup zip maps onto that folder.
#[derive(Clone, Copy, Debug)]
pub struct CopyRules {
    /// Directories (any depth): logs, caches, the app's own backups.
    pub skip_dirs: &'static [&'static str],
    /// File globs, besides the common ones.
    pub skip_files: &'static [&'static str],
    /// Live SQLite files (`*.db`, its `-shm`/`-wal`) are left out too.
    pub skip_sqlite: bool,
    /// Files directly in these folders (not their subfolders) are downloaded caches.
    pub skip_files_in: &'static [&'static str],
    /// Backup-zip member -> where it lives in the config folder.
    pub zip_layout: &'static [(&'static str, &'static str)],
}

/// A download client's work in its config folder: what a backup keeps (its
/// torrents / NZBs and their state) and leaves out (downloaded data).
#[derive(Clone, Copy, Debug)]
pub struct Payload {
    /// Read from its config files in the folder being copied.
    pub rules: fn(&std::path::Path) -> PayloadRules,
    /// What made it into the copy: `[{"label", "count"}]`.
    pub items: fn(&std::path::Path, &PayloadRules) -> Vec<Value>,
}

/// How to copy one app's config folder: its kind's rules, and for a download
/// client what of its work to keep (from its service descriptor).
#[derive(Clone, Copy, Debug)]
pub struct CopySpec {
    pub rules: CopyRules,
    pub payload: Option<Payload>,
}

/// Directories (any depth) and file globs to leave out. Live SQLite files are
/// skipped where the rules say so: copying one while the app writes to it isn't
/// safe; on Postgres they're unused leftovers, and in SQLite mode the consistent
/// copy comes from the backup zip.
fn skip_rules(r: &CopyRules) -> (&'static [&'static str], Vec<&'static str>) {
    let mut globs = r.skip_files.to_vec();
    if r.skip_sqlite {
        globs.splice(0..0, SQLITE.iter().copied());
    }
    (r.skip_dirs, globs)
}

/// Where the files in an app's own backup zip live inside its /config.
fn zip_layout(r: &CopyRules, member: &str) -> String {
    r.zip_layout.iter().find(|(m, _)| *m == member).map_or_else(|| member.into(), |(_, to)| (*to).into())
}

fn fnmatch(name: &str, pat: &str) -> bool {
    Pattern::new(pat).is_ok_and(|p| p.matches(name))
}

/// "a/b/c" style, relative to the config root.
fn rel_str(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

// ---- download clients: back up the .torrent / .nzb files, never the downloaded data ----

/// Container path (absolute, or relative to /config as SABnzbd allows) ->
/// path relative to /config, or None when it lives outside /config.
pub fn in_config(value: &str) -> Option<String> {
    let mount = "/config";
    let v = value.trim().trim_matches('"').trim_matches('\'');
    if v.is_empty()
        || v.contains('\\')
        || v.chars().any(char::is_control)
        || Path::new(v).components().any(|c| matches!(c, Component::ParentDir))
    {
        return None;
    }
    if !v.starts_with('/') {
        let p: Vec<&str> = v.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
        let s = p.join("/");
        return (!s.is_empty()).then_some(s);
    }
    if v.trim_end_matches('/') == mount {
        return Some(String::new());
    }
    v.strip_prefix("/config/").map(|r| r.trim_end_matches('/').to_string())
}

/// `key<sep>value` lines of an ini-style file (the first of each key; comments and
/// sections skipped). Empty when it can't be read.
pub fn kv(path: &Path, sep: char) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(bytes) = fs::read(path) else { return out };
    for line in String::from_utf8_lossy(&bytes).lines() {
        let l = line.trim_start();
        if line.contains(sep)
            && !(l.starts_with('#') || l.starts_with(';') || l.starts_with('['))
            && let Some((k, v)) = line.split_once(sep)
        {
            out.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    out
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PayloadRules {
    /// Folders inside /config holding downloaded data (left out).
    pub skip: Vec<String>,
    /// Folders where only the per-job __ADMIN__ dirs (SABnzbd's NZBs) are kept.
    pub admin_only: Vec<String>,
    /// label -> path for things that live outside /config (not in the backup).
    pub outside: BTreeMap<String, String>,
    pub settings: BTreeMap<String, String>,
    pub labels: HashMap<String, String>,
}

/// Where this client keeps its torrents/NZBs and its downloaded data.
pub fn payload_rules(payload: Option<&Payload>, src: &Path) -> PayloadRules {
    let mut r = payload.map(|p| (p.rules)(src)).unwrap_or_default();
    r.skip.retain(|x| !x.is_empty()); // never skip /config itself
    r
}

fn under<'a>(rel: &str, prefixes: &'a [String]) -> Option<&'a str> {
    prefixes.iter().find(|p| rel == p.as_str() || rel.starts_with(&format!("{p}/"))).map(String::as_str)
}

fn skipped(rel: &Path, is_dir: bool, r: &CopyRules) -> bool {
    let (dirs, globs) = skip_rules(r);
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let check = if is_dir { &parts[..] } else { &parts[..parts.len().saturating_sub(1)] };
    if check.iter().any(|p| dirs.contains(&p.as_str()) || COMMON_SKIP_DIRS.contains(&p.as_str())) {
        return true;
    }
    if !is_dir {
        let parent = rel.parent().map(rel_str).unwrap_or_default();
        if r.skip_files_in.contains(&parent.as_str()) {
            return true;
        }
        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return COMMON_SKIP_FILES.iter().chain(globs.iter()).any(|g| fnmatch(&name, g));
    }
    false
}

/// Copy a regular file with its permissions and times, replacing `dst` atomically.
/// A link at `src` is refused.
pub fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut input =
        fs::OpenOptions::new().read(true).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32).open(src)?;
    let m = input.metadata()?;
    if !m.is_file() {
        return Err(std::io::Error::other("source is not a regular file"));
    }
    let parent = dst.parent().ok_or_else(|| std::io::Error::other("destination has no parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut input, &mut tmp)?;
    tmp.as_file().set_permissions(m.permissions())?;
    let times = fs::FileTimes::new().set_modified(m.modified()?).set_accessed(m.accessed()?);
    tmp.as_file().set_times(times)?;
    tmp.as_file().sync_all()?;
    tmp.persist(dst).map_err(|e| e.error)?;
    Ok(())
}

/// Put `data` at `dest`, replacing it atomically: a temporary file made
/// exclusively beside it (never through a link planted there, and readable by
/// its owner only), given the owner of `owner_like` when there is one, synced,
/// then renamed over `dest`. Readers see the old file or the new one, never a
/// partial write.
pub fn write_atomic(dest: &Path, data: &[u8], owner_like: Option<&Path>) -> std::io::Result<()> {
    use std::io::Write;
    let parent = dest.parent().ok_or_else(|| std::io::Error::other("destination has no parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    if let Some(reference) = owner_like {
        owner::like(tmp.path(), reference);
    }
    tmp.as_file().sync_all()?;
    tmp.persist(dest).map_err(|e| e.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Every file under `dir`.
pub fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            collect_files(&e.path(), out);
        } else if t.is_file() {
            out.push(e.path());
        }
    }
}

/// The files under `dest` (or its `under_dir`) whose name matches `pattern`.
pub fn count(dest: &Path, pattern: &str, under_dir: Option<&str>) -> usize {
    let base = under_dir.map_or_else(|| dest.to_path_buf(), |u| dest.join(u));
    if !base.exists() {
        return 0;
    }
    let mut all = vec![];
    collect_files(&base, &mut all);
    all.iter().filter(|p| p.file_name().is_some_and(|n| fnmatch(&n.to_string_lossy(), pattern))).count()
}

/// Whether `p` has a path component named `part`.
pub fn has_part(p: &Path, part: &str) -> bool {
    p.components().any(|c| c.as_os_str() == part)
}

/// What of the client's work made it into the copy: torrents / NZBs, not data.
pub fn payload_summary(payload: &Payload, dest: &Path, rules: &PayloadRules) -> Value {
    let items = (payload.items)(dest, rules);
    let lab = |x: &str| rules.labels.get(x).cloned().unwrap_or_else(|| "downloads".into());
    let mut excluded: Vec<String> = rules.skip.iter().map(|x| format!("{x}/: {}", lab(x))).collect();
    excluded.extend(rules.admin_only.iter().map(|x| format!("{x}/: {} (each job's NZB kept)", lab(x))));
    json!({"items": items, "excluded": excluded, "outside": rules.outside, "settings": rules.settings})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_config_paths() {
        assert_eq!(in_config("/config"), Some(String::new()));
        assert_eq!(in_config("/config/Downloads/complete/"), Some("Downloads/complete".into()));
        assert_eq!(in_config("Downloads/incomplete"), Some("Downloads/incomplete".into()));
        assert_eq!(in_config("/data/usenet"), None);
        assert_eq!(in_config(""), None);
    }

    /// Rules like an arr's (the service descriptors hold the real ones).
    pub(super) const ARR: CopyRules = CopyRules {
        skip_dirs: &["logs", "Backups", "MediaCover"],
        skip_files: &[],
        skip_sqlite: true,
        skip_files_in: &[],
        zip_layout: &[],
    };
    pub(super) const ARR_SPEC: CopySpec = CopySpec { rules: ARR, payload: None };

    #[test]
    fn skip_rules_apply() {
        assert!(skipped(Path::new("logs/radarr.txt"), false, &ARR));
        assert!(skipped(Path::new("radarr.db"), false, &ARR));
        assert!(skipped(Path::new("x.log.1"), false, &ARR));
        assert!(!skipped(Path::new("config.xml"), false, &ARR));
        let defs = CopyRules { skip_files_in: &["Definitions"], ..ARR };
        assert!(skipped(Path::new("Definitions/x.yml"), false, &defs));
        assert!(!skipped(Path::new("Definitions/Custom/x.yml"), false, &defs));
    }

    #[test]
    fn payload_paths_cannot_traverse() {
        for path in ["../outside", "/config/../outside", "x/../../outside", "x\\y"] {
            assert_eq!(in_config(path), None);
        }
    }

    #[test]
    fn atomic_copy_replaces_links_and_preserves_destination_on_failure() {
        let dir = tempfile::tempdir().expect("tmp");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        let outside = dir.path().join("outside");
        fs::write(&src, "new").expect("source");
        fs::write(&outside, "outside").expect("outside");
        std::os::unix::fs::symlink(&outside, &dst).expect("link");
        copy_file(&src, &dst).expect("copy");
        assert_eq!(fs::read_to_string(&dst).expect("read"), "new");
        assert_eq!(fs::read_to_string(&outside).expect("read"), "outside");
        fs::remove_file(&src).expect("remove");
        std::os::unix::fs::symlink(&outside, &src).expect("link");
        assert!(copy_file(&src, &dst).is_err());
        assert_eq!(fs::read_to_string(&dst).expect("read"), "new");
    }
}
