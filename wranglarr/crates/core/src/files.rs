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
use std::sync::OnceLock;

use glob::Pattern;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::owner;
use crate::store::sha256;

mod dir;
pub use dir::{Dir, NewFile, Owner};

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

/// The plain copy of an app's config folder, as its manifest entry records it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub path: String,
    pub files: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub checksums: String,
    /// Left out against our will: the backup reports it.
    pub unreadable: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

fn io_reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "Permission denied".into(),
        _ => e.to_string(),
    }
}

/// Copy `src` -> `app_dir/config` (natural layout) and write `app_dir/config.sha256`.
pub fn snapshot_config(
    name: &str,
    spec: &CopySpec,
    src: &Path,
    app_dir: &Path,
    zip_overlay: Option<&Path>,
) -> Result<Snapshot> {
    let rules = payload_rules(spec.payload.as_ref(), src);
    if !fs::symlink_metadata(src).is_ok_and(|m| m.is_dir()) {
        return Err(Error::invalid(format!(
            "{name}: config dir {} is not a directory (mount the app's /config into wranglarr)",
            src.display()
        )));
    }
    let dest = app_dir.join("config");
    if fs::symlink_metadata(&dest).is_ok_and(|m| m.is_symlink()) {
        return Err(Error::invalid("symlinks are not allowed in backup paths"));
    }
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::create_dir_all(&dest)?;
    let mut skipped_n = 0u64;
    let mut unreadable = vec![];

    // depth-first walk like os.walk (top-down), pruning skipped directories
    let mut stack = vec![PathBuf::new()];
    while let Some(rroot) = stack.pop() {
        let root = src.join(&rroot);
        let rd = match fs::read_dir(&root) {
            Ok(rd) => rd,
            Err(e) => {
                unreadable.push(format!("{}/ ({})", rel_str(&rroot), io_reason(&e)));
                continue;
            }
        };
        let mut entries = Vec::new();
        for entry in rd {
            match entry {
                Ok(entry) => entries.push(entry),
                Err(e) => unreadable.push(format!("{}/ ({})", rel_str(&rroot), io_reason(&e))),
            }
        }
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = vec![];
        for e in entries {
            let rel = rroot.join(e.file_name());
            let sp = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let rs = rel_str(&rel);
                if skipped(&rel, true, &spec.rules) || under(&rs, &rules.skip).is_some() {
                    skipped_n += 1;
                } else {
                    fs::create_dir_all(dest.join(&rel))?;
                    subdirs.push(rel);
                }
                continue;
            }
            let rs = rel_str(&rel);
            let admin_only = under(&rs, &rules.admin_only).is_some() && !rs.split('/').any(|p| p == "__ADMIN__");
            if ft.is_symlink() || !ft.is_file() || skipped(&rel, false, &spec.rules) || admin_only {
                skipped_n += 1;
                continue;
            }
            match copy_file(&sp, &dest.join(&rel)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => skipped_n += 1, // vanished mid-copy
                Err(e) => {
                    tracing::warn!("{name}: couldn't copy {rs}: {e}");
                    unreadable.push(format!("{rs} ({})", io_reason(&e)));
                }
            }
        }
        // os.walk visits subdirectories in order; a stack reverses, so push backwards
        stack.extend(subdirs.into_iter().rev());
    }

    if let Some(zp) = zip_overlay {
        let f = fs::File::open(zp)?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| Error::invalid(format!("{}: {e}", zp.display())))?;
        for i in 0..z.len() {
            let mut m = z.by_index(i).map_err(|e| Error::invalid(format!("{}: {e}", zp.display())))?;
            let fname = m.name().map_err(|e| Error::invalid(format!("{}: {e}", zp.display())))?.to_string();
            let p = Path::new(&fname);
            if m.is_dir()
                || m.is_symlink()
                || fname.contains('\\')
                || fname.chars().any(char::is_control)
                || p.is_absolute()
                || p.components().any(|c| !matches!(c, Component::Normal(_)))
            {
                continue;
            }
            let target = dest.join(zip_layout(&spec.rules, &fname));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&target)?;
            std::io::copy(&mut m, &mut out)?;
        }
    }

    let mut all = vec![];
    collect_files(&dest, &mut all);
    // pathlib sorts by path parts, not by the whole string
    all.sort_by_key(|p| p.components().map(|c| c.as_os_str().to_os_string()).collect::<Vec<_>>());
    let app_name = app_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mut files, mut total, mut lines) = (0u64, 0u64, String::new());
    for p in &all {
        files += 1;
        total += fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let rel = rel_str(p.strip_prefix(&dest).unwrap_or(p));
        // paths relative to the version folder: `sha256sum -c */config.sha256` from there
        lines.push_str(&format!("{}  {app_name}/config/{rel}\n", sha256(p)?));
    }
    fs::write(app_dir.join("config.sha256"), lines)?;
    let payload = spec.payload.map(|p| payload_summary(&p, &dest, &rules));
    Ok(Snapshot {
        path: format!("{app_name}/config"),
        files,
        bytes: total,
        skipped: skipped_n,
        checksums: format!("{app_name}/config.sha256"),
        unreadable,
        payload,
    })
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

