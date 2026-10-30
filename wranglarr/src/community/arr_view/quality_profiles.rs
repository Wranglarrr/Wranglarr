// SPDX-License-Identifier: GPL-3.0-or-later
//! Quality profiles: config → API body, and what would change, in words.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{Value, json};

use super::super::parse::{Profile, Quality, cerr};
use super::super::values::{same_field, same_value, shown};
use super::{ArrView, CfIds, VDict, flat, lname, portable_qualities, rows};
use crate::error::Result as R;
use crate::value::{text, truthy};

impl ArrView {
    // ---- quality profiles: config → API body ------------------------------------------------
    /// base: what a copy starts from (the importer's own profile of the original name).
    pub async fn profile_body(
        &mut self,
        p: &Profile,
        cur: Option<&Value>,
        ids: &CfIds,
        base: Option<&Value>,
    ) -> R<Value> {
        let schema = self.profile_schema().await?;
        let mut body = match cur {
            Some(c) => c.clone(),
            None => {
                let tmpl = base.unwrap_or(&schema);
                let mut o = tmpl.as_object().cloned().unwrap_or_default();
                o.shift_remove("id");
                Value::Object(o)
            }
        };
        body["name"] = json!(p.name);
        if let Some(u) = p.upgrade_allowed {
            body["upgradeAllowed"] = json!(u);
        }
        if let Some(quals) = &p.qualities {
            let (items, qids) = self.items(p, quals).await?;
            body["items"] = Value::Array(items);
            let cutoff = p.cutoff.clone().unwrap_or_else(|| quals[0].name().to_string());
            let Some(id) = qids.get(&cutoff.to_lowercase()) else {
                return Err(cerr(format!("{}: cutoff {:?} isn't one of the profile's qualities", p.name, cutoff)));
            };
            body["cutoff"] = id.clone();
        } else if let Some(cutoff) = &p.cutoff {
            let (_, names) = portable_qualities(rows(&body["items"]));
            let lo = cutoff.to_lowercase();
            let hit = names.0.iter().find(|(_, n)| n.as_str().is_some_and(|n| !n.is_empty() && n.to_lowercase() == lo));
            let Some((id, _)) = hit else {
                return Err(cerr(format!("{}: cutoff {:?} isn't one of the profile's qualities", p.name, cutoff)));
            };
            body["cutoff"] = id.clone();
        }
        for (k, v) in p.scores() {
            if let Some(v) = v
                && schema.get(k).is_some()
            {
                body[k] = json!(v);
            }
        }
        if let Some(lang) = &p.language {
            if schema.get("language").is_none() {
                return Err(cerr(format!("{}: {} profiles have no language (use a custom format)", p.name, self.kind)));
            }
            let langs = self.list("language").await?;
            let lo = lang.to_lowercase();
            let Some(l) = langs.iter().find(|l| lname(l) == lo) else {
                return Err(cerr(format!("{}: {} doesn't know the language {:?}", p.name, self.c.name(), lang)));
            };
            body["language"] = json!({"id": l["id"], "name": l["name"]});
        }
        let scores: HashMap<String, i64> = p.format_scores.iter().map(|(k, v)| (k.to_lowercase(), *v)).collect();
        if let Some(k) = p.format_scores.keys().find(|k| !ids.contains(&k.to_lowercase())) {
            return Err(cerr(format!(
                "{}: scores custom format {:?}, which neither the config nor {} has",
                p.name,
                k,
                self.c.name()
            )));
        }
        let mut old = VDict::default();
        for fi in rows(&cur.or(base).map_or(Value::Null, |v| v["formatItems"].clone())) {
            old.set(fi["format"].clone(), fi["score"].clone());
        }
        let items: Vec<Value> = ids
            .items_named()
            .map(|(name, fid)| {
                let score = match scores.get(&name.to_lowercase()) {
                    Some(s) => json!(s),
                    None => old.get(fid).cloned().unwrap_or(json!(0)),
                };
                json!({"format": fid, "name": name, "score": score})
            })
            .collect();
        body["formatItems"] = Value::Array(items);
        Ok(body)
    }

