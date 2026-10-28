// SPDX-License-Identifier: GPL-3.0-or-later
//! The synchronous side of the state database: the one connection, owned by
//! the state worker thread. Nothing outside this module can open one; code
//! reaches it only through [`super::State`], in a request the worker runs.

use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{NewUpdate, Run, RunStatus, SuiteSettings, UpdateRow};
use crate::error::{Error, Result};
use crate::time::now_iso_z;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS suite_settings (
  name TEXT PRIMARY KEY, schedule TEXT, enabled INTEGER, keep_last INTEGER, keep_days INTEGER, updated TEXT);
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT, suite TEXT NOT NULL, kind TEXT NOT NULL, target TEXT,
  status TEXT NOT NULL, started TEXT NOT NULL, finished TEXT, log TEXT NOT NULL DEFAULT '', result TEXT);
CREATE INDEX IF NOT EXISTS runs_suite ON runs(suite, id DESC);
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated TEXT);
CREATE TABLE IF NOT EXISTS updates (
  id INTEGER PRIMARY KEY AUTOINCREMENT, container TEXT NOT NULL, image TEXT NOT NULL,
  old_image TEXT, new_image TEXT, old_version TEXT, new_version TEXT,
  action TEXT NOT NULL, status TEXT NOT NULL, run_id INTEGER, at TEXT NOT NULL, note TEXT);
CREATE INDEX IF NOT EXISTS updates_container ON updates(container, id DESC);
";

/// How long a statement waits for another connection's lock (the CLI, a
/// second process) before it fails as busy.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The state database's connection and its typed queries.
pub struct Db {
    conn: Connection,
}

/// One write transaction (see [`Db::write`]): the kv table, and the
/// connection for tables of one's own.
pub struct Tx<'a>(Transaction<'a>);

impl Tx<'_> {
    pub fn conn(&self) -> &Connection {
        &self.0
    }
    pub fn get_value(&self, key: &str) -> Result<Option<Value>> {
        kv_get(&self.0, key)
    }
    /// A typed value; a stored value of the wrong shape reads as missing.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        Ok(self.get_value(key)?.and_then(|v| serde_json::from_value(v).ok()))
    }
    pub fn put<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        kv_put(&self.0, key, value)
    }
    pub fn delete(&self, key: &str) -> Result<()> {
        kv_delete(&self.0, key)
    }
}

fn kv_get(c: &Connection, key: &str) -> Result<Option<Value>> {
    let raw: Option<String> = c.query_row("SELECT value FROM kv WHERE key=?", [key], |r| r.get(0)).optional()?;
    Ok(match raw {
        Some(s) => Some(serde_json::from_str(&s)?),
        None => None,
    })
}

fn kv_put<T: Serialize + ?Sized>(c: &Connection, key: &str, value: &T) -> Result<()> {
    let text = serde_json::to_string(value)?;
    c.execute(
        "INSERT INTO kv VALUES (?,?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated=excluded.updated",
        params![key, text, now_iso_z()],
    )?;
    Ok(())
}

fn kv_delete(c: &Connection, key: &str) -> Result<()> {
    c.execute("DELETE FROM kv WHERE key=?", [key])?;
    Ok(())
}

const RUN_COLS: &str = "id, suite, kind, target, status, started, finished, log, result";

fn run_row(r: &rusqlite::Row) -> rusqlite::Result<Run> {
    let result: Option<String> = r.get(8)?;
    Ok(Run {
        id: r.get(0)?,
        suite: r.get(1)?,
        kind: r.get(2)?,
        target: r.get(3)?,
        status: r.get(4)?,
        started: r.get(5)?,
        finished: r.get(6)?,
        log: r.get(7)?,
        result: result
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| Value::Object(Default::default())),
    })
}

/// The database holds password hashes, TOTP secrets and API-key digests, so
/// it's owner-only: a folder wranglarr makes for it is 0700, the file is made
/// 0600 before SQLite opens it (SQLite gives its -journal, -wal and -shm
/// files the database's mode), and an older install's files are tightened.
/// The owner (PUID, see `owner::prepare_main`) is left as it is.
fn keep_private(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    }
    std::fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o600).open(path)?;
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        let p = Path::new(&p);
        let Ok(m) = std::fs::metadata(p) else { continue };
        let mode = m.permissions().mode();
        if m.is_file() && mode & 0o077 != 0 {
            // someone else's file can't be changed: say so, it still works
            if let Err(e) = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode & 0o700)) {
                tracing::warn!("{} is readable by other users and can't be made private: {e}", p.display());
            }
        }
    }
    Ok(())
}

