// SPDX-License-Identifier: GPL-3.0-or-later
//! Each app's settings as keyframes over a timeline, kept under the state db key
//! `community:timeline` (app name → [`Timeline`]).
//!
//! Every value a config can set is a track, named in the config's own words:
//! `["qualityProfiles", "HD-1080p", "cutoff"]`, `["qualityDefinitions", "Bluray-1080p",
//! "min"]`, `["customFormats", "x265"]` (a whole format). A timeline is a list of
//! marks, oldest first: what the app had before wranglarr first touched a track
//! (the `before` mark, filled in as tracks are touched), each install, and each
//! change wranglarr found made outside it. A track's value is the last keyframe
//! on it.
//!
//! Removing an install replays the timeline without it. Two ways, which only
//! differ when something later touched the same tracks or depends on what it made:
//! - **keep**: later changes stand, and what they depend on stays;
//! - **throughout**: the install's tracks go back to how they were before it, over
//!   later changes too (including changes made outside wranglarr), and what
//!   depended on what it made goes with it.

mod depends;
mod describe;
mod read;
mod service;

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::Part;
use super::arr_view::{ArrView, rows};
use super::export::{ALL, export_one};
use super::parse::{Source, dp_name};
use super::values::same_value;
use crate::app::App;
use crate::error::{Error, Result as R};
use crate::value::text;

pub use service::protected;

pub const KEY_TIMELINE: &str = "community:timeline";
pub const EXTERNAL: &str = "External manual change";
pub const BEFORE: &str = "Before wranglarr";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// What the app had before wranglarr first changed each track.
    Before,
    /// A config install.
    Config,
    /// A change wranglarr found made outside it.
    External,
}

/// One keyframe: a track and its value from this mark on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub path: Vec<String>,
    #[serde(default)]
    pub v: Value,
    /// The track has no value: the item isn't there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gone: bool,
}

