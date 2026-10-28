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

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::arr_view::{ArrView, lname, rows};
use super::export::{ALL, export_one};
use super::parse::{Config, Source, cerr, dp_name};
use super::values::{number, same_value, short};
use super::{Community, FORMAT, Part, builtin};
use crate::app::App;
use crate::config::AppConfig;
use crate::error::{Error, Result as R};
use crate::time::now_iso_z as now;
use crate::value::text;

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

// ---- reading the timeline ------------------------------------------------------------

impl Timeline {
    fn new(kind: &str) -> Timeline {
        Timeline {
            kind: kind.into(),
            marks: vec![Mark {
                id: crate::ids::token_hex(4),
                at: now(),
                kind: Kind::Before,
                label: BEFORE.into(),
                history: None,
                partial: false,
                kept: vec![],
                source: None,
                keys: vec![],
            }],
        }
    }

    fn pos(&self, id: &str) -> Option<usize> {
        self.marks.iter().position(|m| m.id == id)
    }

    /// The last keyframe on a track among the marks before `upto`.
    fn last_before(&self, upto: usize, k: &str) -> Option<&Key> {
        self.marks[..upto].iter().rev().find_map(|m| m.get(k))
    }

    fn tracked(&self, k: &str) -> bool {
        self.marks.iter().any(|m| m.has(k))
    }

    /// Track → its last keyframe.
    fn expected(&self) -> HashMap<String, &Key> {
        let mut e = HashMap::new();
        for m in &self.marks {
            for k in &m.keys {
                e.insert(k.tk(), k);
            }
        }
        e
    }

    /// The item was made by wranglarr: it wasn't there before.
    fn made_here(&self, item: &str) -> bool {
        self.marks.first().and_then(|b| b.get(item)).is_some_and(|k| k.gone)
    }

