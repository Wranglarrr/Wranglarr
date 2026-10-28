// SPDX-License-Identifier: GPL-3.0-or-later
//! A `[[shared]]` resource from wranglarr.toml (a download client, a
//! notification) as the arr API body that creates or updates it in one arr.
//! wranglarr.toml's side, parsing and who it applies to, is
//! [`wranglarr_core::shared::SharedResource`] in core.

use serde_json::{Map, Value, json};

use wranglarr_core::shared::SharedResource;

/// The arr API body for `resource` in an arr of `kind`: the top-level keys in the
/// arrs' camelCase, that kind's overrides on top, the arrs' defaults
/// (`configContract`, enabled, no tags) and `fields` as `[{name, value}]`.
pub fn body(resource: &SharedResource, kind: &str) -> Value {
    let ov = resource.overrides.get(kind).and_then(Value::as_object).cloned().unwrap_or_default();
    let mut top: Map<String, Value> = resource.base.iter().map(|(k, v)| (camel(k), v.clone())).collect();
    for (k, v) in &ov {
        if k != "fields" {
            top.insert(camel(k), v.clone());
        }
    }
    let mut fields = resource.fields.clone();
    if let Some(Value::Object(f)) = ov.get("fields") {
        fields.extend(f.clone());
    }
    let implementation = top.get("implementation").and_then(Value::as_str).unwrap_or("").to_string();
    top.entry("configContract").or_insert_with(|| json!(format!("{implementation}Settings")));
    top.entry("enable").or_insert(json!(true));
    top.entry("tags").or_insert(json!([]));
    top.insert("name".into(), json!(resource.name));
    top.insert(
        "fields".into(),
        Value::Array(fields.into_iter().map(|(k, v)| json!({"name": k, "value": v})).collect()),
    );
    Value::Object(top)
}

/// `snake_case` as the arrs spell it: `camelCase`.
fn camel(key: &str) -> String {
    let mut parts = key.split('_');
    let mut out = parts.next().unwrap_or("").to_string();
    for p in parts {
        let mut c = p.chars();
        if let Some(f) = c.next() {
            out.extend(f.to_uppercase());
            out.push_str(c.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wranglarr_core::shared::parse_shared;

    #[test]
    fn body_merges_overrides() {
        let raw: Map<String, Value> = serde_json::from_value(json!({
            "download_client": [{"name": "qb", "implementation": "QBittorrent", "protocol": "torrent",
                "fields": {"host": "qbittorrent", "movieCategory": "radarr"},
                "overrides": {"sonarr": {"fields": {"tvCategory": "sonarr"}, "priority": 2}}}]
        }))
        .expect("json");
        let r = &parse_shared(&raw).expect("parses")[0];
        let b = body(r, "sonarr");
        assert_eq!(b["configContract"], "QBittorrentSettings");
        assert_eq!(b["priority"], 2);
        assert_eq!(b["enable"], true);
        let names: Vec<&str> =
            b["fields"].as_array().expect("fields").iter().filter_map(|f| f["name"].as_str()).collect();
        assert!(names.contains(&"tvCategory") && names.contains(&"host"));
    }

    #[test]
    fn keys_in_camel_case() {
        assert_eq!(camel("remove_completed_downloads"), "removeCompletedDownloads");
        assert_eq!(camel("priority"), "priority");
    }
}
