// SPDX-License-Identifier: GPL-3.0-or-later
//! Export (a config made from your apps), the inventory it picks from, and
//! "try a release name".

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::arr_view::{ArrView, VDict, cf_portable, flat, lname, portable_qualities, rows};
use super::names::rename_value;
use super::parse::{dp_name, s as checked_text, tag};
use super::plan::dp_detail;
use super::values::{number, same_value, short};
use super::{Community, FORMAT, Part, builtin, count, media_field, parsed_info};
use crate::app::App;
use crate::error::{Error, Result as R};
use crate::value::{text, truthy};

/// An arr error inside one part of the inventory leaves that part out (None, `null`
/// in the answer: the app has no such part, or didn't give it); an empty list is a
/// part the app has with nothing in it yet.
fn soft(r: R<Vec<Value>>) -> R<Option<Vec<Value>>> {
    match r {
        Err(Error::Upstream(_)) => Ok(None),
        other => other.map(Some),
    }
}

fn labels(v: &Value) -> Vec<String> {
    rows(v).iter().map(text).collect()
}

/// Make it an optional extra when the exporter said so: want["extras"][part][name] = {default, note}.
fn mark(want: &Map<String, Value>, part: Part, mut entry: Value) -> Value {
    let marks =
        want.get("extras").and_then(Value::as_object).and_then(|x| x.get(part.key())).and_then(Value::as_object);
    let lo = lname(&entry);
    let x = marks.and_then(|m| {
        // a later key that lower-cases the same wins, like the dict comprehension
        m.iter().filter(|(k, _)| k.to_lowercase() == lo).map(|(_, v)| v).next_back()
    });
    if let Some(Value::Object(x)) = x {
        let default = x.get("default").is_none_or(truthy);
        let note: String =
            x.get("note").filter(|n| truthy(n)).map(text).unwrap_or_default().chars().take(300).collect();
        entry["extra"] = json!({"default": default, "note": note});
    }
    entry
}

/// Add an entry to a part unless one with the same name (tags, for a delay profile) is there.
fn add(sec: &mut Map<String, Value>, part: &str, entry: Value) {
    let ident = |e: &Value| if part == "delayProfiles" { e["tags"].to_string() } else { lname(e) };
    let list = sec.entry(part.to_string()).or_insert_with(|| json!([]));
    if let Value::Array(rs) = list {
        let id = ident(&entry);
        if !rs.iter().any(|r| ident(r) == id) {
            rs.push(entry);
        }
    }
}

fn tagged(tags: &VDict, ids: &Value) -> String {
    let ids = rows(ids);
    if ids.is_empty() {
        return String::new();
    }
    let names: Vec<String> = ids.iter().map(|i| tags.get(i).map_or_else(|| "?".to_string(), text)).collect();
    format!(" · tags {}", names.join(", "))
}