impl Key {
    pub fn new(path: Vec<String>, v: Option<Value>) -> Key {
        match v {
            Some(v) => Key { path, v, gone: false },
            None => Key { path, v: Value::Null, gone: true },
        }
    }
    pub fn val(&self) -> Option<Value> {
        (!self.gone).then(|| self.v.clone())
    }
    fn tk(&self) -> String {
        tk(&self.path)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub id: String,
    pub at: String,
    pub kind: Kind,
    /// The config's name; [`EXTERNAL`]; [`BEFORE`].
    pub label: String,
    /// The install's entry in the history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
    /// The install stopped partway.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
    /// When a removal left some of it, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<String>,
    /// An install: where the config comes from, as it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    pub keys: Vec<Key>,
}

impl Mark {
    fn has(&self, k: &str) -> bool {
        self.keys.iter().any(|x| x.tk() == k)
    }
    fn get(&self, k: &str) -> Option<&Key> {
        self.keys.iter().find(|x| x.tk() == k)
    }
    /// Set a keyframe, replacing one on the same track.
    fn set(&mut self, key: Key) {
        let k = key.tk();
        match self.keys.iter_mut().find(|x| x.tk() == k) {
            Some(x) => *x = key,
            None => self.keys.push(key),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub kind: String,
    /// Oldest first; the first is the `before` mark.
    pub marks: Vec<Mark>,
}

/// A track's key: its path, lower-cased (the arrs match names whatever the case).
pub fn tk(path: &[String]) -> String {
    path.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>().join("\u{1f}")
}

fn same(a: &Option<Value>, b: &Option<Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => same_value(a, b),
        _ => false,
    }
}

/// What kind of track a path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// An item: there or not (a tag, a quality profile), or whole (a custom format).
    Item,
    /// A field of an item with fields (quality, metadata and delay profiles).
    Field,
    /// A quality profile's score for a custom format.
    Score,
    /// An app-wide setting.
    Setting,
}

fn part_of(path: &[String]) -> Option<Part> {
    path.first().and_then(|k| Part::from_key(k))
}

/// The parts whose items have fields of their own.
fn fielded(p: Part) -> bool {
    matches!(p, Part::QualityProfiles | Part::MetadataProfiles | Part::DelayProfiles)
}

fn shape(path: &[String]) -> Option<Shape> {
    let p = part_of(path)?;
    Some(match (p, path.len()) {
        (Part::QualityDefinitions | Part::Naming | Part::MediaManagement, _) => Shape::Setting,
        (_, 2) => Shape::Item,
        (Part::QualityProfiles, 4) if path[2] == "formatScores" => Shape::Score,
        (p, 3) if fielded(p) => Shape::Field,
        _ => return None,
    })
}

fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// A config section (as [`export_one`] makes it) as tracks.
pub fn flatten(sec: &Map<String, Value>) -> Vec<(Vec<String>, Value)> {
    let mut out = vec![];
    let get = |k: &str| sec.get(k).unwrap_or(&Value::Null);
    for t in rows(get("tags")) {
        out.push((path(&["tags", &text(t)]), t.clone()));
    }
    for (part, fields) in [
        (Part::CustomFormats, false),
        (Part::QualityProfiles, true),
        (Part::MetadataProfiles, true),
        (Part::ReleaseProfiles, false),
        (Part::AutoTags, false),
    ] {
        for item in rows(get(part.key())) {
            let name = text(&item["name"]);
            let mut whole = item.clone();
            if let Some(o) = whole.as_object_mut() {
                o.remove("extra");
            }
            if !fields {
                out.push((path(&[part.key(), &name]), whole));
                continue;
            }
            out.push((path(&[part.key(), &name]), json!(name)));
            for (k, v) in item.as_object().into_iter().flatten() {
                match k.as_str() {
                    "name" | "extra" => {}
                    "formatScores" => {
                        for (cf, sc) in v.as_object().into_iter().flatten() {
                            out.push((path(&[part.key(), &name, "formatScores", cf]), sc.clone()));
                        }
                    }
                    _ => out.push((path(&[part.key(), &name, k]), v.clone())),
                }
            }
        }
    }
    for d in rows(get("delayProfiles")) {
        let tags: Vec<String> = rows(&d["tags"]).iter().map(text).collect();
        let label = dp_name(&tags);
        out.push((path(&["delayProfiles", &label]), json!(tags)));
        for (k, v) in d.as_object().into_iter().flatten() {
            if k != "tags" {
                out.push((path(&["delayProfiles", &label, k]), v.clone()));
            }
        }
    }
    if let Some(Value::Object(d)) = sec.get("qualityDefinitions") {
        for (q, o) in d {
            for (k, v) in o.as_object().into_iter().flatten() {
                out.push((path(&["qualityDefinitions", q, k]), v.clone()));
            }
        }
    }
    for part in [Part::Naming, Part::MediaManagement] {
        for (k, v) in get(part.key()).as_object().into_iter().flatten() {
            out.push((path(&[part.key(), k]), v.clone()));
        }
    }
    out
}

/// Everything a config can set in one app, as tracks.
#[derive(Clone, Debug, Default)]
pub struct Snap {
    vals: HashMap<String, (Vec<String>, Value)>,
    /// Parts the app didn't give (an arr error): their tracks are unknown.
    unread: Vec<Part>,
}

impl Snap {
    pub fn from_section(sec: &Map<String, Value>) -> Snap {
        let mut s = Snap::default();
        for (p, v) in flatten(sec) {
            s.vals.insert(tk(&p), (p, v));
        }
        s
    }

    /// Read the app, part by part; a part it doesn't give is left unread.
    pub async fn read(arr: &mut ArrView) -> R<Snap> {
        let mut sec = Map::new();
        let mut unread = vec![];
        for p in Part::ALL {
            if p == Part::MetadataProfiles && !arr.has_metadata_profiles() {
                continue;
            }
            let want: Map<String, Value> =
                [(p.key().to_string(), json!([ALL])), ("raw".to_string(), json!(true))].into_iter().collect();
            match export_one(arr, &want, &mut vec![]).await {
                Ok(part) => sec.extend(part),
                Err(Error::Upstream(e)) => {
                    tracing::debug!("community timeline: {} of {}: {e}", p.key(), arr.name());
                    unread.push(p);
                }
                Err(e) => return Err(e),
            }
        }
        let mut s = Snap::from_section(&sec);
        s.unread = unread;
        Ok(s)
    }

