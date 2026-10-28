// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading a pasted config: the JSON text → a checked [`Config`]. Every message is
//! fixed: the black-box suite and wiki/Community-Configs.md quote them.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{ANY, FORMAT, MAX_TEXT, Part, kinds, media_field, metadata_apps};
use crate::error::Error;

pub type Obj = Map<String, Value>;
type R<T> = Result<T, Error>;

/// A config that can't be used, and why: a 400.
pub fn cerr(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("static regex"));
static SETTING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]{1,60}$").expect("static regex"));

// ---- the model ----------------------------------------------------------------------------

/// An optional part: the importer switches it on or off.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Extra {
    pub default: bool,
    pub note: String,
}

/// One condition of a custom format or an auto tagging rule.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Spec {
    pub name: String,
    pub implementation: String,
    pub negate: bool,
    pub required: bool,
    pub fields: Obj,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Cf {
    pub name: String,
    pub include_custom_format_when_renaming: bool,
    pub specifications: Vec<Spec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Extra>,
    /// The name in the config, before a copy got the prefix.
    #[serde(skip)]
    pub orig: String,
}

/// A quality in a profile: a name, or a group of them.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Quality {
    Name(String),
    Group { name: String, qualities: Vec<String> },
}

impl Quality {
    pub fn name(&self) -> &str {
        match self {
            Quality::Name(n) | Quality::Group { name: n, .. } => n,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualities: Option<Vec<Quality>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_format_score: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff_format_score: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_upgrade_format_score: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub format_scores: IndexMap<String, i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Extra>,
    #[serde(skip)]
    pub orig: String,
}

impl Profile {
    /// The three format score settings, as (API key, value).
    pub fn scores(&self) -> [(&'static str, Option<i64>); 3] {
        [
            ("minFormatScore", self.min_format_score),
            ("cutoffFormatScore", self.cutoff_format_score),
            ("minUpgradeFormatScore", self.min_upgrade_format_score),
        ]
    }
}

/// Lidarr: which album types and release statuses an artist's releases may have.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MetaProfile {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_album_types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_album_types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_statuses: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Extra>,
    #[serde(skip)]
    pub orig: String,
}

impl MetaProfile {
    /// (part, the name key inside each entry, the wanted names)
    pub fn parts(&self) -> [(&'static str, &'static str, Option<&Vec<String>>); 3] {
        [
            ("primaryAlbumTypes", "albumType", self.primary_album_types.as_ref()),
            ("secondaryAlbumTypes", "albumType", self.secondary_album_types.as_ref()),
            ("releaseStatuses", "releaseStatus", self.release_statuses.as_ref()),
        ]
    }
}

pub const MP_PARTS: [(&str, &str); 3] =
    [("primaryAlbumTypes", "albumType"), ("secondaryAlbumTypes", "albumType"), ("releaseStatuses", "releaseStatus")];

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ReleaseProfile {
    pub name: String,
    pub enabled: bool,
    pub required: Vec<String>,
    pub ignored: Vec<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Extra>,
    #[serde(skip)]
    pub orig: String,
}

/// A delay profile, matched by its tags; the untagged one is the app's default.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Delay {
    pub tags: Vec<String>,
    /// The settings it sets, in the order they were given.
    #[serde(flatten)]
    pub settings: Obj,
    #[serde(skip)]
    pub orig: String,
}

impl Delay {
    /// "tagged a, b" or "default"
    pub fn label(&self) -> String {
        dp_name(&self.tags)
    }
}

pub fn dp_name(tags: &[String]) -> String {
    if tags.is_empty() { "default".into() } else { format!("tagged {}", tags.join(", ")) }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AutoTag {
    pub name: String,
    pub remove_tags_automatically: bool,
    pub tags: Vec<String>,
    pub specifications: Vec<Spec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Extra>,
    #[serde(skip)]
    pub orig: String,
}

/// One app kind's part of a config.
#[derive(Clone, Debug, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_formats: Option<Vec<Cf>>,
    /// quality name → {min, preferred, max} (any of them)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_definitions: Option<Obj>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_profiles: Option<Vec<Profile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_profiles: Option<Vec<MetaProfile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_profiles: Option<Vec<ReleaseProfile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_profiles: Option<Vec<Delay>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_tags: Option<Vec<AutoTag>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub naming: Option<Obj>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_management: Option<Obj>,
}

fn vs<T>(v: &Option<Vec<T>>) -> &[T] {
    v.as_deref().unwrap_or(&[])
}

/// A named item: custom format, quality / metadata / release profile, auto tagging rule.
pub trait Named {
    fn name(&self) -> &str;
    fn set_name(&mut self, n: String);
    fn orig(&self) -> &str;
    fn set_orig(&mut self, n: String);
    fn extra(&self) -> Option<&Extra>;
}

macro_rules! named {
    ($($t:ty),*) => {$(
        impl Named for $t {
            fn name(&self) -> &str { &self.name }
            fn set_name(&mut self, n: String) { self.name = n }
            fn orig(&self) -> &str { &self.orig }
            fn set_orig(&mut self, n: String) { self.orig = n }
            fn extra(&self) -> Option<&Extra> { self.extra.as_ref() }
        }
    )*};
}
named!(Cf, Profile, MetaProfile, ReleaseProfile, AutoTag);

impl Section {
    pub fn tags(&self) -> &[String] {
        vs(&self.tags)
    }
    pub fn cfs(&self) -> &[Cf] {
        vs(&self.custom_formats)
    }
    pub fn profiles(&self) -> &[Profile] {
        vs(&self.quality_profiles)
    }
    pub fn mps(&self) -> &[MetaProfile] {
        vs(&self.metadata_profiles)
    }
    pub fn rps(&self) -> &[ReleaseProfile] {
        vs(&self.release_profiles)
    }
    pub fn dps(&self) -> &[Delay] {
        vs(&self.delay_profiles)
    }
    pub fn ats(&self) -> &[AutoTag] {
        vs(&self.auto_tags)
    }
    pub fn defs(&self) -> Option<&Obj> {
        self.quality_definitions.as_ref().filter(|d| !d.is_empty())
    }
    pub fn settings(&self, part: Part) -> Option<&Obj> {
        match part {
            Part::Naming => self.naming.as_ref(),
            Part::MediaManagement => self.media_management.as_ref(),
            _ => None,
        }
        .filter(|d| !d.is_empty())
    }

