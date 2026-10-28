// SPDX-License-Identifier: GPL-3.0-or-later
//! Lenient reading of the apps' loose JSON. The apps' answers change between
//! versions and fields go missing or change type, so a reader takes what is there:
//! a missing or empty value counts as unset, numbers may come as strings, and
//! anything that isn't a number reads as 0.

use serde_json::Value;

/// Whether a value is set: not null, false, 0, "" or an empty array or object.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `a` when it is set, else `b`.
pub fn or<'a>(a: &'a Value, b: &'a Value) -> &'a Value {
    if truthy(a) { a } else { b }
}

/// A value as text: a string as it is, "" for null, anything else as JSON.
pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        o => o.to_string(),
    }
}

/// A number, or a string holding one; anything else is 0.
pub fn float(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        Value::Bool(b) => f64::from(u8::from(*b)),
        _ => 0.0,
    }
}

/// A whole number, or a string holding one (a fraction is cut off); anything else is 0.
pub fn int(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        Value::Bool(b) => i64::from(*b),
        _ => 0,
    }
}

/// How many items an array or object holds, or characters a string; 0 for anything else.
pub fn count(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        Value::String(s) => s.chars().count(),
        _ => 0,
    }
}

/// The first `n` characters.
pub fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---- strict readers -------------------------------------------------------------
// For what people and scripts send (request bodies, query strings, `.env`): these
// take the usual spellings and refuse the rest (None), so a caller can say what
// was wrong instead of guessing.

/// A yes/no word: true/t/yes/y/on/1 or false/f/no/n/off/0, in any case, spaces
/// around ignored.
pub fn flag_word(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "on" => Some(true),
        "0" | "false" | "f" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

/// A flag: a bool, the number 0 or 1, or a [`flag_word`].
pub fn as_flag(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) if n.as_f64() == Some(0.0) => Some(false),
        Value::Number(n) if n.as_f64() == Some(1.0) => Some(true),
        Value::String(s) => flag_word(s),
        _ => None,
    }
}

/// A whole number: an integer, a float without a fraction (within `i64`), or a
/// string holding an integer (spaces around ignored).
pub fn as_whole(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64().filter(|f| f.fract() == 0.0 && (i64::MIN as f64..i64::MAX as f64).contains(f)).map(|f| f as i64)
        }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lenient_reading() {
        assert!(!truthy(&json!(0)) && !truthy(&json!("")) && !truthy(&json!([])) && !truthy(&Value::Null));
        assert!(truthy(&json!(0.5)) && truthy(&json!("x")) && truthy(&json!({"a": 1})));
        assert_eq!(or(&json!(0), &json!(4)), &json!(4));
        assert_eq!(or(&json!(3), &json!(4)), &json!(3));
        assert_eq!(text(&Value::Null), "");
        assert_eq!(text(&json!(12)), "12");
        assert_eq!(text(&json!(true)), "true");
        assert_eq!(text(&json!({"a": 1})), r#"{"a":1}"#);
        assert_eq!(float(&json!("12.5")), 12.5);
        assert_eq!(int(&json!("7")), 7);
        assert_eq!(int(&json!(7.9)), 7);
        assert_eq!(int(&json!("x")), 0);
        assert_eq!(count(&json!({"a": 1, "b": 2})), 2);
        assert_eq!(head("abcdef", 3), "abc");
    }

    #[test]
    fn strict_reading() {
        assert_eq!(flag_word(" Yes "), Some(true));
        assert_eq!(flag_word("off"), Some(false));
        assert_eq!(flag_word("maybe"), None);
        assert_eq!(as_flag(&json!(0)), Some(false));
        assert_eq!(as_flag(&json!(1.0)), Some(true));
        assert_eq!(as_flag(&json!(2)), None);
        assert_eq!(as_flag(&json!("T")), Some(true));
        assert_eq!(as_flag(&Value::Null), None);
        assert_eq!(as_whole(&json!(" 12 ")), Some(12));
        assert_eq!(as_whole(&json!(7878.0)), Some(7878));
        assert_eq!(as_whole(&json!(1.5)), None);
        assert_eq!(as_whole(&json!("1_000")), None);
        assert_eq!(as_whole(&json!(true)), None);
        assert_eq!(as_whole(&json!(1e300)), None);
    }
}