impl Db {
    pub(super) fn open(path: &Path) -> Result<Db> {
        keep_private(path)?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(&format!("BEGIN IMMEDIATE;{SCHEMA}COMMIT;"))?;
        Ok(Db { conn })
    }

    pub(super) fn memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(&format!("BEGIN IMMEDIATE;{SCHEMA}COMMIT;"))?;
        Ok(Db { conn })
    }

    /// The connection, for modules with tables of their own.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Run a read-modify-write in one `BEGIN IMMEDIATE` transaction: it takes
    /// SQLite's write lock before the first read, so no other connection
    /// (`wranglarr-auth` runs as another process) can change what `f` read
    /// before `f`'s writes land. Commits when `f` returns Ok, rolls back on an
    /// error or a panic. Keep it short and never hash or wait in it: every
    /// other request waits for it.
    pub fn write<T>(&self, f: impl FnOnce(&Tx) -> Result<T>) -> Result<T> {
        let tx = Tx(Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?);
        let out = f(&tx)?;
        tx.0.commit()?;
        Ok(out)
    }

    // ---- settings ------------------------------------------------------------
    pub fn settings(&self, name: &str, defaults: SuiteSettings) -> Result<SuiteSettings> {
        let row = self
            .conn
            .query_row(
                "SELECT schedule, enabled, keep_last, keep_days, updated FROM suite_settings WHERE name=?",
                [name],
                |r| {
                    Ok(SuiteSettings {
                        schedule: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        enabled: r.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
                        keep_last: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        keep_days: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        source: "ui",
                        updated: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row.unwrap_or(SuiteSettings { source: "config", ..defaults }))
    }

    pub fn save_settings(&self, name: &str, s: &SuiteSettings) -> Result<()> {
        self.conn.execute(
            "INSERT INTO suite_settings VALUES (?,?,?,?,?,?) ON CONFLICT(name) DO UPDATE SET \
             schedule=excluded.schedule, enabled=excluded.enabled, keep_last=excluded.keep_last, \
             keep_days=excluded.keep_days, updated=excluded.updated",
            params![name, s.schedule, i64::from(s.enabled), s.keep_last, s.keep_days, now_iso_z()],
        )?;
        Ok(())
    }

    pub fn reset_settings(&self, name: &str) -> Result<()> {
        self.conn.execute("DELETE FROM suite_settings WHERE name=?", [name])?;
        Ok(())
    }

    // ---- key/value -------------------------------------------------------------
    pub fn get_value(&self, key: &str) -> Result<Option<Value>> {
        kv_get(&self.conn, key)
    }

    /// A typed value; a stored value of the wrong shape reads as missing.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        Ok(self.get_value(key)?.and_then(|v| serde_json::from_value(v).ok()))
    }

    pub fn put<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        kv_put(&self.conn, key, value)
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        kv_delete(&self.conn, key)
    }

    // ---- update history ----------------------------------------------------------
    pub fn add_update(&self, u: &NewUpdate) -> Result<()> {
        self.conn.execute(
            "INSERT INTO updates (container, image, old_image, new_image, old_version, new_version, \
             action, status, run_id, at, note) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            params![
                u.container,
                u.image,
                u.old_image,
                u.new_image,
                u.old_version,
                u.new_version,
                u.action,
                u.status,
                u.run_id,
                now_iso_z(),
                u.note
            ],
        )?;
        Ok(())
    }

    pub fn list_updates(&self, container: Option<&str>, limit: i64) -> Result<Vec<UpdateRow>> {
        let cols = "id, container, image, old_image, new_image, old_version, new_version, action, status, \
                    run_id, at, note";
        let map = |r: &rusqlite::Row| {
            Ok(UpdateRow {
                id: r.get(0)?,
                container: r.get(1)?,
                image: r.get(2)?,
                old_image: r.get(3)?,
                new_image: r.get(4)?,
                old_version: r.get(5)?,
                new_version: r.get(6)?,
                action: r.get(7)?,
                status: r.get(8)?,
                run_id: r.get(9)?,
                at: r.get(10)?,
                note: r.get(11)?,
            })
        };
        let rows = match container {
            Some(ct) => self
                .conn
                .prepare(&format!("SELECT {cols} FROM updates WHERE container=? ORDER BY id DESC LIMIT ?"))?
                .query_map(params![ct, limit], map)?
                .collect::<rusqlite::Result<_>>()?,
            None => self
                .conn
                .prepare(&format!("SELECT {cols} FROM updates ORDER BY id DESC LIMIT ?"))?
                .query_map([limit], map)?
                .collect::<rusqlite::Result<_>>()?,
        };
        Ok(rows)
    }

    // ---- runs ----------------------------------------------------------------------
    pub fn create_run(&self, suite: &str, kind: &str, target: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (suite, kind, target, status, started) VALUES (?,?,?,?,?)",
            params![suite, kind, target, RunStatus::Queued.as_str(), now_iso_z()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Reserve a suite in the same transaction that checks its active run.
    /// A separate read followed by create_run lets concurrent requests both win.
    pub fn reserve_run(&self, suite: &str, kind: &str, target: &str) -> Result<i64> {
        self.write(|tx| {
            let active: bool = tx.conn().query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE suite=? AND status IN ('queued','running'))",
                [suite],
                |r| r.get(0),
            )?;
            if active {
                return Err(Error::conflict(format!("suite {suite} already has a run in progress")));
            }
            tx.conn().execute(
                "INSERT INTO runs (suite, kind, target, status, started) VALUES (?,?,?,?,?)",
                params![suite, kind, target, RunStatus::Queued.as_str(), now_iso_z()],
            )?;
            Ok(tx.conn().last_insert_rowid())
        })
    }

    pub fn set_status(&self, run_id: i64, status: RunStatus) -> Result<()> {
        self.conn.execute("UPDATE runs SET status=? WHERE id=?", params![status.as_str(), run_id])?;
        Ok(())
    }

    pub fn append_log(&self, run_id: i64, line: &str) -> Result<()> {
        self.append_log_at(run_id, &now_iso_z(), line)
    }

    /// A line written down elsewhere at `at` (the state db's time format), such
    /// as by wranglarr's update helper.
    pub fn append_log_at(&self, run_id: i64, at: &str, line: &str) -> Result<()> {
        let line = format!("{at} {line}\n");
        self.conn.execute("UPDATE runs SET log = log || ? WHERE id=?", params![line, run_id])?;
        Ok(())
    }

    pub fn finish_run(&self, run_id: i64, status: RunStatus, result: &Value) -> Result<()> {
        let result = serde_json::to_string(result)?;
        self.conn.execute(
            "UPDATE runs SET status=?, finished=?, result=? WHERE id=?",
            params![status.as_str(), now_iso_z(), result, run_id],
        )?;
        Ok(())
    }

    pub fn get_run(&self, run_id: i64) -> Result<Option<Run>> {
        Ok(self.conn.query_row(&format!("SELECT {RUN_COLS} FROM runs WHERE id=?"), [run_id], run_row).optional()?)
    }

    pub fn list_runs(&self, suite: Option<&str>, limit: i64) -> Result<Vec<Run>> {
        let rows = match suite {
            Some(s) => self
                .conn
                .prepare(&format!("SELECT {RUN_COLS} FROM runs WHERE suite=? ORDER BY id DESC LIMIT ?"))?
                .query_map(params![s, limit], run_row)?
                .collect::<rusqlite::Result<_>>()?,
            None => self
                .conn
                .prepare(&format!("SELECT {RUN_COLS} FROM runs ORDER BY id DESC LIMIT ?"))?
                .query_map([limit], run_row)?
                .collect::<rusqlite::Result<_>>()?,
        };
        Ok(rows)
    }

    pub fn active_run(&self, suite: &str) -> Result<Option<Run>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {RUN_COLS} FROM runs WHERE suite=? AND status IN ('queued','running') \
                     ORDER BY id DESC LIMIT 1"
                ),
                [suite],
                run_row,
            )
            .optional()?)
    }

    /// The suites with a queued or running run.
    pub fn active_suites(&self) -> Result<std::collections::HashSet<String>> {
        let rows = self
            .conn
            .prepare("SELECT DISTINCT suite FROM runs WHERE status IN ('queued','running')")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Anything still queued/running at startup died with the previous process,
    /// except `keep`: a run handed over to this one (wranglarr's own update).
    pub fn abandon_stale_runs(&self, keep: Option<i64>) -> Result<()> {
        let n = now_iso_z();
        let line = format!(
            "{n} abandoned: wranglarr restarted while this ran. A container it stopped may still be stopped: \
             check the app, and the steps above, before starting it\n"
        );
        self.conn.execute(
            "UPDATE runs SET status='failed', finished=?, log = log || ? \
             WHERE status IN ('queued','running') AND id IS NOT ?",
            params![n, line, keep],
        )?;
        Ok(())
    }

    /// A consistent copy of the whole database at `path` (`VACUUM INTO`),
    /// taken while it is in use. `path` must not exist yet.
    pub fn snapshot_to(&self, path: &Path) -> Result<()> {
        let target = path.to_str().ok_or_else(|| Error::invalid(format!("{path:?} isn't UTF-8")))?;
        self.conn.execute("VACUUM INTO ?", [target])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_round_trip() {
        let db = Db::memory().expect("db");
        let id = db.create_run("media", "backup", "").expect("run");
        assert_eq!(db.active_run("media").expect("q").map(|r| r.id), Some(id));
        assert!(db.active_suites().expect("q").contains("media"));
        db.append_log(id, "hello").expect("log");
        db.finish_run(id, RunStatus::Ok, &serde_json::json!({"version": "v"})).expect("finish");
        let r = db.get_run(id).expect("q").expect("run");
        assert_eq!(r.status, "ok");
        assert!(r.log.ends_with(" hello\n"));
        assert_eq!(r.result["version"], "v");
        assert!(db.active_run("media").expect("q").is_none());
        assert!(db.active_suites().expect("q").is_empty());
    }

    #[test]
    fn a_suite_has_one_reservation_at_a_time() {
        let db = Db::memory().expect("db");
        db.reserve_run("media", "backup", "").expect("first");
        assert!(matches!(db.reserve_run("media", "backup", ""), Err(Error::Conflict(_))));
        db.reserve_run("other", "backup", "").expect("another suite");
        assert_eq!(db.list_runs(None, 100).expect("runs").len(), 2);
    }

    #[test]
    fn settings_override() {
        let db = Db::memory().expect("db");
        let d = SuiteSettings {
            schedule: "0 3 * * *".into(),
            enabled: true,
            keep_last: 14,
            keep_days: 0,
            source: "config",
            updated: None,
        };
        assert_eq!(db.settings("m", d.clone()).expect("q").source, "config");
        let saved = SuiteSettings { schedule: String::new(), enabled: false, keep_last: 3, keep_days: 1, ..d.clone() };
        db.save_settings("m", &saved).expect("save");
        let got = db.settings("m", d.clone()).expect("q");
        assert_eq!((got.source, got.enabled, got.keep_last), ("ui", false, 3));
        db.reset_settings("m").expect("reset");
        assert_eq!(db.settings("m", d).expect("q").source, "config");
    }

    #[test]
    fn a_failed_write_rolls_back() {
        let db = Db::memory().expect("db");
        db.put("a", &1).expect("put");
        let r: Result<()> = db.write(|tx| {
            tx.put("a", &2)?;
            tx.delete("a")?;
            Err(Error::invalid("no"))
        });
        assert!(r.is_err());
        assert_eq!(db.get_value("a").expect("get"), Some(serde_json::json!(1)));
    }

    #[test]
    fn kv() {
        let db = Db::memory().expect("db");
        db.put("a", &serde_json::json!({"x": 1})).expect("put");
        assert_eq!(db.get_value("a").expect("get"), Some(serde_json::json!({"x": 1})));
        assert_eq!(db.get::<String>("a").expect("get"), None, "the wrong shape reads as missing");
        db.delete("a").expect("del");
        assert_eq!(db.get_value("a").expect("get"), None);
    }

    #[test]
    fn the_database_is_owner_only() {
        let mode = |p: &Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("new/config/wranglarr.db");
        Db::open(&path).expect("db");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.path().join("new/config")), 0o700);
        assert_eq!(mode(&dir.path().join("new")), 0o700);
        // an older install's files are tightened, its folder left alone
        let old = dir.path().join("old");
        std::fs::create_dir(&old).expect("mkdir");
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let path = old.join("wranglarr.db");
        Connection::open(&path).expect("db").execute_batch("CREATE TABLE t (x);").expect("table");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let wal = old.join("wranglarr.db-wal");
        std::fs::write(&wal, "").expect("wal");
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o664)).expect("chmod");
        let db = Db::open(&path).expect("db");
        assert_eq!((mode(&path), mode(&wal), mode(&old)), (0o600, 0o600, 0o755));
        db.put("k", &1).expect("still works");
    }

    #[test]
    fn failed_schema_initialization_rolls_back() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("state.db");
        let connection = Connection::open(&path).expect("db");
        // A malformed pre-existing table makes index creation fail after the
        // first CREATE TABLE; no part of this initialization may survive.
        connection.execute_batch("CREATE TABLE runs (wrong_column TEXT);").expect("malformed schema");
        assert!(Db::open(&path).is_err());
        let exists: bool = connection
            .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='suite_settings')", [], |r| r.get(0))
            .expect("schema");
        assert!(!exists);
    }
}