    /// `sec.get(part)` is truthy.
    pub fn has(&self, part: Part) -> bool {
        match part {
            Part::Tags => !self.tags().is_empty(),
            Part::CustomFormats => !self.cfs().is_empty(),
            Part::QualityDefinitions => self.defs().is_some(),
            Part::QualityProfiles => !self.profiles().is_empty(),
            Part::MetadataProfiles => !self.mps().is_empty(),
            Part::ReleaseProfiles => !self.rps().is_empty(),
            Part::DelayProfiles => !self.dps().is_empty(),
            Part::AutoTags => !self.ats().is_empty(),
            Part::Naming | Part::MediaManagement => self.settings(part).is_some(),
        }
    }

    pub fn is_empty(&self) -> bool {
        !Part::ALL.iter().any(|p| self.has(*p))
    }

    /// The named items of one part.
    pub fn named(&self, part: Part) -> Vec<&dyn Named> {
        fn up<T: Named>(v: &Option<Vec<T>>) -> Vec<&dyn Named> {
            v.iter().flatten().map(|x| x as &dyn Named).collect()
        }
        match part {
            Part::CustomFormats => up(&self.custom_formats),
            Part::QualityProfiles => up(&self.quality_profiles),
            Part::MetadataProfiles => up(&self.metadata_profiles),
            Part::ReleaseProfiles => up(&self.release_profiles),
            Part::AutoTags => up(&self.auto_tags),
            _ => vec![],
        }
    }

    pub fn named_mut(&mut self, part: Part) -> Vec<&mut dyn Named> {
        fn up<T: Named>(v: &mut Option<Vec<T>>) -> Vec<&mut dyn Named> {
            v.iter_mut().flatten().map(|x| x as &mut dyn Named).collect()
        }
        match part {
            Part::CustomFormats => up(&mut self.custom_formats),
            Part::QualityProfiles => up(&mut self.quality_profiles),
            Part::MetadataProfiles => up(&mut self.metadata_profiles),
            Part::ReleaseProfiles => up(&mut self.release_profiles),
            Part::AutoTags => up(&mut self.auto_tags),
            _ => vec![],
        }
    }

    /// Keep the named items of one part for which `f` holds.
    pub fn retain(&mut self, part: Part, f: impl Fn(&dyn Named) -> bool) {
        fn keep<T: Named>(v: &mut Option<Vec<T>>, f: impl Fn(&dyn Named) -> bool) {
            if let Some(v) = v {
                v.retain(|x| f(x as &dyn Named));
            }
        }
        match part {
            Part::CustomFormats => keep(&mut self.custom_formats, f),
            Part::QualityProfiles => keep(&mut self.quality_profiles, f),
            Part::MetadataProfiles => keep(&mut self.metadata_profiles, f),
            Part::ReleaseProfiles => keep(&mut self.release_profiles, f),
            Part::AutoTags => keep(&mut self.auto_tags, f),
            _ => {}
        }
    }

    /// Drop a whole part (an app-wide one the importer keeps as theirs).
    pub fn clear(&mut self, part: Part) {
        match part {
            Part::QualityDefinitions => self.quality_definitions = None,
            Part::Naming => self.naming = None,
            Part::MediaManagement => self.media_management = None,
            _ => {}
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Config {
    pub wranglarr: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub link: String,
    /// Who it's from, and where to fetch it again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    pub apps: IndexMap<String, Section>,
}

/// Where a config comes from: a name to show ("TRaSH Guides"), and the URL of its
/// JSON (for fetching a newer version). Either may be left out.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Source {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}

fn http(v: &str, what: &str) -> R<()> {
    if v.is_empty() || v.starts_with("http://") || v.starts_with("https://") {
        Ok(())
    } else {
        Err(cerr(format!("{what}: must start with http:// or https://")))
    }
}

/// `source`: a name, a URL, or both as {"name", "url"}; a string starting with
/// http(s):// is the URL, any other the name. Nothing in it is no source.
pub fn source(v: Option<&Value>) -> R<Option<Source>> {
    let o = match v {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(o)) => o.clone(),
        Some(Value::String(t)) => {
            let t = t.trim();
            let k = if t.starts_with("http://") || t.starts_with("https://") { "url" } else { "name" };
            [(k.to_string(), json!(t))].into_iter().collect()
        }
        Some(_) => return Err(cerr("source: a name, a URL, or {\"name\", \"url\"}")),
    };
    if let Some(k) = o.keys().find(|k| *k != "name" && *k != "url") {
        return Err(cerr(format!("source: unknown key {k:?} (it takes name and url)")));
    }
    let empty = json!("");
    let src = Source {
        name: s(Some(o.get("name").unwrap_or(&empty)), "source name", 100, true)?.trim().to_string(),
        url: s(Some(o.get("url").unwrap_or(&empty)), "source url", 500, true)?.trim().to_string(),
    };
    http(&src.url, "source url")?;
    Ok((src != Source::default()).then_some(src))
}

// ---- checks ---------------------------------------------------------------------------------

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Text up to n characters.
pub fn s(v: Option<&Value>, what: &str, n: usize, empty: bool) -> R<String> {
    match v {
        Some(Value::String(t)) if (empty || !t.trim().is_empty()) && chars(t) <= n => Ok(t.trim().to_string()),
        _ => Err(cerr(format!("{what}: expected text up to {n} characters"))),
    }
}

fn st(v: Option<&Value>, what: &str) -> R<String> {
    s(v, what, 100, false)
}

fn b(v: Option<&Value>, what: &str) -> R<bool> {
    match v {
        Some(Value::Bool(x)) => Ok(*x),
        _ => Err(cerr(format!("{what}: expected true or false"))),
    }
}

/// A number; with `null`, also null.
fn n(v: Option<&Value>, what: &str, null: bool) -> R<Value> {
    match v {
        Some(Value::Null) if null => Ok(Value::Null),
        Some(x @ Value::Number(_)) => Ok(x.clone()),
        _ => Err(cerr(format!("{what}: expected a number"))),
    }
}

/// `int(x)` of a JSON number.
pub fn to_int(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX))).unwrap_or_else(|| {
        #[allow(clippy::cast_possible_truncation)]
        let i = v.as_f64().unwrap_or(0.0).trunc() as i64;
        i
    })
}

