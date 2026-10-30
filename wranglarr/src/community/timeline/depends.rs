// SPDX-License-Identifier: GPL-3.0-or-later
//! What depends on what: working out a removal, and taking a mark out.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::super::Part;
use super::super::arr_view::rows;
use super::super::values::same_value;
use super::{EXTERNAL, Key, Kind, Shape, Snap, Timeline, fielded, part_of, same, shape, tk};
use crate::value::text;

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

pub(super) fn lname_s(v: &Value) -> String {
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

pub(super) fn track_name(p: &[String]) -> String {
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

#[cfg(test)]
mod tests {
    use super::super::service::writes;
    use super::super::tests::{base, p, sec};
    use super::*;

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
}
