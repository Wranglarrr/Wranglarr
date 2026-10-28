// SPDX-License-Identifier: GPL-3.0-or-later
//! History and undo. Kept under the state db key `community:history`, newest
//! first: a list of [`Entry`].

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::arr_view::{id_str, rows};
use super::{Community, HISTORY_KEEP, KEY_HISTORY};
use crate::app::App;
use crate::error::{Error, Result as R};
use crate::ids::token_hex;
use crate::time::now_iso_z;

/// One apply: what it changed in one app, newest first in the history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub at: String,
    /// The config applied.
    #[serde(default)]
    pub config: String,
    /// The app it was applied to.
    #[serde(default)]
    pub app: String,
    /// The changes, in the order they were made (undo goes backwards).
    #[serde(default)]
    pub items: Vec<Change>,
    /// The apply stopped partway.
    #[serde(default)]
    pub partial: bool,
    /// When it was undone.
    #[serde(default)]
    pub undone: Option<String>,
    /// What the last undo said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_notes: Option<Vec<String>>,
}

/// One thing an apply created or replaced in the app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The app's API resource: `customformat`, `qualityprofile`, `tag`, …
    pub res: String,
    /// Its id in the app (null for the quality definitions, which are many).
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub name: String,
    /// The app's copy from before the apply, as the app sent it; null for
    /// something the apply created. For the quality definitions, the list of them.
    #[serde(default)]
    pub before: Value,
    /// A deletion attempted during timeline removal; recreating it may change its ID.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

impl Change {
    /// Something the apply made.
    pub fn created(res: &str, id: &Value, name: impl Into<String>) -> Change {
        Change { res: res.into(), id: id.clone(), name: name.into(), before: Value::Null, deleted: false }
    }

    /// Something the apply changed, with the app's copy from before.
    pub fn replaced(res: &str, before: &Value, name: impl Into<String>) -> Change {
        Change { res: res.into(), id: before["id"].clone(), name: name.into(), before: before.clone(), deleted: false }
    }

    fn was_created(&self) -> bool {
        self.before.is_null()
    }
}

/// An entry as the page lists it: the changes without what they replaced.
#[derive(Debug, Serialize)]
pub struct Listed<'a> {
    id: &'a str,
    at: &'a str,
    config: &'a str,
    app: &'a str,
    partial: bool,
    undone: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    undo_notes: &'a Option<Vec<String>>,
    items: Vec<ListedChange<'a>>,
}

#[derive(Debug, Serialize)]
struct ListedChange<'a> {
    res: &'a str,
    name: &'a str,
    created: bool,
}

impl Entry {
    pub fn listed(&self) -> Listed<'_> {
        Listed {
            id: &self.id,
            at: &self.at,
            config: &self.config,
            app: &self.app,
            partial: self.partial,
            undone: &self.undone,
            undo_notes: &self.undo_notes,
            items: self
                .items
                .iter()
                .map(|c| ListedChange { res: &c.res, name: &c.name, created: c.was_created() })
                .collect(),
        }
    }
}

/// The history as stored. An entry that doesn't read as one is left out (and
/// goes at the next write).
fn entries(stored: Option<Value>) -> Vec<Entry> {
    let Some(Value::Array(rows)) = stored else { return vec![] };
    rows.into_iter()
        .filter_map(|v| {
            serde_json::from_value(v).map_err(|e| tracing::warn!("community history: an entry left out: {e}")).ok()
        })
        .collect()
}

// Recovery data is never evicted while it is still undoable.
fn trim(h: &mut Vec<Entry>) {
    while h.len() > HISTORY_KEEP {
        if let Some(i) = h.iter().rposition(|entry| entry.undone.is_some()) {
            h.remove(i);
        } else {
            break;
        }
    }
}

async fn load(app: &App) -> R<Vec<Entry>> {
    Ok(entries(app.state().get_value(KEY_HISTORY).await?))
}

/// Change the history in one write transaction.
async fn change<T: Send + 'static>(app: &App, f: impl FnOnce(&mut Vec<Entry>) -> R<T> + Send + 'static) -> R<T> {
    app.state()
        .write(move |tx| {
            let mut h = entries(tx.get_value(KEY_HISTORY)?);
            let out = f(&mut h)?;
            tx.put(KEY_HISTORY, &h)?;
            Ok(out)
        })
        .await
}

impl Community {
    /// The history as the page lists it.
    pub async fn history(&self, app: &App) -> R<Value> {
        let h = load(app).await?;
        Ok(serde_json::to_value(h.iter().map(Entry::listed).collect::<Vec<_>>())?)
    }

