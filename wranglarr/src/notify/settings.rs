// SPDX-License-Identifier: GPL-3.0-or-later
//! The watcher's settings: how often it looks, and its thresholds. Stored under
//! `notify:settings` as `{interval, disk_free_gb, stall_minutes, link}`.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::value::{text, truthy};

/// A setting as a whole number: a number (a fraction is cut off) or a string
/// holding a whole number. None for anything else.
pub(super) fn setting_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A setting as a number: a number or a string holding one (not inf or NaN).
/// None for anything else.
fn setting_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok().filter(|f: &f64| f.is_finite()),
        _ => None,
    }
}

const INTERVAL: i64 = 5;
const DISK_FREE_GB: f64 = 20.0;
const STALL_MINUTES: i64 = 60;

/// How often the watcher looks, in minutes: one minute to a day.
fn interval(minutes: i64) -> i64 {
    minutes.clamp(1, 1440)
}

/// The watcher's settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Settings {
    /// Minutes between checks.
    pub interval: i64,
    /// Free space, in GB, below which a disk is reported; 0 is off.
    pub disk_free_gb: f64,
    /// Minutes a download may stall before it's reported; 0 is off.
    pub stall_minutes: i64,
    /// The address of wranglarr in the messages ("" for none).
    pub link: String,
    /// Stored keys this version doesn't use, kept as they are.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            interval: INTERVAL,
            disk_free_gb: DISK_FREE_GB,
            stall_minutes: STALL_MINUTES,
            link: String::new(),
            extra: Map::new(),
        }
    }
}

impl Settings {
    /// The stored settings over the defaults; a value that doesn't read is the default.
    pub fn from_stored(stored: Option<&Value>) -> Settings {
        let d = Settings::default();
        let Some(Value::Object(o)) = stored else { return d };
        let mut extra = o.clone();
        let mut take = |k: &str| extra.remove(k);
        Settings {
            interval: interval(take("interval").as_ref().and_then(setting_int).unwrap_or(d.interval)),
            disk_free_gb: take("disk_free_gb").as_ref().and_then(setting_float).unwrap_or(d.disk_free_gb),
            stall_minutes: take("stall_minutes").as_ref().and_then(setting_int).unwrap_or(d.stall_minutes),
            link: take("link").map(|v| text(&v)).unwrap_or_default(),
            extra,
        }
    }

    /// The settings with a settings body from the page applied, checked. Keys
    /// the body leaves out keep their current value.
    pub(super) fn merged(&self, body: &Map<String, Value>) -> Result<Settings> {
        let bad = || Error::invalid("interval, disk space and stall time must be numbers");
        let num = |k: &str, cur: Value| body.get(k).cloned().unwrap_or(cur);
        let minutes = setting_int(&num("interval", self.interval.into())).ok_or_else(bad)?;
        let disk = setting_float(&num("disk_free_gb", self.disk_free_gb.into())).ok_or_else(bad)?;
        let stall = setting_int(&num("stall_minutes", self.stall_minutes.into())).ok_or_else(bad)?;
        let link = match body.get("link") {
            Some(v) if truthy(v) => text(v),
            Some(_) => String::new(),
            None => self.link.clone(),
        };
        let link = link.trim().trim_end_matches('/').to_string();
        if !link.is_empty() && !link.starts_with("http://") && !link.starts_with("https://") {
            return Err(Error::invalid("the link must start with http:// or https://"));
        }
        Ok(Settings {
            interval: interval(minutes),
            disk_free_gb: disk.max(0.0),
            stall_minutes: stall.max(0),
            link,
            extra: self.extra.clone(),
        })
    }
}