    /// What in the app differs from the timeline (changes made outside wranglarr),
    /// as keyframes. Tracks of a part the app didn't give are left alone.
    pub fn drift(&self, snap: &Snap) -> Vec<Key> {
        let e = self.expected();
        let mut out = vec![];
        for key in e.values() {
            if !snap.known(&key.path) {
                continue;
            }
            let cur = snap.get(&key.path);
            if !same(&cur, &key.val()) {
                out.push(Key::new(snap.spelled(&key.path), cur));
            }
        }
        // a score given outside wranglarr in a profile, or for a format, it made: there was none
        for p in snap.tracks() {
            let made = |i: &[String]| self.made_here(&tk(i));
            if shape(p) == Some(Shape::Score)
                && snap.known(p)
                && !e.contains_key(&tk(p))
                && (made(&p[..2]) || made(&["customFormats".to_string(), p[3].clone()]))
            {
                out.push(Key::new(p.clone(), snap.get(p)));
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Note changes made outside wranglarr: one mark a day, the latest kept.
    pub fn record_drift(&mut self, keys: Vec<Key>) -> bool {
        if keys.is_empty() {
            return false;
        }
        let at = now();
        let day = |t: &str| t.get(..10).unwrap_or("").to_string();
        match self.marks.last_mut() {
            Some(m) if m.kind == Kind::External && day(&m.at) == day(&at) => {
                for k in keys {
                    m.set(k);
                }
                m.at = at;
            }
            _ => self.marks.push(Mark {
                id: crate::ids::token_hex(4),
                at,
                kind: Kind::External,
                label: EXTERNAL.into(),
                history: None,
                partial: false,
                kept: vec![],
                source: None,
                keys,
            }),
        }
        true
    }

    /// Record an install: every track that differs between the snapshots, with
    /// what it was before in the `before` mark when nothing had it yet.
    pub fn record_install(
        &mut self,
        was: &Snap,
        now_: &Snap,
        name: &str,
        source: Option<Source>,
        hid: Option<String>,
        partial: bool,
    ) -> Option<String> {
        let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in was.tracks().chain(now_.tracks()) {
            paths.entry(tk(p)).or_insert_with(|| now_.spelled(p));
        }
        let mut keys = vec![];
        for (k, p) in paths {
            if !was.known(&p) || !now_.known(&p) {
                continue;
            }
            let (old, new) = (was.get(&p), now_.get(&p));
            if same(&old, &new) {
                continue;
            }
            if !self.tracked(&k) {
                self.marks[0].keys.push(Key::new(p.clone(), old));
            }
            keys.push(Key::new(p, new));
        }
        if keys.is_empty() {
            return None;
        }
        let id = crate::ids::token_hex(4);
        self.marks.push(Mark {
            id: id.clone(),
            at: now(),
            kind: Kind::Config,
            label: name.into(),
            history: hid,
            partial,
            kept: vec![],
            source,
            keys,
        });
        Some(id)
    }
}

// ---- what depends on what ---------------------------------------------------------------

/// The tracks in the app that need `parent` there: its fields, scores for a
/// custom format, what uses a tag or a quality profile.
fn dependents(snap: &Snap, parent: &[String]) -> Vec<Vec<String>> {
    let (Some(part), true) = (part_of(parent), parent.len() == 2) else { return vec![] };
    let name = parent[1].to_lowercase();
    let mut out = vec![];
    for (p, v) in snap.vals.values() {
        let is = |i: usize, s: &str| p.get(i).is_some_and(|x| x.to_lowercase() == s);
        let child = fielded(part) && p.len() > 2 && p[0] == parent[0] && is(1, &name);
        let scored =
            part == Part::CustomFormats && shape(p) == Some(Shape::Score) && is(3, &name) && !same_value(v, &json!(0));
        let uses = match (part, part_of(p), p.len()) {
            (Part::Tags, Some(Part::ReleaseProfiles | Part::AutoTags), 2) => {
                rows(&v["tags"]).iter().any(|t| lname_s(t) == name)
                    || rows(&v["specifications"]).iter().any(|s| {
                        s["implementation"] == "TagSpecification"
                            && (rows(&s["fields"]["value"]).iter().any(|t| lname_s(t) == name)
                                || lname_s(&s["fields"]["value"]) == name)
                    })
            }
            (Part::Tags, Some(Part::DelayProfiles), 2) => rows(v).iter().any(|t| lname_s(t) == name),
            (Part::QualityProfiles, Some(Part::AutoTags), 2) => rows(&v["specifications"]).iter().any(|s| {
                s["implementation"] == "QualityProfileSpecification" && lname_s(&s["fields"]["value"]) == name
            }),
            _ => false,
        };
        if child || scored || uses {
            out.push(p.clone());
        }
    }
    out.sort();
    out
}

fn lname_s(v: &Value) -> String {
    text(v).to_lowercase()
}

/// It goes along when its parent goes (a field, a score), rather than holding the parent.
fn goes_along(d: &[String], parent: &[String]) -> bool {
    matches!(shape(d), Some(Shape::Field) | Some(Shape::Score))
        && (tk(&d[..2]) == tk(parent)
            || (part_of(parent) == Some(Part::CustomFormats) && shape(d) == Some(Shape::Score)))
}

/// A removal worked out: each track's value after it.
#[derive(Debug, Default, Clone)]
pub struct Removal {
    /// track → (path, value after); tracks left out stay as they are.
    pub targets: BTreeMap<String, (Vec<String>, Option<Value>)>,
    /// Tracks of the removed install that stay, and why.
    pub kept: BTreeMap<String, String>,
    /// Later marks' keyframes that go (throughout): (mark index, track).
    pub drops: Vec<(usize, String)>,
}

impl Removal {
    /// What changes in the app: (path, now, after).
    pub fn effects(&self, snap: &Snap) -> Vec<(Vec<String>, Option<Value>, Option<Value>)> {
        self.targets
            .values()
            .filter_map(|(p, to)| {
                let cur = snap.get(p);
                (!same(&cur, to)).then(|| (p.clone(), cur, to.clone()))
            })
            .collect()
    }
}

/// Who describes track `d` (key `k`), which holds an item of mark `xi` up. When
/// `xi` set it last, it stays only because the item it belongs to stays: that
/// item is named, not the config being removed.
fn holder_label(tl: &Timeline, k: &str, d: &[String], xi: usize) -> String {
    match tl.marks.iter().enumerate().rev().find(|(_, m)| m.has(k)) {
        Some((i, _)) if i == xi => {
            let what = part_of(d).map(|x| x.label()).unwrap_or("setting");
            format!("{what} {}", d.get(1).cloned().unwrap_or_default())
        }
        Some((_, m)) if m.kind == Kind::External => format!("{EXTERNAL} {}", local_day(&m.at)),
        Some((_, m)) if m.kind == Kind::Config => m.label.clone(),
        _ => "your own settings".into(),
    }
}

/// The day of an RFC 3339 time in wranglarr's time zone (the stack's `TZ`), as
/// the page shows the marks' dates.
fn local_day(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| at.get(..10).unwrap_or("").to_string())
}

fn track_name(p: &[String]) -> String {
    let what = part_of(p).map(|x| x.label()).unwrap_or("setting");
    match shape(p) {
        Some(Shape::Score) => format!("{}'s score for {}", p[1], p[3]),
        Some(Shape::Field) => format!("{} {}", p[1], p[2]),
        Some(Shape::Setting) => p[1..].join(" "),
        _ => format!("{what} {}", p.get(1).cloned().unwrap_or_default()),
    }
}

impl Timeline {
    /// Work out removing mark `xi`.
    pub fn removal(&self, xi: usize, snap: &Snap, throughout: bool) -> Removal {
        let x = &self.marks[xi];
        let mut r = Removal::default();
        for key in &x.keys {
            let k = key.tk();
            let later = self.marks[xi + 1..].iter().any(|m| m.has(&k));
            if later && !throughout {
                continue; // a later change has it: it stays
            }
            let before = self.last_before(xi, &k).and_then(Key::val);
            let p = snap.spelled(&key.path);
            r.targets.insert(k, (p, before));
        }
        // what goes along with, or holds up, each thing that would go
        loop {
            let mut changed = false;
            let going: Vec<(String, Vec<String>)> = r
                .targets
                .iter()
                .filter(|(k, (p, to))| to.is_none() && snap.get(p).is_some() && !r.kept.contains_key(*k))
                .map(|(k, (p, _))| (k.clone(), p.clone()))
                .collect();
            for (k, p) in going {
                let mut holders = vec![];
                for d in dependents(snap, &p) {
                    let dk = tk(&d);
                    let stays = !matches!(r.targets.get(&dk), Some((_, None)));
                    if !stays {
                        continue;
                    }
                    if throughout && goes_along(&d, &p) {
                        r.targets.insert(dk, (d, None));
                        changed = true;
                    } else {
                        holders.push(d);
                    }
                }
                if holders.is_empty() {
                    continue;
                }
                let mut who: Vec<String> = holders.iter().map(|d| holder_label(self, &tk(d), d, xi)).collect();
                who.sort();
                who.dedup();
                let why = format!(
                    "{} stays: {} use{} it",
                    track_name(&p),
                    who.join(", "),
                    if who.len() == 1 && !who[0].starts_with("your") { "s" } else { "" }
                );
                // it stays, and its own fields with it
                let cur = snap.get(&p);
                r.targets.insert(k.clone(), (p.clone(), cur));
                r.kept.insert(k.clone(), why);
                let kids: Vec<String> = r
                    .targets
                    .iter()
                    .filter(|(_, (d, to))| to.is_none() && d.len() > 2 && tk(&d[..2]) == k)
                    .map(|(dk, _)| dk.clone())
                    .collect();
                for dk in kids {
                    let d = r.targets[&dk].0.clone();
                    let cur = snap.get(&d);
                    r.targets.insert(dk.clone(), (d, cur));
                    r.kept.insert(dk, String::new());
                }
                changed = true;
            }
            if !changed {
                break;
            }
        }
        if throughout {
            for (j, m) in self.marks.iter().enumerate().skip(xi + 1) {
                for key in &m.keys {
                    let k = key.tk();
                    if r.targets.contains_key(&k) && !r.kept.contains_key(&k) {
                        r.drops.push((j, k));
                    }
                }
            }
        }
        r
    }

    /// Both ways of removing mark `xi`, and whether they differ in the app.
    pub fn removals(&self, xi: usize, snap: &Snap) -> (Removal, Removal, bool) {
        let keep = self.removal(xi, snap, false);
        let through = self.removal(xi, snap, true);
        let after = |r: &Removal, p: &[String]| r.targets.get(&tk(p)).map_or_else(|| snap.get(p), |(_, v)| v.clone());
        let differ = keep
            .targets
            .values()
            .chain(through.targets.values())
            .any(|(p, _)| !same(&after(&keep, p), &after(&through, p)));
        (keep, through, differ)
    }

    /// Take mark `xi` out after a removal: what was kept stays in it.
    pub fn take_out(&mut self, xi: usize, r: &Removal, failed: &[Key]) {
        for (j, k) in r.drops.iter().rev() {
            self.marks[*j].keys.retain(|x| x.tk() != *k);
        }
        let x = &mut self.marks[xi];
        x.keys.retain(|key| r.kept.contains_key(&key.tk()));
        for f in failed {
            x.set(f.clone());
        }
        let mut why: Vec<String> = r.kept.values().filter(|w| !w.is_empty()).cloned().collect();
        if !failed.is_empty() {
            why.push(format!("{} couldn't be put back", failed.len()));
        }
        x.kept = why;
        let empty = x.keys.is_empty();
        if empty {
            self.marks.remove(xi);
        }
        // a change from outside with nothing left in it goes
        self.marks.retain(|m| m.kind != Kind::External || !m.keys.is_empty());
    }
}

// ---- describing marks and effects -----------------------------------------------------------

fn show(v: &Option<Value>) -> String {
    match v {
        None => "none".into(),
        Some(Value::Null) => "no limit".into(),
        Some(v @ Value::Number(_)) => number(v),
        Some(Value::String(s)) => s.clone(),
        Some(v) => short(v),
    }
}

/// One line for a track's change: "cutoff Bluray-1080p", "min 2 → 5". `from` is
/// what it was before (None: not known, as for the `before` mark).
fn line(p: &[String], from: Option<&Option<Value>>, to: &Option<Value>, before: bool) -> String {
    let field = match shape(p) {
        Some(Shape::Item) => {
            return match (before, from, to) {
                (true, _, None) => "wasn't there".into(),
                (true, _, Some(_)) => "as it was".into(),
                (_, _, None) => "removed".into(),
                (_, Some(None), Some(_)) => "made".into(),
                (_, _, Some(_)) => "changed".into(),
            };
        }
        Some(Shape::Score) => format!("score for {}", p[3]),
        Some(Shape::Field) => p[2].clone(),
        Some(Shape::Setting) => p[p.len() - 1].clone(),
        None => p.join(" "),
    };
    let whole = matches!(to, Some(Value::Array(_) | Value::Object(_)));
    match from {
        Some(Some(f)) if !whole => format!("{field} {} → {}", show(&Some(f.clone())), show(to)),
        _ if whole => format!("{field} changed"),
        _ => format!("{field} {}", show(to)),
    }
}

/// Changes grouped by item: [{part, label, name, changes: [line]}].
fn grouped(
    items: impl IntoIterator<Item = (Vec<String>, Option<Option<Value>>, Option<Value>)>,
    before: bool,
    gone_fields: bool,
) -> Vec<Value> {
    let mut out: Vec<(String, Value)> = vec![];
    for (p, from, to) in items {
        let Some(part) = part_of(&p) else { continue };
        // a field of something that goes: its "removed" says it
        if !gone_fields && to.is_none() && matches!(shape(&p), Some(Shape::Field) | Some(Shape::Score)) {
            continue;
        }
        let name = match part {
            Part::Naming | Part::MediaManagement => part.whole().to_string(),
            _ => p.get(1).cloned().unwrap_or_default(),
        };
        let group = format!("{}\u{1f}{}", part.key(), name.to_lowercase());
        let l = line(&p, from.as_ref(), &to, before);
        match out.iter_mut().find(|(g, _)| *g == group) {
            Some((_, v)) => {
                if let Some(a) = v["changes"].as_array_mut() {
                    a.push(json!(l));
                }
            }
            None => out.push((group, json!({"part": part.key(), "label": part.label(), "name": name, "changes": [l]}))),
        }
    }
    out.into_iter().map(|(_, v)| v).collect()
}

impl Timeline {
    /// The marks as the page lists them, newest first.
    pub fn listed(&self) -> Vec<Value> {
        let mut out = vec![];
        for (i, m) in self.marks.iter().enumerate() {
            let before = m.kind == Kind::Before;
            let items = grouped(
                m.keys.iter().map(|k| {
                    let from = if before { None } else { Some(self.last_before(i, &k.tk()).and_then(Key::val)) };
                    (k.path.clone(), from, k.val())
                }),
                before,
                false,
            );
            out.push(json!({"id": m.id, "at": m.at, "kind": m.kind, "label": m.label, "history": m.history,
                            "partial": m.partial, "kept": m.kept, "source": m.source, "count": m.keys.len(), "items": items}));
        }
        out.reverse();
        out
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

// ---- the service side ------------------------------------------------------------------

/// Which of the plan's rows change something people should OK first: what the arr
/// ships with, app-wide parts, the default delay profile; and what other installed
/// configs set.
/// `config` is the one being checked: its own earlier install isn't another config.
pub fn protected(rows_: &[Value], kind: &str, config: &str, tl: Option<&Timeline>) -> Vec<Value> {
    let mut out = vec![];
    for r in rows_ {
        if r["action"] != "update" {
            continue;
        }
        let Some(part) = r["part"].as_str().and_then(Part::from_key) else { continue };
        let name = text(&r["name"]);
        let why = if Part::APP_WIDE.contains(&part) {
            Some("app-wide: it applies to everything in the app".to_string())
        } else if builtin(part, kind).contains(&name.to_lowercase().as_str()) {
            Some(format!("one the app ships with: changing it changes the app's own {}", part.label()))
        } else if part == Part::DelayProfiles && name == "default" {
            Some("the default delay profile: it applies to everything without a tagged one".to_string())
        } else {
            None
        };
        let item = match part {
            Part::Naming | Part::MediaManagement => vec![part.key().to_string()],
            _ => vec![part.key().to_string(), name.clone()],
        };
        let pre = tk(&item);
        let mut by: Vec<String> = vec![];
        for m in tl.map_or(&[][..], |t| &t.marks[..]) {
            let touches = m.keys.iter().any(|k| {
                let t = k.tk();
                t == pre || t.starts_with(&format!("{pre}\u{1f}"))
            });
            if m.kind == Kind::Config && touches && m.label != config && !by.contains(&m.label) {
                by.push(m.label.clone());
            }
        }
        if why.is_none() && by.is_empty() {
            continue;
        }
        // one line for an app-wide part, naming what in it changes
        if Part::APP_WIDE.contains(&part) {
            if let Some(e) = out.iter_mut().find(|e: &&mut Value| e["part"] == part.key()) {
                if let Some(a) = e["items"].as_array_mut().filter(|_| part == Part::QualityDefinitions) {
                    a.push(json!(name));
                }
                for l in by {
                    if !rows(&e["overrides"]).iter().any(|x| *x == json!(l))
                        && let Some(a) = e["overrides"].as_array_mut()
                    {
                        a.push(json!(l));
                    }
                }
                continue;
            }
            let items = if part == Part::QualityDefinitions { json!([name]) } else { json!([]) };
            out.push(json!({"part": part.key(), "label": part.label(), "name": part.whole(), "items": items,
                            "why": why, "overrides": by}));
            continue;
        }
        out.push(json!({"part": part.key(), "label": part.label(), "name": name, "items": [], "why": why,
                        "overrides": by}));
    }
    out
}

fn effect_rows(r: &Removal, snap: &Snap) -> Vec<Value> {
    grouped(r.effects(snap).into_iter().map(|(p, from, to)| (p, Some(from), to)), false, false)
}

/// The marks after `xi` that lose keyframes, with what they lose.
fn overridden(tl: &Timeline, r: &Removal, snap: &Snap) -> Vec<Value> {
    let mut by: Vec<(usize, Vec<Vec<String>>)> = vec![];
    for (j, k) in &r.drops {
        let p = tl.marks[*j].get(k).map(|x| x.path.clone()).unwrap_or_default();
        match by.iter_mut().find(|(i, _)| i == j) {
            Some((_, v)) => v.push(p),
            None => by.push((*j, vec![p])),
        }
    }
    by.into_iter()
        .map(|(j, ps)| {
            let m = &tl.marks[j];
            let items = grouped(
                ps.into_iter().map(|p| {
                    let to = r.targets.get(&tk(&p)).and_then(|(_, v)| v.clone());
                    let cur = snap.get(&p);
                    (p, Some(cur), to)
                }),
                false,
                true,
            );
            json!({"id": m.id, "kind": m.kind, "label": m.label, "at": m.at, "items": items})
        })
        .collect()
}

/// The section that puts the targets in the app (what a config would say), and
/// what has to be deleted: (part, path).
fn writes(r: &Removal, snap: &Snap, notes: &mut Vec<String>) -> (Map<String, Value>, Vec<Vec<String>>) {
    let mut sec = Map::new();
    let mut deletes = vec![];
    let mut fielded_items: BTreeMap<String, (Part, Map<String, Value>)> = BTreeMap::new();
    for (p, to) in r.targets.values() {
        if same(&snap.get(p), to) {
            continue;
        }
        let Some(part) = part_of(p) else { continue };
        match (shape(p), to) {
            (Some(Shape::Item), None) => deletes.push(p.clone()),
            (Some(Shape::Item), Some(v)) => match part {
                Part::Tags => push(&mut sec, "tags", v.clone()),
                p2 if fielded(p2) => {
                    let e = item_entry(&mut fielded_items, part, p, snap, r);
                    if part == Part::DelayProfiles {
                        e.insert("tags".into(), v.clone());
                    }
                }
                Part::AutoTags
                    if rows(&v["specifications"]).iter().any(|s| s["implementation"] == "RootFolderSpecification") =>
                {
                    notes.push(format!("{}: put it back by hand (it has a root folder condition)", p[1]));
                }
                _ => push(&mut sec, part.key(), v.clone()),
            },
            (Some(Shape::Field), Some(v)) => {
                item_entry(&mut fielded_items, part, p, snap, r).insert(p[2].clone(), v.clone());
            }
            (Some(Shape::Score), Some(v)) => {
                let e = item_entry(&mut fielded_items, part, p, snap, r);
                let fs = e.entry("formatScores").or_insert_with(|| json!({}));
                fs[p[3].as_str()] = v.clone();
            }
            (Some(Shape::Setting), Some(v)) => {
                let s = sec.entry(part.key()).or_insert_with(|| json!({}));
                if part == Part::QualityDefinitions {
                    if s.get(&p[1]).is_none() {
                        s[p[1].as_str()] = json!({});
                    }
                    s[p[1].as_str()][p[2].as_str()] = v.clone();
                } else {
                    s[p[1].as_str()] = v.clone();
                }
            }
            // a field with no value: nothing to write (its item goes, or never had it)
            _ => {}
        }
    }
    for (_, (part, e)) in fielded_items {
        push(&mut sec, part.key(), Value::Object(e));
    }
    (sec, deletes)
}

fn push(sec: &mut Map<String, Value>, k: &str, v: Value) {
    if let Value::Array(a) = sec.entry(k).or_insert_with(|| json!([])) {
        a.push(v);
    }
}

/// The entry for a fielded item in the section being built: its name (or a delay
/// profile's tags) to match by.
fn item_entry<'a>(
    items: &'a mut BTreeMap<String, (Part, Map<String, Value>)>,
    part: Part,
    p: &[String],
    snap: &Snap,
    r: &Removal,
) -> &'a mut Map<String, Value> {
    let item = &p[..2];
    let (_, e) = items.entry(tk(item)).or_insert_with(|| {
        let mut e = Map::new();
        if part == Part::DelayProfiles {
            let tags =
                r.targets.get(&tk(item)).and_then(|(_, v)| v.clone()).or_else(|| snap.get(item)).unwrap_or(json!([]));
            e.insert("tags".into(), tags);
        } else {
            e.insert("name".into(), json!(item[1]));
        }
        (part, e)
    });
    e
}

const DELETE_ORDER: [Part; 7] = [
    Part::AutoTags,
    Part::ReleaseProfiles,
    Part::DelayProfiles,
    Part::QualityProfiles,
    Part::MetadataProfiles,
    Part::CustomFormats,
    Part::Tags,
];

fn resource(p: Part) -> &'static str {
    match p {
        Part::AutoTags => "autotagging",
        Part::ReleaseProfiles => "releaseprofile",
        Part::DelayProfiles => "delayprofile",
        Part::QualityProfiles => "qualityprofile",
        Part::MetadataProfiles => "metadataprofile",
        Part::CustomFormats => "customformat",
        _ => "tag",
    }
}

/// Delete one item; Ok(false) when it was kept (a tag still in use).
async fn delete(
    arr: &mut ArrView,
    p: &[String],
    notes: &mut Vec<String>,
    done: &mut Vec<super::Change>,
    journal: &super::Journal<'_>,
) -> R<bool> {
    let Some(part) = part_of(p) else { return Ok(false) };
    let name = p[1].to_lowercase();
    let res = resource(part);
    let current = match part {
        Part::DelayProfiles => {
            arr.drop_cache("delayprofile");
            arr.delays().await?.into_iter().find(|(t, _)| dp_name(t).to_lowercase() == name).map(|(_, d)| d)
        }
        Part::Tags => {
            arr.drop_cache("tag");
            arr.tag_list().await?.into_iter().find(|t| lname_s(&t["label"]) == name)
        }
        Part::ReleaseProfiles => {
            arr.drop_cache(res);
            arr.list(res).await?.into_iter().find(|x| lname(x) == name)
        }
        _ => {
            arr.drop_cache(res);
            arr.list(res).await?.into_iter().find(|x| lname(x) == name)
        }
    };
    let Some(current) = current else { return Ok(true) }; // already gone
    let id = current["id"].clone();
    if part == Part::Tags {
        arr.drop_cache("tag/detail");
        let used = arr.tag_use(&id).await;
        if !used.is_empty() {
            notes.push(format!("tag {}: kept, it's on {used}", p[1]));
            return Ok(false);
        }
    }
    let mut item = super::Change::replaced(res, &current, track_name(p));
    item.deleted = true;
    super::record(done, Some(journal), item).await?;
    match arr.c.delete(&format!("{res}/{}", super::arr_view::id_str(&id))).await {
        Ok(_) => Ok(true),
        Err(e) if e.is_not_found() => Ok(true),
        Err(e) => {
            notes.push(format!("{}: kept, the app said: {e}", track_name(p)));
            Ok(false)
        }
    }
}

impl Community {
    async fn view(a: &AppConfig) -> R<ArrView> {
        Ok(ArrView::new(Self::client(a).await?))
    }

