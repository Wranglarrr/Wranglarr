// SPDX-License-Identifier: GPL-3.0-or-later
//! The lists under each app on the home page.
//!
//! The arrs: what's coming up (their calendar) and what's missing (aired or released,
//! monitored, no file).  Bazarr: subtitles it still wants.  Prowlarr: its indexers,
//! failing ones first.  Seerr: the latest requests.  (The download clients' queues come
//! from the downloads module, which the page polls faster.)
//!
//! Which lists an app has is its descriptor's [`feeds`](crate::service::ServiceDef::feeds);
//! its client makes them ([`Feed`]), as a [`FeedPage`]. Here: the home settings,
//! and the lists fetched for the page and kept for a while ([`FeedCache`]).
//!
//! Each app's lists are fetched in parallel and cached for a couple of minutes; what
//! is shown, and how far ahead, is in the home settings (System → Home page).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::app::App;
use crate::config::AppConfig;
use crate::error::Result;
use crate::service::AppConfigExt;
use crate::service::{Feed, FeedKind, ServiceDef};

pub use crate::service::feed::*;

/// How long an app's lists are kept.
const TTL: Duration = Duration::from_secs(120);

// ---- the cache ----------------------------------------------------------------------------

/// The home page lists kept on the service: each app's, for [`TTL`], and what
/// the clients looked up once while making them.
#[derive(Default)]
pub struct FeedCache {
    /// app -> (fetched at, its lists)
    lists: Mutex<HashMap<String, (Instant, Value)>>,
    lookups: Lookups,
}

impl FeedCache {
    /// Forget every app's lists (the settings changed). What was looked up stays.
    pub fn clear(&self) {
        if let Ok(mut c) = self.lists.lock() {
            c.clear();
        }
    }

    /// An app's lists, when fetched less than [`TTL`] ago.
    fn fresh(&self, app: &str) -> Option<Value> {
        let g = self.lists.lock().ok()?;
        g.get(app).filter(|(at, _)| at.elapsed() < TTL).map(|(_, v)| v.clone())
    }

    /// Keep an app's lists, fetched at `at`.
    fn keep(&self, app: String, at: Instant, lists: Value) {
        if let Ok(mut g) = self.lists.lock() {
            g.insert(app, (at, lists));
        }
    }
}

// ---- the service --------------------------------------------------------------------------

/// The view of the service for the home page lists: `app.feeds()`.
pub struct Feeds<'a>(&'a App);

impl App {
    pub fn feeds(&self) -> Feeds<'_> {
        Feeds(self)
    }
}

/// The lists of a kind that the settings switch on, in its order.
fn shown<'s>(def: &'static ServiceDef, st: &'s HomeSettings) -> impl Iterator<Item = FeedKind> + 's {
    def.feeds.iter().copied().filter(|k| st.shows(*k))
}

impl<'a> Feeds<'a> {
    pub async fn settings(&self) -> HomeSettings {
        self.0.state().get::<HomeSettings>(KEY).await.ok().flatten().unwrap_or_default()
    }

    pub async fn save_settings(&self, body: &Map<String, Value>) -> Result<HomeSettings> {
        let s = apply(self.settings().await, body)?;
        self.0.state().put(KEY, &s).await?;
        self.0.feed_cache().clear();
        Ok(s)
    }

    pub async fn reset(&self) -> Result<HomeSettings> {
        self.0.state().put(KEY, &json!({})).await?;
        self.0.feed_cache().clear();
        Ok(self.settings().await)
    }

    /// The apps with lists: the library arrs, Bazarr, Prowlarr, Seerr.
    pub fn apps(&self) -> Vec<&'a AppConfig> {
        self.0.cfg().all_apps().filter(|a| a.enabled && a.def().is_some_and(|d| !d.feeds.is_empty())).collect()
    }

    /// `{"settings": …, "apps": {name: {"lists": […]}}}`
    pub async fn all(&self, refresh: bool) -> Value {
        let st = self.settings().await;
        let apps = self.apps();
        let res = futures::future::join_all(apps.iter().map(|a| self.app(a, &st, refresh))).await;
        let mut out = Map::new();
        for (a, v) in apps.iter().zip(res) {
            out.insert(a.name.clone(), v);
        }
        json!({"settings": st, "apps": out})
    }

    /// One app's lists (cached for two minutes).
    pub async fn app(&self, app: &AppConfig, st: &HomeSettings, refresh: bool) -> Value {
        let cache = &self.0.feed_cache();
        if !refresh && let Some(v) = cache.fresh(&app.name) {
            return v;
        }
        let started = Instant::now();
        let c = match crate::service::client(app).await {
            Ok(c) => c,
            Err(e) => return json!({"lists": [], "error": crate::value::head(&e.to_string(), 200)}),
        };
        let lists = match c.service().feed() {
            Some(feed) => lists(feed, &app.name, shown(c.def(), st), &st.query(), &cache.lookups).await,
            None => vec![],
        };
        let out = json!({"lists": lists});
        cache.keep(app.name.clone(), started, out.clone());
        out
    }
}

/// A list for the page: the app's, and which one it is.
#[derive(serde::Serialize)]
struct Listed {
    #[serde(flatten)]
    page: FeedPage,
    key: &'static str,
}

