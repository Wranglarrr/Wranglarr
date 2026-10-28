// SPDX-License-Identifier: GPL-3.0-or-later
//! How a check compares an arr's values with a config's, and how its one-line
//! notes show them.

use serde_json::Value;

use crate::value::text;

/// Whether the arr would see a change: numbers compare by value (an arr answers
/// `1.0` where a config says `1`), objects whatever their key order, lists item by
/// item. A boolean never equals a number.
pub fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(x), Some(y)) => x == y,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same_value(a, b)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same_value(v, w)))
        }
        _ => a == b,
    }
}

/// [`same_value`] of two fields that may be missing; missing and null are the same.
pub fn same_field(a: Option<&Value>, b: Option<&Value>) -> bool {
    same_value(a.unwrap_or(&Value::Null), b.unwrap_or(&Value::Null))
}

/// A field for a note: text as it is, "none" when missing or null, anything else
/// as JSON.
pub fn shown(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "none".into(),
        Some(v) => text(v),
    }
}

/// A number as people write it (`2`, `19.6`); anything else as [`text`].
pub fn number(v: &Value) -> String {
    match v {
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => n.as_f64().map_or_else(|| n.to_string(), |f| f.to_string()),
        },
        other => text(other),
    }
}

/// The most of a value a one-line note shows.
const SHORT: usize = 70;

/// A value as JSON, cut to [`SHORT`] characters for a one-line note.
pub fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() <= SHORT { s } else { format!("{}…", s.chars().take(SHORT - 1).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compares_as_the_arr_would() {
        assert!(same_value(&json!(1), &json!(1.0)));
        assert!(same_value(&json!({"a": 1, "b": [2.0]}), &json!({"b": [2], "a": 1.0})));
        assert!(!same_value(&json!(1), &json!(true)));
        assert!(!same_value(&json!([1, 2]), &json!([2, 1])));
        assert!(same_field(None, Some(&Value::Null)));
        assert!(!same_field(None, Some(&json!(0))));
    }

    #[test]
    fn shows_values() {
        assert_eq!(shown(None), "none");
        assert_eq!(shown(Some(&json!("HD"))), "HD");
        assert_eq!(shown(Some(&json!(true))), "true");
        for (v, s) in [(json!(2), "2"), (json!(2.0), "2"), (json!(19.6), "19.6"), (json!(1234567), "1234567")] {
            assert_eq!(number(&v), s, "{v}");
        }
        assert_eq!(short(&json!("x")), r#""x""#);
        assert_eq!(short(&json!({"a": [1, null]})), r#"{"a":[1,null]}"#);
        assert_eq!(short(&json!("y".repeat(100))).chars().count(), SHORT);
    }
}
