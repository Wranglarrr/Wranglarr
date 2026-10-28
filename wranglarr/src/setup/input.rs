// SPDX-License-Identifier: GPL-3.0-or-later
//! Readers for the wizard's request bodies that take what a form or a script
//! would reasonably send: a number may come as a string (" 12 "), a flag as
//! true/false, 0/1 or "yes"/"no".

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::{self, Deserializer};
use serde_json::Value;

use crate::value;

const NOT_A_NUMBER: &str = "expected a whole number";
const NOT_A_FLAG: &str = "expected true or false";

/// A whole number (see [`value::as_whole`]).
pub fn whole_number<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    value::as_whole(&Value::deserialize(d)?).ok_or_else(|| de::Error::custom(NOT_A_NUMBER))
}

/// A map of names to whole numbers (see [`whole_number`]).
pub fn whole_numbers<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, i64>, D::Error> {
    BTreeMap::<String, Value>::deserialize(d)?
        .into_iter()
        .map(|(k, v)| value::as_whole(&v).map(|n| (k, n)).ok_or_else(|| de::Error::custom(NOT_A_NUMBER)))
        .collect()
}

/// A flag (see [`value::as_flag`]).
pub fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    value::as_flag(&Value::deserialize(d)?).ok_or_else(|| de::Error::custom(NOT_A_FLAG))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Deserialize, Debug)]
    struct Body {
        #[serde(deserialize_with = "whole_number")]
        n: i64,
        #[serde(deserialize_with = "whole_numbers")]
        ports: BTreeMap<String, i64>,
        #[serde(deserialize_with = "flag")]
        on: bool,
    }

    fn read(v: Value) -> Result<Body, String> {
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    #[test]
    fn lenient() {
        let b = read(json!({"n": " 12 ", "ports": {"radarr": 7878.0, "sonarr": "8989"}, "on": "Yes"})).expect("reads");
        assert_eq!((b.n, b.ports["radarr"], b.ports["sonarr"], b.on), (12, 7878, 8989, true));
        assert!(!read(json!({"n": 1, "ports": {}, "on": 0})).expect("reads").on);
        assert_eq!(read(json!({"n": 1.5, "ports": {}, "on": true})).err().as_deref(), Some(NOT_A_NUMBER));
        assert_eq!(read(json!({"n": "1_000", "ports": {}, "on": true})).err().as_deref(), Some(NOT_A_NUMBER));
        assert_eq!(read(json!({"n": true, "ports": {}, "on": true})).err().as_deref(), Some(NOT_A_NUMBER));
        assert_eq!(read(json!({"n": 1, "ports": {}, "on": "maybe"})).err().as_deref(), Some(NOT_A_FLAG));
    }
}
