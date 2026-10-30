// SPDX-License-Identifier: GPL-3.0-or-later
//! Metadata profiles (Lidarr): config → API body, and the portable form.

use serde_json::{Map, Value, json};

use super::super::parse::{MP_PARTS, MetaProfile, cerr};
use super::{ArrView, lname, rows};
use crate::error::Result as R;
use crate::value::truthy;

impl ArrView {
    // ---- metadata profiles (Lidarr) ---------------------------------------------------------
    /// base: what a copy starts from; a new one starts from Lidarr's own Standard profile.
    pub async fn mp_body(&mut self, m: &MetaProfile, cur: Option<&Value>, base: Option<&Value>) -> R<Value> {
        let mut body = match cur {
            Some(c) => c.clone(),
            None => {
                let mps = self.list("metadataprofile").await?;
                let tmpl = match base.cloned().or_else(|| mps.iter().find(|x| lname(x) == "standard").cloned()) {
                    Some(t) => t,
                    None => match mps.first() {
                        Some(t) => t.clone(),
                        None => self.r("metadataprofile/schema").await?,
                    },
                };
                let mut o = tmpl.as_object().cloned().unwrap_or_default();
                o.shift_remove("id");
                Value::Object(o)
            }
        };
        body["name"] = json!(m.name);
        for (k, inner, want) in m.parts() {
            let Some(want) = want else { continue };
            let want: Vec<String> = want.iter().map(|x| x.to_lowercase()).collect();
            let have: Vec<String> = rows(&body[k]).iter().map(|i| lname(&i[inner])).collect();
            let mut bad: Vec<&String> = want.iter().filter(|w| !have.contains(w)).collect();
            bad.sort();
            bad.dedup();
            if let Some(b) = bad.first() {
                return Err(cerr(format!("{}: {} has no {} {:?}", m.name, self.c.name(), &k[..k.len() - 1], b)));
            }
            if let Some(Value::Array(a)) = body.get_mut(k) {
                for i in a.iter_mut() {
                    let on = want.contains(&lname(&i[inner]));
                    i["allowed"] = json!(on);
                }
            }
        }
        Ok(body)
    }

    pub fn mp_portable(mp: &Value) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), mp["name"].clone());
        for (k, inner) in MP_PARTS {
            if let Some(v) = mp.get(k) {
                let names: Vec<Value> =
                    rows(v).iter().filter(|i| truthy(&i["allowed"])).map(|i| i[inner]["name"].clone()).collect();
                out.insert(k.into(), Value::Array(names));
            }
        }
        Value::Object(out)
    }
}
