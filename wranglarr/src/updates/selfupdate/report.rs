// SPDX-License-Identifier: GPL-3.0-or-later
//! The self-update helper's report: the one file it shares with wranglarr.
//!
//! The helper writes it in wranglarr's state folder, first as `in_progress`
//! (before it stops wranglarr), then `done` with the outcome. Whichever
//! wranglarr runs next (the new one, or the old one put back) reads it, finishes
//! the run, and deletes it. Writes are atomic, so a reader never sees half a file.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::state::{NewUpdate, RunStatus};
use crate::time::iso_z;

/// The report's file name, in the state database's folder.
pub(crate) const FILE: &str = "self-update.json";

/// Older than this, a report is left over from something that never finished:
/// it is deleted, and its run (if still open) abandoned like any other.
const STALE: Duration = Duration::hours(24);

/// Where the report for the state database `state_db` lives.
pub(crate) fn path(state_db: &Path) -> PathBuf {
    state_db.parent().unwrap_or(Path::new(".")).join(FILE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    /// The helper is at work; `deadline` says until when.
    InProgress,
    /// The helper has finished; `ok` says how.
    Done,
}

/// One history row the helper made.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UpdateRecord {
    pub container: String,
    pub image: String,
    pub action: String,
    pub status: String,
    pub old_image: String,
    pub new_image: String,
    pub old_version: String,
    pub new_version: String,
    pub note: String,
}

impl UpdateRecord {
    pub(crate) fn from_row(u: NewUpdate) -> UpdateRecord {
        UpdateRecord {
            container: u.container,
            image: u.image,
            action: u.action,
            status: u.status,
            old_image: u.old_image,
            new_image: u.new_image,
            old_version: u.old_version,
            new_version: u.new_version,
            note: u.note,
        }
    }

    pub(crate) fn into_row(self, run_id: i64) -> NewUpdate {
        NewUpdate {
            container: self.container,
            image: self.image,
            action: self.action,
            status: self.status,
            run_id: Some(run_id),
            old_image: self.old_image,
            new_image: self.new_image,
            old_version: self.old_version,
            new_version: self.new_version,
            note: self.note,
        }
    }
}

/// An image the update rejected, so the scheduled self-update skips it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Rejected {
    pub image: String,
    pub digests: Vec<String>,
    pub reason: String,
}

/// What the helper did, for the wranglarr that runs after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Report {
    pub run_id: i64,
    /// wranglarr's container name.
    pub container: String,
    /// The helper's container name: when it's gone and the report still says
    /// in progress, the helper died.
    pub helper: String,
    pub phase: Phase,
    /// Done: whether the new wranglarr is the one running.
    pub ok: bool,
    pub started: String,
    /// In progress: when to stop waiting for `done`.
    pub deadline: String,
    /// Done and failed: why.
    pub error: String,
    /// The new version, when it is known.
    pub version: String,
    /// container -> updated | recreated | failed | kept
    pub results: Vec<(String, String)>,
    /// (time, line) for the run's log.
    pub lines: Vec<(String, String)>,
    pub updates: Vec<UpdateRecord>,
    pub rejected: Option<Rejected>,
}

impl Report {
    /// The report of run `run_id`, in progress until `deadline`.
    pub(crate) fn begin(
        run_id: i64,
        container: &str,
        helper: &str,
        now: DateTime<Utc>,
        deadline: DateTime<Utc>,
    ) -> Report {
        Report {
            run_id,
            container: container.to_owned(),
            helper: helper.to_owned(),
            phase: Phase::InProgress,
            ok: false,
            started: iso_z(now),
            deadline: iso_z(deadline),
            error: String::new(),
            version: String::new(),
            results: vec![],
            lines: vec![],
            updates: vec![],
            rejected: None,
        }
    }

    pub(crate) fn line(&mut self, at: DateTime<Utc>, msg: impl Into<String>) {
        self.lines.push((iso_z(at), msg.into()));
    }

