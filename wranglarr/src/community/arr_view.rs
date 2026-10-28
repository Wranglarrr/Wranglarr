// SPDX-License-Identifier: GPL-3.0-or-later
//! The arr's side: what one arr has now, read once per check or apply (each list
//! on first use), and the config ↔ API body conversions.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use super::parse::{AutoTag, Cf, Delay, MP_PARTS, MetaProfile, Profile, Quality, cerr};
use super::tag_field;
use super::values::{same_field, same_value, shown};
use crate::error::{Error, Result as R};
use crate::service::arr::ArrClient;
use crate::service::arr::CommunityDef;
use crate::value::{text, truthy};

/// What else lists a tag in `tag/detail`, after each arr's own items ([`CommunityDef::tag_use`]).
const TAG_USE: [(&str, &str); 8] = [
    ("releaseProfileIds", "release profile"),
    ("restrictionIds", "release profile"),
    ("delayProfileIds", "delay profile"),
    ("autoTagIds", "auto tagging rule"),
    ("indexerIds", "indexer"),
    ("downloadClientIds", "download client"),
    ("notificationIds", "notification"),
    ("importListIds", "import list"),
];

/// The rows of a list answer (none when it isn't a list).
pub fn rows(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

/// `x.get("name") or ""`, lower-cased.
pub fn lname(v: &Value) -> String {
    v["name"].as_str().unwrap_or("").to_lowercase()
}

/// An id as it goes into a path.
pub fn id_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A map keyed by JSON values (ids): insertion order, first position kept.
#[derive(Default, Debug, Clone)]
pub struct VDict(pub Vec<(Value, Value)>);

impl VDict {
    pub fn set(&mut self, k: Value, v: Value) {
        match self.0.iter_mut().find(|(x, _)| same_value(x, &k)) {
            Some(e) => e.1 = v,
            None => self.0.push((k, v)),
        }
    }
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.0.iter().find(|(x, _)| same_value(x, k)).map(|(_, v)| v)
    }
}

/// Every quality in a profile's item tree, in the arr's order (worst first).
pub fn flat(items: &[Value]) -> Vec<Value> {
    let mut out = vec![];
    for it in items {
        if truthy(&it["quality"]) {
            out.push(it["quality"].clone());
        }
        out.extend(flat(rows(&it["items"])));
    }
    out
}

/// A profile's items → the config's best-first list of allowed qualities and
/// groups, plus id → name for the cutoff.
pub fn portable_qualities(items: &[Value]) -> (Vec<Value>, VDict) {
    let mut names = VDict::default();
    let mut out = vec![];
    for it in items {
        if truthy(&it["quality"]) {
            names.set(it["quality"]["id"].clone(), it["quality"]["name"].clone());
            if truthy(&it["allowed"]) {
                out.push(it["quality"]["name"].clone());
            }
        } else {
            names.set(it["id"].clone(), it["name"].clone());
            for sub in rows(&it["items"]) {
                names.set(sub["quality"]["id"].clone(), sub["quality"]["name"].clone());
            }
            if truthy(&it["allowed"]) {
                let qs: Vec<Value> = rows(&it["items"]).iter().rev().map(|s| s["quality"]["name"].clone()).collect();
                out.push(json!({"name": it["name"], "qualities": qs}));
            }
        }
    }
    out.reverse();
    (out, names)
}

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

/// A list's items as text, sorted, to compare lists whatever their order.
fn sorted_strs(a: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = a.iter().map(text).collect();
    v.sort();
    v
}

/// Loosely the same: text ignoring case, lists ignoring order.
pub fn same(a: &Value, b: Option<&Value>) -> bool {
    let b = b.unwrap_or(&Value::Null);
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.to_lowercase() == y.to_lowercase(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && sorted_strs(x) == sorted_strs(y),
        _ => same_value(a, b),
    }
}

pub struct ArrView {
    pub c: ArrClient,
    pub kind: String,
    cache: HashMap<String, Value>,
    lib: Option<(Vec<Value>, &'static str)>,
}

impl ArrView {
    pub fn new(c: ArrClient) -> ArrView {
        ArrView { kind: c.kind().to_string(), c, cache: HashMap::new(), lib: None }
    }

    pub fn name(&self) -> &str {
        self.c.name()
    }

    /// What community configs know about this arr (None: it takes none).
    pub fn community(&self) -> Option<&'static CommunityDef> {
        self.c.arr_def().community.as_ref()
    }

    /// It has metadata profiles (Lidarr).
    pub fn has_metadata_profiles(&self) -> bool {
        self.community().is_some_and(|c| c.metadata_profiles.is_some())
    }

    pub async fn r(&mut self, path: &str) -> R<Value> {
        if let Some(v) = self.cache.get(path) {
            return Ok(v.clone());
        }
        let v = self.c.get(path).await?;
        self.cache.insert(path.to_string(), v.clone());
        Ok(v)
    }

    pub async fn list(&mut self, path: &str) -> R<Vec<Value>> {
        self.r(path)
            .await?
            .as_array()
            .cloned()
            .ok_or_else(|| Error::upstream(format!("{}: {path} returned a non-list response", self.name())))
    }

    pub fn drop_cache(&mut self, path: &str) {
        self.cache.remove(path);
    }

