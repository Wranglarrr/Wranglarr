// SPDX-License-Identifier: GPL-3.0-or-later
//! The preview of one config section against one app (rows), and the work
//! apply does from it.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::arr_view::{ArrView, CfIds, is_config, lname, portable_qualities, rows};
use super::names::{all_tags, rename, slug};
use super::parse::{AutoTag, Delay, MetaProfile, Profile, ReleaseProfile, Section};
use super::values::{number, same_field, same_value, short};
use super::{CHOICES, Part, builtin, count};
use crate::error::Result as R;
use crate::value::{text, truthy};

/// What apply does for one app, in order. `None` for cur: create.
#[derive(Default, Debug, Clone)]
pub struct Work {
    pub tags: Vec<String>,
    /// (API body, what the app has now)
    pub cfs: Vec<(Value, Option<Value>)>,
    /// (new definition, the one there now)
    pub defs: Vec<(Value, Value)>,
    pub profiles: Vec<(Profile, Option<Value>)>,
    pub mps: Vec<(MetaProfile, Option<Value>)>,
    pub rps: Vec<(ReleaseProfile, Option<Value>)>,
    pub dps: Vec<(Delay, Option<Value>)>,
    pub ats: Vec<(AutoTag, Option<Value>)>,
    /// (new settings, the ones there now)
    pub naming: Option<(Value, Value)>,
    pub mm: Option<(Value, Value)>,
}

/// A delay profile in a line: "usenet +30 min, torrent".
pub fn dp_detail(p: &Value) -> String {
    let plus = |d: &Value| if truthy(d) { format!(" +{} min", text(d)) } else { String::new() };
    if let Some(items) = p.get("items") {
        let on: Vec<String> = rows(items)
            .iter()
            .filter(|i| truthy(&i["allowed"]))
            .map(|i| {
                let n = if truthy(&i["name"]) { &i["name"] } else { &i["protocol"] };
                format!("{}{}", text(n), plus(&i["delay"]))
            })
            .collect();
        return if on.is_empty() { "nothing allowed".into() } else { on.join(" → ") };
    }
    let mut out = vec![];
    for (proto, title) in [("usenet", "Usenet"), ("torrent", "Torrent")] {
        if p.get(format!("enable{title}")).is_none_or(truthy) {
            let d = p.get(format!("{proto}Delay")).cloned().unwrap_or(json!(0));
            out.push(format!("{proto}{}", plus(&d)));
        }
    }
    let first = p.get("preferredProtocol").filter(|f| truthy(f));
    let head = match first {
        Some(f) if out.len() > 1 => format!("{} first · ", text(f)),
        _ => String::new(),
    };
    head + &(if out.is_empty() { "nothing allowed".to_string() } else { out.join(", ") })
}

struct Rows<'a> {
    rows: Vec<Value>,
    app: &'a str,
    resolve: Option<&'a serde_json::Map<String, Value>>,
    renamed: HashMap<(Part, String), String>,
}

impl Rows<'_> {
    fn key(&self, part: Part, o: &str) -> String {
        format!("{}|{}|{o}", self.app, part.key())
    }

    fn choice(&self, part: Part, o: &str) -> Option<&str> {
        self.resolve.and_then(|r| r.get(&self.key(part, o))).and_then(Value::as_str)
    }

    /// The importer's choice for something the app has, if it's one of `can`; else "change".
    fn pick(&self, part: Part, o: &str, can: &[&str]) -> String {
        match self.choice(part, o) {
            Some(c) if can.contains(&c) => c.to_string(),
            _ => "change".into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        &mut self,
        part: Part,
        name: &str,
        action: &str,
        details: Vec<String>,
        warn: Vec<String>,
        orig: Option<&str>,
        can: &[&str],
    ) {
        let k = (part, name.to_lowercase());
        let copied = self.renamed.contains_key(&k);
        let o = match orig {
            Some(o) => o.to_string(),
            None => self.renamed.get(&k).cloned().unwrap_or_else(|| name.to_string()),
        };
        let can: Vec<&str> = if !can.is_empty() {
            can.to_vec()
        } else if copied {
            if part == Part::Tags { vec!["change", "prefix"] } else { CHOICES.to_vec() }
        } else {
            vec![]
        };
        let mut details: Vec<String> = details.into_iter().filter(|d| !d.is_empty()).collect();
        if copied {
            details.insert(0, format!("a copy: your {o} stays as it is"));
        }
        let choice = if can.is_empty() {
            Value::Null
        } else {
            match self.choice(part, &o) {
                Some(c) if can.contains(&c) => json!(c),
                _ => json!(if copied { "prefix" } else { "change" }),
            }
        };
        let key = self.key(part, &o);
        self.rows.push(json!({"part": part.key(), "label": part.label(), "name": name, "action": action,
                              "details": details, "warnings": warn, "key": key, "orig": o, "can": can,
                              "choice": choice}));
    }
}