    pub(crate) fn result(&mut self, container: &str, result: &str) {
        match self.results.iter_mut().find(|(n, _)| n == container) {
            Some(r) => r.1 = result.to_owned(),
            None => self.results.push((container.to_owned(), result.to_owned())),
        }
    }

    /// Done, the way `error` says (empty: it worked).
    pub(crate) fn finish(&mut self, error: Option<String>) {
        self.phase = Phase::Done;
        self.ok = error.is_none();
        self.error = error.unwrap_or_default();
    }

    /// Written in place of any earlier one, atomically.
    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::files::write_atomic(path, &text, None)
    }

    /// The report at `path`; None when there is none. One that can't be read is
    /// an error, so it isn't mistaken for "no update in progress".
    pub(crate) fn load(path: &Path) -> Result<Option<Report>, String> {
        let text = match std::fs::read(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        serde_json::from_slice(&text).map(Some).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Too old to be about an update still going on.
    pub(crate) fn stale(&self, now: DateTime<Utc>) -> bool {
        parse(&self.started).is_none_or(|t| now - t > STALE)
    }

    /// In progress, and the helper has run out of time.
    pub(crate) fn overdue(&self, now: DateTime<Utc>) -> bool {
        self.phase == Phase::InProgress && parse(&self.deadline).is_none_or(|t| now > t)
    }

    pub(crate) fn status(&self) -> RunStatus {
        if self.ok { RunStatus::Ok } else { RunStatus::Failed }
    }

    /// The run's result.
    pub(crate) fn run_result(&self) -> Value {
        let results: serde_json::Map<String, Value> = self.results.iter().map(|(n, r)| (n.clone(), json!(r))).collect();
        let mut out = json!({"results": results, "version": self.version});
        if !self.ok {
            out["error"] = json!(self.error);
        }
        out
    }
}

fn parse(t: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(t).ok().map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        parse(s).expect("time")
    }

    #[test]
    fn a_report_round_trips_and_says_how_it_ended() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = path(&dir.path().join("wranglarr.db"));
        assert_eq!(p, dir.path().join(FILE));
        assert_eq!(Report::load(&p), Ok(None), "no report: no update going on");

        let mut r = Report::begin(
            7,
            "wranglarr",
            "wranglarr-wranglarr-updater",
            at("2026-10-27T10:00:00Z"),
            at("2026-10-27T10:10:00Z"),
        );
        r.line(at("2026-10-27T10:00:01Z"), "helper started");
        r.result("wranglarr", "updated");
        r.result("wranglarr-init", "recreated");
        r.result("wranglarr", "failed");
        r.save(&p).expect("save");
        let back = Report::load(&p).expect("load").expect("there");
        assert_eq!(back, r);
        assert_eq!(back.phase, Phase::InProgress);
        assert!(!back.overdue(at("2026-10-27T10:09:59Z")));
        assert!(back.overdue(at("2026-10-27T10:10:01Z")));

        r.version = "0.3.0".into();
        r.finish(Some("not healthy after 300s".into()));
        assert!(!r.overdue(at("2026-10-28T00:00:00Z")), "done is never overdue");
        assert_eq!(r.status(), RunStatus::Failed);
        assert_eq!(
            r.run_result(),
            json!({"results": {"wranglarr": "failed", "wranglarr-init": "recreated"}, "version": "0.3.0",
                   "error": "not healthy after 300s"})
        );
        r.finish(None);
        assert_eq!(r.status(), RunStatus::Ok);
        assert!(r.run_result().get("error").is_none());
    }

    #[test]
    fn stale_and_unreadable_reports() {
        let r = Report::begin(1, "w", "h", at("2026-10-25T10:00:00Z"), at("2026-10-25T10:10:00Z"));
        assert!(r.stale(at("2026-10-26T10:00:01Z")));
        assert!(!r.stale(at("2026-10-26T09:59:59Z")));
        let mut broken = r.clone();
        broken.started = "yesterday".into();
        assert!(broken.stale(at("2026-10-25T10:00:00Z")), "a start that can't be read is stale");

        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join(FILE);
        std::fs::write(&p, "{not json").expect("write");
        assert!(Report::load(&p).is_err());
    }
}
