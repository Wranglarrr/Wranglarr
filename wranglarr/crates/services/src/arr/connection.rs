// SPDX-License-Identifier: GPL-3.0-or-later
//! A restored `config.xml` keeps the Postgres connection of the stack it is
//! restored into. A new install has its own Postgres password (and maybe host),
//! so the one in an older backup would leave the app unable to reach its
//! databases once it restarts.

use std::io::{Read, Write};
use std::path::Path;

use wranglarr_core::http::{ArrError, ArrResult};

/// The `config.xml` settings that say how to reach Postgres. The database names
/// (`PostgresMainDb`, `PostgresLogDb`) stay as restored: the dumps go back under
/// those names.
pub const CONNECTION: [&str; 4] = ["PostgresHost", "PostgresPort", "PostgresUser", "PostgresPassword"];

/// The text of `<key>…</key>` (still XML-escaped), `""` for `<key />`.
fn element<'a>(xml: &'a str, key: &str) -> Option<&'a str> {
    let open = format!("<{key}>");
    if let Some(start) = xml.find(&open).map(|i| i + open.len()) {
        let end = xml[start..].find(&format!("</{key}>"))?;
        return Some(&xml[start..start + end]);
    }
    [format!("<{key} />"), format!("<{key}/>")].iter().any(|e| xml.contains(e.as_str())).then_some("")
}

/// `xml` with `<key>` set to `value` (already XML-escaped), added before
/// `</Config>` when missing.
fn set_element(xml: &str, key: &str, value: &str) -> String {
    let full = format!("<{key}>{value}</{key}>");
    let open = format!("<{key}>");
    if let Some(start) = xml.find(&open)
        && let Some(len) = xml[start..].find(&format!("</{key}>")).map(|e| e + key.len() + 3)
    {
        return format!("{}{full}{}", &xml[..start], &xml[start + len..]);
    }
    for empty in [format!("<{key} />"), format!("<{key}/>")] {
        if xml.contains(empty.as_str()) {
            return xml.replacen(empty.as_str(), &full, 1);
        }
    }
    match xml.rfind("</Config>") {
        Some(end) => format!("{}  {full}\n{}", &xml[..end], &xml[end..]),
        None => xml.to_owned(),
    }
}

/// `restored` with the connection settings `live` has. Returns the new text and
/// the names of the settings that changed.
pub fn merge(restored: &str, live: &str) -> (String, Vec<&'static str>) {
    let mut out = restored.to_owned();
    let mut changed = vec![];
    for key in CONNECTION {
        if let Some(value) = element(live, key)
            && element(&out, key) != Some(value)
        {
            out = set_element(&out, key, value);
            changed.push(key);
        }
    }
    (out, changed)
}

/// Write a copy of the backup zip at `zip` to `out`, its `config.xml` given the
/// connection settings of `live` (the app's current `config.xml`). Returns the
/// settings that changed; when none did, `out` isn't written.
pub fn keep_connection(name: &str, zip: &Path, live: &str, out: &Path) -> ArrResult<Vec<&'static str>> {
    let err = |e: &dyn std::fmt::Display| ArrError::new(format!("{name}: {}: {e}", zip.display()));
    let mut src = zip::ZipArchive::new(std::fs::File::open(zip).map_err(|e| err(&e))?).map_err(|e| err(&e))?;
    let Some(index) = src.index_for_name("config.xml") else {
        return Ok(vec![]);
    };
    let mut restored = String::new();
    src.by_index(index).map_err(|e| err(&e))?.read_to_string(&mut restored).map_err(|e| err(&e))?;
    let (text, changed) = merge(&restored, live);
    if changed.is_empty() {
        return Ok(changed);
    }
    let mut dst = zip::ZipWriter::new(std::fs::File::create(out).map_err(|e| err(&e))?);
    for i in 0..src.len() {
        if i == index {
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            dst.start_file("config.xml", options).map_err(|e| err(&e))?;
            dst.write_all(text.as_bytes()).map_err(|e| err(&e))?;
        } else {
            dst.raw_copy_file(src.by_index_raw(i).map_err(|e| err(&e))?).map_err(|e| err(&e))?;
        }
    }
    dst.finish().map_err(|e| err(&e))?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "<Config>\n  <ApiKey>old</ApiKey>\n  <PostgresHost>postgres</PostgresHost>\n  \
                       <PostgresPassword>old&amp;pw</PostgresPassword>\n  <PostgresMainDb>radarr</PostgresMainDb>\n  \
                       <PostgresUser />\n</Config>\n";
    const LIVE: &str = "<Config>\n  <ApiKey>new</ApiKey>\n  <PostgresHost>postgres</PostgresHost>\n  \
                        <PostgresPort>5433</PostgresPort>\n  <PostgresUser>arr</PostgresUser>\n  \
                        <PostgresPassword>new&lt;pw</PostgresPassword>\n  <PostgresMainDb>other</PostgresMainDb>\n\
                        </Config>\n";

    #[test]
    fn keeps_the_live_connection_and_the_restored_rest() {
        let (out, changed) = merge(OLD, LIVE);
        assert_eq!(changed, ["PostgresPort", "PostgresUser", "PostgresPassword"]);
        assert_eq!(element(&out, "PostgresPassword"), Some("new&lt;pw"), "escaped text copied as is");
        assert_eq!(element(&out, "PostgresUser"), Some("arr"), "an empty element filled in");
        assert_eq!(element(&out, "PostgresPort"), Some("5433"), "a missing one added");
        assert_eq!(element(&out, "ApiKey"), Some("old"), "the restored API key stays");
        assert_eq!(element(&out, "PostgresMainDb"), Some("radarr"), "the restored database name stays");
        assert!(out.trim_end().ends_with("</Config>"));
        assert_eq!(merge(&out, LIVE).1, Vec::<&str>::new(), "nothing left to change");
        assert_eq!(merge(OLD, "<Config><ApiKey>x</ApiKey></Config>").1, Vec::<&str>::new(), "a SQLite stack");
    }

    #[test]
    fn rewrites_only_config_xml_in_the_zip() {
        let t = tempfile::tempdir().expect("tmp");
        let (zip, out) = (t.path().join("b.zip"), t.path().join("patched.zip"));
        let mut z = zip::ZipWriter::new(std::fs::File::create(&zip).expect("zip"));
        z.start_file("config.xml", zip::write::SimpleFileOptions::default()).expect("member");
        z.write_all(OLD.as_bytes()).expect("write");
        z.start_file("radarr.db", zip::write::SimpleFileOptions::default()).expect("member");
        z.write_all(b"sqlite").expect("write");
        z.finish().expect("finish");

        assert_eq!(keep_connection("radarr", &zip, LIVE, &out).expect("patch").len(), 3);
        let mut r = zip::ZipArchive::new(std::fs::File::open(&out).expect("open")).expect("read");
        let (mut cfg, mut db) = (String::new(), String::new());
        r.by_name("config.xml").expect("cfg").read_to_string(&mut cfg).expect("read");
        r.by_name("radarr.db").expect("db").read_to_string(&mut db).expect("read");
        assert_eq!((element(&cfg, "PostgresPassword"), db.as_str()), (Some("new&lt;pw"), "sqlite"));

        std::fs::remove_file(&out).expect("rm");
        assert!(keep_connection("radarr", &zip, OLD, &out).expect("same").is_empty());
        assert!(!out.exists(), "nothing written when nothing changes");
    }
}