fn int(v: Option<&Value>, what: &str) -> R<i64> {
    n(v, what, false).map(|x| to_int(&x))
}

fn list<'a>(v: Option<&'a Value>, what: &str, max: usize) -> R<&'a Vec<Value>> {
    match v {
        Some(Value::Array(a)) if a.len() > max => Err(cerr(format!("{what}: more than {max} entries"))),
        Some(Value::Array(a)) => Ok(a),
        _ => Err(cerr(format!("{what}: expected a list"))),
    }
}

static EMPTY: LazyLock<Value> = LazyLock::new(|| json!([]));

/// `d.get(k, [])`
fn or_list<'a>(o: &'a Obj, k: &str) -> Option<&'a Value> {
    Some(o.get(k).unwrap_or(&EMPTY))
}

pub fn tag(v: Option<&Value>, what: &str) -> R<String> {
    let t = s(v, what, 60, false)?.to_lowercase();
    if !TAG_RE.is_match(&t) {
        return Err(cerr(format!("{what}: {:?}: a tag is lower-case letters, digits and dashes", t)));
    }
    Ok(t)
}

fn tags(v: Option<&Value>, what: &str, max: usize) -> R<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for t in list(v, what, max)? {
        let t = tag(Some(t), what)?;
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

fn obj<'a>(v: &'a Value, where_: &str) -> R<&'a Obj> {
    v.as_object().ok_or_else(|| cerr(format!("{where_}: expected an object")))
}

fn extra(item: &Obj, name: &str) -> R<Option<Extra>> {
    let Some(x) = item.get("extra") else { return Ok(None) };
    let empty = Map::new();
    let x = match x {
        Value::Bool(true) => &empty,
        Value::Object(o) if o.keys().all(|k| k == "default" || k == "note") => o,
        _ => return Err(cerr(format!("{name}: extra must be {{\"default\": true|false, \"note\": \"…\"}}"))),
    };
    Ok(Some(Extra {
        default: b(Some(x.get("default").unwrap_or(&Value::Bool(true))), &format!("{name}: extra default"))?,
        note: s(Some(x.get("note").unwrap_or(&json!(""))), &format!("{name}: extra note"), 300, true)?,
    }))
}

fn is_num(v: &Value) -> bool {
    v.is_number()
}

fn value(v: &Value, what: &str, lists: bool) -> R<()> {
    if let (Value::Array(a), true) = (v, lists) {
        if a.len() > 200 || !a.iter().all(|x| x.is_string() || is_num(x)) {
            return Err(cerr(format!("{what}: a list here holds up to 200 names or numbers")));
        }
        if a.iter().any(|x| x.as_str().is_some_and(|t| chars(t) > 200)) {
            return Err(cerr(format!("{what}: a name in the list is too long")));
        }
        return Ok(());
    }
    let ok = match v {
        Value::String(t) => chars(t) <= 4000,
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        _ => false,
    };
    if ok { Ok(()) } else { Err(cerr(format!("{what}: must be text or a number"))) }
}

fn specs(item: &Obj, name: &str, lists: bool) -> R<Vec<Spec>> {
    let mut out = vec![];
    for (i, sp) in list(or_list(item, "specifications"), &format!("{name}: specifications"), 60)?.iter().enumerate() {
        let w = format!("{name}, condition {}", i + 1);
        let sp = sp.as_object().ok_or_else(|| cerr(format!("{w}: expected an object")))?;
        let mut fields = Map::new();
        match sp.get("fields") {
            None => {}
            Some(Value::Object(o)) => fields = o.clone(),
            // the API's own shape: [{name, value}]
            Some(Value::Array(a)) => {
                for f in a.iter().filter_map(Value::as_object) {
                    let Some(k) = f.get("name").and_then(Value::as_str) else {
                        return Err(cerr(format!("{w}: field name: expected text up to 60 characters")));
                    };
                    fields.insert(k.to_string(), f.get("value").cloned().unwrap_or(Value::Null));
                }
            }
            Some(_) => return Err(cerr(format!("{w}: fields must be an object"))),
        }
        for (k, v) in &fields {
            s(Some(&Value::String(k.clone())), &format!("{w}: field name"), 60, false)?;
            value(v, &format!("{w}: field {:?}", k), lists)?;
        }
        out.push(Spec {
            name: st(sp.get("name"), &format!("{w} name"))?,
            implementation: s(sp.get("implementation"), &format!("{w} implementation"), 80, false)?,
            negate: b(Some(sp.get("negate").unwrap_or(&Value::Bool(false))), &format!("{w} negate"))?,
            required: b(Some(sp.get("required").unwrap_or(&Value::Bool(false))), &format!("{w} required"))?,
            fields,
        });
    }
    if out.is_empty() {
        return Err(cerr(format!("{name}: needs at least one condition")));
    }
    Ok(out)
}

