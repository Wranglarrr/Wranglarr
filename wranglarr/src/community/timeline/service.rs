// SPDX-License-Identifier: GPL-3.0-or-later
//! The service side: protected rows, what a removal writes and deletes, and the
//! [`Community`] methods that read, record and remove.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::super::arr_view::{ArrView, lname, rows};
use super::super::parse::{Config, cerr, dp_name};
use super::super::{Community, FORMAT, Part, builtin};
use super::depends::{Removal, lname_s, track_name};
use super::describe::grouped;
use super::{Key, Kind, Shape, Snap, Timeline, change, fielded, load, part_of, same, shape, tk};
use crate::app::App;
use crate::config::AppConfig;
use crate::error::{Error, Result as R};
use crate::value::text;

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
pub(super) fn writes(r: &Removal, snap: &Snap, notes: &mut Vec<String>) -> (Map<String, Value>, Vec<Vec<String>>) {
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
    done: &mut Vec<super::super::Change>,
    journal: &super::super::Journal<'_>,
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
    let mut item = super::super::Change::replaced(res, &current, track_name(p));
    item.deleted = true;
    super::super::record(done, Some(journal), item).await?;
    match arr.c.delete(&format!("{res}/{}", super::super::arr_view::id_str(&id))).await {
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
    pub(in crate::community) async fn catch_up(&self, app: &App, a: &AppConfig) -> R<Snap> {
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
    pub(in crate::community) async fn record_install(
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
    pub(in crate::community) async fn forget_install(&self, app: &App, a: &str, hid: &str) -> R<()> {
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
            let parsed = super::super::parse::parse(Some(&cfg.to_string()))?;
            let section = &parsed.apps[&a.kind];
            let (rows_, work) =
                super::super::plan::plan_one(section, &mut arr, &a.name, None, super::super::DEFAULT_PREFIX).await?;
            if let Some(bad) = rows_.iter().find(|r| r["action"] == "error") {
                return Err(cerr(format!("can't put {} back: {}", text(&bad["name"]), text(&bad["details"][0]))));
            }
            Some(work)
        } else {
            None
        };
        let hid = self.remember(app, &format!("Removing {label}"), &a.name, vec![], true).await?;
        let journal = super::super::Journal { community: self, app, hid: &hid };
        let mut done = vec![];
        if let Some(work) = work {
            super::super::apply_one(&mut arr, work, &mut done, Some(&journal)).await?;
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
    use super::super::Mark;
    use super::super::tests::{base, p, sec};
    use super::*;
    use crate::community::parse::Source;
    use crate::time::now_iso_z as now;

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
}