    /// Read the app and note what changed outside wranglarr since the timeline last saw it.
    pub(super) async fn catch_up(&self, app: &App, a: &AppConfig) -> R<Snap> {
        let snap = Snap::read(&mut Self::view(a).await?).await?;
        let all = load(app).await?;
        if let Some(tl) = all.get(&a.name) {
            let d = tl.drift(&snap);
            if !d.is_empty() {
                change(app, &a.name, &a.kind, move |tl| Ok(tl.record_drift(d))).await?;
            }
        }
        Ok(snap)
    }

    /// After an install: what it changed goes on the timeline.
    pub(super) async fn record_install(
        &self,
        app: &App,
        a: &AppConfig,
        was: Snap,
        cfg: &Config,
        hid: Option<String>,
        partial: bool,
    ) -> R<Option<String>> {
        let now_ = Snap::read(&mut Self::view(a).await?).await?;
        let (name, source) = (cfg.name.clone(), cfg.source.clone());
        change(app, &a.name, &a.kind, move |tl| Ok(tl.record_install(&was, &now_, &name, source, hid, partial))).await
    }

    /// An undo put back what an install replaced: its mark goes.
    pub(super) async fn forget_install(&self, app: &App, a: &str, hid: &str) -> R<()> {
        let (a, hid) = (a.to_owned(), hid.to_owned());
        let all = load(app).await?;
        let Some(tl) = all.get(&a) else { return Ok(()) };
        let kind = tl.kind.clone();
        change(app, &a, &kind, move |tl| {
            tl.marks.retain(|m| m.history.as_deref() != Some(hid.as_str()));
            Ok(())
        })
        .await
    }