fn or_false(o: &Obj, k: &str) -> Option<&'static Value> {
    static F: Value = Value::Bool(false);
    if o.contains_key(k) { None } else { Some(&F) }
}

fn get_or<'a>(o: &'a Obj, k: &str, default: &'a Value) -> Option<&'a Value> {
    Some(o.get(k).unwrap_or(default))
}

/// A custom format in the shape the arrs' own Export button (and TRaSH Guides) use.
pub fn cf(v: &Value, where_: &str) -> R<Cf> {
    let o = obj(v, where_)?;
    let name = st(o.get("name"), &format!("{where_} name"))?;
    let inc = b(
        o.get("includeCustomFormatWhenRenaming").or(or_false(o, "includeCustomFormatWhenRenaming")),
        &format!("{name}: includeCustomFormatWhenRenaming"),
    )?;
    let specifications = specs(o, &name, false)?;
    let extra = extra(o, &name)?;
    Ok(Cf { orig: name.clone(), name, include_custom_format_when_renaming: inc, specifications, extra })
}

fn profile(v: &Value, where_: &str) -> R<Profile> {
    let o = obj(v, where_)?;
    let name = st(o.get("name"), &format!("{where_} name"))?;
    let upgrade_allowed = if o.contains_key("upgradeAllowed") {
        Some(b(o.get("upgradeAllowed"), &format!("{name}: upgradeAllowed"))?)
    } else {
        None
    };
    let mut quals = vec![];
    for q in list(or_list(o, "qualities"), &format!("{name}: qualities"), 80)? {
        match q {
            Value::String(_) => quals.push(Quality::Name(st(Some(q), &format!("{name}: quality"))?)),
            Value::Object(g) => {
                let gname = st(g.get("name"), &format!("{name}: group name"))?;
                let qs = list(g.get("qualities"), &format!("{name}: group"), 40)?
                    .iter()
                    .map(|x| st(Some(x), &format!("{name}: quality")))
                    .collect::<R<Vec<_>>>()?;
                quals.push(Quality::Group { name: gname, qualities: qs });
            }
            _ => return Err(cerr(format!("{name}: each quality is a name, or a group {{name, qualities}}"))),
        }
    }
    let cutoff = if o.contains_key("cutoff") { Some(st(o.get("cutoff"), &format!("{name}: cutoff"))?) } else { None };
    let mut sc = [None, None, None];
    for (i, k) in ["minFormatScore", "cutoffFormatScore", "minUpgradeFormatScore"].iter().enumerate() {
        if o.contains_key(*k) {
            sc[i] = Some(int(o.get(*k), &format!("{name}: {k}"))?);
        }
    }
    let language = if o.contains_key("language") {
        Some(s(o.get("language"), &format!("{name}: language"), 60, false)?)
    } else {
        None
    };
    let empty = json!({});
    let scores = match o.get("formatScores").unwrap_or(&empty) {
        Value::Object(m) if m.len() <= 300 => m,
        _ => return Err(cerr(format!("{name}: formatScores must be an object of custom format name → score"))),
    };
    let mut format_scores = IndexMap::new();
    for (k, v) in scores {
        let key = st(Some(&Value::String(k.clone())), &format!("{name}: format name"))?;
        format_scores.insert(key, int(Some(v), &format!("{name}: score for {k}"))?);
    }
    let extra = extra(o, &name)?;
    Ok(Profile {
        orig: name.clone(),
        name,
        upgrade_allowed,
        qualities: (!quals.is_empty()).then_some(quals),
        cutoff,
        min_format_score: sc[0],
        cutoff_format_score: sc[1],
        min_upgrade_format_score: sc[2],
        language,
        format_scores,
        extra,
    })
}

fn sizes(v: &Value, where_: &str) -> R<Obj> {
    let d = v
        .as_object()
        .ok_or_else(|| cerr(format!("{where_}: expected an object of quality name → {{min, preferred, max}}")))?;
    if d.len() > super::limit(Part::QualityDefinitions) {
        return Err(cerr(format!("{where_}: too many qualities")));
    }
    let mut out = Map::new();
    for (q, v) in d {
        s(Some(&Value::String(q.clone())), &format!("{where_}: quality name"), 100, false)?;
        let x = match v {
            Value::Object(x)
                if !x.is_empty() && x.keys().all(|k| ["min", "preferred", "max"].contains(&k.as_str())) =>
            {
                x
            }
            _ => return Err(cerr(format!("{where_} {q}: expected {{min, preferred, max}} (any of them)"))),
        };
        let mut e = Map::new();
        for (k, val) in x {
            e.insert(k.clone(), n(Some(val), &format!("{q} {k}"), k != "min")?);
        }
        out.insert(q.clone(), Value::Object(e));
    }
    Ok(out)
}

fn release_profile(v: &Value, where_: &str) -> R<ReleaseProfile> {
    let o = obj(v, where_)?;
    let name = st(o.get("name"), &format!("{where_} name"))?;
    let terms = |k: &str| -> R<Vec<String>> {
        list(or_list(o, k), &format!("{name}: {k}"), 200)?
            .iter()
            .map(|t| s(Some(t), &format!("{name}: {k} term"), 500, false))
            .collect()
    };
    let enabled = b(get_or(o, "enabled", &Value::Bool(true)), &format!("{name}: enabled"))?;
    let required = terms("required")?;
    let ignored = terms("ignored")?;
    let tags = tags(or_list(o, "tags"), &format!("{name}: tags"), 20)?;
    if required.is_empty() && ignored.is_empty() {
        return Err(cerr(format!("{name}: a release profile needs required or ignored terms")));
    }
    let extra = extra(o, &name)?;
    Ok(ReleaseProfile { orig: name.clone(), name, enabled, required, ignored, tags, extra })
}

