// SPDX-License-Identifier: GPL-3.0-or-later
//! On-disk backup store.
//!
//! ```text
//! <root>/<suite>/<version>/manifest.json
//! <root>/<suite>/<version>/globals.sql
//! <root>/<suite>/<version>/<app>/<app>_backup_….zip
//! <root>/<suite>/<version>/<app>/<db>.dump
//! <root>/<suite>/<version>/<app>/config/           plain copy of the app's /config
//! <root>/<suite>/<version>/<app>/config.sha256     sha256sum -c format
//! <root>/<suite>/<version>/compose.yaml, .env, RESTORE.md, postgres/initdb/
//! ```
//!
//! The manifest is the source of truth; nothing about a version lives only in
//! the state db, so the store can be copied elsewhere and still be understood.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::manifest::{Manifest, VersionStatus};
use crate::owner::StackOwner;

pub const MANIFEST: &str = "manifest.json";
pub const VERSION_FMT: &str = "%Y%m%dT%H%M%SZ";

/// Backup names are single visible path components, including on Windows.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\']) && !name.chars().any(char::is_control)
}

/// Check an untrusted relative path before it reaches a filesystem or CLI.
/// Reject symlinks at every level, including the final file; restored snapshots
/// must contain their data rather than references into the live filesystem.
pub fn checked_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let rel = Path::new(relative);
    if relative.is_empty()
        || relative.contains('\\')
        || relative.chars().any(char::is_control)
        || rel.components().any(|c| !matches!(c, Component::Normal(_)))
        || rel.is_absolute()
    {
        return Err(Error::invalid("unsafe backup path"));
    }
    let mut path = root.to_path_buf();
    for part in rel.components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_symlink() => return Err(Error::invalid("symlinks are not allowed in backup paths")),
            Ok(_) => {}
            Err(e) => return Err(Error::internal(format!("{}: {e}", path.display()))),
        }
    }
    Ok(path)
}

