// SPDX-License-Identifier: GPL-3.0-or-later
//! App connections, trackers/indexers and the download queue: the
//! `/api/connect`, `/api/indexers…` and `/api/downloads` endpoints and the
//! `/indexers` page.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use super::{Body, Ctx, NO_BODY, NOT_AN_OBJECT, Page, Q, TRUE_OR_FALSE, WHOLE_NUMBER, page, unprocessable};
use crate::app::App;
use crate::error::Result;
use crate::indexers::{SourceIn, public};
use crate::service::Roles;
use crate::value;
use crate::web::ApiResult;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/api/connect", get(api_connect).post(api_connect_run))
        .route("/api/indexers", get(api_indexers))
        .route("/api/indexers/sources", post(api_indexer_save))
        .route("/api/indexers/sources/{sid}", delete(api_indexer_delete))
        .route("/api/indexers/sync", post(api_indexer_sync))
        .route("/api/downloads", get(api_downloads))
        .route("/indexers", get(indexers_page))
}

type Accepted = (StatusCode, Json<Value>);

fn accepted(v: Value) -> Accepted {
    (StatusCode::ACCEPTED, Json(v))
}

// ---- request bodies ----------------------------------------------------------------------

/// Reads the fields of a JSON request body. Numbers and flags may also come as
/// strings (`"25"`, `"yes"`), as forms and scripts send them. What is missing or
/// of the wrong type is collected as `"<field>: <problem>"`, up to five, joined
/// by "; ".
struct Fields<'a> {
    obj: &'a Map<String, Value>,
    errs: Vec<String>,
}

impl<'a> Fields<'a> {
    /// The body as an object. None (no body) only when `optional`.
    fn of(body: &'a Value, optional: bool) -> Result<Option<Fields<'a>>> {
        match body {
            Value::Object(obj) => Ok(Some(Fields { obj, errs: vec![] })),
            Value::Null if optional => Ok(None),
            Value::Null => Err(unprocessable(NO_BODY)),
            _ => Err(unprocessable(NOT_AN_OBJECT)),
        }
    }

    fn err(&mut self, k: &str, msg: &str) {
        self.errs.push(format!("{k}: {msg}"));
    }

    fn get(&mut self, k: &str, required: bool) -> Option<&'a Value> {
        let v = self.obj.get(k);
        if v.is_none() && required {
            self.err(k, "required");
        }
        v
    }

    fn string(&mut self, k: &str, default: Option<&str>) -> String {
        match self.get(k, default.is_none()) {
            None => default.unwrap_or_default().to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => {
                self.err(k, "must be text");
                String::new()
            }
        }
    }

    /// `read` of the field, or `default` (with a problem noted when it is there
    /// but unreadable: `"<text>" isn't <what>` for text, `generic` otherwise).
    fn read<T>(&mut self, k: &str, default: T, read: fn(&Value) -> Option<T>, what: &str, generic: &str) -> T {
        let Some(v) = self.get(k, false) else { return default };
        read(v).unwrap_or_else(|| {
            match v {
                Value::String(s) => self.err(k, &format!("{s:?} isn't {what}")),
                _ => self.err(k, generic),
            }
            default
        })
    }

    fn int(&mut self, k: &str, default: i64) -> i64 {
        self.read(k, default, value::as_whole, "a whole number", WHOLE_NUMBER)
    }

    fn flag(&mut self, k: &str, default: bool) -> bool {
        self.read(k, default, value::as_flag, "true or false", TRUE_OR_FALSE)
    }

    fn strings(&mut self, k: &str) -> Vec<String> {
        match self.get(k, false) {
            None => vec![],
            Some(Value::Array(a)) => {
                let mut out = vec![];
                for (i, v) in a.iter().enumerate() {
                    match v {
                        Value::String(s) => out.push(s.clone()),
                        _ => self.err(&format!("{k}.{i}"), "must be text"),
                    }
                }
                out
            }
            Some(_) => {
                self.err(k, "must be a list");
                vec![]
            }
        }
    }

    fn done(self) -> Result<()> {
        if self.errs.is_empty() {
            return Ok(());
        }
        Err(unprocessable(&self.errs.iter().take(5).cloned().collect::<Vec<_>>().join("; ")))
    }
}

/// A connect request: `mode` ("all" when left out) and `links` (none); the body
/// may be left out.
pub fn connect_in(body: &Value) -> Result<(String, Vec<String>)> {
    let Some(mut f) = Fields::of(body, true)? else { return Ok(("all".into(), vec![])) };
    let mode = f.string("mode", Some("all"));
    let links = f.strings("links");
    f.done()?;
    Ok((mode, links))
}

