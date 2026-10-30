// SPDX-License-Identifier: GPL-3.0-or-later
//! Taking a plain copy of an app's config folder, and checking one against its
//! checksums before it's restored.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CopySpec, collect_files, copy_file, payload_rules, payload_summary, rel_str, skipped, under, zip_layout};
use crate::error::{Error, Result};
use crate::store::sha256;

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

#[cfg(test)]
mod tests {
    use super::super::tests::ARR_SPEC;
    use super::*;

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
}