    pub fn known(&self, path: &[String]) -> bool {
        part_of(path).is_some_and(|p| !self.unread.contains(&p))
            && (shape(path) != Some(Shape::Score) || !self.unread.contains(&Part::CustomFormats))
    }

    /// The track's value: None when it has none (the item isn't there). A score
    /// the profile doesn't list is 0; one for a custom format the app hasn't got is none.
    pub fn get(&self, path: &[String]) -> Option<Value> {
        let own = self.vals.get(&tk(path)).map(|(_, v)| v.clone());
        match shape(path) {
            Some(Shape::Field) => {
                self.vals.get(&tk(&path[..2]))?;
                own
            }
            Some(Shape::Score) => {
                self.vals.get(&tk(&path[..2]))?;
                let cf = ["customFormats".to_string(), path[3].clone()];
                if !self.unread.contains(&Part::CustomFormats) && !self.vals.contains_key(&tk(&cf)) {
                    return None;
                }
                own.or(Some(json!(0)))
            }
            _ => own,
        }
    }

    /// The path as the app spells it, when it has the track.
    fn spelled(&self, path: &[String]) -> Vec<String> {
        self.vals.get(&tk(path)).map(|(p, _)| p.clone()).unwrap_or_else(|| path.to_vec())
    }

    fn tracks(&self) -> impl Iterator<Item = &Vec<String>> {
        self.vals.values().map(|(p, _)| p)
    }
}

// ---- storage -------------------------------------------------------------------------

type Store = BTreeMap<String, Timeline>;

fn stored(v: Option<Value>) -> Store {
    v.and_then(|v| {
        serde_json::from_value(v).map_err(|e| tracing::warn!("community timeline: unreadable, starting over: {e}")).ok()
    })
    .unwrap_or_default()
}

pub async fn load(app: &App) -> R<Store> {
    Ok(stored(app.state().get_value(KEY_TIMELINE).await?))
}

/// Change one app's timeline in one write transaction.
async fn change<T: Send + 'static>(
    app: &App,
    name: &str,
    kind: &str,
    f: impl FnOnce(&mut Timeline) -> R<T> + Send + 'static,
) -> R<T> {
    let (name, kind) = (name.to_owned(), kind.to_owned());
    app.state()
        .write(move |tx| {
            let mut all = stored(tx.get_value(KEY_TIMELINE)?);
            let tl = all.entry(name).or_insert_with(|| Timeline::new(&kind));
            let out = f(tl)?;
            tx.put(KEY_TIMELINE, &all)?;
            Ok(out)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn sec(v: Value) -> Snap {
        Snap::from_section(v.as_object().expect("object"))
    }

    pub(super) fn p(parts: &[&str]) -> Vec<String> {
        path(parts)
    }

    pub(super) fn base() -> Value {
        json!({"qualityDefinitions": {"Bluray-1080p": {"min": 2, "preferred": null, "max": 100}},
               "qualityProfiles": [{"name": "HD-1080p", "cutoff": "Bluray-1080p", "formatScores": {}}],
               "customFormats": [], "tags": []})
    }

    #[test]
    fn stored_shape() {
        let k = Key::new(p(&["tags", "x"]), None);
        assert_eq!(serde_json::to_value(&k).expect("json"), json!({"path": ["tags", "x"], "v": null, "gone": true}));
        let k = Key::new(p(&["qualityDefinitions", "SDTV", "max"]), Some(Value::Null));
        assert_eq!(
            serde_json::to_value(&k).expect("json"),
            json!({"path": ["qualityDefinitions", "SDTV", "max"], "v": null})
        );
        assert_eq!(serde_json::from_value::<Key>(json!({"path": ["a"]})).expect("key").val(), Some(Value::Null));
    }
}
