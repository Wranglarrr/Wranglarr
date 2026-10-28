// SPDX-License-Identifier: GPL-3.0-or-later
//! A channel as stored, as the browser sees it (secrets masked), and the
//! checks a channel from the page goes through.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::ids::token_hex;
use crate::value::{head, text, truthy};

use super::MASK;
use super::catalog::{ARR_EVENTS, ARR_EVENTS_DEFAULT, event, kind};
use crate::time::now_iso;

// ---- stored shapes ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ArrPick {
    #[serde(default)]
    pub apps: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
}

/// One channel, as stored under `notify:channels`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    #[serde(default)]
    pub arr: ArrPick,
    #[serde(default)]
    pub created: String,
}

impl Channel {
    pub(super) fn cfg(&self, k: &str) -> &str {
        self.config.get(k).map_or("", String::as_str)
    }
}

pub(super) fn secret_keys(kind_id: &str) -> HashSet<&'static str> {
    kind(kind_id).map(|k| k.fields.iter().filter(|f| f.kind == "secret").map(|f| f.key).collect()).unwrap_or_default()
}

/// A channel as the browser sees it: secrets masked.
pub fn public(c: &Channel) -> Value {
    let sk = secret_keys(&c.kind);
    let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
    let cfg: Map<String, Value> = c
        .config
        .iter()
        .map(|(k, val)| (k.clone(), json!(if sk.contains(k.as_str()) && !val.is_empty() { MASK } else { val })))
        .collect();
    v["config"] = Value::Object(cfg);
    v
}

/// Secrets of the channel replaced by "…" in an error text, cut to 300 characters.
pub(super) fn scrub(c: &Channel, text: &str) -> String {
    let mut text = text.to_string();
    for k in secret_keys(&c.kind) {
        let v = c.cfg(k);
        if v.chars().count() >= 4 {
            text = text.replace(v, "…");
        }
    }
    head(&text, 300)
}

/// A channel field that must be a whole number.
pub(super) fn whole_number(label: &str, v: &str) -> std::result::Result<i64, String> {
    v.trim().parse().map_err(|_| format!("{label} {v:?} isn't a whole number"))
}

// ---- validating a channel -------------------------------------------------------------

/// A channel from a request body (and the stored one it replaces), checked.
pub(super) fn clean(body: &Map<String, Value>, old: Option<&Channel>, arr_names: &HashSet<String>) -> Result<Channel> {
    let get = |k: &str| body.get(k).filter(|v| truthy(v));
    let config = match get("config") {
        None => None,
        Some(Value::Object(o)) => Some(o),
        Some(_) => return Err(Error::invalid("config and arr must be objects")),
    };
    let arr = match get("arr") {
        None => None,
        Some(Value::Object(o)) => Some(o),
        Some(_) => return Err(Error::invalid("config and arr must be objects")),
    };
    let events = match get("events") {
        None => vec![],
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err(Error::invalid("events must be a list")),
    };
    let kind_v = get("kind").cloned().or_else(|| old.map(|o| json!(o.kind))).filter(truthy).unwrap_or(Value::Null);
    let k = kind_v.as_str().and_then(kind).ok_or_else(|| Error::invalid(crate::mediaservers::unknown_kind(&kind_v)))?;
    let name_v = get("name").map(text).unwrap_or_default();
    let mut name = name_v.trim().to_string();
    if name.is_empty() {
        name = k.label.to_string();
    }
    if name.chars().count() > 60 {
        return Err(Error::invalid("the name is too long"));
    }
    let old_cfg = old.map(|o| &o.config);
    let mut cfg = BTreeMap::new();
    for fd in k.fields {
        let v = match config.and_then(|c| c.get(fd.key)) {
            None => fd.default.to_string(),
            Some(Value::Null) => String::new(),
            Some(v) => text(v).trim().to_string(),
        };
        let v = if fd.kind == "secret" && v == MASK {
            old_cfg.and_then(|c| c.get(fd.key)).cloned().unwrap_or_default()
        } else {
            v
        };
        if fd.kind == "number" && !v.is_empty() && !v.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::invalid(format!("{}: a number", fd.label)));
        }
        if fd.kind == "select" && !v.is_empty() && !fd.options.iter().any(|(o, _)| *o == v) {
            return Err(Error::invalid(format!("{}: not one of the choices", fd.label)));
        }
        if fd.required && v.is_empty() {
            return Err(Error::invalid(format!("{}: {} is needed", k.label, fd.label)));
        }
        cfg.insert(fd.key.to_string(), v);
    }
    let has = |key: &str| cfg.get(key).is_some_and(|v: &String| !v.is_empty());
    if k.id == "apprise" && !(has("key") || has("urls")) {
        return Err(Error::invalid("Apprise: a config key or Apprise URLs is needed"));
    }
    for key in ["server", "url", "webhook_url"] {
        if let Some(v) = cfg.get(key)
            && !v.is_empty()
            && !v.starts_with("http://")
            && !v.starts_with("https://")
        {
            return Err(Error::invalid(format!("{}: must start with http:// or https://", key.replace('_', " "))));
        }
    }
    let events: Vec<String> =
        events.iter().filter_map(Value::as_str).filter(|e| event(e).is_some()).map(str::to_string).collect();
    let arr_apps: Vec<String> = arr
        .and_then(|a| a.get("apps"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|a| arr_names.contains(*a))
        .map(str::to_string)
        .collect();
    let arr_events: Vec<String> = match arr.and_then(|a| a.get("events")).filter(|v| truthy(v)) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(_) => vec![],
        None => ARR_EVENTS_DEFAULT.iter().map(|s| s.to_string()).collect(),
    };
    let arr_events = arr_events.into_iter().filter(|e| ARR_EVENTS.iter().any(|(id, _, _)| id == e)).collect();
    Ok(Channel {
        id: old.map(|o| o.id.clone()).filter(|i| !i.is_empty()).unwrap_or_else(|| token_hex(4)),
        name,
        kind: k.id.to_string(),
        enabled: body.get("enabled").is_none_or(truthy),
        events,
        config: cfg,
        arr: ArrPick { apps: arr_apps, events: arr_events },
        created: old.map(|o| o.created.clone()).filter(|c| !c.is_empty()).unwrap_or_else(now_iso),
    })
}