    async fn items(&mut self, p: &Profile, quals_cfg: &[Quality]) -> R<(Vec<Value>, HashMap<String, Value>)> {
        let quals = self.qualities().await?;
        let mut used: Vec<String> = vec![];
        let mut ids = HashMap::new();
        let mut picked = vec![];
        let mut gid = 1000;
        for q in quals_cfg {
            let names: Vec<&String> = match q {
                Quality::Name(n) => vec![n],
                Quality::Group { qualities, .. } => qualities.iter().collect(),
            };
            let mut subs = vec![];
            for n in names {
                let lo = n.to_lowercase();
                let Some(found) = quals.get(&lo) else {
                    return Err(cerr(format!("{}: {} has no quality {:?}", p.name, self.c.name(), n)));
                };
                if used.contains(&lo) {
                    return Err(cerr(format!("{}: {:?} is listed twice", p.name, n)));
                }
                used.push(lo);
                subs.push(found.clone());
            }
            match q {
                Quality::Name(n) => {
                    ids.insert(n.to_lowercase(), subs[0]["id"].clone());
                    picked.push(json!({"quality": subs[0], "items": [], "allowed": true}));
                }
                Quality::Group { name, .. } => {
                    gid += 1;
                    ids.insert(name.to_lowercase(), json!(gid));
                    let inner: Vec<Value> =
                        subs.iter().rev().map(|s| json!({"quality": s, "items": [], "allowed": true})).collect();
                    picked.push(json!({"id": gid, "name": name, "allowed": true, "items": inner}));
                }
            }
        }
        let schema = self.profile_schema().await?;
        let mut out: Vec<Value> = flat(rows(&schema["items"]))
            .into_iter()
            .filter(|q| !used.contains(&lname(q)))
            .map(|q| json!({"quality": q, "items": [], "allowed": false}))
            .collect();
        picked.reverse();
        out.extend(picked);
        Ok((out, ids))
    }

    pub fn profile_compare(body: &Value, cur: &Value) -> Vec<String> {
        let mut out = vec![];
        let (qa, na) = portable_qualities(rows(&body["items"]));
        let (qb, nb) = portable_qualities(rows(&cur["items"]));
        if !same_value(&Value::Array(qa), &Value::Array(qb)) {
            out.push("qualities".to_string());
        }
        let (ca, cb) = (na.get(&body["cutoff"]), nb.get(&cur["cutoff"]));
        if !same_field(ca, cb) {
            out.push(format!("cutoff {} → {}", shown(cb), shown(ca)));
        }
        for k in ["upgradeAllowed", "minFormatScore", "cutoffFormatScore", "minUpgradeFormatScore"] {
            if !same_field(body.get(k), cur.get(k)) {
                out.push(format!("{k} {} → {}", shown(cur.get(k)), shown(body.get(k))));
            }
        }
        let lid = |v: &Value| v.get("language").filter(|l| truthy(l)).and_then(|l| l.get("id")).cloned();
        if !same_field(lid(body).as_ref(), lid(cur).as_ref()) {
            let cname = cur.get("language").filter(|l| truthy(l)).and_then(|l| l.get("name"));
            out.push(format!("language {} → {}", shown(cname), shown(body["language"].get("name"))));
        }
        let mut old: IndexMap<String, Value> = IndexMap::new();
        for fi in rows(&cur["formatItems"]) {
            old.insert(text(&fi["name"]), fi["score"].clone());
        }
        let changed: Vec<String> = rows(&body["formatItems"])
            .iter()
            .filter(|fi| fi["format"].as_f64().is_some_and(|f| f > 0.0))
            .filter_map(|fi| {
                let o = old.get(&text(&fi["name"])).cloned().unwrap_or(json!(0));
                (!same_value(&fi["score"], &o))
                    .then(|| format!("{} {} → {}", text(&fi["name"]), text(&o), text(&fi["score"])))
            })
            .collect();
        if !changed.is_empty() {
            let more = if changed.len() > 6 { format!(" and {} more", changed.len() - 6) } else { String::new() };
            out.push(format!("scores: {}{more}", changed[..changed.len().min(6)].join(", ")));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_diff_words() {
        let cur = json!({"name": "P", "upgradeAllowed": false, "cutoff": 4, "minFormatScore": 0,
            "items": [{"quality": {"id": 4, "name": "HDTV-720p"}, "items": [], "allowed": true}],
            "formatItems": [{"format": 1, "name": "A", "score": 0}]});
        let mut body = cur.clone();
        body["upgradeAllowed"] = json!(true);
        body["minFormatScore"] = json!(10);
        body["formatItems"] =
            json!([{"format": 1, "name": "A", "score": 5}, {"format": -1, "name": "New", "score": 5}]);
        assert_eq!(
            ArrView::profile_compare(&body, &cur),
            ["upgradeAllowed false → true", "minFormatScore 0 → 10", "scores: A 0 → 5"]
        );
    }
}
