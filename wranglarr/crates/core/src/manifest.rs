// SPDX-License-Identifier: GPL-3.0-or-later
//! A backup version's `manifest.json`: what the version holds and how it went.
//!
//! The format is wranglarr's own and lives on disk next to the files it lists, so
//! these types keep it exactly:
//! - fields are written in the order declared here;
//! - a field marked "absent until …" is left out of the file until then, and one
//!   that may be null keeps its null;
//! - every field has a default, so an older or partial manifest still reads;
//! - fields these types don't know are kept (`extra`) and written back.
//!
//! What the apps report about themselves (`collection`, `scope`, their status
//! fields) stays as JSON: it differs per app and per version.

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::files::Snapshot;

/// A present field's value, even null (a missing field stays `None`).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// How a backup version turned out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VersionStatus {
    /// The backup is still being made.
    Running,
    /// Everything was backed up.
    Ok,
    /// Some apps were backed up, some failed.
    Partial,
    /// Nothing usable was backed up.
    Failed,
    /// No readable manifest (or one with a status wranglarr doesn't know).
    #[default]
    #[serde(other)]
    Incomplete,
}

/// A finished backup job's status, as the version records it.
impl From<crate::state::RunStatus> for VersionStatus {
    fn from(s: crate::state::RunStatus) -> Self {
        use crate::state::RunStatus;
        match s {
            RunStatus::Queued | RunStatus::Running => VersionStatus::Running,
            RunStatus::Ok => VersionStatus::Ok,
            RunStatus::Partial => VersionStatus::Partial,
            RunStatus::Failed => VersionStatus::Failed,
        }
    }
}

impl VersionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionStatus::Running => "running",
            VersionStatus::Ok => "ok",
            VersionStatus::Partial => "partial",
            VersionStatus::Failed => "failed",
            VersionStatus::Incomplete => "incomplete",
        }
    }
}

impl std::fmt::Display for VersionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One backup version of a suite.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    /// Always the suite folder's name when read from the store.
    pub suite: String,
    /// Always the version folder's name when read from the store.
    pub version: String,
    /// When the backup started (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// What started it: `manual`, `schedule`, `pre-update`, ….
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// The wranglarr version that made it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wranglarr: Option<String>,
    pub status: VersionStatus,
    /// Each app's part, in the suite's order.
    pub apps: IndexMap<String, AppBackup>,
    /// The Postgres server's version ("" when the suite has no databases).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postgres_version: Option<String>,
    /// The VPN's status at backup time (null without a VPN).
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub vpn: Option<Value>,
    /// Bytes in the version folder, counted when the manifest is written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// The Postgres container as it ran. Absent when the suite has no databases;
    /// null when Docker couldn't say.
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub postgres_container: Option<Option<ContainerInfo>>,
    /// The Postgres roles and settings (`pg_dumpall --globals-only`), when dumped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub globals: Option<Globals>,
    /// The standalone restore stack's files. Absent until the backup has finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standalone: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Manifest {
    /// A backup starting now.
    pub fn started(suite: &str, version: &str, created: String, trigger: &str, wranglarr: &str) -> Manifest {
        Manifest {
            suite: suite.into(),
            version: version.into(),
            created: Some(created),
            trigger: Some(trigger.into()),
            wranglarr: Some(wranglarr.into()),
            status: VersionStatus::Running,
            postgres_version: Some(String::new()),
            vpn: Some(Value::Null),
            ..Manifest::default()
        }
    }

    /// A version folder without a readable manifest.
    pub fn incomplete(suite: &str, version: &str, total_bytes: Option<u64>) -> Manifest {
        Manifest { suite: suite.into(), version: version.into(), total_bytes, ..Manifest::default() }
    }

    /// Bytes in the version folder (0 when not counted).
    pub fn bytes(&self) -> u64 {
        self.total_bytes.unwrap_or(0)
    }
}