    pub(super) async fn remember(
        &self,
        app: &App,
        name: &str,
        on: &str,
        items: Vec<Change>,
        partial: bool,
    ) -> R<String> {
        let hid = token_hex(4);
        let entry = Entry {
            id: hid.clone(),
            at: now_iso_z(),
            config: name.into(),
            app: on.into(),
            items,
            partial,
            undone: None,
            undo_notes: None,
        };
        change(app, move |h| {
            h.insert(0, entry);
            trim(h);
            Ok(())
        })
        .await?;
        Ok(hid)
    }

    pub(super) async fn journal_item(&self, app: &App, hid: &str, item: Change) -> R<usize> {
        let hid = hid.to_owned();
        change(app, move |entries| {
            let entry =
                entries.iter_mut().find(|e| e.id == hid).ok_or_else(|| Error::internal("community journal missing"))?;
            let index = entry.items.len();
            entry.items.push(item);
            Ok(index)
        })
        .await
    }

    pub(super) async fn journal_replace(&self, app: &App, hid: &str, index: usize, item: Change) -> R<()> {
        let hid = hid.to_owned();
        change(app, move |entries| {
            let entry =
                entries.iter_mut().find(|e| e.id == hid).ok_or_else(|| Error::internal("community journal missing"))?;
            *entry.items.get_mut(index).ok_or_else(|| Error::internal("community journal item missing"))? = item;
            Ok(())
        })
        .await
    }

    pub(super) async fn journal_id(&self, app: &App, hid: &str, index: usize, id: Value) -> R<()> {
        let hid = hid.to_owned();
        change(app, move |entries| {
            let entry =
                entries.iter_mut().find(|e| e.id == hid).ok_or_else(|| Error::internal("community journal missing"))?;
            let item = entry.items.get_mut(index).ok_or_else(|| Error::internal("community journal item missing"))?;
            item.id = id;
            Ok(())
        })
        .await
    }

    pub(super) async fn finish_journal(&self, app: &App, hid: &str, partial: bool) -> R<()> {
        let hid = hid.to_owned();
        change(app, move |entries| {
            let entry =
                entries.iter_mut().find(|e| e.id == hid).ok_or_else(|| Error::internal("community journal missing"))?;
            entry.partial = partial;
            if entry.items.is_empty() {
                entries.retain(|e| e.id != hid);
            }
            Ok(())
        })
        .await
    }