    /// Every app's timeline, newest first, after a look for changes made outside wranglarr.
    pub async fn installed(&self, app: &App) -> R<Value> {
        let _turn = self.lock.lock().await;
        let mut out = vec![];
        let names: Vec<String> = load(app).await?.into_keys().collect();
        for a in Self::apps(app).into_iter().filter(|a| names.contains(&a.name)) {
            let error = match self.catch_up(app, &a).await {
                Ok(_) => Value::Null,
                Err(Error::Upstream(e)) => json!(e.message()),
                Err(e) => return Err(e),
            };
            let all = load(app).await?;
            let Some(tl) = all.get(&a.name) else { continue };
            if tl.marks.iter().all(|m| m.kind == Kind::Before) {
                continue;
            }
            out.push(json!({"app": a.name, "kind": a.kind, "error": error, "marks": tl.listed()}));
        }
        Ok(json!(out))
    }

    fn mark_of(tl: &Timeline, id: &str) -> R<usize> {
        match tl.pos(id) {
            Some(i) if tl.marks[i].kind == Kind::Config => Ok(i),
            Some(_) => Err(cerr("only an installed config can be removed")),
            None => Err(Error::not_found("not found")),
        }
    }

    /// What removing an install would do: `mode` is `none` (nothing in the app
    /// changes), `confirm` (one way), or `choose` (keep later changes, or remove throughout).
    pub async fn removal(&self, app: &App, name: &str, id: &str) -> R<Value> {
        let _turn = self.lock.lock().await;
        let a = Self::app(app, name)?;
        let snap = self.catch_up(app, &a).await?;
        let all = load(app).await?;
        let tl = all.get(name).ok_or_else(|| Error::not_found("not found"))?;
        let xi = Self::mark_of(tl, id)?;
        if !snap.unread.is_empty() {
            return Err(Error::upstream("cannot remove an install while app resources are unreadable"));
        }
        let (keep, through, differ) = tl.removals(xi, &snap);
        let way = |r: &Removal| {
            json!({"effects": effect_rows(r, &snap), "kept": r.kept.values().filter(|w| !w.is_empty()).collect::<Vec<_>>(),
                   "overrides": overridden(tl, r, &snap)})
        };
        let mode = if differ {
            "choose"
        } else if keep.effects(&snap).is_empty() {
            "none"
        } else {
            "confirm"
        };
        Ok(json!({"app": name, "id": id, "label": tl.marks[xi].label, "mode": mode,
                  "keep": way(&keep), "throughout": way(&through)}))
    }