impl Community {
    /// What the app has, part by part, for export.
    pub async fn inventory(&self, app: &App, name: &str) -> R<Value> {
        let a = Self::app(app, name)?;
        let mut arr = ArrView::new(Self::client(&a).await?);
        let kind = a.kind.as_str();
        let is_builtin = |part: Part, n: &Value| builtin(part, kind).contains(&text(n).to_lowercase().as_str());
        let cfs = arr.cfs().await?;
        let mut cfn = VDict::default();
        for cf in &cfs {
            cfn.set(cf["id"].clone(), cf["name"].clone());
        }
        let tags = match arr.tag_names().await {
            Ok(t) => t,
            Err(Error::Upstream(_)) => VDict::default(),
            Err(e) => return Err(e),
        };
        let custom: Vec<Value> = cfs
            .iter()
            .map(|cf| json!({"name": cf["name"], "detail": count(rows(&cf["specifications"]).len(), "condition")}))
            .collect();
        let profiles: Vec<Value> = arr
            .profiles()
            .await?
            .iter()
            .map(|p| {
                let scored: Vec<&Value> = rows(&p["formatItems"]).iter().filter(|fi| truthy(&fi["score"])).collect();
                let formats: Vec<Value> = scored
                    .iter()
                    .map(|fi| cfn.get(&fi["format"]).cloned().unwrap_or_else(|| fi["name"].clone()))
                    .collect();
                json!({"name": p["name"], "builtin": is_builtin(Part::QualityProfiles, &p["name"]), "formats": formats,
                       "detail": count(scored.len(), "scored format")})
            })
            .collect();
        let mps = if arr.has_metadata_profiles() {
            soft(
                async {
                    Ok(arr
                        .list("metadataprofile")
                        .await?
                        .iter()
                        .map(|m| {
                            let types = labels(&ArrView::mp_portable(m)["primaryAlbumTypes"]).join(", ");
                            json!({"name": m["name"], "builtin": is_builtin(Part::MetadataProfiles, &m["name"]),
                               "detail": if types.is_empty() { "no album types".to_string() } else { types }})
                        })
                        .collect())
                }
                .await,
            )?
        } else {
            None // this kind has no metadata profiles
        };
        let defs: Vec<Value> = arr
            .defs()
            .await?
            .iter()
            .map(|d| {
                let parts: Vec<String> = ["minSize", "preferredSize", "maxSize"]
                    .iter()
                    .map(|k| match d.get(*k) {
                        None | Some(Value::Null) => "–".to_string(),
                        Some(v) => number(v),
                    })
                    .collect();
                // the numbers too (MB per minute; null = no limit), for the size table
                json!({"name": d["quality"]["name"], "detail": parts.join(" / "),
                       "min": d.get("minSize"), "preferred": d.get("preferredSize"), "max": d.get("maxSize")})
            })
            .collect();
        let rps = soft(
            async {
                Ok(arr
                    .release_profiles()
                    .await?
                    .iter()
                    .map(|r| {
                        let name =
                            if truthy(&r["name"]) { r["name"].clone() } else { json!(format!("#{}", text(&r["id"]))) };
                        let detail = format!(
                            "{} required, {} ignored{}",
                            rows(&r["required"]).len(),
                            rows(&r["ignored"]).len(),
                            tagged(&tags, &r["tags"])
                        );
                        json!({"name": name, "detail": detail})
                    })
                    .collect())
            }
            .await,
        )?;
        let dps = soft(
            async {
                let names = arr.tag_names().await?;
                Ok(arr
                    .list("delayprofile")
                    .await?
                    .iter()
                    .map(|d| {
                        let p = ArrView::dp_portable_with(&names, d);
                        let t = labels(&p["tags"]);
                        json!({"name": dp_name(&t), "detail": dp_detail(&p), "appWide": t.is_empty()})
                    })
                    .collect())
            }
            .await,
        )?;
        let ats = soft(
            async {
                Ok(arr
                    .list("autotagging")
                    .await?
                    .iter()
                    .map(|a| {
                        let d = format!(
                            "{}{}",
                            count(rows(&a["specifications"]).len(), "condition"),
                            tagged(&tags, &a["tags"])
                        );
                        json!({"name": a["name"], "detail": d})
                    })
                    .collect())
            }
            .await,
        )?;
        let tag_rows = soft(
            async {
                let mut out = vec![];
                for t in arr.tag_list().await? {
                    let u = arr.tag_use(&t["id"]).await;
                    out.push(
                        json!({"name": t["label"], "detail": if u.is_empty() { "not used".to_string() } else { u }}),
                    );
                }
                Ok(out)
            }
            .await,
        )?;
        let settings = |v: R<Value>, only_media: bool| -> R<Option<Vec<Value>>> {
            soft(v.map(|v| {
                v.as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(k, _)| if only_media { media_field(k) } else { *k != "id" })
                    .map(|(k, v)| json!({"name": k, "detail": short(v)}))
                    .collect()
            }))
        };
        let naming = settings(arr.r("config/naming").await, false)?;
        let mm = settings(arr.r("config/mediamanagement").await, true)?;
        let app_wide: Vec<&str> = Part::APP_WIDE.iter().map(|p| p.key()).collect();
        Ok(json!({
            "app": a.name, "kind": a.kind, "appWide": app_wide,
            "customFormats": custom, "qualityProfiles": profiles, "metadataProfiles": mps,
            "qualityDefinitions": defs, "releaseProfiles": rps, "delayProfiles": dps, "autoTags": ats,
            "tags": tag_rows, "naming": naming, "mediaManagement": mm,
        }))
    }

    /// A config made from your apps; changes nothing.
    pub async fn export(&self, app: &App, body: &Map<String, Value>) -> R<Value> {
        let or = |k: &str, d: &str| -> Value { body.get(k).filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!(d)) };
        let mut out = json!({
            "wranglarr": FORMAT,
            "name": checked_text(Some(&or("name", "My config")), "name", 100, false)?,
            "description": checked_text(Some(&or("description", "")), "description", 4000, true)?,
            "author": checked_text(Some(&or("author", "")), "author", 100, true)?,
        });
        if let Some(src) = super::parse::source(body.get("source"))? {
            out["source"] = json!(src);
        }
        let picks = match body.get("apps") {
            Some(Value::Object(p)) if !p.is_empty() => p,
            _ => return Err(super::parse::cerr("pick something to export")),
        };
        let mut notes: Vec<String> = vec![];
        let mut apps: Map<String, Value> = Map::new();
        for (name, want) in picks {
            let a = Self::app(app, name)?;
            let Some(want) = want.as_object() else {
                return Err(super::parse::cerr(format!("{name}: expected the parts to export")));
            };
            let mut arr = ArrView::new(Self::client(&a).await?);
            let part = export_one(&mut arr, want, &mut notes).await?;
            let Value::Object(sec) = apps.entry(a.kind.clone()).or_insert_with(|| json!({})) else { continue };
            for (k, v) in part {
                if k == "tags" {
                    let mut all = labels(sec.get("tags").unwrap_or(&Value::Null));
                    for t in labels(&v) {
                        if !all.contains(&t) {
                            all.push(t);
                        }
                    }
                    sec.insert(k, json!(all));
                } else if let Value::Object(m) = v {
                    if let Value::Object(have) = sec.entry(k).or_insert_with(|| json!({})) {
                        have.extend(m);
                    }
                } else {
                    for e in rows(&v) {
                        add(sec, &k, e.clone());
                    }
                }
            }
        }
        let mut cleaned = Map::new();
        for (k, v) in apps {
            let mut sec = Map::new();
            for p in Part::ALL {
                if let Some(x) = v.get(p.key()).filter(|x| truthy(x)) {
                    sec.insert(p.key().into(), x.clone());
                }
            }
            if !sec.is_empty() {
                cleaned.insert(k, Value::Object(sec));
            }
        }
        if cleaned.is_empty() {
            return Err(super::parse::cerr("nothing picked"));
        }
        out["apps"] = Value::Object(cleaned);
        // what we hand out must read back in
        super::parse::parse(Some(&out.to_string()))?;
        Ok(json!({"config": out, "notes": notes}))
    }

    /// How the app parses a release name, and what each profile would score it.
    pub async fn try_title(&self, app: &App, name: &str, title: Option<&Value>) -> R<Value> {
        let title = checked_text(title, "release name", 500, false)?;
        let a = Self::app(app, name)?;
        let c = Self::client(&a).await?;
        let r = c.get(&format!("parse?title={}", encode(&title))).await?;
        let profiles = c.get("qualityprofile").await?;
        let info = parsed_info().map(|k| &r[k]).find(|v| truthy(v)).cloned().unwrap_or(json!({}));
        let q = info["quality"]["quality"].get("name").cloned().unwrap_or(Value::Null);
        let matched: Vec<&Value> = rows(&r["customFormats"]).iter().map(|cf| &cf["id"]).collect();
        let langs = if truthy(&r["languages"]) { &r["languages"] } else { &info["languages"] };
        let profiles: Vec<Value> = rows(&profiles)
            .iter()
            .map(|p| {
                let score: f64 = rows(&p["formatItems"])
                    .iter()
                    .filter(|fi| matched.iter().any(|m| same_value(m, &fi["format"])))
                    .filter_map(|fi| fi["score"].as_f64())
                    .sum();
                let allowed = truthy(&q) && {
                    let on: Vec<Value> = rows(&p["items"]).iter().filter(|i| truthy(&i["allowed"])).cloned().collect();
                    flat(&on).iter().any(|x| x["name"] == q)
                };
                let score = if score.fract() == 0.0 && score.abs() < 9e15 { json!(score as i64) } else { json!(score) };
                json!({"name": p["name"], "score": score, "min": p.get("minFormatScore").cloned().unwrap_or(json!(0)),
                       "allowed": allowed})
            })
            .collect();
        Ok(json!({"quality": q, "languages": rows(langs).iter().map(|l| &l["name"]).collect::<Vec<_>>(),
                  "formats": rows(&r["customFormats"]).iter().map(|cf| &cf["name"]).collect::<Vec<_>>(),
                  "profiles": profiles}))
    }
}