pub fn verify_snapshot(app_dir: &Path) -> Result<()> {
    if !fs::symlink_metadata(app_dir)?.is_dir() {
        return Err(Error::invalid("snapshot app folder must be a directory, not a symlink"));
    }
    let sums = crate::store::checked_path(app_dir, "config.sha256")?;
    if !fs::symlink_metadata(&sums)?.is_file() {
        return Err(Error::invalid("snapshot checksums must be a regular file"));
    }
    let config = crate::store::checked_path(app_dir, "config")?;
    let mut actual = std::collections::BTreeSet::new();
    snapshot_files(&config, &mut actual)?;
    let text = fs::read_to_string(&sums).map_err(|_| Error::invalid(format!("{} missing", sums.display())))?;
    let app_name = app_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut listed = std::collections::BTreeSet::new();
    for line in text.lines() {
        let (digest, rel) = line.split_once("  ").unwrap_or((line, ""));
        // current format is relative to the version folder; early ones to the app folder
        let relative = rel.strip_prefix(&format!("{app_name}/")).unwrap_or(rel);
        if !relative.starts_with("config/") || digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::invalid(format!("{rel}: checksum mismatch, refusing to restore")));
        }
        let p = crate::store::checked_path(app_dir, relative)?;
        if !listed.insert(p.clone()) || !actual.contains(&p) || sha256(&p).ok().as_deref() != Some(digest) {
            return Err(Error::invalid(format!("{rel}: checksum mismatch, refusing to restore")));
        }
    }
    if listed != actual {
        return Err(Error::invalid("snapshot contains files not covered by its checksums, refusing to restore"));
    }
    Ok(())
}

/// Enumerate exactly what a restore would copy, without following links or
/// silently skipping unreadable entries. An empty directory needs no checksum.
fn snapshot_files(dir: &Path, files: &mut std::collections::BTreeSet<PathBuf>) -> Result<()> {
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Err(Error::invalid("snapshot contains a symlink or special file"));
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let kind = fs::symlink_metadata(&path)?.file_type();
        if kind.is_dir() {
            snapshot_files(&path, files)?;
        } else if kind.is_file() {
            files.insert(path);
        } else {
            return Err(Error::invalid("snapshot contains a symlink or special file"));
        }
    }
    Ok(())
}

/// Where a restore keeps what it replaced when the folder next to the app's
/// config isn't writable (wranglarr runs as PUID, and the mount points belong
/// to root): a folder of wranglarr's own, unless the command set another.
pub const DEFAULT_PRE_RESTORE: &str = "/config/pre-restore";

static PRE_RESTORE: OnceLock<PathBuf> = OnceLock::new();

/// Use `dir` instead of [`DEFAULT_PRE_RESTORE`] (WRANGLARR_PRE_RESTORE, read
/// where the command starts). The first call wins.
pub fn set_pre_restore_dir(dir: PathBuf) {
    if PRE_RESTORE.set(dir).is_err() {
        tracing::debug!("the pre-restore folder was already set");
    }
}

fn pre_restore() -> &'static Path {
    PRE_RESTORE.get().map_or(Path::new(DEFAULT_PRE_RESTORE), PathBuf::as_path)
}

