// SPDX-License-Identifier: GPL-3.0-or-later
//! Describing marks and effects: one line per changed track, grouped by item.

use serde_json::{Value, json};

use super::super::Part;
use super::super::values::{number, short};
use super::{Key, Kind, Shape, Timeline, part_of, shape};

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
pub(super) fn grouped(
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
