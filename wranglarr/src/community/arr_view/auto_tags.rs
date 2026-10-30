// SPDX-License-Identifier: GPL-3.0-or-later
//! Auto tagging: config → API body (names → ids), the portable form, and what would
//! change, in words.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use super::super::parse::{AutoTag, cerr};
use super::super::tag_field;
use super::super::values::same_field;
use super::{ArrView, VDict, lname, rows, same};
use crate::error::Result as R;
use crate::value::{text, truthy};

impl ArrView {
    // ---- auto tagging: conditions name profiles and tags; the API wants their ids ----------
    async fn named(&mut self, path: &str) -> HashMap<String, Value> {
        match self.list(path).await {
            Ok(rs) => rs.iter().map(|r| (lname(r), r["id"].clone())).collect(),
            Err(_) => HashMap::new(),
        }
    }

    /// tag_ids None: only check (profiles and tags that will be made count as there).
    pub async fn at_body(
        &mut self,
        a: &AutoTag,
        tag_ids: Option<&HashMap<String, Value>>,
        extra_profiles: &[String],
        extra_mps: &[String],
    ) -> R<Value> {
        let schema = self.by_impl("autotagging/schema").await?;
        let profs = self.named("qualityprofile").await;
        let mps = if self.has_metadata_profiles() { self.named("metadataprofile").await } else { HashMap::new() };
        let mut specs = vec![];
        for sp in &a.specifications {
            let who = format!("{} / {}", a.name, sp.name);
            let sch = schema.get(&sp.implementation).ok_or_else(|| {
                cerr(format!("{}: {} has no auto tagging condition {:?}", a.name, self.c.name(), sp.implementation))
            })?;
            let known: Vec<String> = rows(&sch["fields"]).iter().map(|f| text(&f["name"])).collect();
            if let Some(k) = sp.fields.keys().find(|k| !known.contains(k)) {
                return Err(cerr(format!("{who}: {} has no field {:?}", sp.implementation, k)));
            }
            let mut fields = vec![];
            for f in rows(&sch["fields"]) {
                let fname = text(&f["name"]);
                let mut v = sp.fields.get(&fname).or(f.get("value")).cloned().unwrap_or(Value::Null);
                let t = f["type"].as_str().unwrap_or("");
                if sp.fields.contains_key(&fname) {
                    if t == "rootFolder" {
                        return Err(cerr(format!(
                            "{who}: a root folder is a path on one server; take that condition out"
                        )));
                    }
                    if (t == "qualityProfile" || t == "metadataProfile") && v.is_string() {
                        let s = text(&v);
                        let lo = s.to_lowercase();
                        let (have, more) =
                            if t == "qualityProfile" { (&profs, extra_profiles) } else { (&mps, extra_mps) };
                        if let Some(id) = have.get(&lo) {
                            v = id.clone();
                        } else if more.iter().any(|x| x.to_lowercase() == lo) {
                            v = json!(0); // made by this import; filled in when it's applied
                        } else {
                            let what = if t == "qualityProfile" { "quality" } else { "metadata" };
                            return Err(cerr(format!("{who}: {} has no {what} profile {:?}", self.c.name(), s)));
                        }
                    } else if tag_field(t) && v.is_array() {
                        v = Value::Array(
                            rows(&v)
                                .iter()
                                .filter_map(Value::as_str)
                                .map(|x| match tag_ids {
                                    Some(ids) => ids.get(&x.to_lowercase()).cloned().unwrap_or(json!(0)),
                                    None => json!(0),
                                })
                                .collect(),
                        );
                    } else {
                        v = Self::choice(&who, f, v)?;
                    }
                }
                fields.push(json!({"name": f["name"], "value": v}));
            }
            specs.push(json!({"name": sp.name, "implementation": sp.implementation, "negate": sp.negate,
                              "required": sp.required, "fields": fields}));
        }
        Ok(json!({"name": a.name, "removeTagsAutomatically": a.remove_tags_automatically, "specifications": specs}))
    }

