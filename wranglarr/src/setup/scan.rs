// SPDX-License-Identifier: GPL-3.0-or-later
//! Scanning appdata for what a previous install left: wranglarr backups
//! (versions to restore after install), the apps' own backup zips (a
//! config.xml to reuse), live configs and SQLite databases.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use super::env::services;

use crate::value::count;
use crate::value::truthy;

fn dirs(d: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn zips_in(d: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(d)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".zip")))
                .collect()
        })
        .unwrap_or_default()
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The member names of a zip, or None when it isn't one.
pub fn zip_names(p: &Path) -> Option<Vec<String>> {
    let f = std::fs::File::open(p).ok()?;
    let z = zip::ZipArchive::new(f).ok()?;
    Some(z.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect())
}

pub fn scan_backups(appdata: &Path) -> Value {
    let (mut versions, mut legacy, mut sqlite, mut configs) = (vec![], vec![], vec![], vec![]);
    let root = appdata.join("backups");
    if root.is_dir() {
        for suite_dir in dirs(&root) {
            let mut vdirs = dirs(&suite_dir);
            vdirs.reverse();
            for vdir in vdirs {
                let Ok(text) = std::fs::read_to_string(vdir.join("manifest.json")) else { continue };
                let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) else { continue };
                let empty = Map::new();
                let apps: Map<String, Value> = m
                    .get("apps")
                    .and_then(Value::as_object)
                    .unwrap_or(&empty)
                    .iter()
                    .map(|(n, a)| {
                        let g = |k: &str| a.get(k).cloned().unwrap_or(Value::Null);
                        let errors = a.get("errors").cloned().unwrap_or_else(|| json!([]));
                        let dbs = a.get("databases").map_or(0, count);
                        (
                            n.clone(),
                            json!({"kind": g("kind"), "version": g("version"), "databases": dbs,
                                   "config": a.get("config_zip").is_some_and(truthy), "errors": errors}),
                        )
                    })
                    .collect();
                let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let g = |k: &str| m.get(k).cloned().unwrap_or(Value::Null);
                versions.push(json!({"suite": name(&suite_dir), "version": name(&vdir), "status": g("status"),
                                     "created": g("created"), "total_bytes": g("total_bytes"), "apps": apps}));
            }
        }
    }
    for s in services() {
        let d = appdata.join(s.id);
        if !d.is_dir() {
            continue;
        }
        if let Some(c) = s.wizard.and_then(|w| w.config)
            && d.join(c).exists()
        {
            configs.push(s.id);
        }
        if d.join(format!("{}.db", s.id)).exists() {
            sqlite.push(s.id);
        }
        let mut zips: Vec<PathBuf> = dirs(&d.join("Backups")).iter().flat_map(|x| zips_in(x)).collect();
        zips.extend(zips_in(&d.join("backup")));
        let mut zips: Vec<(SystemTime, PathBuf)> = zips.into_iter().map(|z| (mtime(&z), z)).collect();
        zips.sort_by_key(|z| std::cmp::Reverse(z.0));
        for (mt, z) in zips.into_iter().take(10) {
            let Some(names) = zip_names(&z) else { continue };
            let size = std::fs::metadata(&z).map(|m| m.len()).unwrap_or(0);
            legacy.push(json!({
                "app": s.id, "path": z.to_string_lossy(),
                "name": z.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                "size": size, "mtime": crate::time::iso(&DateTime::<Utc>::from(mt)),
                "has_config": names.iter().any(|n| ["config.xml", "config.yaml"].contains(&n.to_lowercase().as_str())),
                "has_sqlite": names.iter().any(|n| n.to_lowercase().ends_with(".db")),
            }));
        }
    }
    json!({"versions": versions, "legacy": legacy, "sqlite": sqlite, "configs": configs})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn finds_what_is_there() {
        let d = tempfile::tempdir().expect("tmp");
        let ad = d.path();
        let v = ad.join("backups/media/20260101T030000Z");
        std::fs::create_dir_all(&v).expect("mkdir");
        std::fs::write(
            v.join("manifest.json"),
            r#"{"status":"ok","created":"x","apps":{"radarr":{"kind":"radarr","databases":["a","b"],"config_zip":"c.zip"}}}"#,
        )
        .expect("write");
        std::fs::create_dir_all(ad.join("backups/media/20250101T030000Z")).expect("mkdir");
        std::fs::create_dir_all(ad.join("radarr/Backups/scheduled")).expect("mkdir");
        std::fs::write(ad.join("radarr/config.xml"), "<Config/>").expect("write");
        std::fs::write(ad.join("radarr/radarr.db"), "").expect("write");
        let mut zw =
            zip::ZipWriter::new(std::fs::File::create(ad.join("radarr/Backups/scheduled/r.zip")).expect("zip"));
        zw.start_file("config.xml", zip::write::SimpleFileOptions::default()).expect("member");
        zw.write_all(b"<Config/>").expect("write");
        zw.finish().expect("finish");
        std::fs::write(ad.join("radarr/Backups/scheduled/bad.zip"), "PK").expect("write");
        let s = scan_backups(ad);
        assert_eq!(s["versions"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            s["versions"][0]["apps"]["radarr"],
            json!({"kind": "radarr", "version": null, "databases": 2, "config": true, "errors": []})
        );
        assert_eq!(s["configs"], json!(["radarr"]));
        assert_eq!(s["sqlite"], json!(["radarr"]));
        assert_eq!(s["legacy"].as_array().map(Vec::len), Some(1), "{s}");
        assert_eq!(s["legacy"][0]["has_config"], json!(true));
        assert_eq!(s["legacy"][0]["has_sqlite"], json!(false));
    }
}