    /// Put back what an apply changed. A step that fails stays in the entry, so
    /// Undo can be pressed again to retry just those.
    pub async fn undo(&self, app: &App, hid: &str) -> R<Value> {
        let _turn = self.lock.lock().await;
        let mut hist = load(app).await?;
        let Some(pos) = hist.iter().position(|x| x.id == hid) else {
            return Err(Error::not_found("not found"));
        };
        if hist[pos].undone.as_deref().is_some_and(|u| !u.is_empty()) {
            return Err(super::parse::cerr("already undone"));
        }
        let a = Self::app(app, &hist[pos].app)?;
        let c = Self::client(&a).await?;
        let mut notes: Vec<String> = vec![];
        let mut failed: Vec<usize> = vec![];
        let items = hist[pos].items.clone();
        for (i, it) in items.iter().enumerate().rev() {
            let res = it.res.as_str();
            if it.deleted {
                notes.push(format!("{}: deletion outcome may be uncertain; restore it by hand if needed before removing this history entry", it.name));
                failed.push(i);
                continue;
            }
            if it.was_created() && it.id.is_null() {
                notes.push(format!(
                    "{}: creation outcome is unknown; check the app before removing this history entry",
                    it.name
                ));
                failed.push(i);
                continue;
            }
            let step: R<()> = async {
                if res == "qualitydefinition" {
                    for d in rows(&it.before) {
                        c.put(&format!("qualitydefinition/{}", id_str(&d["id"])), d).await?;
                    }
                } else if it.was_created() {
                    c.delete(&format!("{res}/{}", id_str(&it.id))).await?;
                } else {
                    let body = restorable(&c, it).await?;
                    c.put(&format!("{res}/{}", id_str(&it.id)), &body).await?;
                }
                Ok(())
            }
            .await;
            match step {
                Ok(()) => {}
                Err(Error::Upstream(e)) => {
                    let name = &it.name;
                    if e.is_not_found() {
                        notes.push(format!("{name}: already gone"));
                    } else {
                        notes.push(format!("{name}: {e}"));
                        failed.push(i);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if failed.is_empty() {
            // it's off the timeline too
            self.forget_install(app, &hist[pos].app, hid).await?;
        }
        let h = &mut hist[pos];
        if failed.is_empty() {
            h.undone = Some(now_iso_z());
        } else {
            // keep only what's left, so Undo can be pressed again
            h.items = items.into_iter().enumerate().filter(|(i, _)| failed.contains(i)).map(|(_, x)| x).collect();
            notes.push(format!("{} change(s) couldn't be undone; press Undo again to retry them", failed.len()));
        }
        h.undo_notes = Some(notes.clone());
        // write back just this entry: the history may have changed since it was read
        let updated = h.clone();
        change(app, move |hist| {
            if let Some(e) = hist.iter_mut().find(|x| x.id == updated.id) {
                *e = updated;
            }
            Ok(())
        })
        .await?;
        Ok(json!({"notes": notes, "history": self.history(app).await?}))
    }

    /// Drop an entry from the history; the apps stay as they are.
    pub async fn forget(&self, app: &App, hid: &str) -> R<Value> {
        let _turn = self.lock.lock().await;
        self.discard_journal(app, hid).await?;
        self.history(app).await
    }

    pub(super) async fn discard_journal(&self, app: &App, hid: &str) -> R<()> {
        let hid = hid.to_owned();
        change(app, move |hist| {
            let n = hist.len();
            hist.retain(|x| x.id != hid);
            if hist.len() == n {
                return Err(Error::not_found("not found"));
            }
            Ok(())
        })
        .await
    }
}

/// The saved copy, with a profile's scores lined up with the formats there now.
async fn restorable(c: &crate::service::arr::ArrClient, it: &Change) -> R<Value> {
    let mut body = it.before.clone();
    if it.res == "qualityprofile" {
        let mut old = super::arr_view::VDict::default();
        for fi in rows(&body["formatItems"]) {
            old.set(fi["format"].clone(), fi["score"].clone());
        }
        let formats = c.get("customformat").await?;
        let formats = formats.as_array().ok_or_else(|| Error::upstream("customformat returned a non-list response"))?;
        let items: Vec<Value> = formats.iter()
            .map(|cf| json!({"format": cf["id"], "name": cf["name"], "score": old.get(&cf["id"]).cloned().unwrap_or(json!(0))}))
            .collect();
        body["formatItems"] = Value::Array(items);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> Value {
        json!({"id": "ab", "at": "t", "config": "c", "app": "radarr", "items": [
            {"res": "tag", "id": 1, "name": "tag x", "before": null},
            {"res": "customformat", "id": 2, "name": "cf", "before": {"id": 2}}], "partial": false, "undone": null})
    }

    #[test]
    fn history_limit_never_discards_pending_changes() {
        let pending: Entry = serde_json::from_value(stored()).expect("entry");
        let mut completed = pending.clone();
        completed.undone = Some("done".into());
        let mut entries = vec![pending.clone(); HISTORY_KEEP + 2];
        entries.push(completed);
        trim(&mut entries);
        assert_eq!(entries.len(), HISTORY_KEEP + 2);
        assert!(entries.iter().all(|entry| entry.undone.is_none()));
    }

    #[test]
    fn listed_items() {
        let h: Entry = serde_json::from_value(stored()).expect("entry");
        let p = serde_json::to_value(h.listed()).expect("json");
        assert_eq!(
            p["items"],
            json!([{"res": "tag", "name": "tag x", "created": true},
                                      {"res": "customformat", "name": "cf", "created": false}])
        );
        assert_eq!(p.as_object().map(|o| o.keys().next_back().cloned()), Some(Some("items".to_string())));
        assert!(p.get("undo_notes").is_none());
        assert_eq!(token_hex(4).len(), 8);
    }

    #[test]
    fn entries_round_trip() {
        // a stored entry is written back as it was read
        let h: Entry = serde_json::from_value(stored()).expect("entry");
        assert_eq!(serde_json::to_value(&h).expect("json"), stored());
        let mut undone = h.clone();
        undone.undone = Some("2026-10-27T10:00:00Z".into());
        undone.undo_notes = Some(vec!["cf: already gone".into()]);
        let back: Entry = serde_json::from_value(serde_json::to_value(&undone).expect("json")).expect("entry");
        assert_eq!(back, undone);
        // an older, sparser one still reads
        let old: Entry = serde_json::from_value(json!({"id": "cd", "items": [{"res": "tag"}]})).expect("entry");
        assert_eq!((old.items[0].was_created(), old.partial, old.undone), (true, false, None));
    }
}
