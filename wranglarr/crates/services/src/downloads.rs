// SPDX-License-Identifier: GPL-3.0-or-later
//! What the download clients share: the Downloads list's entry and the readers of
//! their answers.

use serde::Serialize;
use serde_json::{Value, json};

use wranglarr_core::http::{ArrError, ArrResult};
use wranglarr_core::value::text;

/// One entry of the Downloads list, the same shape for every client.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Download {
    pub id: String,
    pub name: String,
    pub category: String,
    pub state: String,
    pub state_text: String,
    pub progress: f64,
    pub size: i64,
    pub done: i64,
    pub speed: i64,
    pub eta: Option<i64>,
    pub added: Option<i64>,
}

impl Default for Download {
    fn default() -> Self {
        Download {
            id: String::new(),
            name: String::new(),
            category: String::new(),
            state: "queued".into(),
            state_text: String::new(),
            progress: 0.0,
            size: 0,
            done: 0,
            speed: 0,
            eta: None,
            added: None,
        }
    }
}

impl Download {
    /// Progress kept within 0..1.
    pub(crate) fn clamped(mut self) -> Self {
        self.progress = if self.progress.is_nan() { 0.0 } else { self.progress.clamp(0.0, 1.0) };
        self
    }
}

/// SABnzbd's timeleft: "0:05:12" or "1:02:05:12" (days first). None for 0 or junk.
pub fn hms(v: &Value) -> Option<i64> {
    let s = text(v);
    let parts: Vec<i64> = s.split(':').map(|x| x.trim().parse::<i64>()).collect::<Result<_, _>>().ok()?;
    if parts.is_empty() || parts.len() > 4 || parts.iter().any(|n| *n < 0) {
        return None;
    }
    let secs = parts
        .iter()
        .rev()
        .zip([1i64, 60, 3600, 86400])
        .try_fold(0i64, |total, (n, mul)| total.checked_add(n.checked_mul(mul)?))?;
    (secs != 0).then_some(secs)
}

pub(crate) fn rows(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        _ => vec![],
    }
}

pub(crate) fn status_map(version: Value, instance: &str, database_version: Value) -> Value {
    json!({"version": version, "databaseType": "files", "instanceName": instance, "branch": null,
           "osName": null, "migrationVersion": null, "databaseVersion": database_version})
}

/// JSON body, or an ArrError naming what was asked.
pub(crate) fn parse_json(name: &str, what: &str, body: &str) -> ArrResult<Value> {
    serde_json::from_str(body).map_err(|e| ArrError::new(format!("{name}: {what}: bad JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeleft() {
        assert_eq!(hms(&json!("0:05:12")), Some(312));
        assert_eq!(hms(&json!("1:02:05:12")), Some(86400 + 2 * 3600 + 312));
        assert_eq!(hms(&json!("0:00:00")), None);
        assert_eq!(hms(&json!("")), None);
        assert_eq!(hms(&Value::Null), None);
        assert_eq!(hms(&json!("soon")), None);
        assert_eq!(hms(&json!(42)), Some(42));
    }

    #[test]
    fn timeleft_rejects_overflow_and_invalid_fields() {
        for time in ["9223372036854775807:00", "-1:00", "1:2:3:4:5"] {
            assert_eq!(hms(&json!(time)), None);
        }
    }
}