/// Each list in turn; one that fails says why in its place.
async fn lists(
    feed: &dyn Feed,
    name: &str,
    kinds: impl Iterator<Item = FeedKind>,
    query: &FeedQuery,
    lookups: &Lookups,
) -> Vec<Value> {
    let mut out = Vec::new();
    for kind in kinds {
        let key = kind.key();
        out.push(match feed.list(kind, query, lookups).await {
            Ok(page) => json!(Listed { page, key }),
            Err(e) => {
                tracing::debug!("feeds: {name} {key}: {e}");
                json!({"key": key, "title": key, "items": [], "total": 0,
                       "error": crate::value::head(&e.to_string(), 200)})
            }
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use chrono::{Local, TimeDelta, Utc};

    #[test]
    fn reads_the_apps_dates() {
        let utc = |s: &str| parse(s).map(|d| d.with_timezone(&Utc).to_rfc3339());
        assert_eq!(utc("2026-01-01T01:00:00Z").as_deref(), Some("2026-01-01T01:00:00+00:00"));
        assert_eq!(utc("2026-01-01T01:00:00.1234567Z").as_deref(), Some("2026-01-01T01:00:00.123456700+00:00"));
        assert_eq!(utc("2026-01-01T01:00:00").as_deref(), Some("2026-01-01T01:00:00+00:00"), "no zone: UTC");
        assert_eq!(utc("2026-01-01T01:00:00+02:00").as_deref(), Some("2025-12-31T23:00:00+00:00"));
        assert_eq!(utc("2026-01-01").as_deref(), Some("2026-01-01T00:00:00+00:00"));
        assert_eq!(utc(""), None);
        assert_eq!(utc("soon"), None);
    }

    #[test]
    fn window_is_today_and_the_next_days() {
        let (start, end) = window(7);
        assert_eq!(start.time(), chrono::NaiveTime::MIN);
        assert_eq!(end - start, TimeDelta::days(8));
        assert!(start <= Local::now().fixed_offset());
    }

    #[test]
    fn settings_are_clamped() {
        let body = |v: Value| v.as_object().cloned().unwrap_or_default();
        let s = apply(
            HomeSettings::default(),
            &body(json!({"days": "500", "missing_days": "-3", "limit": "1", "details": "always", "queue": false,
                         "release_days": "9999", "release_next": "0", "missing": 0, "requests": "x"})),
        )
        .expect("ok");
        assert_eq!((s.days, s.missing_days, s.limit, s.release_days, s.release_next), (90, 0, 3, 365, 1));
        assert_eq!(s.details, "always");
        assert!(!s.queue && !s.missing && s.requests);
        let s = apply(HomeSettings::default(), &body(json!({"days": 12.9, "limit": "7"}))).expect("numbers");
        assert_eq!((s.days, s.limit), (12, 7));
        for b in [
            json!({"details": "sideways"}),
            json!({"days": "lots"}),
            json!({"limit": null}),
            json!({"days": [1]}),
            json!({"limit": true}),
        ] {
            assert!(matches!(apply(HomeSettings::default(), &body(b)), Err(Error::Invalid(_))));
        }
        assert_eq!(
            apply(HomeSettings::default(), &body(json!({"details": 1}))).expect_err("bad").to_string(),
            "details: hover or always"
        );
    }

    #[test]
    fn stored_settings_fill_in_the_defaults() {
        let s: HomeSettings = serde_json::from_value(json!({"days": 3})).expect("partial");
        assert_eq!(s, HomeSettings { days: 3, ..Default::default() });
        let s: HomeSettings = serde_json::from_value(json!({})).expect("reset");
        assert_eq!(s, HomeSettings::default());
    }

    #[test]
    fn each_kind_of_list_is_a_settings_switch() {
        let on = serde_json::to_value(HomeSettings::default()).expect("json");
        for kind in FeedKind::ALL {
            assert_eq!(on[kind.key()], true, "{}", kind.key());
            let off =
                apply(HomeSettings::default(), &json!({kind.key(): false}).as_object().cloned().unwrap_or_default())
                    .expect("switch");
            assert!(!off.shows(kind) && HomeSettings::default().shows(kind), "{}", kind.key());
        }
    }

    #[test]
    fn every_kind_with_lists_has_a_feed() {
        for d in crate::service::ALL {
            let app = AppConfig {
                name: format!("my{}", d.id),
                kind: d.id.into(),
                url: "http://127.0.0.1:9/".into(),
                api_key: "k".into(),
                enabled: true,
                ..Default::default()
            };
            let c = crate::service::client_blocking(&app).expect("client");
            assert_eq!(c.service().feed().is_some(), !d.feeds.is_empty(), "{}", d.id);
        }
    }

    #[tokio::test]
    async fn a_list_the_app_lacks_is_an_error_in_its_place() {
        let app = AppConfig { name: "q".into(), kind: "qbittorrent".into(), ..Default::default() };
        assert!(crate::service::client(&app).await.expect("client").service().feed().is_none());
        let bz = AppConfig { name: "b".into(), kind: "bazarr".into(), api_key: "k".into(), ..Default::default() };
        let c = crate::service::client(&bz).await.expect("client");
        let feed = c.service().feed().expect("bazarr lists");
        let got = lists(feed, "b", [FeedKind::Requests].into_iter(), &FeedQuery::default(), &Lookups::default()).await;
        assert_eq!(
            got,
            vec![json!({"key": "requests", "title": "requests", "items": [], "total": 0,
                        "error": "b: has no requests list"})]
        );
    }
}