/// An indexer source; what is left out takes `SourceIn`'s defaults.
pub fn source_in(body: &Value) -> Result<SourceIn> {
    let Some(mut f) = Fields::of(body, false)? else { return Err(unprocessable(NO_BODY)) };
    let d = SourceIn::default();
    let s = SourceIn {
        id: f.string("id", Some("")),
        r#type: f.string("type", None),
        name: f.string("name", None),
        url: f.string("url", None),
        api_key: f.string("api_key", Some("")),
        apply_to: f.strings("apply_to"),
        enabled: f.flag("enabled", d.enabled),
        priority: f.int("priority", d.priority),
        min_seeders: f.int("min_seeders", d.min_seeders),
        torrents: f.flag("torrents", d.torrents),
    };
    f.done()?;
    Ok(s)
}

// ---- app connections -----------------------------------------------------------------

async fn api_connect(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(app.connect().status(None).await?))
}

async fn api_connect_run(State(app): State<App>, Body(body): Body<Value>) -> ApiResult<Accepted> {
    let (mode, links) = connect_in(&body)?;
    Ok(accepted(json!({"run": app.connect().run(&mode, &links).await?})))
}

// ---- trackers / aggregators ---------------------------------------------------------------

async fn api_indexers(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(app.indexers().status().await?))
}

async fn api_indexer_save(State(app): State<App>, Body(body): Body<Value>) -> ApiResult<Accepted> {
    let data = source_in(&body)?;
    let ix = app.indexers();
    let src = ix.save_source(&data).await?;
    let name = src.get("name").and_then(Value::as_str).unwrap_or_default();
    // None: another system run is busy; the sync follows it
    let run = ix.sync_or_later(&format!("saved {name}")).await?;
    Ok(accepted(json!({"source": public(&src), "run": run})))
}

async fn api_indexer_delete(State(app): State<App>, Path(sid): Path<String>) -> ApiResult<Accepted> {
    let ix = app.indexers();
    ix.delete_source(&sid).await?;
    let run = ix.sync_or_later("deleted").await?;
    Ok(accepted(json!({"run": run})))
}

async fn api_indexer_sync(State(app): State<App>) -> ApiResult<Accepted> {
    Ok(accepted(json!({"run": app.indexers().sync("manual").await?})))
}

async fn indexers_page(State(app): State<App>, ctx: Ctx) -> Page {
    let ix = app.indexers();
    let status = ix.status().await?;
    let links = app.connect().status(Some(Roles::INDEXERS)).await?;
    let connect: Vec<Value> = links["links"].as_array().cloned().unwrap_or_default();
    let sources: Map<String, Value> = ix
        .sources()
        .await?
        .iter()
        .map(|s| (s.get("id").and_then(Value::as_str).unwrap_or_default().to_string(), public(s)))
        .collect();
    page(&app, &ctx, "indexers.html", "indexers", json!({"ix": status, "connect": connect, "sources_json": sources}))
        .await
        .map_err(Into::into)
}

// ---- downloads -------------------------------------------------------------------------------

async fn api_downloads(State(app): State<App>, q: Q) -> ApiResult<Json<Value>> {
    let refresh = q.flag("refresh")?;
    Ok(Json(app.downloads().list(refresh).await))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_bodies() {
        let s = source_in(&json!({"type": "torznab", "name": "t", "url": "http://x/api", "min_seeders": "0",
                                  "enabled": "yes"}))
        .expect("valid");
        assert_eq!((s.min_seeders, s.priority, s.enabled, s.torrents), (0, 25, true, false));
        let e = source_in(&json!({"type": "torznab", "name": "t", "url": "u", "min_seeders": "x"})).expect_err("bad");
        assert_eq!(e.message(), r#"min_seeders: "x" isn't a whole number"#);
        assert_eq!(crate::web::status_of(&e), StatusCode::UNPROCESSABLE_ENTITY);
        let e = source_in(&json!({"name": 5, "apply_to": ["a", 1]})).expect_err("bad");
        assert_eq!(e.message(), "type: required; name: must be text; url: required; apply_to.1: must be text");
        assert_eq!(source_in(&Value::Null).expect_err("no body").message(), NO_BODY);
        let e = source_in(&json!({"type": "t", "name": "t", "url": "u", "priority": 1.5, "enabled": "maybe"}));
        assert_eq!(
            e.expect_err("bad").message(),
            r#"enabled: "maybe" isn't true or false; priority: must be a whole number"#
        );
    }

    #[test]
    fn connect_bodies() {
        assert_eq!(connect_in(&Value::Null).expect("optional"), ("all".into(), vec![]));
        assert_eq!(connect_in(&json!({"mode": "new"})).expect("ok"), ("new".into(), vec![]));
        assert_eq!(connect_in(&json!({"links": ["a>b"]})).expect("ok"), ("all".into(), vec!["a>b".to_string()]));
        assert_eq!(connect_in(&json!({"links": "a>b"})).expect_err("bad").message(), "links: must be a list");
    }
}