    pub async fn cfs(&mut self) -> R<Vec<Value>> {
        self.list("customformat").await
    }
    pub async fn profiles(&mut self) -> R<Vec<Value>> {
        self.list("qualityprofile").await
    }
    pub async fn profile_schema(&mut self) -> R<Value> {
        self.r("qualityprofile/schema").await
    }
    pub async fn defs(&mut self) -> R<Vec<Value>> {
        self.list("qualitydefinition").await
    }
    pub async fn release_profiles(&mut self) -> R<Vec<Value>> {
        self.list("releaseprofile").await
    }
    pub async fn tag_list(&mut self) -> R<Vec<Value>> {
        self.list("tag").await
    }
    async fn by_impl(&mut self, path: &str) -> R<HashMap<String, Value>> {
        Ok(self.list(path).await?.into_iter().map(|x| (text(&x["implementation"]), x)).collect())
    }

    /// A tag it just made joins the cached list.
    pub fn push_tag(&mut self, made: Value) {
        if let Some(Value::Array(a)) = self.cache.get_mut("tag") {
            a.push(made);
        } else {
            self.cache.insert("tag".into(), json!([made]));
        }
    }

    /// Tag id → label.
    pub async fn tag_names(&mut self) -> R<VDict> {
        let mut d = VDict::default();
        for t in self.tag_list().await? {
            d.set(t["id"].clone(), t["label"].clone());
        }
        Ok(d)
    }

    /// Lower-case quality name → the quality, as the profile schema lists them.
    pub async fn qualities(&mut self) -> R<HashMap<String, Value>> {
        let schema = self.profile_schema().await?;
        Ok(flat(rows(&schema["items"])).into_iter().map(|q| (lname(&q), q)).collect())
    }

    pub fn by_name(rs: &[Value]) -> HashMap<String, Value> {
        rs.iter().map(|r| (lname(r), r.clone())).collect()
    }

    // ---- what uses what, for the warnings ------------------------------------------------
    async fn library(&mut self) -> &(Vec<Value>, &'static str) {
        if self.lib.is_none() {
            let mut lib = (vec![], "");
            for (path, noun) in self.community().map_or(&[][..], |c| c.library) {
                if let Ok(v) = self.c.get(path).await {
                    lib = (rows(&v).to_vec(), *noun);
                    break;
                }
            }
            self.lib = Some(lib);
        }
        self.lib.get_or_insert_with(|| (vec![], ""))
    }

    /// "used by 3 movies, which follow the change", or ""
    pub async fn used_by(&mut self, field: &str, rid: &Value) -> String {
        let (rs, noun) = self.library().await;
        let n = rs.iter().filter(|x| x.get(field).is_some_and(|v| same_value(v, rid))).count();
        if n == 0 {
            return String::new();
        }
        let one = if *noun == "series" { noun.to_string() } else { noun[..noun.len().saturating_sub(1)].to_string() };
        format!(
            "used by {n} {}, which follow{} the change",
            if n == 1 { one.as_str() } else { noun },
            if n == 1 { "s" } else { "" }
        )
    }

    pub async fn tag_use(&mut self, tid: &Value) -> String {
        let Ok(detail) = self.r("tag/detail").await else { return String::new() };
        let empty = json!({});
        let d = rows(&detail).iter().find(|t| same_value(&t["id"], tid)).unwrap_or(&empty);
        let mut out = vec![];
        let own = crate::service::arr::all().filter_map(|a| a.community.as_ref()?.tag_use);
        for (k, what) in own.chain(TAG_USE) {
            let n = rows(&d[k]).len();
            if n > 0 {
                out.push(format!("{n} {what}{}", if n == 1 || what == "series" { "" } else { "s" }));
            }
        }
        out.join(", ")
    }

    pub async fn scored_by(&mut self, fid: &Value) -> R<Vec<String>> {
        Ok(self
            .profiles()
            .await?
            .iter()
            .filter(|p| rows(&p["formatItems"]).iter().any(|fi| same_value(&fi["format"], fid) && truthy(&fi["score"])))
            .map(|p| text(&p["name"]))
            .collect())
    }

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

    // ---- delay profiles: matched by their tags; the untagged one is the app's default ------
    fn label_list(names: &VDict, ids: &Value) -> Vec<String> {
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
        let keys = super::parse::DELAY_BOOL
            .iter()
            .chain(super::parse::DELAY_INT.iter())
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

/// An error that a check shows as a row (a config error) rather than failing the app.
pub fn is_config(e: &Error) -> bool {
    matches!(e, Error::Invalid(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualities_round_trip() {
        let items = json!([
            {"quality": {"id": 1, "name": "SDTV"}, "items": [], "allowed": false},
            {"id": 1001, "name": "HD", "allowed": true, "items": [
                {"quality": {"id": 4, "name": "HDTV-720p"}, "items": [], "allowed": true},
                {"quality": {"id": 9, "name": "HDTV-1080p"}, "items": [], "allowed": true}]},
            {"quality": {"id": 19, "name": "Bluray-2160p"}, "items": [], "allowed": true}]);
        let (q, names) = portable_qualities(rows(&items));
        assert_eq!(Value::Array(q), json!(["Bluray-2160p", {"name": "HD", "qualities": ["HDTV-1080p", "HDTV-720p"]}]));
        assert_eq!(names.get(&json!(1001)), Some(&json!("HD")));
        assert_eq!(flat(rows(&items)).len(), 4);
    }

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

    #[test]
    fn same_loosely() {
        assert!(same(&json!("English"), Some(&json!("english"))));
        assert!(same(&json!(["b", "a"]), Some(&json!(["a", "b"]))));
        assert!(same(&json!(2), Some(&json!(2.0))));
        assert!(!same(&json!("x"), None));
    }
}