fn when(diff: &[String], w: Vec<String>) -> Vec<String> {
    if diff.is_empty() { vec![] } else { w }
}

fn ud(diff: &[String]) -> &'static str {
    if diff.is_empty() { "same" } else { "update" }
}

fn can_if<'a>(diff: &[String], can: &'a [&'a str]) -> &'a [&'a str] {
    if diff.is_empty() { &[] } else { can }
}

/// Lower-case names the app already has, per part the config uses.
async fn have(arr: &mut ArrView, sec: &Section) -> R<HashMap<Part, HashSet<String>>> {
    let mut h: HashMap<Part, HashSet<String>> = HashMap::new();
    for part in Part::NAMED.into_iter().chain([Part::Tags]) {
        h.insert(part, HashSet::new());
    }
    for (part, path) in [
        (Part::CustomFormats, "customformat"),
        (Part::QualityProfiles, "qualityprofile"),
        (Part::MetadataProfiles, "metadataprofile"),
        (Part::ReleaseProfiles, "releaseprofile"),
        (Part::AutoTags, "autotagging"),
    ] {
        if sec.has(part) {
            h.insert(part, arr.list(path).await?.iter().map(lname).collect());
        }
    }
    if !all_tags(sec).is_empty() {
        h.insert(
            Part::Tags,
            arr.tag_list().await?.iter().map(|t| t["label"].as_str().unwrap_or("").to_lowercase()).collect(),
        );
    }
    Ok(h)
}

/// Rows for the preview, and what apply needs. resolve: "app|part|name" →
/// change, keep or prefix, for what the app already has.
pub async fn plan_one(
    sec: &Section,
    arr: &mut ArrView,
    app: &str,
    resolve: Option<&Value>,
    prefix: &str,
) -> R<(Vec<Value>, Work)> {
    let mut sec = sec.clone();
    for part in Part::NAMED {
        for x in sec.named_mut(part) {
            let n = x.name().to_string();
            x.set_orig(n);
        }
    }
    for d in sec.delay_profiles.iter_mut().flatten() {
        d.orig = d.label();
    }
    let have = have(arr, &sec).await?;
    let rs = Rows { rows: vec![], app, resolve: resolve.and_then(Value::as_object), renamed: HashMap::new() };
    let mut p = Planner { sec, arr, rs, work: Work::default() };
    let kept = p.choices(&have, prefix).await?;
    p.tags().await?;
    let ids = p.custom_formats().await?;
    p.quality_definitions().await?;
    p.quality_profiles(&ids).await?;
    p.metadata_profiles().await?;
    p.release_profiles().await?;
    p.delay_profiles().await?;
    p.auto_tags().await?;
    p.settings().await?;
    p.kept(kept);
    Ok((p.rs.rows, p.work))
}

/// Something the importer chose to keep as the app has it: (part, row name, original name).
type Kept = (Part, String, String);

/// One plan in the making: the section (as the importer's choices change it), the
/// app, the preview rows so far and the work for apply. Each step adds one part's
/// rows, in the order the preview shows them.
struct Planner<'a> {
    sec: Section,
    arr: &'a mut ArrView,
    rs: Rows<'a>,
    work: Work,
}