    /// Remove an install: `throughout` or not (see [`Timeline::removal`]).
    pub async fn remove(&self, app: &App, name: &str, id: &str, throughout: bool) -> R<Value> {
        let _turn = self.lock.lock().await;
        let a = Self::app(app, name)?;
        let snap = self.catch_up(app, &a).await?;
        let all = load(app).await?;
        let tl = all.get(name).ok_or_else(|| Error::not_found("not found"))?;
        let xi = Self::mark_of(tl, id)?;
        if !snap.unread.is_empty() {
            return Err(Error::upstream("cannot remove an install while app resources are unreadable"));
        }
        let (keep, through, differ) = tl.removals(xi, &snap);
        let r = if throughout && differ { through } else { keep };
        let label = tl.marks[xi].label.clone();
        let mut notes = vec![];
        let (sec, deletes) = writes(&r, &snap, &mut notes);
        let mut arr = Self::view(&a).await?;
        // Plan before making the recovery entry. Keep it until verification and
        // timeline updates finish, so interruption never loses the preimages.
        let work = if !sec.is_empty() {
            let cfg = json!({"wranglarr": FORMAT, "name": format!("Removing {label}"), "apps": {a.kind.clone(): sec}});
            let parsed = super::parse::parse(Some(&cfg.to_string()))?;
            let section = &parsed.apps[&a.kind];
            let (rows_, work) = super::plan::plan_one(section, &mut arr, &a.name, None, super::DEFAULT_PREFIX).await?;
            if let Some(bad) = rows_.iter().find(|r| r["action"] == "error") {
                return Err(cerr(format!("can't put {} back: {}", text(&bad["name"]), text(&bad["details"][0]))));
            }
            Some(work)
        } else {
            None
        };
        let hid = self.remember(app, &format!("Removing {label}"), &a.name, vec![], true).await?;
        let journal = super::Journal { community: self, app, hid: &hid };
        let mut done = vec![];
        if let Some(work) = work {
            super::apply_one(&mut arr, work, &mut done, Some(&journal)).await?;
        }
        for part in DELETE_ORDER {
            for p in deletes.iter().filter(|p| part_of(p) == Some(part)) {
                delete(&mut arr, p, &mut notes, &mut done, &journal).await?;
            }
        }
        // what didn't end up as planned stays in the mark
        let after = Snap::read(&mut Self::view(&a).await?).await?;
        if !after.unread.is_empty() {
            return Err(Error::upstream("cannot verify removal while app resources are unreadable; retry removal"));
        }
        let failed: Vec<Key> = r
            .targets
            .values()
            .filter(|(p, to)| after.known(p) && !same(&after.get(p), to))
            .map(|(p, _)| Key::new(after.spelled(p), after.get(p)))
            .collect();
        let id_ = id.to_owned();
        change(app, &a.name, &a.kind, move |tl| {
            if let Some(xi) = tl.pos(&id_) {
                tl.take_out(xi, &r, &failed);
            }
            Ok(())
        })
        .await?;
        self.discard_journal(app, &hid).await?;
        Ok(json!({"notes": notes, "installed": self.installed_unlocked(app).await?}))
    }