fn metadata_profile(v: &Value, where_: &str) -> R<MetaProfile> {
    let o = obj(v, where_)?;
    let name = st(o.get("name"), &format!("{where_} name"))?;
    let part = |k: &str| -> R<Option<Vec<String>>> {
        if !o.contains_key(k) {
            return Ok(None);
        }
        list(o.get(k), &format!("{name}: {k}"), 40)?
            .iter()
            .map(|x| s(Some(x), &format!("{name}: {k}"), 60, false))
            .collect::<R<Vec<_>>>()
            .map(Some)
    };
    let (p, sec, rs) = (part("primaryAlbumTypes")?, part("secondaryAlbumTypes")?, part("releaseStatuses")?);
    let extra = extra(o, &name)?;
    Ok(MetaProfile {
        orig: name.clone(),
        name,
        primary_album_types: p,
        secondary_album_types: sec,
        release_statuses: rs,
        extra,
    })
}

pub const DELAY_BOOL: [&str; 4] =
    ["enableUsenet", "enableTorrent", "bypassIfHighestQuality", "bypassIfAboveCustomFormatScore"];
pub const DELAY_INT: [&str; 3] = ["usenetDelay", "torrentDelay", "minimumCustomFormatScore"];

fn delay(v: &Value, where_: &str) -> R<Delay> {
    let d = obj(v, where_)?;
    let mut unknown: Vec<&String> = d
        .keys()
        .filter(|k| {
            !DELAY_BOOL.contains(&k.as_str())
                && !DELAY_INT.contains(&k.as_str())
                && !["preferredProtocol", "tags", "items", "name"].contains(&k.as_str())
        })
        .collect();
    unknown.sort();
    if let Some(k) = unknown.first() {
        return Err(cerr(format!("{where_}: unknown setting {:?}", k)));
    }
    let mut t = tags(or_list(d, "tags"), &format!("{where_}: tags"), 20)?;
    t.sort();
    let mut out = Map::new();
    for k in DELAY_BOOL {
        if d.contains_key(k) {
            out.insert(k.into(), json!(b(d.get(k), &format!("{where_}: {k}"))?));
        }
    }
    for k in DELAY_INT {
        if d.contains_key(k) {
            let x = int(d.get(k), &format!("{where_}: {k}"))?;
            if k.ends_with("Delay") && !(0..=100_000).contains(&x) {
                return Err(cerr(format!("{where_}: {k} is minutes, 0 or more")));
            }
            out.insert(k.into(), json!(x));
        }
    }
    if d.contains_key("preferredProtocol") {
        let p = s(d.get("preferredProtocol"), &format!("{where_}: preferredProtocol"), 20, false)?;
        if p != "usenet" && p != "torrent" {
            return Err(cerr(format!("{where_}: preferredProtocol is usenet or torrent")));
        }
        out.insert("preferredProtocol".into(), json!(p));
    }
    if d.contains_key("items") {
        // Lidarr: one entry per protocol, in order
        let mut items = vec![];
        for it in list(d.get("items"), &format!("{where_}: items"), 10)? {
            let it = it.as_object().ok_or_else(|| cerr(format!("{where_}: items: expected objects")))?;
            items.push(json!({
                "name": s(get_or(it, "name", &json!("")), &format!("{where_}: item name"), 60, true)?,
                "protocol": s(it.get("protocol"), &format!("{where_}: item protocol"), 60, false)?,
                "allowed": b(get_or(it, "allowed", &Value::Bool(true)), &format!("{where_}: item allowed"))?,
                "delay": int(get_or(it, "delay", &json!(0)), &format!("{where_}: item delay"))?,
            }));
        }
        out.insert("items".into(), Value::Array(items));
    }
    if d.contains_key("name") {
        out.insert("name".into(), json!(st(d.get("name"), &format!("{where_}: name"))?));
    }
    let orig = dp_name(&t);
    Ok(Delay { tags: t, settings: out, orig })
}

fn auto_tag(v: &Value, where_: &str) -> R<AutoTag> {
    let a = obj(v, where_)?;
    let name = st(a.get("name"), &format!("{where_} name"))?;
    let rta = b(
        a.get("removeTagsAutomatically").or(or_false(a, "removeTagsAutomatically")),
        &format!("{name}: removeTagsAutomatically"),
    )?;
    let tags = tags(or_list(a, "tags"), &format!("{name}: tags"), 20)?;
    let specifications = specs(a, &name, true)?;
    if tags.is_empty() {
        return Err(cerr(format!("{name}: auto tagging needs at least one tag to add")));
    }
    let extra = extra(a, &name)?;
    Ok(AutoTag { orig: name.clone(), name, remove_tags_automatically: rta, tags, specifications, extra })
}

fn settings(v: &Value, where_: &str, allowed: Option<fn(&str) -> bool>) -> R<Obj> {
    let d = match v {
        Value::Object(d) if !d.is_empty() && d.len() <= 40 => d,
        _ => return Err(cerr(format!("{where_}: expected an object of setting → value"))),
    };
    let mut out = Map::new();
    for (k, v) in d {
        if !SETTING_RE.is_match(k) || k == "id" {
            return Err(cerr(format!("{where_}: {:?} isn't a setting name", k)));
        }
        if let Some(a) = allowed
            && !a(k)
        {
            return Err(cerr(format!("{where_}: {:?} can't go in a config (it's tied to one server, or unknown)", k)));
        }
        let ok = match v {
            Value::String(t) => chars(t) <= 1000,
            Value::Bool(_) => true,
            Value::Number(x) => x.is_i64() || x.is_u64(),
            _ => false,
        };
        if !ok {
            return Err(cerr(format!("{where_}: {k} must be text, a number or true/false")));
        }
        out.insert(k.clone(), v.clone());
    }
    Ok(out)
}