fn writable(dir: &Path) -> bool {
    rustix::fs::access(dir, rustix::fs::Access::WRITE_OK).is_ok()
}

/// Recovery copies are never overwritten by a later restore.
fn recovery_dir(config_dir: &Path) -> Result<PathBuf> {
    let name = config_dir.file_name().ok_or_else(|| Error::invalid("config_dir has no name"))?;
    let base = if config_dir.parent().is_some_and(writable) {
        config_dir.with_file_name(format!("{}.pre-wranglarr", name.to_string_lossy()))
    } else {
        fs::create_dir_all(pre_restore())?;
        pre_restore().join(format!("{}.pre-wranglarr", name.to_string_lossy()))
    };
    let parent = base.parent().ok_or_else(|| Error::invalid("recovery directory has no parent"))?;
    if fs::canonicalize(parent)?.starts_with(fs::canonicalize(config_dir)?) {
        return Err(Error::invalid("recovery directory must be outside the live config directory"));
    }
    match fs::create_dir(&base) {
        Ok(()) => Ok(base),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let parent = base.parent().ok_or_else(|| Error::invalid("recovery directory has no parent"))?;
            Ok(tempfile::Builder::new()
                .prefix(&format!("{}.pre-wranglarr-", name.to_string_lossy()))
                .tempdir_in(parent)?
                .keep())
        }
        Err(e) => Err(e.into()),
    }
}

const RESTORE_TRANSACTION: &str = ".wranglarr-restore-transaction";