/// A query value, percent-encoded.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `*` in a part's list picks everything in it.
pub(super) const ALL: &str = "*";

fn has(pick: &std::collections::HashMap<Part, HashSet<String>>, p: Part, name: &str) -> bool {
    let s = &pick[&p];
    s.contains(ALL) || s.contains(name)
}

fn lower_set(v: Option<&Value>) -> HashSet<String> {
    rows(v.unwrap_or(&Value::Null)).iter().filter_map(Value::as_str).map(str::to_lowercase).collect()
}

/// One app's part of an export. `raw` (for the timeline's snapshots) keeps what a
/// shared config can't hold, such as root folder conditions.
pub(super) async fn export_one(
    arr: &mut ArrView,
    want: &Map<String, Value>,
    notes: &mut Vec<String>,
) -> R<Map<String, Value>> {
    let mut pick: std::collections::HashMap<Part, HashSet<String>> =
        Part::ALL.iter().map(|p| (*p, lower_set(want.get(p.key())))).collect();
    let picked = |pick: &std::collections::HashMap<Part, HashSet<String>>, p: Part| !pick[&p].is_empty();
    let raw = want.get("raw").is_some_and(truthy);
    let mut sec = Map::new();
    let mut cfn = VDict::default();
    if picked(&pick, Part::CustomFormats) || picked(&pick, Part::QualityProfiles) {
        for cf in arr.cfs().await? {
            cfn.set(cf["id"].clone(), cf["name"].clone());
        }
    }
    if picked(&pick, Part::QualityProfiles) {
        for p in arr.profiles().await? {
            if !has(&pick, Part::QualityProfiles, &lname(&p)) {
                continue;
            }
            let (quals, names) = portable_qualities(rows(&p["items"]));
            let mut entry = json!({"name": p["name"], "upgradeAllowed": truthy(&p["upgradeAllowed"]),
                                   "cutoff": names.get(&p["cutoff"]).cloned().unwrap_or(Value::Null), "qualities": quals});
            for k in ["minFormatScore", "cutoffFormatScore", "minUpgradeFormatScore"] {
                if let Some(v) = p.get(k) {
                    entry[k] = v.clone();
                }
            }
            if truthy(&p["language"]) {
                entry["language"] = p["language"]["name"].clone();
            }
            let mut scores = Map::new();
            for fi in rows(&p["formatItems"]).iter().filter(|fi| truthy(&fi["score"])) {
                let n = cfn.get(&fi["format"]).cloned().unwrap_or_else(|| fi["name"].clone());
                scores.insert(text(&n), fi["score"].clone());
            }
            if truthy(want.get("withFormats").unwrap_or(&Value::Null)) {
                let more: Vec<String> = scores.keys().map(|k| k.to_lowercase()).collect();
                pick.entry(Part::CustomFormats).or_default().extend(more);
            }
            entry["formatScores"] = Value::Object(scores);
            add(&mut sec, "qualityProfiles", mark(want, Part::QualityProfiles, entry));
        }
    }
    if picked(&pick, Part::CustomFormats) {
        for cf in arr.cfs().await? {
            if has(&pick, Part::CustomFormats, &lname(&cf)) {
                add(&mut sec, "customFormats", mark(want, Part::CustomFormats, cf_portable(&cf)));
            }
        }
    }
    if picked(&pick, Part::QualityDefinitions) {
        let mut d = Map::new();
        for q in arr.defs().await? {
            if has(&pick, Part::QualityDefinitions, &lname(&q["quality"])) {
                let min = q.get("minSize").filter(|v| truthy(v)).cloned().unwrap_or(json!(0));
                d.insert(
                    text(&q["quality"]["name"]),
                    json!({"min": min, "preferred": q.get("preferredSize").cloned().unwrap_or(Value::Null),
                           "max": q.get("maxSize").cloned().unwrap_or(Value::Null)}),
                );
            }
        }
        sec.insert("qualityDefinitions".into(), Value::Object(d));
    }
    if picked(&pick, Part::MetadataProfiles) {
        for m in arr.list("metadataprofile").await? {
            if has(&pick, Part::MetadataProfiles, &lname(&m)) {
                add(&mut sec, "metadataProfiles", mark(want, Part::MetadataProfiles, ArrView::mp_portable(&m)));
            }
        }
    }
    if picked(&pick, Part::ReleaseProfiles) {
        let tags = arr.tag_names().await?;
        for r in arr.release_profiles().await? {
            let id = text(&r["id"]);
            let key = if truthy(&r["name"]) { text(&r["name"]) } else { format!("#{id}") };
            if !has(&pick, Part::ReleaseProfiles, &key.to_lowercase()) {
                continue;
            }
            let name = if truthy(&r["name"]) { text(&r["name"]) } else { format!("Release profile {id}") };
            let t: Vec<Value> = rows(&r["tags"]).iter().filter_map(|i| tags.get(i).cloned()).collect();
            let entry = json!({"name": name, "enabled": r.get("enabled").is_none_or(truthy),
                               "required": rows(&r["required"]), "ignored": rows(&r["ignored"]), "tags": t});
            add(&mut sec, "releaseProfiles", mark(want, Part::ReleaseProfiles, entry));
        }
    }
    if picked(&pick, Part::DelayProfiles) {
        let names = arr.tag_names().await?;
        for d in arr.list("delayprofile").await? {
            let pd = ArrView::dp_portable_with(&names, &d);
            if has(&pick, Part::DelayProfiles, &dp_name(&labels(&pd["tags"])).to_lowercase()) {
                add(&mut sec, "delayProfiles", pd);
            }
        }
    }
    if picked(&pick, Part::AutoTags) {
        for a in arr.list("autotagging").await? {
            if !has(&pick, Part::AutoTags, &lname(&a)) {
                continue;
            }
            let mut pa = arr.at_portable(&a).await?;
            let is_root = |s: &Value| s["implementation"] == "RootFolderSpecification";
            let roots: Vec<String> =
                rows(&pa["specifications"]).iter().filter(|s| is_root(s)).map(|s| text(&s["name"])).collect();
            let an = text(&a["name"]);
            if !roots.is_empty() && !raw {
                let keep: Vec<Value> = rows(&pa["specifications"]).iter().filter(|s| !is_root(s)).cloned().collect();
                pa["specifications"] = Value::Array(keep);
                notes.push(format!(
                    "{an}: left out the root folder condition{} {} (a path on this server)",
                    if roots.len() > 1 { "s" } else { "" },
                    roots.join(", ")
                ));
            }
            if rows(&pa["specifications"]).is_empty() && !raw {
                notes.push(format!("{an}: left out, it has no other conditions"));
                continue;
            }
            add(&mut sec, "autoTags", mark(want, Part::AutoTags, pa));
        }
    }
    if picked(&pick, Part::Tags) {
        let t: Vec<Value> = arr
            .tag_list()
            .await?
            .iter()
            .filter(|t| has(&pick, Part::Tags, &t["label"].as_str().unwrap_or("").to_lowercase()))
            .map(|t| t["label"].clone())
            .collect();
        sec.insert("tags".into(), Value::Array(t));
    }
    for (part, path) in [(Part::Naming, "config/naming"), (Part::MediaManagement, "config/mediamanagement")] {
        if !picked(&pick, part) {
            continue;
        }
        let cur = arr.r(path).await?;
        let m: Map<String, Value> = cur
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, _)| {
                (if part == Part::Naming { *k != "id" } else { media_field(k) }) && has(&pick, part, &k.to_lowercase())
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        sec.insert(part.key().into(), Value::Object(m));
    }
    // only what the config can stand on (a snapshot keeps every score)
    let have_cf: HashSet<String> = rows(sec.get("customFormats").unwrap_or(&Value::Null)).iter().map(lname).collect();
    let profiles = if raw { None } else { sec.get_mut("qualityProfiles").and_then(Value::as_array_mut) };
    for p in profiles.into_iter().flatten() {
        let Some(Value::Object(fs)) = p.get_mut("formatScores") else { continue };
        let gone: Vec<String> = fs.keys().filter(|k| !have_cf.contains(&k.to_lowercase())).cloned().collect();
        if gone.is_empty() {
            continue;
        }
        fs.retain(|k, _| have_cf.contains(&k.to_lowercase()));
        let many = gone.len() > 1;
        notes.push(format!(
            "{}: left out the score{} for {}{} (custom format{} not picked)",
            text(&p["name"]),
            if many { "s" } else { "" },
            gone[..gone.len().min(5)].join(", "),
            if gone.len() > 5 { " and more" } else { "" },
            if many { "s" } else { "" }
        ));
    }
    let have_p: HashSet<String> = rows(sec.get("qualityProfiles").unwrap_or(&Value::Null)).iter().map(lname).collect();
    for a in rows(sec.get("autoTags").unwrap_or(&Value::Null)) {
        for sp in rows(&a["specifications"]) {
            if sp["implementation"] == "QualityProfileSpecification"
                && let Some(v) = sp["fields"]["value"].as_str()
                && !have_p.contains(&v.to_lowercase())
            {
                notes.push(format!(
                    "{}: needs a quality profile called {:?} wherever it's imported",
                    text(&a["name"]),
                    v
                ));
            }
        }
    }
    // new names
    if let Some(ren) = want.get("renames").and_then(Value::as_object) {
        for part in Part::NAMED.into_iter().chain([Part::Tags]) {
            let Some(m) = ren.get(part.key()).and_then(Value::as_object) else { continue };
            for (old, new) in m {
                let Some(n) = new.as_str() else { continue };
                if n.trim().is_empty() || n.trim() == old {
                    continue;
                }
                let n = if part == Part::Tags {
                    tag(Some(new), &format!("new name for tag {old}"))?
                } else {
                    checked_text(Some(new), &format!("new name for {old}"), 100, false)?
                };
                rename_value(&mut sec, part, old, &n);
            }
        }
    }
    Ok(sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_and_adds() {
        let want = json!({"extras": {"customFormats": {"X265": {"default": false, "note": "n"}}}});
        let want = want.as_object().cloned().unwrap_or_default();
        let e = mark(&want, Part::CustomFormats, json!({"name": "x265"}));
        assert_eq!(e["extra"], json!({"default": false, "note": "n"}));
        let e = mark(&want, Part::QualityProfiles, json!({"name": "x265"}));
        assert!(e.get("extra").is_none());
        let mut sec = Map::new();
        add(&mut sec, "customFormats", json!({"name": "A"}));
        add(&mut sec, "customFormats", json!({"name": "a"}));
        add(&mut sec, "delayProfiles", json!({"tags": ["x"]}));
        add(&mut sec, "delayProfiles", json!({"tags": ["x"]}));
        assert_eq!(rows(&sec["customFormats"]).len(), 1);
        assert_eq!(rows(&sec["delayProfiles"]).len(), 1);
        assert_eq!(encode("A b.c/é"), "A%20b.c%2F%C3%A9");
    }
}