/// One app's part of a backup version.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppBackup {
    pub name: String,
    pub kind: String,
    pub databases: Vec<DatabaseDump>,
    /// The app's own backup zip (null for apps without one).
    pub config_zip: Option<ConfigArchive>,
    /// The plain copy of its config folder (null when there's none).
    pub config_snapshot: Option<Snapshot>,
    /// What the app holds (films, series, …), as it counts them.
    pub collection: Value,
    /// How it's set up (indexers, download clients, …), as it counts them.
    pub scope: Value,
    /// What went wrong; empty when the app was backed up in full.
    pub errors: Vec<String>,
    /// The app's container as it ran (null when Docker couldn't say).
    pub container: Option<ContainerInfo>,
    /// The app's own report, from its status call. Absent when it didn't answer.
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub version: Option<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub database_type: Option<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub database_version: Option<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub migration_version: Option<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub branch: Option<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub os: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl AppBackup {
    /// An app's entry before anything is backed up.
    pub fn new(name: &str, kind: &str, container: Option<ContainerInfo>) -> AppBackup {
        AppBackup {
            name: name.into(),
            kind: kind.into(),
            collection: Value::Object(Map::new()),
            scope: Value::Object(Map::new()),
            container,
            ..AppBackup::default()
        }
    }

    /// Backed up without errors.
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Something to restore: databases or a copy of its config folder.
    pub fn restorable(&self) -> bool {
        self.config_snapshot.is_some() || !self.databases.is_empty()
    }

    /// The bytes of its database dumps.
    pub fn database_bytes(&self) -> u64 {
        self.databases.iter().map(|d| d.size).sum()
    }
}

