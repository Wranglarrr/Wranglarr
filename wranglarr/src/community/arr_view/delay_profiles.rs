// SPDX-License-Identifier: GPL-3.0-or-later
//! Delay profiles: matched by their tags; config → API body, the portable form, and
//! what would change, in words.

use serde_json::{Map, Value, json};

use super::super::parse::Delay;
use super::super::values::{same_field, same_value, shown};
use super::{ArrView, VDict, rows};
use crate::error::Result as R;
use crate::value::{text, truthy};

impl ArrView {
    // ---- delay profiles: matched by their tags; the untagged one is the app's default ------
    pub(super) fn label_list(names: &VDict, ids: &Value) -> Vec<String> {
        let mut v: Vec<String> =
            rows(ids).iter().map(|i| names.get(i).and_then(Value::as_str).unwrap_or("").to_string()).collect();
        v.sort();
        v
    }

    pub async fn delays(&mut self) -> R<Vec<(Vec<String>, Value)>> {
        let names = self.tag_names().await?;
        let mut out: Vec<(Vec<String>, Value)> = vec![];
        for d in self.list("delayprofile").await? {
            let key = Self::label_list(&names, &d["tags"]);
            match out.iter_mut().find(|(k, _)| *k == key) {
                Some(e) => e.1 = d,
                None => out.push((key, d)),
            }
        }
        Ok(out)
    }

    pub async fn dp_portable(&mut self, d: &Value) -> R<Value> {
        let names = self.tag_names().await?;
        Ok(Self::dp_portable_with(&names, d))
    }

    pub fn dp_portable_with(names: &VDict, d: &Value) -> Value {
        let mut out = Map::new();
        out.insert("tags".into(), json!(Self::label_list(names, &d["tags"])));
        let keys = super::super::parse::DELAY_BOOL
            .iter()
            .chain(super::super::parse::DELAY_INT.iter())
            .chain(["preferredProtocol", "name"].iter());
        for k in keys {
            if let Some(v) = d.get(*k) {
                out.insert((*k).into(), v.clone());
            }
        }
        if let Some(items) = d.get("items") {
            let items: Vec<Value> = rows(items)
                .iter()
                .map(|i| {
                    json!({"name": i.get("name").cloned().unwrap_or(json!("")), "protocol": i["protocol"],
                           "allowed": truthy(&i["allowed"]), "delay": i.get("delay").cloned().unwrap_or(json!(0))})
                })
                .collect();
            out.insert("items".into(), Value::Array(items));
        }
        Value::Object(out)
    }

    pub async fn dp_compare(&mut self, d: &Delay, cur: &Value) -> R<Vec<String>> {
        let have = self.dp_portable(cur).await?;
        let mut out = vec![];
        let norm = |xs: &Value| -> Vec<Value> {
            rows(xs).iter().map(|i| json!([i["protocol"], i["allowed"], i["delay"]])).collect()
        };
        for (k, v) in &d.settings {
            if k == "items" {
                if !same_value(&Value::Array(norm(v)), &Value::Array(norm(have.get("items").unwrap_or(&json!([]))))) {
                    out.push("protocols".to_string());
                }
            } else if !same_field(have.get(k), Some(v)) {
                out.push(format!("{k} {} → {}", shown(have.get(k)), text(v)));
            }
        }
        Ok(out)
    }

    pub async fn dp_body(&mut self, d: &Delay, cur: Option<&Value>, tag_ids: Vec<Value>) -> R<Value> {
        let mut base = match cur {
            Some(c) => c.as_object().cloned().unwrap_or_default(),
            None => {
                let dflt = self.delays().await?.into_iter().find(|(k, _)| k.is_empty()).map(|(_, v)| v);
                let mut o = dflt.and_then(|v| v.as_object().cloned()).unwrap_or_default();
                o.shift_remove("id");
                o.shift_remove("order");
                o
            }
        };
        for (k, v) in &d.settings {
            base.insert(k.clone(), v.clone());
        }
        base.insert("tags".into(), Value::Array(tag_ids));
        Ok(Value::Object(base))
    }
}