fn remove_entry(path: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// Rename within the mounted config filesystem. Never fall back to a destructive
/// copy/delete operation: a mount point or failed rename must abort the restore.
fn install_staged(
    live: &Path,
    transaction: &Path,
    original: &[std::ffi::OsString],
    replacement: &[std::ffi::OsString],
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let old = transaction.join("old");
    let new = transaction.join("new");
    let mut moved = Vec::new();
    let mut installed = Vec::new();
    let result: std::io::Result<()> = (|| {
        for name in original {
            rename(&live.join(name), &old.join(name))?;
            moved.push(name);
        }
        for name in replacement {
            rename(&new.join(name), &live.join(name))?;
            installed.push(name);
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        for name in installed.into_iter().rev() {
            if let Err(e) = remove_entry(&live.join(name)) {
                failures.push(e.to_string());
            }
        }
        for name in moved.into_iter().rev() {
            if let Err(e) = fs::rename(old.join(name), live.join(name)) {
                failures.push(e.to_string());
            }
        }
        if failures.is_empty() {
            fs::remove_dir_all(transaction)?;
            return Err(Error::internal(format!("restore failed; original config restored: {error}")));
        }
        return Err(Error::internal(format!(
            "restore failed ({error}); rollback incomplete ({}). Keep the container stopped; recovery journal: {}",
            failures.join("; "),
            transaction.join("recovery.json").display()
        )));
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let t = e.file_type()?;
        let to = dst.join(e.file_name());
        if t.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else if t.is_symlink() {
            let target = fs::read_link(e.path())?;
            let _ = fs::remove_file(&to);
            std::os::unix::fs::symlink(target, &to)?;
        } else if t.is_file() {
            copy_file(&e.path(), &to)?;
        } else {
            return Err(std::io::Error::other(format!(
                "{} is not a regular file, folder or symlink",
                e.path().display()
            )));
        }
    }
    fs::set_permissions(dst, fs::metadata(src)?.permissions())?;
    fs::File::open(dst)?.sync_all()?;
    Ok(())
}

/// Validate the entire source before moving any live files aside. Checksums
/// cover file contents, but cannot authorize symlinks or device nodes.
fn validate_snapshot_tree(source: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_dir() {
        return Err(Error::invalid("snapshot contains a symlink or special file"));
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            validate_snapshot_tree(&entry.path())?;
        } else if !kind.is_file() {
            return Err(Error::invalid("snapshot contains a symlink or special file"));
        }
    }
    Ok(())
}

/// Validate before stopping an application, and again before installation.
pub fn validate_restore_source(source: &Path) -> Result<()> {
    if !source.is_dir() {
        return Err(Error::invalid(format!("{} is not a config snapshot", source.display())));
    }
    validate_snapshot_tree(source)?;
    verify_snapshot(source.parent().unwrap_or(source))?;
    if source.join(RESTORE_TRANSACTION).exists() {
        return Err(Error::invalid("snapshot contains a reserved restore transaction directory"));
    }
    Ok(())
}

/// Restore a mounted config directory while its application is stopped. Stage
/// all incoming files and retain a unique recovery copy before replacing any
/// live entries. Ordinary install failures roll back; an interrupted transaction
/// is preserved and blocks another restore until an operator recovers it.
pub fn restore_config(name: &str, source: &Path, config_dir: &Path) -> Result<()> {
    if !config_dir.is_dir() {
        return Err(Error::invalid(format!("{name}: config_dir {} missing", config_dir.display())));
    }
    validate_restore_source(source).map_err(|e| e.map_msg(|m| format!("{name}: {m}")))?;
    let source_path = fs::canonicalize(source)?;
    let live_path = fs::canonicalize(config_dir)?;
    if source_path.starts_with(&live_path) || live_path.starts_with(&source_path) {
        return Err(Error::invalid("snapshot and live config directories must not overlap"));
    }
    let transaction = config_dir.join(RESTORE_TRANSACTION);
    fs::create_dir(&transaction).map_err(|e| Error::caused(
        &format!("{name}: cannot create restore transaction {} (an existing transaction needs manual recovery before retry)", transaction.display()), e
    ))?;
    let prepared: Result<_> = (|| {
        copy_tree(source, &transaction.join("new"))?;
        owner::like(&transaction.join("new"), config_dir);
        fs::create_dir(transaction.join("old"))?;
        let original = fs::read_dir(config_dir)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|n| n != RESTORE_TRANSACTION)
            .collect::<Vec<_>>();
        let replacement = fs::read_dir(transaction.join("new"))?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        let keep = recovery_dir(config_dir)?;
        for n in &original {
            let from = config_dir.join(n);
            let to = keep.join(n);
            let meta = fs::symlink_metadata(&from)?;
            if meta.is_dir() {
                copy_tree(&from, &to)?;
            } else if meta.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
            } else {
                copy_file(&from, &to)?;
            }
        }
        owner::like(&keep, config_dir);
        fs::File::open(&keep)?.sync_all()?;
        let journal = json!({"recovery_copy": keep, "original": original, "replacement": replacement,
            "instructions": "Keep the application stopped. The recovery_copy contains the complete original config. old/ contains original entries already moved; new/ contains replacements not yet installed. Recover before removing this transaction directory."});
        write_atomic(&transaction.join("recovery.json"), &serde_json::to_vec_pretty(&journal)?, None)?;
        fs::File::open(&transaction)?.sync_all()?;
        Ok((keep, original, replacement))
    })();
    let (keep, original, replacement) = match prepared {
        Ok(p) => p,
        Err(e) => {
            // No original entry has moved yet.
            let _ = fs::remove_dir_all(&transaction);
            return Err(e);
        }
    };
    install_staged(config_dir, &transaction, &original, &replacement, |a, b| fs::rename(a, b))?;
    fs::File::open(config_dir)?.sync_all()?;
    fs::remove_dir_all(&transaction)?;
    tracing::info!(
        "{name}: restored {} into {} (previous contents in {})",
        source.display(),
        config_dir.display(),
        keep.display()
    );
    Ok(())
}

/// The SQLite databases in a config snapshot: its top-level `*.db` files.
pub fn sqlite_in(snapshot: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(snapshot) else { return vec![] };
    let mut dbs: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".db"))
        .collect();
    dbs.sort();
    dbs
}

/// Why the SQLite databases in `snapshot` can't go back into `config_dir`
/// offline, or None when they can.
pub fn sqlite_restore_blocker(snapshot: &Path, config_dir: &Path) -> Option<String> {
    if sqlite_in(snapshot).is_empty() {
        return Some("its backup has no SQLite database".into());
    }
    if !config_dir.is_dir() {
        return Some(format!("its config folder {} isn't mounted into wranglarr", config_dir.display()));
    }
    if !writable(config_dir) {
        return Some(format!("its config folder {} is mounted read-only", config_dir.display()));
    }
    None
}