pub fn sha256(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Bytes in every file under `path` (symlinks not followed).
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let Ok(rd) = std::fs::read_dir(path) else { return 0 };
    for e in rd.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            total += dir_size(&e.path());
        } else if t.is_file() {
            total += e.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

pub fn parse_version(ts: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(ts, VERSION_FMT).ok().map(|n| n.and_utc())
}

/// The versions on disk, one directory per suite and version. Every method
/// reads or writes the filesystem on the calling thread, and a suite's scan
/// reads each version's manifest: async code calls them from a blocking thread
/// (`spawn_blocking`), never on the async threads directly.
pub struct BackupStore {
    pub root: PathBuf,
    /// Where the store lives on the host, for messages (`copy <host path>/…`).
    pub host_root: String,
    /// Who the versions are handed to when wranglarr runs as root on the host.
    owner: StackOwner,
}

fn sorted_dirs_desc(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v
}

impl BackupStore {
    /// The store at `root` (created when missing), which is `host_root` on the
    /// host (empty when unknown).
    pub fn new(root: &Path, host_root: &str) -> Result<BackupStore> {
        std::fs::create_dir_all(root)?;
        Ok(BackupStore { root: root.to_path_buf(), host_root: host_root.to_string(), owner: StackOwner::default() })
    }

    /// The store, handing what it writes to `owner`.
    pub fn with_owner(self, owner: StackOwner) -> BackupStore {
        BackupStore { owner, ..self }
    }

    pub fn suite_dir(&self, suite: &str) -> PathBuf {
        self.root.join(suite)
    }

    pub fn version_dir(&self, suite: &str, version: &str) -> Result<PathBuf> {
        if !valid_name(suite) || !valid_name(version) {
            return Err(Error::invalid("bad version id"));
        }
        let suite_dir = self.suite_dir(suite);
        for path in [&suite_dir, &suite_dir.join(version)] {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) {
                return Err(Error::invalid("symlinks are not allowed in backup paths"));
            }
        }
        Ok(suite_dir.join(version))
    }

    pub fn new_version(&self, suite: &str) -> Result<(String, PathBuf)> {
        let version = Utc::now().format(VERSION_FMT).to_string();
        self.version_dir(suite, &version)?;
        std::fs::create_dir_all(self.suite_dir(suite))?;
        for n in 0u64.. {
            let name = if n == 0 { version.clone() } else { format!("{version}-{n}") };
            let d = self.version_dir(suite, &name)?;
            match std::fs::create_dir(&d) {
                Ok(()) => return Ok((name, d)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::internal("version namespace exhausted"))
    }

    /// Write the version's manifest, with `total_bytes` counted now.
    pub fn write_manifest(&self, suite: &str, version: &str, manifest: &mut Manifest) -> Result<()> {
        let d = self.version_dir(suite, version)?;
        manifest.total_bytes = Some(dir_size(&d));
        crate::files::write_atomic(&d.join(MANIFEST), serde_json::to_string_pretty(manifest)?.as_bytes(), None)?;
        self.owner.give(&d, true);
        if let Some(p) = d.parent() {
            self.owner.give(p, false);
        }
        self.owner.give(&self.root, false);
        Ok(())
    }

    /// None for a bad id, a missing version or an unreadable manifest.
    pub fn read_manifest(&self, suite: &str, version: &str) -> Option<Manifest> {
        let p = checked_path(&self.version_dir(suite, version).ok()?, MANIFEST).ok()?;
        let text = std::fs::read_to_string(&p).ok()?;
        match serde_json::from_str::<Manifest>(&text) {
            Ok(mut m) => {
                // The directory owns identity; a tampered manifest must not
                // redirect pruning or restore to a different version.
                m.version = version.into();
                m.suite = suite.into();
                Some(m)
            }
            Err(e) => {
                tracing::warn!("unreadable manifest {}: {e}", p.display());
                None
            }
        }
    }

    /// Newest first.
    pub fn list_versions(&self, suite: &str) -> Vec<Manifest> {
        if !valid_name(suite) || self.version_dir(suite, "probe").is_err() {
            return vec![];
        }
        let sd = self.suite_dir(suite);
        sorted_dirs_desc(&sd)
            .into_iter()
            .map(|v| {
                self.read_manifest(suite, &v)
                    .unwrap_or_else(|| Manifest::incomplete(suite, &v, Some(dir_size(&sd.join(&v)))))
            })
            .collect()
    }

    /// The newest version's manifest, without reading the rest.
    pub fn latest(&self, suite: &str) -> Option<Manifest> {
        self.version_dir(suite, "probe").ok()?;
        let v = sorted_dirs_desc(&self.suite_dir(suite)).into_iter().next()?;
        Some(self.read_manifest(suite, &v).unwrap_or_else(|| Manifest::incomplete(suite, &v, None)))
    }

    pub fn delete_version(&self, suite: &str, version: &str) -> Result<()> {
        let d = self.version_dir(suite, version)?;
        if !d.is_dir() {
            return Err(Error::not_found("no such version"));
        }
        std::fs::remove_dir_all(&d)?;
        Ok(())
    }

    /// Retention counts good versions only, so failed backups never push a good
    /// one out: keep the newest `keep_last` good versions and/or those younger
    /// than `keep_days`, and always the newest good one. A failed, partial or
    /// unfinished version goes once a newer good version exists.
    pub fn prune(&self, suite: &str, keep_last: i64, keep_days: i64) -> Result<Vec<String>> {
        let cutoff = (keep_days > 0)
            .then(|| chrono::Duration::try_days(keep_days).and_then(|d| Utc::now().checked_sub_signed(d)))
            .flatten();
        let mut doomed = vec![];
        let (mut good, mut seen_good) = (0, false);
        for m in self.list_versions(suite) {
            let v = m.version;
            let Some(created) = parse_version(v.split('-').next().unwrap_or("")) else { continue };
            if m.status != VersionStatus::Ok {
                if seen_good {
                    doomed.push(v); // superseded by a newer good version
                }
                continue;
            }
            good += 1;
            if good == 1 {
                seen_good = true; // the newest good version always stays
            } else if (keep_last > 0 && good > keep_last) || cutoff.is_some_and(|c| created < c) {
                doomed.push(v);
            }
        }
        for v in &doomed {
            self.delete_version(suite, v)?;
        }
        Ok(doomed)
    }

    pub fn usage(&self, suite: &str) -> Value {
        let versions = self.list_versions(suite);
        let bytes: u64 = versions.iter().map(Manifest::bytes).sum();
        json!({"versions": versions.len(), "bytes": bytes, "newest": versions.first()})
    }

    pub fn total_usage(&self) -> u64 {
        if self.root.exists() { dir_size(&self.root) } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BackupStore) {
        let t = tempfile::tempdir().expect("tmp");
        let s = BackupStore::new(&t.path().join("store"), "").expect("store");
        (t, s)
    }

    fn put(s: &BackupStore, suite: &str, v: &str, status: &str) {
        std::fs::create_dir_all(s.version_dir(suite, v).expect("id")).expect("mkdir");
        let mut m: Manifest = serde_json::from_value(json!({"version": v, "status": status})).expect("manifest");
        s.write_manifest(suite, v, &mut m).expect("write");
    }

    fn left(s: &BackupStore, suite: &str) -> Vec<String> {
        s.list_versions(suite).into_iter().map(|m| m.version).collect()
    }

    #[test]
    fn prune_counts_good_versions_only() {
        let (_t, s) = store();
        for (v, st) in [
            ("20260101T000000Z", "ok"),
            ("20260102T000000Z", "failed"),
            ("20260103T000000Z", "ok"),
            ("20260104T000000Z", "failed"),
        ] {
            put(&s, "s", v, st);
        }
        let mut gone = s.prune("s", 1, 0).expect("prune");
        gone.sort();
        assert_eq!(gone, ["20260101T000000Z", "20260102T000000Z"]);
        assert_eq!(left(&s, "s"), ["20260104T000000Z", "20260103T000000Z"]);
    }

    #[test]
    fn failures_never_push_out_the_only_good_version() {
        let (_t, s) = store();
        put(&s, "f", "20260101T000000Z", "ok");
        for v in ["20260201T000000Z", "20260202T000000Z", "20260203T000000Z"] {
            put(&s, "f", v, "failed");
        }
        s.prune("f", 1, 0).expect("prune");
        assert_eq!(left(&s, "f").len(), 4);
    }

    #[test]
    fn bad_ids() {
        let (_t, s) = store();
        assert!(s.version_dir("s", ".hidden").is_err());
        assert!(s.version_dir("s", "a/b").is_err());
        assert!(s.read_manifest("s", ".hidden").is_none());
        assert!(matches!(s.delete_version("s", "20200101T000000Z"), Err(Error::NotFound(_))));
    }

    #[test]
    fn incomplete_version_listed() {
        let (_t, s) = store();
        std::fs::create_dir_all(s.suite_dir("s").join("20260101T000000Z")).expect("mkdir");
        let v = s.list_versions("s");
        assert_eq!(v[0].status, VersionStatus::Incomplete);
    }
    #[test]
    fn concurrent_versions_are_distinct() {
        let (_t, store) = store();
        let store = std::sync::Arc::new(store);
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let store = store.clone();
                std::thread::spawn(move || store.new_version("s").expect("version").0)
            })
            .collect();
        let names: std::collections::HashSet<_> = threads.into_iter().map(|t| t.join().expect("thread")).collect();
        assert_eq!(names.len(), 16);
    }

    #[test]
    fn extreme_retention_never_panics() {
        let (_t, s) = store();
        put(&s, "s", "20260101T000000Z", "ok");
        put(&s, "s", "20260102T000000Z", "ok");
        assert!(s.prune("s", 0, i64::MAX).expect("prune").is_empty());
    }

    #[test]
    fn manifest_identity_comes_from_directory() {
        let (_t, s) = store();
        let dir = s.new_version("s").expect("version");
        std::fs::write(dir.1.join(MANIFEST), r#"{"version":"other","suite":"elsewhere"}"#).expect("write");
        let manifest = s.read_manifest("s", &dir.0).expect("manifest");
        assert_eq!(manifest.version, dir.0);
        assert_eq!(manifest.suite, "s");
        std::fs::write(dir.1.join(MANIFEST), "[]").expect("write");
        assert!(s.read_manifest("s", &dir.0).is_none());
    }

    #[test]
    fn paths_cannot_escape_or_follow_symlinks() {
        let (t, s) = store();
        for name in [
            "..", "../s", "/tmp", "a\\b", "x
",
        ] {
            assert!(s.version_dir(name, "v").is_err(), "{name:?}");
            assert!(s.version_dir("s", name).is_err(), "{name:?}");
        }
        std::os::unix::fs::symlink(t.path(), s.root.join("linked")).expect("symlink");
        assert!(s.new_version("linked").is_err());
        assert!(s.list_versions("linked").is_empty());
        assert!(s.latest("linked").is_none());
        for relative in ["../file", "/file", "x/../../file", "a\\b"] {
            assert!(checked_path(t.path(), relative).is_err());
        }
        std::os::unix::fs::symlink(t.path(), t.path().join("linked")).expect("symlink");
        assert!(checked_path(t.path(), "linked/file").is_err());
    }

    #[test]
    fn manifest_write_does_not_follow_old_temporary_symlink() {
        let (t, s) = store();
        let (v, d) = s.new_version("s").expect("version");
        let outside = t.path().join("outside");
        std::fs::write(&outside, "untouched").expect("write");
        std::os::unix::fs::symlink(&outside, d.join("manifest.json.tmp")).expect("symlink");
        s.write_manifest("s", &v, &mut Manifest::default()).expect("manifest");
        assert_eq!(std::fs::read_to_string(outside).expect("read"), "untouched");
    }
}
