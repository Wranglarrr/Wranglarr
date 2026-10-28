// SPDX-License-Identifier: GPL-3.0-or-later
//! The message for a finished run (backup, restore, update, self-update, sync).

use serde_json::Value;

use crate::value::{text, truthy};

/// `(event, title, message, level)` for a finished job, or None for nothing to send.
pub(super) fn run_message(
    suite: &str,
    kind: &str,
    target: &str,
    status: &str,
    result: &Value,
    run_id: i64,
) -> Option<(&'static str, String, String, &'static str)> {
    let bad = status == "failed" || status == "partial";
    let err = result.get("error").filter(|e| truthy(e));
    let what = match err {
        Some(e) => format!("{status}: {}", text(e)),
        None => status.to_string(),
    };
    let version = result.get("version").map_or_else(|| "?".to_string(), text);
    match kind {
        "backup" if bad => Some((
            "backup_failed",
            format!("Backup of {suite} {status}"),
            format!("Version {version} is {what}. See Activity, run {run_id}."),
            "error",
        )),
        "backup" => Some((
            "backup_ok",
            format!("Backup of {suite} done"),
            format!("Version {version}, {}.", human(result.get("total_bytes").unwrap_or(&Value::Null))),
            "ok",
        )),
        "restore" | "restore-plan" => Some((
            "restore",
            format!("Restore {target} {status}"),
            format!("Suite {suite}: {what}. See Activity, run {run_id}."),
            if bad { "error" } else { "ok" },
        )),
        "update" | "rollback" => {
            let detail = match result.get("results") {
                Some(Value::Object(res)) if !res.is_empty() => {
                    res.iter().map(|(n, r)| format!("{n} {}", text(r))).collect::<Vec<_>>().join(", ")
                }
                _ => what.clone(),
            };
            if bad {
                Some((
                    "update_failed",
                    format!("Container {kind} {status}"),
                    format!("{detail}. See Activity, run {run_id}."),
                    "error",
                ))
            } else {
                let msg = if detail.is_empty() { target.to_string() } else { detail };
                Some(("update_done", format!("Container {kind} done"), msg, "ok"))
            }
        }
        "self-update" if bad => Some((
            "update_failed",
            "wranglarr update failed".to_string(),
            format!("{what}. The version before it is running. See Activity, run {run_id}."),
            "error",
        )),
        "self-update" => {
            // pulled nothing new: nothing to tell
            let current =
                result.get("results").and_then(Value::as_object).is_some_and(|r| r.values().all(|v| v == "current"));
            let now = result.get("version").map(text).filter(|v| !v.is_empty());
            (!current).then(|| {
                let msg =
                    now.map_or_else(|| format!("{target} is on its new image."), |v| format!("{target} is now {v}."));
                ("update_done", "wranglarr updated".to_string(), msg, "ok")
            })
        }
        "sync" | "connect" | "indexers" if bad => {
            let label = match kind {
                "sync" => "Shared settings sync",
                "connect" => "Connecting apps",
                _ => "Indexer sync",
            };
            let who = if target.is_empty() { suite } else { target };
            Some((
                "sync_failed",
                format!("{label} {status}"),
                format!("{who}: {what}. See Activity, run {run_id}."),
                "warning",
            ))
        }
        _ => None,
    }
}

/// A size in bytes for people: `512 B`, `1.5 GB`.
pub(super) fn human(v: &Value) -> String {
    let n = match v {
        Value::Null => 0.0,
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) if s.is_empty() => 0.0,
        Value::String(s) => match s.trim().parse::<f64>() {
            Ok(x) => x,
            Err(_) => return "?".into(),
        },
        Value::Array(a) if a.is_empty() => 0.0,
        Value::Object(o) if o.is_empty() => 0.0,
        _ => return "?".into(),
    };
    crate::format::bytes(n)
}
