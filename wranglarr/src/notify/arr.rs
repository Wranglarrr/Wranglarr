// SPDX-License-Identifier: GPL-3.0-or-later
//! A channel as an arr's own Connect entry, so the arr sends its grabs, imports
//! and health issues to the same place.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::arr_notifications;

use super::catalog::{arr_flags, kind};
use super::channel::{Channel, whole_number};

// ---- the arrs' fields ----------------------------------------------------------------

/// Our channel settings in the arrs' field names.
pub(super) fn arr_fields(c: &Channel) -> std::result::Result<Map<String, Value>, String> {
    let g = |k: &str| json!(c.cfg(k));
    let s = |k: &str| c.cfg(k).to_string();
    let v = match c.kind.as_str() {
        "telegram" => {
            let mut d = json!({"botToken": g("bot_token"), "chatId": g("chat_id"), "sendSilently": false});
            if !s("topic_id").is_empty() {
                d["topicId"] = json!(whole_number("topic ID", &s("topic_id"))?);
            }
            d
        }
        "discord" => json!({"webHookUrl": g("webhook_url")}),
        "slack" => json!({"webHookUrl": g("webhook_url"), "username": "wranglarr"}),
        "ntfy" => json!({"serverUrl": g("server"), "topics": [s("topic")], "accessToken": g("token"),
                         "userName": g("username"), "password": g("password"), "priority": 3}),
        "gotify" => json!({"server": g("server"), "appToken": g("token"), "priority": 5}),
        "pushover" => json!({"apiKey": g("token"), "userKey": g("user_key"), "priority": 0}),
        "apprise" => json!({"serverUrl": g("server"), "configurationKey": g("key"), "statelessUrls": g("urls"),
                            "notificationType": 0}),
        "email" => {
            let port = if s("port").is_empty() { 587 } else { whole_number("port", &s("port"))? };
            let enc = match s("security").as_str() {
                "ssl" => 1,
                "none" => 2,
                _ => 0,
            };
            let to: Vec<String> =
                s("to").split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect();
            json!({"server": g("host"), "port": port,
                   "useEncryption": enc,                         // Radarr / Sonarr
                   "requireEncryption": s("security") != "none", // Lidarr / Readarr
                   "username": g("username"), "password": g("password"), "from": g("sender"), "to": to})
        }
        "webhook" => json!({"url": g("url"), "method": 1, "username": g("username"), "password": g("password")}),
        _ => json!({}),
    };
    Ok(v.as_object().cloned().unwrap_or_default())
}

/// The arr notification body for a channel, from the arr's schema.
pub(super) fn arr_body(schema: &[Value], c: &Channel) -> std::result::Result<Value, String> {
    let flags: HashSet<&str> = c.arr.events.iter().flat_map(|e| arr_flags(e)).collect();
    let k = kind(&c.kind).ok_or_else(|| format!("unknown kind {:?}", c.kind))?;
    let mut body = arr_notifications::entry_body(schema, k.arr, &c.name, &arr_fields(c)?, |f| flags.contains(f), None)?;
    if body.get("includeHealthWarnings").is_some() {
        body["includeHealthWarnings"] = json!(c.arr.events.iter().any(|e| e == "health"));
    }
    Ok(body)
}
