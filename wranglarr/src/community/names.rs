// SPDX-License-Identifier: GPL-3.0-or-later
//! Names: copies under a prefix, and what points at what.

use serde_json::Value;

use super::Part;
use super::parse::Section;

/// The prefix as tags allow it: "v2 · " → "v2-".
pub fn slug(prefix: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in prefix.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
    }
    if out.is_empty() { "new-".into() } else { out + "-" }
}

fn dedupe(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(v.len());
    for x in v {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

/// Every tag the section names: its own list, the profiles' and rules' tags, and
/// the tags auto tagging conditions look for.
pub fn all_tags(sec: &Section) -> Vec<String> {
    let mut out: Vec<String> = sec.tags().to_vec();
    for r in sec.rps() {
        out.extend(r.tags.iter().cloned());
    }
    for d in sec.dps() {
        out.extend(d.tags.iter().cloned());
    }
    for a in sec.ats() {
        out.extend(a.tags.iter().cloned());
    }
    for a in sec.ats() {
        for sp in &a.specifications {
            if sp.implementation == "TagSpecification"
                && let Some(Value::Array(v)) = sp.fields.get("value")
            {
                out.extend(v.iter().filter_map(Value::as_str).map(str::to_lowercase));
            }
        }
    }
    dedupe(out)
}

/// Rename one item in a config section, and everything in it that refers to it.
pub fn rename(sec: &mut Section, part: Part, old: &str, new: &str) {
    let lo = old.to_lowercase();
    if part == Part::Tags {
        let swap = |ts: &[String]| -> Vec<String> {
            dedupe(ts.iter().map(|t| if t.to_lowercase() == lo { new.to_string() } else { t.clone() }).collect())
        };
        if let Some(t) = &mut sec.tags {
            *t = swap(t);
        }
        for r in sec.release_profiles.iter_mut().flatten() {
            r.tags = swap(&r.tags);
        }
        for d in sec.delay_profiles.iter_mut().flatten() {
            d.tags = swap(&d.tags);
            d.tags.sort();
        }
        for a in sec.auto_tags.iter_mut().flatten() {
            a.tags = swap(&a.tags);
            for sp in &mut a.specifications {
                if sp.implementation == "TagSpecification"
                    && let Some(Value::Array(v)) = sp.fields.get_mut("value")
                {
                    for t in v.iter_mut() {
                        if t.as_str().is_some_and(|s| s.to_lowercase() == lo) {
                            *t = Value::String(new.to_string());
                        }
                    }
                }
            }
        }
        return;
    }
    for x in sec.named_mut(part) {
        if x.name().to_lowercase() == lo {
            x.set_name(new.to_string());
        }
    }
    if part == Part::CustomFormats {
        for p in sec.quality_profiles.iter_mut().flatten() {
            p.format_scores = std::mem::take(&mut p.format_scores)
                .into_iter()
                .map(|(k, v)| if k.to_lowercase() == lo { (new.to_string(), v) } else { (k, v) })
                .collect();
        }
    }
    let r = match part {
        Part::QualityProfiles => "QualityProfileSpecification",
        Part::MetadataProfiles => "MetadataProfileSpecification",
        _ => return,
    };
    for a in sec.auto_tags.iter_mut().flatten() {
        for sp in &mut a.specifications {
            if sp.implementation == r
                && let Some(v) = sp.fields.get_mut("value")
                && v.as_str().is_some_and(|s| s.to_lowercase() == lo)
            {
                *v = Value::String(new.to_string());
            }
        }
    }
}

/// [`rename`] on a section still in its JSON form (export builds those).
pub fn rename_value(sec: &mut serde_json::Map<String, Value>, part: Part, old: &str, new: &str) {
    let lo = old.to_lowercase();
    let hit = |t: &Value| t.as_str().is_some_and(|s| s.to_lowercase() == lo);
    let swap = |ts: &mut Value| {
        if let Value::Array(a) = ts {
            let mut out: Vec<Value> = vec![];
            for t in a.drain(..) {
                let t = if hit(&t) { Value::String(new.to_string()) } else { t };
                if !out.contains(&t) {
                    out.push(t);
                }
            }
            *a = out;
        }
    };
    if part == Part::Tags {
        if let Some(t) = sec.get_mut("tags") {
            swap(t);
        }
        for p in ["releaseProfiles", "delayProfiles", "autoTags"] {
            for x in sec.get_mut(p).and_then(Value::as_array_mut).into_iter().flatten() {
                if let Some(t) = x.get_mut("tags") {
                    swap(t);
                    if p == "delayProfiles"
                        && let Value::Array(a) = t
                    {
                        a.sort_by_key(|v| v.as_str().unwrap_or("").to_string());
                    }
                }
                if p == "autoTags" {
                    for sp in x.get_mut("specifications").and_then(Value::as_array_mut).into_iter().flatten() {
                        if sp["implementation"] == "TagSpecification"
                            && let Some(Value::Array(v)) = sp.pointer_mut("/fields/value")
                        {
                            for t in v.iter_mut() {
                                if hit(t) {
                                    *t = Value::String(new.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        return;
    }
    for x in sec.get_mut(part.key()).and_then(Value::as_array_mut).into_iter().flatten() {
        if hit(&x["name"]) {
            x["name"] = Value::String(new.to_string());
        }
    }
    if part == Part::CustomFormats {
        for p in sec.get_mut("qualityProfiles").and_then(Value::as_array_mut).into_iter().flatten() {
            if let Some(Value::Object(fs)) = p.get_mut("formatScores") {
                *fs = std::mem::take(fs)
                    .into_iter()
                    .map(|(k, v)| if k.to_lowercase() == lo { (new.to_string(), v) } else { (k, v) })
                    .collect();
            }
        }
    }
    let r = match part {
        Part::QualityProfiles => "QualityProfileSpecification",
        Part::MetadataProfiles => "MetadataProfileSpecification",
        _ => return,
    };
    for a in sec.get_mut("autoTags").and_then(Value::as_array_mut).into_iter().flatten() {
        for sp in a.get_mut("specifications").and_then(Value::as_array_mut).into_iter().flatten() {
            if sp["implementation"] == r
                && let Some(v) = sp.pointer_mut("/fields/value")
                && hit(v)
            {
                *v = Value::String(new.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::parse::parse;
    use serde_json::json;

    #[test]
    fn slugs() {
        assert_eq!(slug("v2 · "), "v2-");
        assert_eq!(slug("My Copy!"), "my-copy-");
        assert_eq!(slug(" · "), "new-");
    }

    #[test]
    fn renames_follow() {
        let c = parse(Some(
            &json!({"apps": {"radarr": {
                "tags": ["anime"],
                "customFormats": [{"name": "CF", "specifications": [{"name": "s", "implementation": "X"}]}],
                "qualityProfiles": [{"name": "HD", "formatScores": {"cf": 10}}],
                "delayProfiles": [{"tags": ["zz", "anime"]}],
                "autoTags": [{"name": "A", "tags": ["anime"], "specifications": [
                    {"name": "p", "implementation": "QualityProfileSpecification", "fields": {"value": "hd"}},
                    {"name": "t", "implementation": "TagSpecification", "fields": {"value": ["Anime"]}}]}]}}})
            .to_string(),
        ))
        .expect("parse");
        let mut sec = c.apps["radarr"].clone();
        assert_eq!(all_tags(&sec), ["anime", "zz"]);
        rename(&mut sec, Part::CustomFormats, "CF", "v2 · CF");
        assert_eq!(sec.cfs()[0].name, "v2 · CF");
        assert_eq!(sec.profiles()[0].format_scores.keys().collect::<Vec<_>>(), ["v2 · CF"]);
        rename(&mut sec, Part::QualityProfiles, "HD", "v2 · HD");
        assert_eq!(sec.ats()[0].specifications[0].fields["value"], "v2 · HD");
        rename(&mut sec, Part::Tags, "anime", "v2-anime");
        assert_eq!(sec.tags(), ["v2-anime"]);
        assert_eq!(sec.dps()[0].tags, ["v2-anime", "zz"]);
        assert_eq!(sec.ats()[0].tags, ["v2-anime"]);
        assert_eq!(sec.ats()[0].specifications[1].fields["value"], json!(["v2-anime"]));
    }
}
