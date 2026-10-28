// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared ("global") download clients and notifications.
//!
//! Defined once in wranglarr.toml and pushed to every app by name, so the same
//! qBittorrent or Discord webhook is not typed into Radarr, Sonarr and Lidarr
//! separately. Field names differ per app for a few settings (movieCategory /
//! tvCategory / musicCategory) and those go in `overrides`. This is the config's
//! side; the arr API body is the services crate's (`arr::shared`).

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// toml section -> arr API path
const RESOURCE_PATH: &[(&str, &str)] = &[("download_client", "downloadclient"), ("notification", "notification")];
const RESERVED: &[&str] = &["fields", "overrides", "apply_to", "test"];

#[derive(Debug, Clone)]
pub struct SharedResource {
    /// API path: downloadclient | notification
    pub resource: String,
    pub name: String,
    /// Top-level keys (implementation, protocol, enable, …)
    pub base: Map<String, Value>,
    pub fields: Map<String, Value>,
    /// kind -> `{fields: {...}, <top-level keys>}`
    pub overrides: Map<String, Value>,
    /// App kinds or app names; empty = all.
    pub apply_to: Vec<String>,
    pub test: bool,
}

impl SharedResource {
    pub fn label(&self) -> &str {
        match self.resource.as_str() {
            "downloadclient" => "download client",
            "notification" => "notification",
            r => r,
        }
    }

    pub fn applies_to(&self, app_name: &str, kind: &str) -> bool {
        self.apply_to.is_empty() || self.apply_to.iter().any(|a| a == app_name || a == kind)
    }
}

pub fn parse_shared(raw: &Map<String, Value>) -> Result<Vec<SharedResource>> {
    let known: HashSet<&str> = RESOURCE_PATH.iter().map(|(k, _)| *k).collect();
    let mut unknown: Vec<&str> = raw.keys().map(String::as_str).filter(|k| !known.contains(k)).collect();
    unknown.sort_unstable();
    if unknown.contains(&"indexer") {
        return Err(Error::invalid("shared.indexer is not supported: add indexers on the Indexers page"));
    }
    if !unknown.is_empty() {
        return Err(Error::invalid(format!(
            "shared: unknown section(s) {} (use download_client or notification)",
            unknown.join(", ")
        )));
    }
    let mut out = vec![];
    for (key, path) in RESOURCE_PATH {
        for item in raw.get(*key).and_then(Value::as_array).into_iter().flatten() {
            let Some(item) = item.as_object() else { continue };
            let (Some(name), Some(_)) = (item.get("name"), item.get("implementation")) else {
                return Err(Error::invalid(format!("shared.{key}: 'name' and 'implementation' are required")));
            };
            let obj = |k: &str| item.get(k).and_then(Value::as_object).cloned().unwrap_or_default();
            out.push(SharedResource {
                resource: (*path).to_string(),
                name: name.as_str().map(str::to_string).unwrap_or_else(|| name.to_string()),
                base: item
                    .iter()
                    .filter(|(k, _)| !RESERVED.contains(&k.as_str()) && k.as_str() != "name")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                fields: obj("fields"),
                overrides: obj("overrides"),
                apply_to: item
                    .get("apply_to")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                test: crate::config::flag(item.get("test"), true),
            });
        }
    }
    let mut seen = HashSet::new();
    if !out.iter().all(|r| seen.insert((r.resource.clone(), r.name.clone()))) {
        return Err(Error::invalid("shared resource names must be unique per type"));
    }
    Ok(out)
}

static IDENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9 ._()\-]+$").expect("static regex"));

pub fn validate_name(name: &str) -> Result<()> {
    if IDENT.is_match(name) { Ok(()) } else { Err(Error::invalid(format!("bad resource name {name:?}"))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_indexer() {
        let raw: Map<String, Value> = serde_json::from_value(json!({"indexer": []})).expect("json");
        assert!(parse_shared(&raw).is_err());
    }
}
