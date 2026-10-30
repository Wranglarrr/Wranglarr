// SPDX-License-Identifier: GPL-3.0-or-later
//! The arr's side: what one arr has now, read once per check or apply (each list
//! on first use), and the config ↔ API body conversions.

mod auto_tags;
mod custom_formats;
mod delay_profiles;
mod metadata_profiles;
mod quality_profiles;

use std::collections::HashMap;

use serde_json::{Value, json};

use super::values::same_value;
use crate::error::{Error, Result as R};
use crate::service::arr::ArrClient;
use crate::service::arr::CommunityDef;
use crate::value::{text, truthy};

pub use custom_formats::{CfIds, cf_portable};

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
    pub(super) async fn by_impl(&mut self, path: &str) -> R<HashMap<String, Value>> {
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
    fn same_loosely() {
        assert!(same(&json!("English"), Some(&json!("english"))));
        assert!(same(&json!(["b", "a"]), Some(&json!(["a", "b"]))));
        assert!(same(&json!(2), Some(&json!(2.0))));
        assert!(!same(&json!("x"), None));
    }
}