    /// [`Community::installed`] for a caller that holds the lock (no new look at the apps).
    async fn installed_unlocked(&self, app: &App) -> R<Value> {
        let all = load(app).await?;
        let mut out = vec![];
        for a in Self::apps(app) {
            let Some(tl) = all.get(&a.name) else { continue };
            if tl.marks.iter().all(|m| m.kind == Kind::Before) {
                continue;
            }
            out.push(json!({"app": a.name, "kind": a.kind, "error": null, "marks": tl.listed()}));
        }
        Ok(json!(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(v: Value) -> Snap {
        Snap::from_section(v.as_object().expect("object"))
    }

    fn p(parts: &[&str]) -> Vec<String> {
        path(parts)
    }

    fn base() -> Value {
        json!({"qualityDefinitions": {"Bluray-1080p": {"min": 2, "preferred": null, "max": 100}},
               "qualityProfiles": [{"name": "HD-1080p", "cutoff": "Bluray-1080p", "formatScores": {}}],
               "customFormats": [], "tags": []})
    }

    #[test]
    fn unread_format_does_not_change_scores_or_install_keyframes() {
        let mut before = base();
        before["customFormats"] = json!([{"name":"format", "specifications":[]}]);
        before["qualityProfiles"][0]["formatScores"] = json!({"format": 100});
        let was = sec(before.clone());
        let mut changed = before;
        changed["tags"] = json!(["new tag"]);
        let mut now = sec(changed);
        now.unread.push(Part::CustomFormats);
        now.vals.retain(|_, (p, _)| part_of(p) != Some(Part::CustomFormats));
        let score = p(&["qualityProfiles", "HD-1080p", "formatScores", "format"]);
        assert!(!now.known(&score));
        let mut tl = Timeline::new("radarr");
        tl.record_install(&was, &now, "tags", None, None, false).expect("tag mark");
        assert!(
            tl.marks
                .iter()
                .flat_map(|m| &m.keys)
                .all(|k| part_of(&k.path) != Some(Part::CustomFormats) && shape(&k.path) != Some(Shape::Score))
        );
        assert!(tl.drift(&was).iter().all(|k| shape(&k.path) != Some(Shape::Score)));
    }

    /// The user's example: 1080p at 2, "Better 1080p" makes it 4, then it's put back to
    /// 2 by hand. Removing "Better 1080p" changes nothing, either way.
    #[test]
    fn balanced_needs_no_choice() {
        let s0 = sec(base());
        let mut b = base();
        b["qualityDefinitions"]["Bluray-1080p"]["min"] = json!(4);
        let s1 = sec(b);
        let mut tl = Timeline::new("radarr");
        let id = tl.record_install(&s0, &s1, "Better 1080p", None, None, false).expect("mark");
        assert_eq!(tl.marks[0].keys, vec![Key::new(p(&["qualityDefinitions", "Bluray-1080p", "min"]), Some(json!(2)))]);
        // set back by hand
        let d = tl.drift(&s0);
        assert_eq!(d.len(), 1);
        assert!(tl.record_drift(d));
        assert_eq!(tl.marks.last().map(|m| m.kind), Some(Kind::External));
        assert!(tl.drift(&s0).is_empty(), "seen once");
        let xi = tl.pos(&id).expect("pos");
        let (keep, through, differ) = tl.removals(xi, &s0);
        assert!(!differ);
        assert!(keep.effects(&s0).is_empty() && through.effects(&s0).is_empty());
    }

    /// Owned values go back to what was there before.
    #[test]
    fn own_values_go_back() {
        let s0 = sec(base());
        let mut b = base();
        b["qualityDefinitions"]["Bluray-1080p"]["max"] = json!(null);
        b["qualityProfiles"][0]["cutoff"] = json!("Remux-1080p");
        let s1 = sec(b);
        let mut tl = Timeline::new("radarr");
        let id = tl.record_install(&s0, &s1, "C", None, None, false).expect("mark");
        let (keep, _, differ) = tl.removals(tl.pos(&id).expect("pos"), &s1);
        assert!(!differ);
        let fx = keep.effects(&s1);
        assert_eq!(fx.len(), 2);
        let (sec_, del) = writes(&keep, &s1, &mut vec![]);
        assert!(del.is_empty());
        assert_eq!(sec_["qualityDefinitions"], json!({"Bluray-1080p": {"max": 100}}));
        assert_eq!(sec_["qualityProfiles"], json!([{"name": "HD-1080p", "cutoff": "Bluray-1080p"}]));
    }

    /// A later change wins on the same track; throughout puts it back over that.
    #[test]
    fn later_changes_and_throughout() {
        let s0 = sec(base());
        let mut b = base();
        b["qualityDefinitions"]["Bluray-1080p"]["min"] = json!(4);
        let s1 = sec(b.clone());
        let mut tl = Timeline::new("radarr");
        let x = tl.record_install(&s0, &s1, "Better 1080p", None, None, false).expect("mark");
        b["qualityDefinitions"]["Bluray-1080p"]["min"] = json!(3);
        let s2 = sec(b);
        let d = tl.drift(&s2);
        tl.record_drift(d);
        let xi = tl.pos(&x).expect("pos");
        let (keep, through, differ) = tl.removals(xi, &s2);
        assert!(differ);
        assert!(keep.effects(&s2).is_empty(), "keep: the manual 3 stays");
        let fx = through.effects(&s2);
        assert_eq!(fx, vec![(p(&["qualityDefinitions", "Bluray-1080p", "min"]), Some(json!(3)), Some(json!(2)))]);
        assert_eq!(through.drops.len(), 1, "the manual change loses its keyframe");
        let mut t2 = tl.clone();
        t2.take_out(xi, &through, &[]);
        assert_eq!(t2.marks.len(), 1, "the install and the emptied manual change both go");
        let mut t3 = tl.clone();
        t3.take_out(xi, &keep, &[]);
        assert_eq!(t3.marks.iter().map(|m| m.kind).collect::<Vec<_>>(), [Kind::Before, Kind::External]);
    }

    /// What it made, which a later change uses, stays when keeping; goes throughout.
    #[test]
    fn dependents_hold_or_go() {
        let s0 = sec(base());
        let mut b = base();
        b["customFormats"] = json!([{"name": "x265", "specifications": []}]);
        b["qualityProfiles"].as_array_mut().expect("qp").push(
            json!({"name": "x265 HD", "cutoff": "Bluray-1080p", "upgradeAllowed": true, "formatScores": {"x265": 100}}),
        );
        let s1 = sec(b.clone());
        let mut tl = Timeline::new("radarr");
        let x = tl.record_install(&s0, &s1, "x265", None, None, false).expect("mark");
        assert!(tl.made_here(&tk(&p(&["customFormats", "x265"]))));
        // by hand: HD-1080p scores x265 too
        b["qualityProfiles"][0]["formatScores"] = json!({"x265": 50});
        let s2 = sec(b);
        let d = tl.drift(&s2);
        assert_eq!(d.len(), 1, "{d:?}");
        tl.record_drift(d);
        let xi = tl.pos(&x).expect("pos");
        let (keep, through, differ) = tl.removals(xi, &s2);
        assert!(differ);
        // keep: x265 stays (HD-1080p scores it), its own profile goes
        let fx = keep.effects(&s2);
        assert!(fx.iter().any(|(q, _, to)| *q == p(&["qualityProfiles", "x265 HD"]) && to.is_none()), "{fx:?}");
        assert!(!fx.iter().any(|(q, _, _)| *q == p(&["customFormats", "x265"])), "{fx:?}");
        assert_eq!(keep.kept.len(), 1);
        // throughout: the format goes, and the score by hand with it
        let fx = through.effects(&s2);
        assert!(fx.iter().any(|(q, _, to)| *q == p(&["customFormats", "x265"]) && to.is_none()), "{fx:?}");
        assert!(
            through.drops.iter().any(|(_, k)| *k == tk(&p(&["qualityProfiles", "HD-1080p", "formatScores", "x265"])))
        );
        let (_, del) = writes(&through, &s2, &mut vec![]);
        assert_eq!(del.len(), 2, "{del:?}");
        // the kept one stays in the timeline
        let mut t = tl.clone();
        t.take_out(xi, &keep, &[]);
        let m = &t.marks[t.pos(&x).expect("kept")];
        assert_eq!(m.keys.len(), 1);
        assert!(!m.kept.is_empty());
    }

    /// Its profile, edited by hand, stays; the format that profile scores names
    /// the profile as what uses it, not the config being removed.
    #[test]
    fn kept_notes_name_what_holds_an_item() {
        let s0 = sec(base());
        let mut b = base();
        b["customFormats"] = json!([{"name": "x265", "specifications": []}]);
        b["qualityProfiles"].as_array_mut().expect("qp").push(
            json!({"name": "x265 HD", "cutoff": "Bluray-1080p", "upgradeAllowed": true, "formatScores": {"x265": 100}}),
        );
        let s1 = sec(b.clone());
        let mut tl = Timeline::new("radarr");
        let x = tl.record_install(&s0, &s1, "x265", None, None, false).expect("mark");
        b["qualityProfiles"][1]["upgradeAllowed"] = json!(false);
        let s2 = sec(b);
        let d = tl.drift(&s2);
        tl.record_drift(d);
        let xi = tl.pos(&x).expect("pos");
        let (keep, _, _) = tl.removals(xi, &s2);
        let notes: Vec<&String> = keep.kept.values().filter(|w| !w.is_empty()).collect();
        assert!(notes.iter().any(|w| *w == "custom format x265 stays: quality profile x265 HD uses it"), "{notes:?}");
        assert!(notes.iter().all(|w| !w.contains("stays: x265 uses")), "{notes:?}");
    }

    #[test]
    fn delay_profiles_and_tags() {
        let s0 = sec(json!({"tags": [], "delayProfiles": [{"tags": [], "usenetDelay": 0}]}));
        let s1 = sec(json!({"tags": ["anime"], "delayProfiles": [{"tags": [], "usenetDelay": 30},
                                                                 {"tags": ["anime"], "torrentDelay": 5}]}));
        let mut tl = Timeline::new("sonarr");
        let x = tl.record_install(&s0, &s1, "Anime", None, None, false).expect("mark");
        let xi = tl.pos(&x).expect("pos");
        let (keep, _, differ) = tl.removals(xi, &s1);
        assert!(!differ);
        let (sec_, mut del) = writes(&keep, &s1, &mut vec![]);
        assert_eq!(sec_["delayProfiles"], json!([{"tags": [], "usenetDelay": 0}]));
        del.sort();
        assert_eq!(del, vec![p(&["delayProfiles", "tagged anime"]), p(&["tags", "anime"])]);
        // the listing names what changed
        let l = tl.listed();
        assert_eq!(l[0]["label"], "Anime");
        let items = l[0]["items"].as_array().expect("items");
        assert!(
            items.iter().any(|i| i["name"] == "default" && i["changes"] == json!(["usenetDelay 0 → 30"])),
            "{items:?}"
        );
    }

    #[test]
    fn protected_rows() {
        let rows_ = vec![
            json!({"part": "qualityProfiles", "name": "HD-1080p", "action": "update"}),
            json!({"part": "qualityProfiles", "name": "Mine", "action": "update"}),
            json!({"part": "qualityDefinitions", "name": "Quality sizes", "action": "update"}),
            json!({"part": "qualityProfiles", "name": "Any", "action": "same"}),
        ];
        let got = protected(&rows_, "radarr", "Mine", None);
        assert_eq!(got.len(), 2);
        let mut tl = Timeline::new("radarr");
        tl.marks.push(Mark {
            id: "a".into(),
            at: now(),
            kind: Kind::Config,
            label: "Other".into(),
            history: None,
            partial: false,
            kept: vec![],
            source: Some(Source { name: "TRaSH Guides".into(), url: String::new() }),
            keys: vec![Key::new(p(&["qualityProfiles", "mine", "cutoff"]), Some(json!("x")))],
        });
        assert_eq!(protected(&rows_, "radarr", "Other", Some(&tl)).len(), 2, "its own install");
        let got = protected(&rows_, "radarr", "Mine", Some(&tl));
        assert_eq!(got.len(), 3);
        assert_eq!(got[1]["overrides"], json!(["Other"]));
        assert!(got[1]["why"].is_null());
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
