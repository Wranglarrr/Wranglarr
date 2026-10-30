// SPDX-License-Identifier: GPL-3.0-or-later
//! Custom formats: config → API body, and what would change, in words.

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use super::super::parse::{Cf, cerr};
use super::super::values::same_value;
use super::{ArrView, rows};
use crate::error::Result as R;
use crate::value::{text, truthy};

/// A custom format as the config writes it.
pub fn cf_portable(cf: &Value) -> Value {
    let specs: Vec<Value> = rows(&cf["specifications"])
        .iter()
        .map(|s| {
            let fields: Map<String, Value> = rows(&s["fields"])
                .iter()
                .map(|f| (text(&f["name"]), f.get("value").cloned().unwrap_or(Value::Null)))
                .collect();
            json!({"name": s.get("name").cloned().unwrap_or(json!("")), "implementation": s["implementation"],
                   "negate": truthy(&s["negate"]), "required": truthy(&s["required"]), "fields": fields})
        })
        .collect();
    json!({"name": cf["name"], "includeCustomFormatWhenRenaming": truthy(&cf["includeCustomFormatWhenRenaming"]),
           "specifications": specs})
}

/// Lower-case custom format name → (name, id); negative ids for ones not made yet.
#[derive(Default, Debug, Clone)]
pub struct CfIds(IndexMap<String, (String, Value)>);

impl CfIds {
    pub fn add(&mut self, name: &str, id: Value) {
        self.0.insert(name.to_lowercase(), (name.to_string(), id));
    }
    pub fn contains(&self, lower: &str) -> bool {
        self.0.contains_key(lower)
    }
    pub fn items_named(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.values().map(|(n, id)| (n.as_str(), id))
    }
}

impl ArrView {
    // ---- custom formats: config → API body (fields as the schema lists them) ----------------
    /// A select given by its label ("English", "Bluray") → its value.
    pub fn choice(who: &str, f: &Value, v: Value) -> R<Value> {
        let opts = rows(&f["selectOptions"]);
        if let Value::String(s) = &v
            && !opts.is_empty()
        {
            let lo = s.to_lowercase();
            let hit = opts.iter().find(|o| text(o.get("name").unwrap_or(&json!(""))).to_lowercase() == lo);
            return match hit {
                Some(o) => Ok(o.get("value").cloned().unwrap_or(Value::Null)),
                None => Err(cerr(format!("{who}: {:?} isn't a choice for {}", s, text(&f["name"])))),
            };
        }
        Ok(v)
    }

    pub async fn cf_body(&mut self, cf: &Cf) -> R<Value> {
        let schema = self.by_impl("customformat/schema").await?;
        let mut specs = vec![];
        for sp in &cf.specifications {
            let sch = schema.get(&sp.implementation).ok_or_else(|| {
                cerr(format!("{}: {} has no condition type {:?}", cf.name, self.c.name(), sp.implementation))
            })?;
            let known: Vec<String> = rows(&sch["fields"]).iter().map(|f| text(&f["name"])).collect();
            if let Some(k) = sp.fields.keys().find(|k| !known.contains(k)) {
                return Err(cerr(format!("{} / {}: {} has no field {:?}", cf.name, sp.name, sp.implementation, k)));
            }
            let mut fields = vec![];
            for f in rows(&sch["fields"]) {
                let name = text(&f["name"]);
                let v = sp.fields.get(&name).or(f.get("value")).cloned().unwrap_or(Value::Null);
                fields.push(
                    json!({"name": f["name"], "value": Self::choice(&format!("{} / {}", cf.name, sp.name), f, v)?}),
                );
            }
            specs.push(json!({"name": sp.name, "implementation": sp.implementation, "negate": sp.negate,
                              "required": sp.required, "fields": fields}));
        }
        Ok(json!({"name": cf.name, "includeCustomFormatWhenRenaming": cf.include_custom_format_when_renaming,
                  "specifications": specs}))
    }

    /// What would change, in words; empty when it's the same.
    pub fn cf_compare(body: &Value, cur: &Value) -> Vec<String> {
        let (a, b) = (cf_portable(body), cf_portable(cur));
        let mut out = vec![];
        if a["includeCustomFormatWhenRenaming"] != b["includeCustomFormatWhenRenaming"] {
            out.push("renaming option".to_string());
        }
        let by_name = |v: &Value| -> IndexMap<String, Value> {
            rows(&v["specifications"]).iter().map(|s| (text(&s["name"]), s.clone())).collect()
        };
        let (new, old) = (by_name(&a), by_name(&b));
        for (n, s) in &new {
            match old.get(n) {
                None => out.push(format!("adds condition {:?}", n)),
                Some(o) if !same_value(s, o) => out.push(format!("changes condition {:?}", n)),
                _ => {}
            }
        }
        out.extend(old.keys().filter(|n| !new.contains_key(*n)).map(|n| format!("removes condition {:?}", n)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_diff_words() {
        let cur = json!({"name": "x", "includeCustomFormatWhenRenaming": false, "specifications": [
            {"name": "a", "implementation": "T", "negate": false, "required": false, "fields": [{"name": "value", "value": "old"}]},
            {"name": "gone", "implementation": "T", "negate": false, "required": false, "fields": []}]});
        let body = json!({"name": "x", "includeCustomFormatWhenRenaming": true, "specifications": [
            {"name": "a", "implementation": "T", "negate": false, "required": false, "fields": [{"name": "value", "value": "new"}]},
            {"name": "b", "implementation": "T", "negate": false, "required": false, "fields": []}]});
        assert_eq!(
            ArrView::cf_compare(&body, &cur),
            ["renaming option", "changes condition \"a\"", "adds condition \"b\"", "removes condition \"gone\""]
        );
        assert!(ArrView::cf_compare(&cur, &cur).is_empty());
    }
}