fn items<T>(kind: &str, sec: &Obj, part: Part, f: impl Fn(&Value, &str) -> R<T>) -> R<Vec<T>> {
    list(sec.get(part.key()), &format!("{kind}: {}", part.key()), super::limit(part))?
        .iter()
        .enumerate()
        .map(|(i, x)| f(x, &format!("{kind} {} {}", part.label(), i + 1)))
        .collect()
}

fn section(kind: &str, sec: &Obj) -> R<Section> {
    let mut s = Section::default();
    let has = |p: Part| sec.contains_key(p.key());
    if has(Part::Tags) {
        s.tags = Some(tags(sec.get("tags"), &format!("{kind}: tags"), super::limit(Part::Tags))?);
    }
    if has(Part::CustomFormats) {
        s.custom_formats = Some(items(kind, sec, Part::CustomFormats, cf)?);
    }
    if let Some(v) = sec.get("qualityDefinitions") {
        s.quality_definitions = Some(sizes(v, &format!("{kind} qualityDefinitions"))?);
    }
    if has(Part::QualityProfiles) {
        s.quality_profiles = Some(items(kind, sec, Part::QualityProfiles, profile)?);
    }
    if has(Part::MetadataProfiles) {
        if !super::def(kind).is_some_and(|c| c.metadata_profiles.is_some()) {
            return Err(cerr(format!("{kind}: metadata profiles are {}'s", metadata_apps())));
        }
        s.metadata_profiles = Some(items(kind, sec, Part::MetadataProfiles, metadata_profile)?);
    }
    if has(Part::ReleaseProfiles) {
        s.release_profiles = Some(items(kind, sec, Part::ReleaseProfiles, release_profile)?);
    }
    if has(Part::DelayProfiles) {
        let dps = items(kind, sec, Part::DelayProfiles, delay)?;
        let mut seen: Vec<&Vec<String>> = vec![];
        for d in &dps {
            if seen.contains(&&d.tags) {
                let t = if d.tags.is_empty() { "none".to_string() } else { format!("{:?}", d.tags) };
                return Err(cerr(format!("{kind}: two delay profiles have the tags {t}")));
            }
            seen.push(&d.tags);
        }
        s.delay_profiles = Some(dps);
    }
    if has(Part::AutoTags) {
        s.auto_tags = Some(items(kind, sec, Part::AutoTags, auto_tag)?);
    }
    if let Some(v) = sec.get("naming") {
        s.naming = Some(settings(v, &format!("{kind} naming"), None)?);
    }
    if let Some(v) = sec.get("mediaManagement") {
        s.media_management = Some(settings(v, &format!("{kind} mediaManagement"), Some(media_field))?);
    }
    for part in Part::NAMED {
        let mut seen = std::collections::HashSet::new();
        for x in s.named(part) {
            if !seen.insert(x.name().to_lowercase()) {
                return Err(cerr(format!("{kind}: two {}s are called {:?}", part.label(), x.name())));
            }
        }
    }
    Ok(s)
}

/// Why pasted text isn't JSON, and where: "expected `,` or `}` at line 3 column 5".
fn json_error(e: &serde_json::Error) -> String {
    format!("not valid JSON: {e}")
}

/// Pasted text → a checked config. Also takes a bare custom format (or a list of
/// them) as the arrs' Export button or TRaSH Guides give it. `None`: not text.
pub fn parse(text: Option<&str>) -> R<Config> {
    let text = text.filter(|t| !t.trim().is_empty()).ok_or_else(|| cerr("paste a config first"))?;
    if text.len() > MAX_TEXT && chars(text) > MAX_TEXT {
        return Err(cerr(format!("that's over {} KB; a config is usually a few KB", MAX_TEXT / 1024)));
    }
    let mut data: Value = serde_json::from_str(text).map_err(|e| cerr(json_error(&e)))?;
    if data.as_object().is_some_and(|o| o.contains_key("specifications")) {
        data = Value::Array(vec![data]);
    }
    if let Value::Array(a) = &data {
        if a.is_empty() || !a.iter().all(|x| x.as_object().is_some_and(|o| o.contains_key("specifications"))) {
            return Err(cerr("a list here must be custom formats (each with a name and specifications)"));
        }
        let names: Vec<&str> = a.iter().filter_map(|x| x["name"].as_str()).collect();
        let name = if names.len() == 1 { names[0].to_string() } else { format!("{} custom formats", names.len()) };
        data = json!({"name": name, "apps": {ANY: {"customFormats": a}}});
    }
    let Some(d) = data.as_object().filter(|d| d.get("apps").is_some_and(Value::is_object)) else {
        return Err(cerr("this isn't a wranglarr config: it needs \"apps\" (or paste a custom format)"));
    };
    let fmt = d.get("wranglarr").cloned().unwrap_or_else(|| json!(FORMAT));
    if fmt != FORMAT {
        return Err(cerr(format!("made for {fmt}; this wranglarr reads {FORMAT:?}")));
    }
    let mut out = Config {
        wranglarr: FORMAT.into(),
        name: st(get_or(d, "name", &json!("Untitled")), "name")?,
        description: s(get_or(d, "description", &json!("")), "description", 4000, true)?,
        author: s(get_or(d, "author", &json!("")), "author", 100, true)?,
        link: s(get_or(d, "link", &json!("")), "link", 300, true)?,
        source: source(d.get("source"))?,
        apps: IndexMap::new(),
    };
    http(&out.link, "link")?;
    for (kind, sec) in d["apps"].as_object().into_iter().flatten() {
        if !kinds().contains(&kind.as_str()) && kind != ANY {
            return Err(cerr(format!("apps: {:?} isn't one of {}", kind, kinds().join(", "))));
        }
        let sec = sec.as_object().ok_or_else(|| cerr(format!("{kind}: expected an object")))?;
        let mut extra: Vec<&String> = sec.keys().filter(|k| Part::from_key(k).is_none()).collect();
        extra.sort();
        if let Some(x) = extra.first() {
            let known: Vec<&str> = Part::ALL.iter().map(|p| p.key()).collect();
            return Err(cerr(format!("{kind}: unknown part {:?} (known: {})", x, known.join(", "))));
        }
        if kind == ANY && sec.keys().any(|k| k != "customFormats") {
            return Err(cerr("only custom formats can be left without an app"));
        }
        let s = section(kind, sec)?;
        if !s.is_empty() {
            out.apps.insert(kind.clone(), s);
        }
    }
    if out.apps.is_empty() {
        return Err(cerr("this config has nothing in it"));
    }
    Ok(out)
}