/// Put the SQLite databases of a config snapshot back into `config_dir` while
/// the app is stopped: each `*.db` replaces the live one, and the live one's
/// `-wal`, `-shm` and `-journal` go, since SQLite would apply them to the
/// restored file. Nothing else in the folder changes. The files replaced are
/// copied to a recovery folder first. Returns the databases restored; on an
/// error, `done` has the ones already back.
pub fn restore_sqlite(name: &str, snapshot: &Path, config_dir: &Path, done: &mut Vec<String>) -> Result<PathBuf> {
    validate_restore_source(snapshot).map_err(|e| e.map_msg(|m| format!("{name}: {m}")))?;
    if let Some(why) = sqlite_restore_blocker(snapshot, config_dir) {
        return Err(Error::invalid(format!("{name}: can't restore its database offline: {why}")));
    }
    let dbs = sqlite_in(snapshot);
    for db in &dbs {
        let mut magic = [0u8; 16];
        let ok = fs::File::open(snapshot.join(db)).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic));
        if ok.is_err() || &magic != b"SQLite format 3\0" {
            return Err(Error::invalid(format!("{name}: {db} in the backup isn't an SQLite database")));
        }
    }
    let sidecars = |db: &str| ["-wal", "-shm", "-journal"].map(|x| format!("{db}{x}"));
    // everything about to change, kept first
    let keep = recovery_dir(config_dir)?;
    for db in &dbs {
        for f in std::iter::once(db.clone()).chain(sidecars(db)) {
            let live = config_dir.join(&f);
            if live.is_file() {
                copy_file(&live, &keep.join(&f))?;
            }
        }
    }
    owner::like(&keep, config_dir);
    fs::File::open(&keep)?.sync_all()?;
    for db in &dbs {
        let live = config_dir.join(db);
        for f in sidecars(db) {
            match fs::remove_file(config_dir.join(&f)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(Error::caused(
                        &format!("{name}: can't remove {f} (the replaced files are in {})", keep.display()),
                        e,
                    ));
                }
                _ => {}
            }
        }
        let reference = if live.is_file() { live.clone() } else { config_dir.to_path_buf() };
        let staged = config_dir.join(format!(".{db}.wranglarr-restore"));
        copy_file(&snapshot.join(db), &staged)?;
        owner::like(&staged, &reference);
        fs::rename(&staged, &live).map_err(|e| {
            Error::caused(&format!("{name}: can't put {db} back (the replaced files are in {})", keep.display()), e)
        })?;
        done.push(db.clone());
    }
    fs::File::open(config_dir)?.sync_all()?;
    tracing::info!(
        "{name}: restored {} into {} (replaced files in {})",
        dbs.join(", "),
        config_dir.display(),
        keep.display()
    );
    Ok(keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_rejects_overlapping_source_and_destination_before_staging() {
        let t = tempfile::tempdir().expect("temp");
        let source = t.path().join("app/config");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("settings"), "original").expect("file");
        fs::write(
            t.path().join("app/config.sha256"),
            format!("{}  app/config/settings\n", sha256(&source.join("settings")).expect("hash")),
        )
        .expect("sums");
        for live in [&source, t.path()] {
            assert!(
                restore_config("app", &source, live).expect_err("overlap").to_string().contains("must not overlap")
            );
            assert!(!live.join(RESTORE_TRANSACTION).exists());
        }
        assert_eq!(fs::read_to_string(source.join("settings")).expect("original"), "original");
    }

    #[test]
    fn failed_install_restores_all_original_entries() {
        for failure_at in 0..4 {
            let t = tempfile::tempdir().expect("temp");
            let live = t.path().join("live");
            let transaction = live.join(RESTORE_TRANSACTION);
            fs::create_dir_all(transaction.join("old")).expect("old");
            fs::create_dir_all(transaction.join("new")).expect("new");
            for name in ["a", "b"] {
                fs::write(live.join(name), format!("original-{name}")).expect("original");
                fs::write(transaction.join("new").join(name), "replacement").expect("replacement");
            }
            let names = ["a".into(), "b".into()];
            let mut n = 0;
            let result = install_staged(&live, &transaction, &names, &names, |from, to| {
                let fail = n == failure_at;
                n += 1;
                if fail { Err(std::io::Error::other("injected rename failure")) } else { fs::rename(from, to) }
            });
            assert!(result.expect_err("failure").to_string().contains("original config restored"));
            for name in ["a", "b"] {
                assert_eq!(fs::read_to_string(live.join(name)).expect("original"), format!("original-{name}"));
            }
            assert!(!transaction.exists());
        }
    }

    #[test]
    fn restores_retain_distinct_recovery_copies_and_refuse_interrupted_transactions() {
        let t = tempfile::tempdir().expect("temp");
        let source = t.path().join("backup/app/config");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("settings"), "new").expect("snapshot");
        fs::write(
            source.parent().expect("parent").join("config.sha256"),
            format!("{}  app/config/settings\n", sha256(&source.join("settings")).expect("hash")),
        )
        .expect("sums");
        let live = t.path().join("live");
        fs::create_dir(&live).expect("live");
        fs::write(live.join("settings"), "first").expect("original");
        restore_config("app", &source, &live).expect("first restore");
        fs::write(live.join("settings"), "second").expect("change");
        restore_config("app", &source, &live).expect("second restore");
        assert_eq!(fs::read_to_string(t.path().join("live.pre-wranglarr/settings")).expect("first copy"), "first");
        let copies: Vec<_> = fs::read_dir(t.path())
            .expect("dir")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.file_name().expect("name").to_string_lossy().starts_with("live.pre-wranglarr"))
            .collect();
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().any(|p| fs::read_to_string(p.join("settings")).expect("copy") == "second"));
        fs::create_dir(live.join(RESTORE_TRANSACTION)).expect("interrupted");
        fs::write(live.join(RESTORE_TRANSACTION).join("recovery.json"), "retain me").expect("journal");
        assert!(restore_config("app", &source, &live).expect_err("blocked").to_string().contains("manual recovery"));
        assert_eq!(fs::read_to_string(live.join("settings")).expect("live"), "new");
        assert_eq!(
            fs::read_to_string(live.join(RESTORE_TRANSACTION).join("recovery.json")).expect("journal"),
            "retain me"
        );
    }

    #[test]
    fn in_config_paths() {
        assert_eq!(in_config("/config"), Some(String::new()));
        assert_eq!(in_config("/config/Downloads/complete/"), Some("Downloads/complete".into()));
        assert_eq!(in_config("Downloads/incomplete"), Some("Downloads/incomplete".into()));
        assert_eq!(in_config("/data/usenet"), None);
        assert_eq!(in_config(""), None);
    }

    /// Rules like an arr's (the service descriptors hold the real ones).
    const ARR: CopyRules = CopyRules {
        skip_dirs: &["logs", "Backups", "MediaCover"],
        skip_files: &[],
        skip_sqlite: true,
        skip_files_in: &[],
        zip_layout: &[],
    };
    const ARR_SPEC: CopySpec = CopySpec { rules: ARR, payload: None };

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
    fn snapshot_copies_and_reports_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().expect("tmp");
        let src = t.path().join("cfg");
        fs::create_dir_all(src.join("sub")).expect("mkdir");
        fs::create_dir_all(src.join("logs")).expect("mkdir");
        fs::write(src.join("config.xml"), "x").expect("w");
        fs::write(src.join("logs/a.txt"), "x").expect("w");
        fs::write(src.join("secret.xml"), "y").expect("w");
        fs::write(src.join("sub/b"), "z").expect("w");
        fs::set_permissions(src.join("secret.xml"), fs::Permissions::from_mode(0o0)).expect("chmod");
        fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o0)).expect("chmod");
        let snap = snapshot_config("radarr", &ARR_SPEC, &src, &t.path().join("snap/radarr"), None);
        fs::set_permissions(src.join("secret.xml"), fs::Permissions::from_mode(0o644)).expect("chmod");
        fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o755)).expect("chmod");
        let snap = snap.expect("snapshot");
        if rustix::process::geteuid().is_root() {
            return; // root reads everything
        }
        assert_eq!(snap.files, 1, "{snap:?}");
        assert!(snap.unreadable.iter().any(|u| u.starts_with("secret.xml")), "{snap:?}");
        assert!(snap.unreadable.iter().any(|u| u.starts_with("sub/")), "{snap:?}");
        verify_snapshot(&t.path().join("snap/radarr")).expect("verifies");
    }
    #[test]
    fn payload_paths_cannot_traverse() {
        for path in ["../outside", "/config/../outside", "x/../../outside", "x\\y"] {
            assert_eq!(in_config(path), None);
        }
    }

    #[test]
    fn snapshot_checksums_cover_exactly_the_files_restored() {
        let t = tempfile::tempdir().expect("tmp");
        let app = t.path().join("app");
        fs::create_dir_all(app.join("config/sub")).expect("mkdir");
        let settings = app.join("config/settings");
        fs::write(&settings, "settings").expect("write");
        let line = format!("{}  app/config/settings\n", sha256(&settings).expect("hash"));
        fs::write(app.join("config.sha256"), &line).expect("sums");
        verify_snapshot(&app).expect("complete checksums");

        fs::write(app.join("config/sub/injected"), "unverified").expect("extra");
        assert!(verify_snapshot(&app).is_err(), "an unlisted regular file must fail");
        fs::remove_file(app.join("config/sub/injected")).expect("remove");
        fs::write(app.join("config.sha256"), "").expect("sums");
        assert!(verify_snapshot(&app).is_err(), "empty checksums cannot cover existing files");
        fs::write(app.join("config.sha256"), format!("{line}{line}")).expect("sums");
        assert!(verify_snapshot(&app).is_err(), "duplicate entries must fail");

        fs::write(app.join("config.sha256"), line.replace("app/config/", "config/")).expect("sums");
        verify_snapshot(&app).expect("app-relative checksums");
        fs::remove_file(&settings).expect("remove");
        assert!(verify_snapshot(&app).is_err(), "missing listed file must fail");
        fs::write(app.join("config.sha256"), "").expect("sums");
        verify_snapshot(&app).expect("an actually empty snapshot is covered");
    }

    #[test]
    fn an_sqlite_restore_replaces_only_the_database_and_drops_its_wal() {
        let t = tempfile::tempdir().expect("tmp");
        let app = t.path().join("backup/radarr");
        fs::create_dir_all(app.join("config")).expect("mkdir");
        let old_db = [b"SQLite format 3\0".as_slice(), b"the data from before"].concat();
        fs::write(app.join("config/radarr.db"), &old_db).expect("db");
        fs::write(app.join("config/config.xml"), "<Config>backup</Config>").expect("xml");
        let sums: String = ["radarr.db", "config.xml"]
            .iter()
            .map(|f| format!("{}  config/{f}\n", sha256(&app.join("config").join(f)).expect("hash")))
            .collect();
        fs::write(app.join("config.sha256"), sums).expect("sums");
        let live = t.path().join("live/config");
        fs::create_dir_all(live.join("MediaCover")).expect("mkdir");
        fs::write(live.join("MediaCover/1.jpg"), "cover").expect("cover");
        fs::write(live.join("config.xml"), "<Config>live</Config>").expect("xml");
        fs::write(live.join("radarr.db"), "SQLite format 3\0migrated").expect("db");
        fs::write(live.join("radarr.db-wal"), "wal of the migrated db").expect("wal");
        fs::write(live.join("radarr.db-shm"), "shm").expect("shm");

        assert_eq!(sqlite_restore_blocker(&app.join("config"), &live), None);
        let mut done = vec![];
        let keep = restore_sqlite("radarr", &app.join("config"), &live, &mut done).expect("restore");
        assert_eq!(done, ["radarr.db"]);
        assert_eq!(fs::read(live.join("radarr.db")).expect("db"), old_db);
        assert!(!live.join("radarr.db-wal").exists() && !live.join("radarr.db-shm").exists());
        assert_eq!(fs::read_to_string(live.join("config.xml")).expect("xml"), "<Config>live</Config>");
        assert!(live.join("MediaCover/1.jpg").is_file(), "the rest of the folder stays");
        assert_eq!(fs::read_to_string(keep.join("radarr.db-wal")).expect("kept wal"), "wal of the migrated db");
        assert!(!keep.starts_with(&live));

        fs::write(app.join("config/radarr.db"), "not a database").expect("db");
        let sums = format!(
            "{}  config/radarr.db\n{}  config/config.xml\n",
            sha256(&app.join("config/radarr.db")).expect("hash"),
            sha256(&app.join("config/config.xml")).expect("hash")
        );
        fs::write(app.join("config.sha256"), sums).expect("sums");
        assert!(restore_sqlite("radarr", &app.join("config"), &live, &mut vec![]).is_err());
        assert!(sqlite_restore_blocker(&app.join("config"), &t.path().join("nowhere")).is_some());
    }

    #[test]
    fn snapshot_checks_reject_unlisted_links_and_special_files() {
        let t = tempfile::tempdir().expect("tmp");
        let app = t.path().join("app");
        fs::create_dir_all(app.join("config")).expect("mkdir");
        fs::write(app.join("config.sha256"), "").expect("sums");
        std::os::unix::fs::symlink(t.path(), app.join("config/link")).expect("link");
        assert!(verify_snapshot(&app).is_err());
        fs::remove_file(app.join("config/link")).expect("remove");
        let socket = std::os::unix::net::UnixListener::bind(app.join("config/socket")).expect("socket");
        assert!(verify_snapshot(&app).is_err());
        drop(socket);
        fs::remove_file(app.join("config/socket")).expect("remove");
        fs::remove_file(app.join("config.sha256")).expect("remove");
        fs::write(t.path().join("sums"), "").expect("sums");
        std::os::unix::fs::symlink(t.path().join("sums"), app.join("config.sha256")).expect("link");
        assert!(verify_snapshot(&app).is_err(), "checksum sidecar cannot be a symlink");
    }

    #[test]
    fn checksum_paths_cannot_escape_or_follow_links() {
        let t = tempfile::tempdir().expect("tmp");
        let app = t.path().join("app");
        fs::create_dir_all(app.join("config")).expect("mkdir");
        let outside = t.path().join("outside");
        fs::write(&outside, "secret").expect("write");
        let digest = sha256(&outside).expect("hash");
        for path in ["../outside", "app/../outside", "/outside"] {
            fs::write(
                app.join("config.sha256"),
                format!(
                    "{digest}  {path}
"
                ),
            )
            .expect("sums");
            assert!(verify_snapshot(&app).is_err());
        }
        std::os::unix::fs::symlink(&outside, app.join("config/link")).expect("link");
        fs::write(
            app.join("config.sha256"),
            format!(
                "{digest}  app/config/link
"
            ),
        )
        .expect("sums");
        assert!(verify_snapshot(&app).is_err());
    }

    #[test]
    fn unlisted_symlink_is_rejected_before_live_files_move() {
        let t = tempfile::tempdir().expect("tmp");
        let app = t.path().join("app");
        let live = t.path().join("live");
        fs::create_dir_all(app.join("config")).expect("mkdir");
        fs::create_dir(&live).expect("mkdir");
        fs::write(live.join("settings"), "live").expect("write");
        fs::write(app.join("config.sha256"), "").expect("sums");
        std::os::unix::fs::symlink(&live, app.join("config/link")).expect("link");
        assert!(restore_config("app", &app.join("config"), &live).is_err());
        assert_eq!(fs::read_to_string(live.join("settings")).expect("read"), "live");
        assert!(!t.path().join("live.pre-wranglarr").exists());
    }

    #[test]
    fn zip_overlay_cannot_escape() {
        use std::io::Write;
        let t = tempfile::tempdir().expect("tmp");
        let src = t.path().join("src");
        fs::create_dir(&src).expect("mkdir");
        let zip = t.path().join("overlay.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip).expect("zip"));
        for path in ["../../escape", "/absolute", "config.xml"] {
            writer.start_file(path, zip::write::SimpleFileOptions::default()).expect("entry");
            writer.write_all(b"data").expect("data");
        }
        writer.finish().expect("finish");
        let app = t.path().join("snap/app");
        snapshot_config("app", &ARR_SPEC, &src, &app, Some(&zip)).expect("snapshot");
        assert!(!t.path().join("snap/escape").exists());
        assert!(app.join("config/config.xml").is_file());
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