impl Planner<'_> {
    /// The importer's choices for what the app already has: copies under the prefix
    /// (and what points at them follows), or keep theirs (left out of the plan).
    async fn choices(&mut self, have: &HashMap<Part, HashSet<String>>, prefix: &str) -> R<Vec<Kept>> {
        let mut kept: Vec<Kept> = vec![];
        for part in Part::NAMED {
            let items: Vec<(String, String)> =
                self.sec.named(part).iter().map(|x| (x.name().to_string(), x.orig().to_string())).collect();
            for (name, o) in items {
                if !have[&part].contains(&o.to_lowercase()) {
                    continue;
                }
                match self.rs.pick(part, &o, &CHOICES).as_str() {
                    "prefix" => {
                        let new = format!("{prefix}{o}");
                        rename(&mut self.sec, part, &name, &new);
                        self.rs.renamed.insert((part, new.to_lowercase()), o);
                    }
                    "keep" => {
                        self.sec.retain(part, |x| x.name() != name);
                        kept.push((part, o.clone(), o));
                    }
                    _ => {}
                }
            }
        }
        for t in all_tags(&self.sec) {
            if have[&Part::Tags].contains(&t) && self.rs.pick(Part::Tags, &t, &["change", "prefix"]) == "prefix" {
                let new = format!("{}{t}", slug(prefix));
                rename(&mut self.sec, Part::Tags, &t, &new);
                self.rs.renamed.insert((Part::Tags, new), t);
            }
        }
        if self.sec.has(Part::DelayProfiles) {
            let have_dp = self.arr.delays().await?;
            let mut keep_out = vec![];
            for d in self.sec.dps() {
                if have_dp.iter().any(|(k, _)| *k == d.tags)
                    && self.rs.pick(Part::DelayProfiles, &d.orig, &["change", "keep"]) == "keep"
                {
                    keep_out.push(d.orig.clone());
                    kept.push((Part::DelayProfiles, d.orig.clone(), d.orig.clone()));
                }
            }
            if let Some(dps) = &mut self.sec.delay_profiles {
                dps.retain(|d| !keep_out.contains(&d.orig));
            }
        }
        for part in Part::APP_WIDE {
            if self.sec.has(part) && self.rs.pick(part, "*", &["change", "keep"]) == "keep" {
                self.sec.clear(part);
                kept.push((part, part.whole().to_string(), "*".into()));
            }
        }
        Ok(kept)
    }

    /// Tags: the ones to create, and a warning for one already in use.
    async fn tags(&mut self) -> R<()> {
        let tags_all = all_tags(&self.sec);
        if !tags_all.is_empty() {
            let have_t: HashMap<String, Value> = self
                .arr
                .tag_list()
                .await?
                .into_iter()
                .map(|t| (t["label"].as_str().unwrap_or("").to_lowercase(), t))
                .collect();
            for t in tags_all {
                if let Some(cur) = have_t.get(&t) {
                    let use_ = self.arr.tag_use(&cur["id"]).await;
                    let (w, can): (Vec<String>, &[&str]) = if use_.is_empty() {
                        (vec![], &[])
                    } else {
                        (
                            vec![format!(
                                "already on {use_}: what the config attaches to this tag applies to those too"
                            )],
                            &["change", "prefix"],
                        )
                    };
                    self.rs.row(Part::Tags, &t, "same", vec!["you have this tag".into()], w, None, can);
                } else {
                    self.rs.row(Part::Tags, &t, "create", vec![], vec![], None, &[]);
                    self.work.tags.push(t);
                }
            }
        }
        Ok(())
    }

    /// Custom formats, and the ids profiles score them by (a new one gets a negative
    /// placeholder until apply creates it).
    async fn custom_formats(&mut self) -> R<CfIds> {
        let mut ids = CfIds::default();
        if self.sec.has(Part::CustomFormats) || self.sec.has(Part::QualityProfiles) {
            for cf in self.arr.cfs().await? {
                ids.add(cf["name"].as_str().unwrap_or(""), cf["id"].clone());
            }
        }
        let have_cf =
            if self.sec.has(Part::CustomFormats) { ArrView::by_name(&self.arr.cfs().await?) } else { HashMap::new() };
        for (i, cf) in self.sec.cfs().iter().enumerate() {
            let body = match self.arr.cf_body(cf).await {
                Ok(b) => b,
                Err(e) if is_config(&e) => {
                    self.rs.row(
                        Part::CustomFormats,
                        &cf.name,
                        "error",
                        vec![e.to_string()],
                        vec![],
                        Some(&cf.orig),
                        &[],
                    );
                    continue;
                }
                Err(e) => return Err(e),
            };
            if let Some(cur) = have_cf.get(&cf.name.to_lowercase()) {
                let diff = ArrView::cf_compare(&body, cur);
                let by = if diff.is_empty() { vec![] } else { self.arr.scored_by(&cur["id"]).await? };
                let w = if by.is_empty() {
                    vec![]
                } else {
                    vec![format!(
                        "scored by your {}{}: {} the changed one",
                        by[..by.len().min(4)].join(", "),
                        if by.len() > 4 { " and more" } else { "" },
                        if by.len() > 1 { "those profiles score" } else { "that profile scores" }
                    )]
                };
                self.rs.row(
                    Part::CustomFormats,
                    &cf.name,
                    ud(&diff),
                    diff.clone(),
                    w,
                    Some(&cf.orig),
                    can_if(&diff, &CHOICES),
                );
                if !diff.is_empty() {
                    self.work.cfs.push((body, Some(cur.clone())));
                }
            } else {
                ids.add(&cf.name, json!(-(i64::try_from(i).unwrap_or(0) + 1)));
                let n = rows(&body["specifications"]).len();
                self.rs.row(
                    Part::CustomFormats,
                    &cf.name,
                    "create",
                    vec![count(n, "condition")],
                    vec![],
                    Some(&cf.orig),
                    &[],
                );
                self.work.cfs.push((body, None));
            }
        }
        Ok(ids)
    }

    /// Quality sizes, which are the whole app's.
    async fn quality_definitions(&mut self) -> R<()> {
        if let Some(want_all) = self.sec.defs() {
            let defs: HashMap<String, Value> =
                self.arr.defs().await?.into_iter().map(|d| (lname(&d["quality"]), d)).collect();
            for (q, want) in want_all {
                let Some(d) = defs.get(&q.to_lowercase()) else {
                    let msg = format!("{} has no quality {:?}", self.arr.name(), q);
                    self.rs.row(Part::QualityDefinitions, q, "error", vec![msg], vec![], Some("*"), &[]);
                    continue;
                };
                let mut new = d.clone();
                let keys = [("min", "minSize"), ("preferred", "preferredSize"), ("max", "maxSize")];
                for (k, api) in keys {
                    if let Some(v) = want.get(k) {
                        new[api] = v.clone();
                    }
                }
                let fmt = |v: Option<&Value>| match v {
                    None | Some(Value::Null) => "no limit".to_string(),
                    Some(v) => number(v),
                };
                let diff: Vec<String> = keys
                    .iter()
                    .filter(|(_, api)| !same_field(d.get(*api), new.get(*api)))
                    .map(|(k, api)| format!("{k} {} → {}", fmt(d.get(*api)), fmt(new.get(*api))))
                    .collect();
                let w = when(&diff, vec![format!("app-wide: every profile in {} uses these sizes", self.arr.name())]);
                let qname = text(&d["quality"]["name"]);
                self.rs.row(
                    Part::QualityDefinitions,
                    &qname,
                    ud(&diff),
                    diff.clone(),
                    w,
                    Some("*"),
                    can_if(&diff, &["change", "keep"]),
                );
                if !diff.is_empty() {
                    self.work.defs.push((new, d.clone()));
                }
            }
        }
        Ok(())
    }

    /// Quality profiles, scoring custom formats by `ids`.
    async fn quality_profiles(&mut self, ids: &CfIds) -> R<()> {
        let profs = if self.sec.has(Part::QualityProfiles) {
            ArrView::by_name(&self.arr.profiles().await?)
        } else {
            HashMap::new()
        };
        for p in self.sec.profiles() {
            let cur = profs.get(&p.name.to_lowercase());
            let base = if cur.is_none() && p.orig != p.name { profs.get(&p.orig.to_lowercase()) } else { None };
            let body = match self.arr.profile_body(p, cur, ids, base).await {
                Ok(b) => b,
                Err(e) if is_config(&e) => {
                    self.rs.row(
                        Part::QualityProfiles,
                        &p.name,
                        "error",
                        vec![e.to_string()],
                        vec![],
                        Some(&p.orig),
                        &[],
                    );
                    continue;
                }
                Err(e) => return Err(e),
            };
            if let Some(cur) = cur {
                let diff = ArrView::profile_compare(&body, cur);
                let mut w = vec![];
                if !diff.is_empty()
                    && builtin(Part::QualityProfiles, &self.arr.kind).contains(&p.name.to_lowercase().as_str())
                {
                    w.push(format!("one of {}'s own profiles", self.arr.c.def().label));
                }
                let use_ = if diff.is_empty() {
                    String::new()
                } else {
                    self.arr.used_by("qualityProfileId", &cur["id"]).await
                };
                if !use_.is_empty() {
                    w.push(use_);
                }
                self.rs.row(
                    Part::QualityProfiles,
                    &p.name,
                    ud(&diff),
                    diff.clone(),
                    w,
                    Some(&p.orig),
                    can_if(&diff, &CHOICES),
                );
                if !diff.is_empty() {
                    self.work.profiles.push((p.clone(), Some(cur.clone())));
                }
            } else {
                let (q, _) = portable_qualities(rows(&body["items"]));
                let scored = rows(&body["formatItems"]).iter().filter(|fi| truthy(&fi["score"])).count();
                let d = vec![
                    format!("{} qualit{}", q.len(), if q.len() != 1 { "ies" } else { "y" }),
                    format!("scores {}", count(scored, "custom format")),
                ];
                self.rs.row(Part::QualityProfiles, &p.name, "create", d, vec![], Some(&p.orig), &[]);
                self.work.profiles.push((p.clone(), None));
            }
        }
        Ok(())
    }

    /// Metadata profiles (Lidarr).
    async fn metadata_profiles(&mut self) -> R<()> {
        if self.sec.has(Part::MetadataProfiles) {
            let have_mp = ArrView::by_name(&self.arr.list("metadataprofile").await?);
            for m in self.sec.mps() {
                let cur = have_mp.get(&m.name.to_lowercase());
                let base = if cur.is_none() && m.orig != m.name { have_mp.get(&m.orig.to_lowercase()) } else { None };
                let body = match self.arr.mp_body(m, cur, base).await {
                    Ok(b) => b,
                    Err(e) if is_config(&e) => {
                        self.rs.row(
                            Part::MetadataProfiles,
                            &m.name,
                            "error",
                            vec![e.to_string()],
                            vec![],
                            Some(&m.orig),
                            &[],
                        );
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let a = ArrView::mp_portable(&body);
                let names = |v: &Value, k: &str| -> Vec<String> { rows(&v[k]).iter().map(text).collect() };
                if let Some(cur) = cur {
                    let b = ArrView::mp_portable(cur);
                    let mut diff = vec![];
                    for (k, _) in super::parse::MP_PARTS {
                        let (mut x, mut y) = (names(&a, k), names(&b, k));
                        let (xs, ys) = (x.join(", "), y.join(", "));
                        x.sort();
                        y.sort();
                        if x != y {
                            let none = |s: String| if s.is_empty() { "none".to_string() } else { s };
                            diff.push(format!("{k}: {} → {}", none(ys), none(xs)));
                        }
                    }
                    let mut w = vec![];
                    if !diff.is_empty()
                        && builtin(Part::MetadataProfiles, &self.arr.kind).contains(&m.name.to_lowercase().as_str())
                    {
                        w.push(format!("one of {}'s own metadata profiles", self.arr.c.def().label));
                    }
                    let use_ = if diff.is_empty() {
                        String::new()
                    } else {
                        self.arr.used_by("metadataProfileId", &cur["id"]).await
                    };
                    if !use_.is_empty() {
                        w.push(use_);
                    }
                    self.rs.row(
                        Part::MetadataProfiles,
                        &m.name,
                        ud(&diff),
                        diff.clone(),
                        w,
                        Some(&m.orig),
                        can_if(&diff, &CHOICES),
                    );
                    if !diff.is_empty() {
                        self.work.mps.push((m.clone(), Some(cur.clone())));
                    }
                } else {
                    let types = names(&a, "primaryAlbumTypes").join(", ");
                    let d = format!("takes {}", if types.is_empty() { "no album types".to_string() } else { types });
                    self.rs.row(Part::MetadataProfiles, &m.name, "create", vec![d], vec![], Some(&m.orig), &[]);
                    self.work.mps.push((m.clone(), None));
                }
            }
        }
        Ok(())
    }

    /// Release profiles.
    async fn release_profiles(&mut self) -> R<()> {
        if self.sec.has(Part::ReleaseProfiles) {
            let have_rp = ArrView::by_name(&self.arr.release_profiles().await?);
            let tag_names = self.arr.tag_names().await?;
            for r in self.sec.rps() {
                if let Some(cur) = have_rp.get(&r.name.to_lowercase()) {
                    let mut old_tags: Vec<String> = rows(&cur["tags"])
                        .iter()
                        .map(|t| tag_names.get(t).and_then(Value::as_str).unwrap_or("").to_string())
                        .collect();
                    old_tags.sort();
                    let mut new_tags = r.tags.clone();
                    new_tags.sort();
                    let list = |v: &Value| if v.is_null() { json!([]) } else { v.clone() };
                    let pairs = [
                        ("enabled", json!(r.enabled), cur.get("enabled").cloned().unwrap_or(Value::Null)),
                        ("required", json!(r.required), list(&cur["required"])),
                        ("ignored", json!(r.ignored), list(&cur["ignored"])),
                        ("tags", json!(new_tags), json!(old_tags)),
                    ];
                    let diff: Vec<String> =
                        pairs.iter().filter(|(_, n, o)| !same_value(n, o)).map(|(k, _, _)| (*k).to_string()).collect();
                    self.rs.row(
                        Part::ReleaseProfiles,
                        &r.name,
                        ud(&diff),
                        diff.clone(),
                        vec![],
                        Some(&r.orig),
                        can_if(&diff, &CHOICES),
                    );
                    if !diff.is_empty() {
                        self.work.rps.push((r.clone(), Some(cur.clone())));
                    }
                } else {
                    let mut d = format!("{} required, {} ignored", r.required.len(), r.ignored.len());
                    if !r.tags.is_empty() {
                        d += &format!(", tags {}", r.tags.join(", "));
                    }
                    let w = if r.tags.is_empty() {
                        vec!["no tags: it applies to everything in the app".to_string()]
                    } else {
                        vec![]
                    };
                    self.rs.row(Part::ReleaseProfiles, &r.name, "create", vec![d], w, Some(&r.orig), &[]);
                    self.work.rps.push((r.clone(), None));
                }
            }
        }
        Ok(())
    }

    /// Delay profiles, matched by their tags.
    async fn delay_profiles(&mut self) -> R<()> {
        if self.sec.has(Part::DelayProfiles) {
            let have_dp = self.arr.delays().await?;
            for d in self.sec.dps() {
                let nm = d.label();
                if let Some((_, cur)) = have_dp.iter().find(|(k, _)| *k == d.tags) {
                    let diff = self.arr.dp_compare(d, cur).await?;
                    let w = if !diff.is_empty() && d.tags.is_empty() {
                        vec!["the default delay profile: it applies to everything without a tagged one".to_string()]
                    } else {
                        vec![]
                    };
                    self.rs.row(
                        Part::DelayProfiles,
                        &nm,
                        ud(&diff),
                        diff.clone(),
                        w,
                        Some(&d.orig),
                        can_if(&diff, &["change", "keep"]),
                    );
                    if !diff.is_empty() {
                        self.work.dps.push((d.clone(), Some(cur.clone())));
                    }
                } else {
                    let detail = dp_detail(&serde_json::to_value(d)?);
                    self.rs.row(Part::DelayProfiles, &nm, "create", vec![detail], vec![], Some(&d.orig), &[]);
                    self.work.dps.push((d.clone(), None));
                }
            }
        }
        Ok(())
    }

    /// Auto tagging rules.
    async fn auto_tags(&mut self) -> R<()> {
        if self.sec.has(Part::AutoTags) {
            let have_at = ArrView::by_name(&self.arr.list("autotagging").await?);
            let more_p: Vec<String> = self.sec.profiles().iter().map(|p| p.name.clone()).collect();
            let more_m: Vec<String> = self.sec.mps().iter().map(|m| m.name.clone()).collect();
            let noun = self.arr.community().map_or("items", |c| c.auto_tag_noun);
            for a in self.sec.ats() {
                match self.arr.at_body(a, None, &more_p, &more_m).await {
                    Ok(_) => {}
                    Err(e) if is_config(&e) => {
                        self.rs.row(Part::AutoTags, &a.name, "error", vec![e.to_string()], vec![], Some(&a.orig), &[]);
                        continue;
                    }
                    Err(e) => return Err(e),
                }
                if let Some(cur) = have_at.get(&a.name.to_lowercase()) {
                    let diff = self.arr.at_compare(a, cur).await?;
                    self.rs.row(
                        Part::AutoTags,
                        &a.name,
                        ud(&diff),
                        diff.clone(),
                        vec![],
                        Some(&a.orig),
                        can_if(&diff, &CHOICES),
                    );
                    if !diff.is_empty() {
                        self.work.ats.push((a.clone(), Some(cur.clone())));
                    }
                } else {
                    let d = format!("{} → tags {}", count(a.specifications.len(), "condition"), a.tags.join(", "));
                    let w = vec![format!("tags the {noun} in your library that match, from the next refresh")];
                    self.rs.row(Part::AutoTags, &a.name, "create", vec![d], w, Some(&a.orig), &[]);
                    self.work.ats.push((a.clone(), None));
                }
            }
        }
        Ok(())
    }

    /// File naming and media management, which are the whole app's.
    async fn settings(&mut self) -> R<()> {
        for (part, path, label, warn) in [
            (
                Part::Naming,
                "config/naming",
                "File naming",
                "app-wide: new imports get these names; files already there keep theirs until you rename them",
            ),
            (Part::MediaManagement, "config/mediamanagement", "Media management", "app-wide"),
        ] {
            let Some(want) = self.sec.settings(part) else { continue };
            let cur = self.arr.r(path).await?;
            if let Some(bad) = want.keys().find(|k| cur.get(*k).is_none()) {
                let msg = format!("{} has no {} setting {:?}", self.arr.name(), label.to_lowercase(), bad);
                self.rs.row(part, label, "error", vec![msg], vec![], Some("*"), &[]);
                continue;
            }
            let diff: Vec<String> = want
                .iter()
                .filter(|(k, v)| !same_value(&cur[k.as_str()], v))
                .map(|(k, v)| format!("{k} {} → {}", short(&cur[k.as_str()]), short(v)))
                .collect();
            self.rs.row(
                part,
                label,
                ud(&diff),
                diff.clone(),
                when(&diff, vec![warn.to_string()]),
                Some("*"),
                can_if(&diff, &["change", "keep"]),
            );
            if !diff.is_empty() {
                let mut new = cur.as_object().cloned().unwrap_or_default();
                for (k, v) in want {
                    new.insert(k.clone(), v.clone());
                }
                let pair = (Value::Object(new), cur.clone());
                if part == Part::Naming {
                    self.work.naming = Some(pair);
                } else {
                    self.work.mm = Some(pair);
                }
            }
        }
        Ok(())
    }

    /// A row for each thing the importer kept as the app has it.
    fn kept(&mut self, kept: Vec<Kept>) {
        for (part, name, o) in kept {
            let can: &[&str] = if Part::APP_WIDE.contains(&part) || part == Part::DelayProfiles {
                &["change", "keep"]
            } else {
                &CHOICES
            };
            self.rs.row(part, &name, "keep", vec!["yours stays as it is".into()], vec![], Some(&o), can);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_details() {
        assert_eq!(dp_detail(&json!({"tags": []})), "usenet, torrent");
        assert_eq!(
            dp_detail(&json!({"tags": [], "usenetDelay": 30, "preferredProtocol": "torrent"})),
            "torrent first · usenet +30 min, torrent"
        );
        assert_eq!(dp_detail(&json!({"enableUsenet": false, "enableTorrent": false})), "nothing allowed");
        assert_eq!(
            dp_detail(&json!({"items": [{"name": "", "protocol": "usenet", "allowed": true, "delay": 5},
                                        {"name": "T", "protocol": "torrent", "allowed": true, "delay": 0}]})),
            "usenet +5 min → T"
        );
    }
}