/// One database's `pg_dump`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseDump {
    pub name: String,
    /// The dump, relative to the version folder.
    pub file: String,
    pub size: u64,
    pub sha256: String,
    /// The database's size in Postgres when it was dumped.
    pub db_size: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The app's own backup zip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigArchive {
    /// The zip, relative to the version folder.
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub format: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The Postgres globals dump.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Globals {
    pub file: String,
    pub size: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A container as it ran: what the standalone restore stack needs to run it again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerInfo {
    /// The image reference it was created from.
    pub image: Option<String>,
    pub image_id: String,
    /// The image's repository digest, when it has one.
    pub pinned: Option<String>,
    /// The image's version label ("" when it has none).
    pub version: String,
    /// The environment values the restore stack copies (ids, timezone, umask).
    pub env: IndexMap<String, String>,
    /// Published ports, `host:container[/proto]`.
    pub ports: Vec<String>,
    /// Its bind mounts.
    pub mounts: Vec<Mount>,
    pub network_mode: String,
    /// The image ran its own VPN tunnel.
    pub vpn: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ContainerInfo {
    /// The image to run it again: its digest when pinned, else the reference it ran.
    pub fn image_ref(&self) -> Option<&str> {
        [&self.pinned, &self.image].into_iter().flatten().map(String::as_str).find(|x| !x.is_empty())
    }
}

/// A bind mount.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mount {
    pub source: Option<String>,
    pub target: Option<String>,
    pub ro: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_status_reads_as_incomplete() {
        let m: Manifest = serde_json::from_value(json!({"status": "exploded"})).expect("manifest");
        assert_eq!(m.status, VersionStatus::Incomplete);
        let m: Manifest = serde_json::from_value(json!({"status": "partial"})).expect("manifest");
        assert_eq!(m.status, VersionStatus::Partial);
    }

    #[test]
    fn null_and_absent_stay_apart() {
        let m: Manifest = serde_json::from_value(json!({"vpn": null, "postgres_container": null})).expect("m");
        assert_eq!((m.vpn, m.postgres_container), (Some(Value::Null), Some(None)));
        let m: Manifest = serde_json::from_value(json!({})).expect("m");
        assert_eq!((m.vpn, m.postgres_container), (None, None));
    }

    /// Manifests as the backup has always written them: three from real backup
    /// runs against the test doubles, two laid out by hand in the backup's
    /// order (an app that didn't answer and a failed globals dump; a backup
    /// still running).
    const WRITTEN: [(&str, &str); 5] = [
        ("media", include_str!("../../../tests/fixtures/manifests/media.json")),
        ("downloads", include_str!("../../../tests/fixtures/manifests/downloads.json")),
        ("unreadable-config", include_str!("../../../tests/fixtures/manifests/unreadable-config.json")),
        ("partial", include_str!("../../../tests/fixtures/manifests/partial.json")),
        ("running", include_str!("../../../tests/fixtures/manifests/running.json")),
    ];

    #[test]
    fn written_manifests_rewrite_byte_identical() {
        for (name, text) in WRITTEN {
            let m: Manifest = serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            // every field is a typed one, so nothing rides along in `extra`
            assert!(m.extra.is_empty(), "{name}: {:?}", m.extra.keys());
            for (app, a) in &m.apps {
                let container = a.container.as_ref().map_or(0, |c| c.extra.len());
                let dumps: usize = a.databases.iter().map(|d| d.extra.len()).sum();
                assert_eq!((a.extra.len(), container, dumps), (0, 0, 0), "{name}/{app}");
            }
            assert_eq!(serde_json::to_string_pretty(&m).expect("write"), text, "{name}");
        }
    }

    #[test]
    fn the_constructors_lay_out_what_the_backup_writes() {
        let mut m = Manifest::started(
            "media",
            "20261027T040000Z-1",
            "2026-10-27T04:00:00.000000+00:00".into(),
            "pre-update",
            "0.2.0",
        );
        m.total_bytes = Some(0);
        assert_eq!(serde_json::to_string_pretty(&m).expect("write"), WRITTEN[4].1);

        let partial: Manifest = serde_json::from_str(WRITTEN[3].1).expect("partial");
        let mut sonarr = AppBackup::new("sonarr", "sonarr", None);
        sonarr.databases = partial.apps["sonarr"].databases.clone();
        sonarr.errors = vec!["status: connection refused".into()];
        assert_eq!(sonarr, partial.apps["sonarr"]);
    }

    #[test]
    fn unknown_fields_are_kept() {
        let text = r#"{"suite": "s", "version": "v", "status": "ok",
                       "apps": {"a": {"name": "a", "future": {"x": 1}}}, "later": [1]}"#;
        let m: Manifest = serde_json::from_str(text).expect("manifest");
        assert_eq!(m.extra["later"], json!([1]));
        let back = serde_json::to_value(&m).expect("write");
        assert_eq!((&back["later"], &back["apps"]["a"]["future"]), (&json!([1]), &json!({"x": 1})));
    }

    #[test]
    fn an_old_manifest_reads_with_defaults() {
        let m: Manifest =
            serde_json::from_str(include_str!("../../../tests/fixtures/manifests/minimal.json")).expect("manifest");
        assert_eq!((m.status, m.created.as_deref(), m.bytes()), (VersionStatus::Ok, None, 0));
        let a = &m.apps["radarr7"];
        assert_eq!((a.databases[0].size, a.databases[0].db_size, a.database_bytes()), (0, 0, 0));
        assert!(a.ok() && a.restorable() && a.config_zip.is_none() && a.container.is_none());
        assert!(a.collection.is_null() && a.version.is_none());
        let empty: Manifest = serde_json::from_str("{}").expect("empty");
        assert_eq!(empty.status, VersionStatus::Incomplete);
        assert!(empty.apps.is_empty() && empty.postgres_container.is_none() && empty.standalone.is_none());
    }

    #[test]
    fn a_minimal_entry_reads_with_defaults() {
        let a: AppBackup = serde_json::from_value(json!({"config_zip": {"file": "radarr/x.zip"}})).expect("app");
        assert_eq!(a.config_zip.as_ref().map(|z| (z.file.as_str(), z.size)), Some(("radarr/x.zip", 0)));
        assert!(a.databases.is_empty() && a.ok() && !a.restorable() && a.version.is_none());
    }
}