    pub async fn at_portable(&mut self, at: &Value) -> R<Value> {
        let schema = self.by_impl("autotagging/schema").await?;
        let tags = self.tag_names().await?;
        let mut profs = VDict::default();
        for r in self.profiles().await? {
            profs.set(r["id"].clone(), r["name"].clone());
        }
        let mut mps = VDict::default();
        if self.has_metadata_profiles()
            && let Ok(rs) = self.list("metadataprofile").await
        {
            for r in rs {
                mps.set(r["id"].clone(), r["name"].clone());
            }
        }
        let mut specs = vec![];
        for sp in rows(&at["specifications"]) {
            let empty = json!({});
            let sch_fields = schema.get(&text(&sp["implementation"])).map_or(&Value::Null, |s| &s["fields"]);
            let mut fields = Map::new();
            for f in rows(&sp["fields"]) {
                let fname = text(&f["name"]);
                let fs = rows(sch_fields).iter().find(|x| text(&x["name"]) == fname).unwrap_or(&empty);
                let mut v = f.get("value").cloned().unwrap_or(Value::Null);
                let t = fs["type"].as_str().unwrap_or("");
                let opts = rows(&fs["selectOptions"]);
                if t == "qualityProfile" {
                    v = profs.get(&v).cloned().unwrap_or(v);
                } else if t == "metadataProfile" {
                    v = mps.get(&v).cloned().unwrap_or(v);
                } else if tag_field(t) && v.is_array() {
                    v = Value::Array(
                        rows(&v).iter().map(|i| tags.get(i).cloned().unwrap_or_else(|| i.clone())).collect(),
                    );
                } else if !opts.is_empty() && !v.is_array() {
                    v = opts
                        .iter()
                        .find(|o| same_field(o.get("value"), Some(&v)))
                        .map_or(v.clone(), |o| o["name"].clone());
                }
                fields.insert(fname, v);
            }
            specs.push(
                json!({"name": sp.get("name").cloned().unwrap_or(json!("")), "implementation": sp["implementation"],
                              "negate": truthy(&sp["negate"]), "required": truthy(&sp["required"]), "fields": fields}),
            );
        }
        Ok(json!({"name": at["name"], "removeTagsAutomatically": truthy(&at["removeTagsAutomatically"]),
                  "tags": Self::label_list(&tags, &at["tags"]), "specifications": specs}))
    }

    pub async fn at_compare(&mut self, a: &AutoTag, cur: &Value) -> R<Vec<String>> {
        let have = self.at_portable(cur).await?;
        let mut out = vec![];
        if json!(a.remove_tags_automatically) != have["removeTagsAutomatically"] {
            out.push("remove tags automatically".to_string());
        }
        let mut mine = a.tags.clone();
        mine.sort();
        let theirs: Vec<String> = rows(&have["tags"]).iter().map(text).collect();
        if mine != theirs {
            let t = if theirs.is_empty() { "none".to_string() } else { theirs.join(", ") };
            out.push(format!("tags {t} → {}", mine.join(", ")));
        }
        let old: IndexMap<String, &Value> =
            rows(&have["specifications"]).iter().map(|s| (text(&s["name"]), s)).collect();
        for sp in &a.specifications {
            match old.get(&sp.name) {
                None => out.push(format!("adds condition {:?}", sp.name)),
                Some(o) => {
                    let head = json!([sp.implementation, sp.negate, sp.required])
                        != json!([o["implementation"], o["negate"], o["required"]]);
                    if head || sp.fields.iter().any(|(k, v)| !same(v, o["fields"].get(k))) {
                        out.push(format!("changes condition {:?}", sp.name));
                    }
                }
            }
        }
        let names: Vec<&String> = a.specifications.iter().map(|s| &s.name).collect();
        out.extend(old.keys().filter(|n| !names.contains(n)).map(|n| format!("removes condition {:?}", n)));
        Ok(out)
    }
}
