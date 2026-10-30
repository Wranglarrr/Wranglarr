// SPDX-License-Identifier: GPL-3.0-or-later
//! Putting a config snapshot back into a mounted config folder while the app
//! is stopped: the whole folder, or just its SQLite databases.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::json;

use super::{copy_file, verify_snapshot, write_atomic};
use crate::error::{Error, Result};
use crate::owner;

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
    use crate::store::sha256;

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
}
