// SPDX-License-Identifier: GPL-3.0-or-later
//! The restore plan the setup wizard leaves for the running app:
//! `restore-plan.json` in wranglarr's appdata folder, listing the backed-up
//! versions to restore once the stack is up. The wizard writes it; the app shows
//! it, runs it and removes it once every item is restored.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The plan's file name, in wranglarr's appdata folder.
pub const FILE: &str = "restore-plan.json";
pub const LOCK: &str = "restore-plan";

/// What the wizard was asked to restore: one version of one app.
#[derive(Debug, Clone, Deserialize)]
pub struct RestoreItem {
    pub suite: String,
    pub version: String,
    pub app: String,
}

/// `restore-plan.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RestorePlan {
    /// Set by the reader when the file can't be read; never written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When the plan was made (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Who made it ("setup wizard").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub items: Vec<PlanItem>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One version of one app to restore.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanItem {
    pub suite: String,
    pub version: String,
    pub app: String,
    /// Restore its databases.
    pub databases: bool,
    /// Restore its config.
    pub config: bool,
    /// Persisted before starting. An interrupted item requires manual review.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub started: bool,
    /// Set by the reader: the app is configured and the version is in the store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for PlanItem {
    /// Both halves restored unless the plan says otherwise.
    fn default() -> PlanItem {
        PlanItem {
            suite: String::new(),
            version: String::new(),
            app: String::new(),
            databases: true,
            config: true,
            started: false,
            available: None,
            extra: Map::new(),
        }
    }
}

impl PlanItem {
    /// `app@version`, as the run log and the result name it.
    pub fn label(&self) -> String {
        format!("{}@{}", self.app, self.version)
    }
}

impl RestorePlan {
    /// A plan made now by the wizard: each item restores its databases and its config.
    pub fn from_wizard(items: &[RestoreItem]) -> RestorePlan {
        RestorePlan {
            created: Some(crate::time::now_iso()),
            source: Some("setup wizard".into()),
            items: items
                .iter()
                .map(|i| PlanItem {
                    suite: i.suite.clone(),
                    version: i.version.clone(),
                    app: i.app.clone(),
                    ..PlanItem::default()
                })
                .collect(),
            ..RestorePlan::default()
        }
    }

    /// What the app shows for a plan file it can't read.
    pub fn unreadable() -> RestorePlan {
        RestorePlan { error: Some(format!("unreadable {FILE}")), ..RestorePlan::default() }
    }

    /// The items that can be restored (see [`PlanItem::available`]).
    pub fn available(&self) -> impl Iterator<Item = &PlanItem> {
        self.items.iter().filter(|i| i.available == Some(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_wizards_plan_reads_back() {
        let asked = [RestoreItem { suite: "media".into(), version: "20260101T000000Z".into(), app: "radarr".into() }];
        let plan = RestorePlan::from_wizard(&asked);
        let text = serde_json::to_string_pretty(&plan).expect("write");
        let back: RestorePlan = serde_json::from_str(&text).expect("read");
        assert_eq!(back, plan);
        assert_eq!(serde_json::to_string_pretty(&back).expect("rewrite"), text);
        let v: Value = serde_json::from_str(&text).expect("json");
        let keys: Vec<&str> = v.as_object().expect("object").keys().map(String::as_str).collect();
        assert_eq!(keys, ["created", "source", "items"]);
        assert_eq!(
            v["items"],
            json!([{"suite": "media", "version": "20260101T000000Z", "app": "radarr", "databases": true, "config": true}])
        );
    }

    #[test]
    fn a_partial_plan_reads_with_defaults() {
        let plan: RestorePlan =
            serde_json::from_value(json!({"items": [{"suite": "media", "version": "v", "app": "sonarr", "note": 1}]}))
                .expect("plan");
        let item = &plan.items[0];
        assert!(item.databases && item.config && item.available.is_none());
        assert_eq!(item.label(), "sonarr@v");
        assert_eq!(serde_json::to_value(item).expect("item")["note"], 1);
        assert_eq!(
            serde_json::to_value(RestorePlan::unreadable()).expect("plan"),
            json!({"error": "unreadable restore-plan.json", "items": []})
        );
    }
}
