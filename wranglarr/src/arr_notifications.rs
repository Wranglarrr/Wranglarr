// SPDX-License-Identifier: GPL-3.0-or-later
//! Keep one entry in each arr's Settings → Connect in step with wranglarr.
//!
//! Used for notification channels (notify.rs) and media servers (mediaservers.rs):
//! wranglarr owns entries named `wranglarr · <name>`, creates them in the arrs that
//! were picked, updates them in place, and removes them from the ones unpicked.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::value::truthy;

use crate::config::AppConfig;
use crate::service;

pub const PREFIX: &str = "wranglarr · ";

/// An arr notification body from the arr's own schema: our name, fields and event flags.
pub fn entry_body(
    schema: &[Value],
    implementation: &str,
    name: &str,
    values: &Map<String, Value>,
    flag_on: impl Fn(&str) -> bool,
    extra: Option<&Map<String, Value>>,
) -> Result<Value, String> {
    let tpl = schema
        .iter()
        .find(|s| s.get("implementation").and_then(Value::as_str) == Some(implementation))
        .and_then(Value::as_object)
        .ok_or_else(|| format!("this app has no {implementation} connection"))?;
    let mut body: Map<String, Value> = tpl
        .iter()
        .filter(|(k, _)| !["id", "presets", "message", "infoLink"].contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    body.insert("name".into(), json!(format!("{PREFIX}{name}")));
    body.insert("tags".into(), json!([]));
    for (k, v) in extra.into_iter().flatten() {
        body.insert(k.clone(), v.clone());
    }
    let keys: Vec<String> = body.keys().cloned().collect();
    for k in keys {
        if !(k.starts_with("on") && body[&k].is_boolean()) {
            continue;
        }
        let mut cs = k.chars();
        let cap: String = cs.next().map(|c| c.to_uppercase().chain(cs).collect()).unwrap_or_default();
        let sup = body.get(&format!("supports{cap}")).is_none_or(truthy);
        let on = sup && flag_on(&k);
        body.insert(k, json!(on));
    }
    let fields: Vec<Value> = tpl
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|f| match f.get("name").and_then(Value::as_str).and_then(|n| values.get(n)) {
            Some(v) => {
                let mut f = f.clone();
                if let Some(o) = f.as_object_mut() {
                    o.insert("value".into(), v.clone());
                }
                f
            }
            None => f.clone(),
        })
        .collect();
    body.insert("fields".into(), Value::Array(fields));
    Ok(Value::Object(body))
}

/// Create / update the entry in the arrs in `want`, remove it from the rest of `had`.
/// A rename removes the old entry first. Returns one result per arr touched.
pub async fn sync(
    apps: &[AppConfig],
    new_name: Option<&str>,
    old_name: Option<&str>,
    want: &HashSet<String>,
    had: &HashSet<String>,
    body_for: impl Fn(&[Value]) -> Result<Value, String>,
    scrub: impl Fn(&str) -> String,
) -> Vec<Value> {
    let mut results = vec![];
    for app in apps {
        if !want.contains(&app.name) && !had.contains(&app.name) {
            continue;
        }
        let mut res = Map::new();
        res.insert("app".into(), json!(app.name));
        match sync_one(app, new_name, old_name, want.contains(&app.name), &body_for).await {
            Ok(action) => {
                res.insert("action".into(), json!(action));
            }
            Err(e) => {
                res.insert("error".into(), json!(scrub(&e)));
            }
        }
        results.push(Value::Object(res));
    }
    results
}

async fn sync_one(
    app: &AppConfig,
    new_name: Option<&str>,
    old_name: Option<&str>,
    wanted: bool,
    body_for: &impl Fn(&[Value]) -> Result<Value, String>,
) -> Result<&'static str, String> {
    let any = service::client(app).await.map_err(|e| e.to_string())?;
    let Some(cl) = any.arr() else {
        return Err(format!("{}: not an arr", app.name));
    };
    let existing = cl.list("notification").await.map_err(|e| e.to_string())?;
    let old = old_name.map(|n| format!("{PREFIX}{n}"));
    let mut removed = false;
    if let Some(old) = &old
        && let Some(cur) = existing.iter().rev().find(|r| r.get("name").and_then(Value::as_str) == Some(old))
        && (!wanted || old_name != new_name)
    {
        // unpicked, or renamed
        cl.delete(&format!("notification/{}", cur["id"])).await.map_err(|e| e.to_string())?;
        removed = true;
    }
    if wanted {
        let schema = cl.list("notification/schema").await.map_err(|e| e.to_string())?;
        let body = body_for(&schema)?;
        let (action, _) = cl.upsert_resource("notification", &body).await.map_err(|e| e.to_string())?;
        Ok(action)
    } else {
        Ok(if removed { "removed" } else { "none" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Vec<Value> {
        vec![json!({
            "id": 0, "implementation": "Discord", "implementationName": "Discord", "configContract": "DiscordSettings",
            "presets": [], "message": {"x": 1}, "infoLink": "https://wiki",
            "onGrab": false, "supportsOnGrab": true, "onDownload": false, "supportsOnDownload": false,
            "onHealthIssue": false, "includeHealthWarnings": false,
            "fields": [{"name": "webHookUrl", "value": null}, {"name": "username", "value": null}, {"name": "avatar"}]
        })]
    }

    #[test]
    fn body_from_schema() {
        let mut values = Map::new();
        values.insert("webHookUrl".into(), json!("https://d/hook"));
        let b = entry_body(&schema(), "Discord", "Alerts", &values, |k| k != "onHealthIssue", None).expect("body");
        assert_eq!(b["name"], "wranglarr · Alerts");
        assert_eq!(b["tags"], json!([]));
        assert!(b.get("id").is_none() && b.get("presets").is_none() && b.get("message").is_none());
        assert!(b.get("infoLink").is_none());
        assert_eq!(b["onGrab"], true);
        assert_eq!(b["onDownload"], false, "the app doesn't support it");
        assert_eq!(b["onHealthIssue"], false, "not picked");
        assert_eq!(b["fields"][0]["value"], "https://d/hook");
        assert_eq!(b["fields"][1]["value"], Value::Null);
        assert!(b["fields"][2].get("value").is_none());
    }

    #[test]
    fn no_such_implementation() {
        let e = entry_body(&schema(), "Telegram", "x", &Map::new(), |_| true, None).expect_err("no telegram");
        assert_eq!(e, "this app has no Telegram connection");
    }
}