/// One optional extra, with whether it's on.
#[derive(Clone, Debug, Serialize)]
pub struct ExtraRow {
    pub key: String,
    pub kind: String,
    pub part: &'static str,
    pub label: &'static str,
    pub name: String,
    pub note: String,
    pub default: bool,
    pub on: bool,
}

/// The config's optional extras, each with whether it's on (choices: key → bool).
pub fn extras(cfg: &Config, choices: Option<&Value>) -> Vec<ExtraRow> {
    let choices = choices.and_then(Value::as_object);
    let mut out = vec![];
    for (kind, sec) in &cfg.apps {
        for part in Part::NAMED {
            for x in sec.named(part) {
                if let Some(e) = x.extra() {
                    let key = format!("{kind}/{}/{}", part.key(), x.name());
                    let on = choices.and_then(|c| c.get(&key)).and_then(Value::as_bool).unwrap_or(e.default);
                    out.push(ExtraRow {
                        key,
                        kind: kind.clone(),
                        part: part.key(),
                        label: part.label(),
                        name: x.name().to_string(),
                        note: e.note.clone(),
                        default: e.default,
                        on,
                    });
                }
            }
        }
    }
    out
}

/// The config with the extras that are off taken out, scores for those formats included.
pub fn choose(cfg: &Config, choices: Option<&Value>) -> Config {
    let off: std::collections::HashSet<String> =
        extras(cfg, choices).into_iter().filter(|x| !x.on).map(|x| x.key).collect();
    let mut cfg = cfg.clone();
    if off.is_empty() {
        return cfg;
    }
    for (kind, sec) in cfg.apps.iter_mut() {
        let gone: std::collections::HashSet<String> = sec
            .cfs()
            .iter()
            .filter(|x| off.contains(&format!("{kind}/customFormats/{}", x.name)))
            .map(|x| x.name.to_lowercase())
            .collect();
        for part in Part::NAMED {
            sec.retain(part, |x| !off.contains(&format!("{kind}/{}/{}", part.key(), x.name())));
        }
        for p in sec.quality_profiles.iter_mut().flatten() {
            p.format_scores.retain(|k, _| !gone.contains(&k.to_lowercase()));
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: Value) -> R<Config> {
        parse(Some(&v.to_string()))
    }

    fn err(v: Value) -> String {
        match p(v) {
            Err(e) => e.message().to_string(),
            Ok(c) => panic!("parsed: {c:?}"),
        }
    }

    #[test]
    fn examples_parse() {
        for f in super::super::Examples::iter() {
            let text = super::super::example_text(&f).expect("example");
            let c = parse(Some(&text)).unwrap_or_else(|e| panic!("{f}: {e}"));
            assert!(!c.apps.is_empty());
        }
    }

    #[test]
    fn keeps_order_and_shape() {
        let text = super::super::example_text("x265-small.json").expect("example");
        let c = parse(Some(&text)).expect("parse");
        assert_eq!(c.apps.keys().collect::<Vec<_>>(), ["radarr", "sonarr"]);
        let defs = c.apps["radarr"].defs().expect("defs");
        assert_eq!(defs.keys().next().map(String::as_str), Some("HDTV-720p"));
        let v = serde_json::to_value(&c).expect("json");
        let keys: Vec<&String> = v.as_object().expect("obj").keys().collect();
        assert_eq!(keys, ["wranglarr", "name", "description", "author", "link", "apps"]);
        let cf0 = &v["apps"]["radarr"]["customFormats"][0];
        assert_eq!(
            cf0.as_object().expect("cf").keys().collect::<Vec<_>>(),
            ["name", "includeCustomFormatWhenRenaming", "specifications"]
        );
        // round trip: what parse gives back reads in again the same
        let again = parse(Some(&v.to_string())).expect("again");
        assert_eq!(again, c);
    }

    #[test]
    fn bare_custom_format() {
        let c = p(json!({"name": "x", "specifications": [{"name": "a", "implementation": "ReleaseTitleSpecification",
                          "fields": [{"name": "value", "value": "y"}]}]}))
        .expect("parse");
        assert_eq!(c.name, "x");
        let sec = &c.apps[ANY];
        assert_eq!(sec.cfs()[0].specifications[0].fields["value"], "y");
        let two = json!([{"name": "a", "specifications": []}, {"name": "b", "specifications": []}]);
        assert_eq!(err(two), "a: needs at least one condition");
    }

    #[test]
    fn messages() {
        assert_eq!(parse(None).err().map(|e| e.to_string()), Some("paste a config first".into()));
        assert!(
            parse(Some("not json")).err().map(|e| e.to_string()).unwrap_or_default()
                == "not valid JSON: expected ident at line 1 column 2"
        );
        assert_eq!(err(json!({"x": 1})), "this isn't a wranglarr config: it needs \"apps\" (or paste a custom format)");
        assert_eq!(
            err(json!({"wranglarr": "other/2", "apps": {}})),
            "made for \"other/2\"; this wranglarr reads \"wranglarr-config/1\""
        );
        assert_eq!(err(json!({"apps": {"plex": {}}})), "apps: \"plex\" isn't one of radarr, sonarr, lidarr, whisparr");
        assert_eq!(
            err(json!({"apps": {"radarr": {"zzz": 1, "aaa": 2}}})),
            "radarr: unknown part \"aaa\" (known: tags, customFormats, qualityDefinitions, qualityProfiles, metadataProfiles, releaseProfiles, delayProfiles, autoTags, naming, mediaManagement)"
        );
        assert_eq!(err(json!({"apps": {"radarr": {}}})), "this config has nothing in it");
        assert_eq!(
            err(json!({"apps": {"radarr": {"tags": ["Bad Tag"]}}})),
            "radarr: tags: \"bad tag\": a tag is lower-case letters, digits and dashes"
        );
        assert_eq!(
            err(json!({"apps": {"radarr": {"metadataProfiles": []}}})),
            "radarr: metadata profiles are Lidarr's"
        );
        assert_eq!(
            err(json!({"apps": {"radarr": {"delayProfiles": [{"tags": ["a"]}, {"tags": ["a"]}]}}})),
            "radarr: two delay profiles have the tags [\"a\"]"
        );
        assert_eq!(
            err(json!({"apps": {"radarr": {"delayProfiles": [{"wat": 1}]}}})),
            "radarr delay profile 1: unknown setting \"wat\""
        );
        assert_eq!(
            err(json!({"apps": {"radarr": {"mediaManagement": {"recycleBin": "/x"}}}})),
            "radarr mediaManagement: \"recycleBin\" can't go in a config (it's tied to one server, or unknown)"
        );
        assert_eq!(
            err(json!({"apps": {"radarr": {"naming": {"x": 1.5}}}})),
            "radarr naming: x must be text, a number or true/false"
        );
        assert_eq!(
            err(json!({"apps": {"radarr": {"qualityDefinitions": {"HDTV-720p": {"mid": 1}}}}})),
            "radarr qualityDefinitions HDTV-720p: expected {min, preferred, max} (any of them)"
        );
        assert_eq!(
            err(json!({"apps": {"radarr": {"qualityDefinitions": {"HDTV-720p": {"min": null}}}}})),
            "HDTV-720p min: expected a number"
        );
        let cf = json!({"name": "A", "specifications": [{"name": "s", "implementation": "X"}]});
        assert_eq!(
            err(
                json!({"apps": {"radarr": {"customFormats": [cf, {"name": "a", "specifications": [{"name": "s", "implementation": "X"}]}]}}})
            ),
            "radarr: two custom formats are called \"a\""
        );
        assert_eq!(
            err(json!({"link": "ftp://x", "apps": {"radarr": {"tags": ["a"]}}})),
            "link: must start with http:// or https://"
        );
        assert_eq!(err(json!({"apps": {"*": {"tags": ["a"]}}})), "only custom formats can be left without an app");
        assert_eq!(
            err(json!({"source": {"url": "ftp://x"}, "apps": {"radarr": {"tags": ["a"]}}})),
            "source url: must start with http:// or https://"
        );
        assert_eq!(
            err(json!({"source": {"site": "x"}, "apps": {"radarr": {"tags": ["a"]}}})),
            "source: unknown key \"site\" (it takes name and url)"
        );
    }

    #[test]
    fn sources() {
        let src = |v: Value| source(Some(&v)).expect("source");
        let s = |n: &str, u: &str| Some(Source { name: n.into(), url: u.into() });
        assert_eq!(src(json!("TRaSH Guides")), s("TRaSH Guides", ""));
        assert_eq!(src(json!(" https://x.example/c.json ")), s("", "https://x.example/c.json"));
        assert_eq!(
            src(json!({"name": "Mine", "url": "https://x.example/c.json"})),
            s("Mine", "https://x.example/c.json")
        );
        assert_eq!(src(json!({"name": " ", "url": ""})), None);
        assert_eq!(src(Value::Null), None);
        assert!(source(Some(&json!(3))).is_err());
        // it reads back, and a config without one has no key for it
        let c = parse(Some(&json!({"name": "c", "source": "Mine", "apps": {"radarr": {"tags": ["a"]}}}).to_string()))
            .expect("parse");
        assert_eq!(serde_json::to_value(&c).expect("json")["source"], json!({"name": "Mine"}));
        let c = parse(Some(&json!({"name": "c", "apps": {"radarr": {"tags": ["a"]}}}).to_string())).expect("parse");
        assert!(serde_json::to_value(&c).expect("json").get("source").is_none());
    }

    #[test]
    fn extras_and_choose() {
        let c = p(json!({"apps": {"radarr": {
            "customFormats": [{"name": "Opt", "extra": {"default": false, "note": "n"},
                               "specifications": [{"name": "s", "implementation": "X"}]},
                              {"name": "Base", "specifications": [{"name": "s", "implementation": "X"}]}],
            "qualityProfiles": [{"name": "P", "formatScores": {"Opt": 5, "Base": 1}}]}}}))
        .expect("parse");
        let x = extras(&c, None);
        assert_eq!(x.len(), 1);
        assert_eq!((x[0].key.as_str(), x[0].on), ("radarr/customFormats/Opt", false));
        let off = choose(&c, None);
        assert_eq!(off.apps["radarr"].cfs().len(), 1);
        assert_eq!(off.apps["radarr"].profiles()[0].format_scores.keys().collect::<Vec<_>>(), ["Base"]);
        let on = choose(&c, Some(&json!({"radarr/customFormats/Opt": true})));
        assert_eq!(on.apps["radarr"].cfs().len(), 2);
        assert_eq!(on.apps["radarr"].profiles()[0].format_scores.len(), 2);
    }
}
