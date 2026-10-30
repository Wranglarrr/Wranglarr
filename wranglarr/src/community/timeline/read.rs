// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the timeline: what the app has drifted from, and recording drift and installs.

use std::collections::{BTreeMap, HashMap};

use super::super::parse::Source;
use super::{BEFORE, EXTERNAL, Key, Kind, Mark, Shape, Snap, Timeline, same, shape, tk};
use crate::time::now_iso_z as now;

impl Timeline {
    pub(super) fn new(kind: &str) -> Timeline {
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

    pub(super) fn pos(&self, id: &str) -> Option<usize> {
        self.marks.iter().position(|m| m.id == id)
    }

    /// The last keyframe on a track among the marks before `upto`.
    pub(super) fn last_before(&self, upto: usize, k: &str) -> Option<&Key> {
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
    pub(super) fn made_here(&self, item: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tests::{base, p, sec};
    use super::super::{Part, part_of};
    use super::*;

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
}
