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
use crate::manifest::{Manifest, PRE_UPDATE, VersionStatus};
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

/// How many pre-update backups a suite keeps ([`BackupStore::prune`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreUpdateKeep {
    /// The newest this many always stay (at least one); a newer one pushes the
    /// rest out.
    pub keep: i64,
    /// Except in a run of quick updates: an older one also stays while each
    /// update after it came within this many days of the one before, so the
    /// version in between never had time to prove itself (0: no exception).
    pub settle_days: i64,
}

impl PreUpdateKeep {
    pub const DEFAULT: PreUpdateKeep = PreUpdateKeep { keep: 1, settle_days: 7 };
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
    ///
    /// Pre-update backups ([`PRE_UPDATE`]) are counted apart, by `pre`, so
    /// scheduled backups never push out what a rollback needs. Retained
    /// manifests preserve run identity before intermediate backups are pruned.
    pub fn prune(&self, suite: &str, keep_last: i64, keep_days: i64, pre: PreUpdateKeep) -> Result<Vec<String>> {
        self.prune_at(suite, keep_last, keep_days, pre, Utc::now())
    }

    /// [`prune`](Self::prune) as of `now`.
    pub fn prune_at(
        &self,
        suite: &str,
        keep_last: i64,
        keep_days: i64,
        pre: PreUpdateKeep,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>> {
        let cutoff = (keep_days > 0)
            .then(|| chrono::Duration::try_days(keep_days).and_then(|d| now.checked_sub_signed(d)))
            .flatten();
        let mut doomed = vec![];
        let (mut good, mut pre_good) = (0, vec![]);
        for m in self.list_versions(suite) {
            let v = m.version.clone();
            let Some(created) = parse_version(v.split('-').next().unwrap_or("")) else { continue };
            let is_pre = m.trigger.as_deref() == Some(PRE_UPDATE);
            if m.status != VersionStatus::Ok {
                if (is_pre && !pre_good.is_empty()) || (!is_pre && good > 0) {
                    doomed.push(v); // superseded by a newer good version of its kind
                }
                continue;
            }
            if is_pre {
                pre_good.push((m, created));
                continue;
            }
            good += 1;
            if good > 1 && ((keep_last > 0 && good > keep_last) || cutoff.is_some_and(|c| created < c)) {
                doomed.push(v); // the newest good one always stays
            }
        }
        let versions: Vec<PreUpdateVersion> = pre_good
            .iter()
            .map(|(m, created)| PreUpdateVersion {
                version: m.version.clone(),
                created: *created,
                run: m.pre_update_run.clone(),
            })
            .collect();
        let plan = pre_update_plan(&versions, pre, now);
        // Persist run identity on survivors before deleting any evidence. A write
        // failure aborts pruning; an interrupted prune can safely repeat this plan.
        for ((mut m, _), run) in pre_good.into_iter().zip(plan.runs) {
            if !plan.doomed.contains(&m.version) && m.pre_update_run.as_ref() != Some(&run) {
                m.pre_update_run = Some(run);
                let version = m.version.clone();
                self.write_manifest(suite, &version, &mut m)?;
            }
        }
        doomed.extend(plan.doomed);
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

/// One good pre-update backup. `run` survives deletion of intermediate backups.
#[derive(Clone)]
struct PreUpdateVersion {
    version: String,
    created: DateTime<Utc>,
    run: Option<String>,
}

struct PreUpdatePlan {
    doomed: Vec<String>,
    runs: Vec<String>,
}

/// The newest `keep` stay, plus the oldest available backup in the newest run
/// until its newest version settles. Runs are assigned oldest first: a new
/// backup inherits the preceding run across a short gap, while persisted run
/// identity takes precedence over gaps created by earlier pruning. The plan's
/// run IDs must be persisted on survivors before any versions are removed.
fn pre_update_plan(pre: &[PreUpdateVersion], keep: PreUpdateKeep, now: DateTime<Utc>) -> PreUpdatePlan {
    let settle = chrono::Duration::try_days(keep.settle_days.max(0)).unwrap_or_default();
    let n = usize::try_from(keep.keep.max(1)).unwrap_or(1);
    let mut runs = vec![String::new(); pre.len()];
    for i in (0..pre.len()).rev() {
        let known = pre[i].run.as_ref().filter(|id| {
            valid_name(id) && parse_version(id.split('-').next().unwrap_or("")).is_some_and(|t| t <= pre[i].created)
        });
        runs[i] = match known {
            Some(id) => id.clone(),
            None if i + 1 < pre.len() && pre[i].created - pre[i + 1].created < settle => runs[i + 1].clone(),
            None => pre[i].version.clone(),
        };
    }
    let Some(newest) = pre.first() else { return PreUpdatePlan { doomed: vec![], runs } };
    let settled = now - newest.created >= settle;
    let anchor = runs.iter().rposition(|run| run == &runs[0]).unwrap_or(0);
    let doomed = pre
        .iter()
        .enumerate()
        .filter(|(i, _)| *i >= n && (settled || *i != anchor))
        .map(|(_, version)| version.version.clone())
        .collect();
    PreUpdatePlan { doomed, runs }
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
        put_by(s, suite, v, status, "schedule");
    }

    fn put_by(s: &BackupStore, suite: &str, v: &str, status: &str, trigger: &str) {
        std::fs::create_dir_all(s.version_dir(suite, v).expect("id")).expect("mkdir");
        let mut m: Manifest =
            serde_json::from_value(json!({"version": v, "status": status, "trigger": trigger})).expect("manifest");
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
        let mut gone = s.prune("s", 1, 0, PreUpdateKeep::DEFAULT).expect("prune");
        gone.sort();
        assert_eq!(gone, ["20260101T000000Z", "20260102T000000Z"]);
        assert_eq!(left(&s, "s"), ["20260104T000000Z", "20260103T000000Z"]);
    }

    #[test]
    fn scheduled_backups_never_push_out_a_pre_update_one() {
        let (_t, s) = store();
        put_by(&s, "s", "20200101T000000Z", "ok", PRE_UPDATE);
        put_by(&s, "s", "20191231T000000Z", "failed", PRE_UPDATE);
        for v in ["20260101T000000Z", "20260102T000000Z", "20260103T000000Z"] {
            put(&s, "s", v, "ok");
        }
        let gone = s.prune("s", 1, 0, PreUpdateKeep::DEFAULT).expect("prune");
        assert_eq!(gone.len(), 3, "two scheduled ones, and the failed pre-update one: {gone:?}");
        assert_eq!(left(&s, "s"), ["20260103T000000Z", "20200101T000000Z"]);
    }

    #[test]
    fn a_newer_pre_update_backup_pushes_the_older_ones_out() {
        let (_t, s) = store();
        for v in ["20260101T000000Z", "20260201T000000Z", "20260301T000000Z"] {
            put_by(&s, "s", v, "ok", PRE_UPDATE);
        }
        let gone = s.prune_at("s", 1, 0, PreUpdateKeep::DEFAULT, at("20260302T000000Z")).expect("prune");
        assert_eq!(gone.len(), 2, "a month apart: no run, only the newest");
        assert_eq!(left(&s, "s"), ["20260301T000000Z"]);
    }

    fn at(v: &str) -> DateTime<Utc> {
        parse_version(v).expect("a version time")
    }

    #[test]
    fn a_run_of_quick_updates_keeps_the_backup_from_before_it() {
        let (_t, s) = store();
        // a month on one version, then catching up: three updates in two days
        for v in ["20260101T000000Z", "20260201T000000Z", "20260201T120000Z", "20260202T000000Z"] {
            put_by(&s, "s", v, "ok", PRE_UPDATE);
        }
        let d = PreUpdateKeep::DEFAULT;
        let gone = s.prune_at("s", 1, 0, d, at("20260203T000000Z")).expect("prune");
        assert_eq!(gone, ["20260201T120000Z", "20260101T000000Z"], "the newest, and the one from before the run");
        assert_eq!(left(&s, "s"), ["20260202T000000Z", "20260201T000000Z"]);
        // the last version has run its week: the run is over
        let gone = s.prune_at("s", 1, 0, d, at("20260209T000000Z")).expect("prune");
        assert_eq!(gone, ["20260201T000000Z"]);
        assert_eq!(left(&s, "s"), ["20260202T000000Z"]);
    }

    #[test]
    fn a_settled_run_keeps_only_the_newest() {
        let (_t, s) = store();
        // three updates on three days, then months without another
        for v in ["20260101T000000Z", "20260102T000000Z", "20260103T000000Z"] {
            put_by(&s, "s", v, "ok", PRE_UPDATE);
        }
        let gone = s.prune_at("s", 1, 0, PreUpdateKeep::DEFAULT, at("20261029T000000Z")).expect("prune");
        assert_eq!(gone, ["20260102T000000Z", "20260101T000000Z"]);
        let none = PreUpdateKeep { keep: 1, settle_days: 0 };
        put_by(&s, "s", "20261028T000000Z", "ok", PRE_UPDATE);
        assert_eq!(s.prune_at("s", 1, 0, none, at("20261028T000100Z")).expect("prune"), ["20260103T000000Z"]);
    }

    #[test]
    fn daily_updates_that_never_settle_keep_two() {
        // a development track: an update every day for two months
        let mut pre: Vec<PreUpdateVersion> = (0..60)
            .map(|d| {
                let t = at("20260101T040000Z") + chrono::Duration::days(d);
                PreUpdateVersion { version: t.format(VERSION_FMT).to_string(), created: t, run: None }
            })
            .collect();
        pre.reverse();
        let now = pre[0].created + chrono::Duration::hours(1);
        let gone = pre_update_plan(&pre, PreUpdateKeep::DEFAULT, now).doomed;
        assert_eq!(gone.len(), 58, "the newest and the one from before the run stay");
        assert!(!gone.contains(&pre[0].version) && !gone.contains(&pre[59].version));
        let three = PreUpdateKeep { keep: 3, settle_days: 7 };
        assert_eq!(pre_update_plan(&pre, three, now).doomed.len(), 56);
    }

    #[test]
    fn repeated_pruning_keeps_the_original_run_anchor_across_restarts() {
        for keep in [1, 3] {
            let (_t, initial) = store();
            let root = initial.root.clone();
            let policy = PreUpdateKeep { keep, settle_days: 7 };
            let anchor = "20260101T040000Z";
            for day in 0..60 {
                // Reopen the store: the run identity must live on disk.
                let s = BackupStore::new(&root, "").expect("reopen");
                let time = at(anchor) + chrono::Duration::days(day);
                let version = time.format(VERSION_FMT).to_string();
                put_by(&s, "s", &version, "ok", PRE_UPDATE);
                let now = time + chrono::Duration::hours(1);
                s.prune_at("s", 1, 0, policy, now).expect("prune");
                let retained = s.list_versions("s");
                assert!(retained.iter().any(|m| m.version == anchor), "anchor lost on day {day}");
                assert_eq!(retained[0].version, version);
                assert!(retained.len() <= keep as usize + 1);
                assert!(retained.iter().all(|m| m.pre_update_run.as_deref() == Some(anchor)));
                assert!(s.prune_at("s", 1, 0, policy, now).expect("repeat prune").is_empty());
            }
            let s = BackupStore::new(&root, "").expect("reopen");
            s.prune_at("s", 1, 0, policy, at(anchor) + chrono::Duration::days(67)).expect("settled prune");
            assert_eq!(s.list_versions("s").len(), keep as usize);
            assert!(!left(&s, "s").iter().any(|v| v == anchor));
        }
    }

    #[test]
    fn a_real_settling_gap_starts_a_new_run_after_repeated_pruning() {
        let (_t, s) = store();
        for day in 1..=12 {
            let v = format!("202601{day:02}T000000Z");
            put_by(&s, "s", &v, "ok", PRE_UPDATE);
            s.prune_at("s", 1, 0, PreUpdateKeep::DEFAULT, at(&v)).expect("prune");
        }
        let v = "20260201T000000Z";
        put_by(&s, "s", v, "ok", PRE_UPDATE);
        s.prune_at("s", 1, 0, PreUpdateKeep::DEFAULT, at(v)).expect("new run");
        assert_eq!(left(&s, "s"), [v]);
        assert_eq!(s.read_manifest("s", v).expect("manifest").pre_update_run.as_deref(), Some(v));
    }

    #[test]
    fn manually_removed_anchor_falls_back_to_the_oldest_available_run_member() {
        let (_t, s) = store();
        let policy = PreUpdateKeep { keep: 3, settle_days: 7 };
        for day in 1..=12 {
            let v = format!("202601{day:02}T000000Z");
            put_by(&s, "s", &v, "ok", PRE_UPDATE);
            s.prune_at("s", 1, 0, policy, at(&v)).expect("prune");
        }
        s.delete_version("s", "20260101T000000Z").expect("manual deletion");
        let v = "20260113T000000Z";
        put_by(&s, "s", v, "ok", PRE_UPDATE);
        s.prune_at("s", 1, 0, PreUpdateKeep::DEFAULT, at(v)).expect("prune");
        assert_eq!(left(&s, "s"), [v, "20260110T000000Z"]);
    }

    #[test]
    fn a_failed_pre_update_backup_goes_once_a_newer_good_one_exists() {
        let (_t, s) = store();
        put_by(&s, "s", "20260101T000000Z", "ok", PRE_UPDATE);
        put_by(&s, "s", "20260102T000000Z", "failed", PRE_UPDATE);
        let now = at("20260102T010000Z");
        assert!(s.prune_at("s", 1, 0, PreUpdateKeep::DEFAULT, now).expect("prune").is_empty(), "nothing newer is good");
        put_by(&s, "s", "20260103T000000Z", "ok", PRE_UPDATE);
        let gone = s.prune_at("s", 1, 0, PreUpdateKeep::DEFAULT, at("20260103T010000Z")).expect("prune");
        assert_eq!(gone, ["20260102T000000Z"], "the good ones are a run: both stay");
    }

    #[test]
    fn failures_never_push_out_the_only_good_version() {
        let (_t, s) = store();
        put(&s, "f", "20260101T000000Z", "ok");
        for v in ["20260201T000000Z", "20260202T000000Z", "20260203T000000Z"] {
            put(&s, "f", v, "failed");
        }
        s.prune("f", 1, 0, PreUpdateKeep::DEFAULT).expect("prune");
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
        assert!(s.prune("s", 0, i64::MAX, PreUpdateKeep::DEFAULT).expect("prune").is_empty());
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
