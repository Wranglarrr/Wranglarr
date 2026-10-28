// SPDX-License-Identifier: GPL-3.0-or-later
//! What the home page lists are made of: the home settings and the
//! [`FeedQuery`] they make, the [`FeedPage`] a list is and the [`Item`]s in it,
//! what lists look up once ([`Lookups`]), and the helpers the kinds'
//! [`Feed`](super::Feed)s share. Fetching and caching them for the page is the
//! app's `feeds` module.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::FeedKind;
use wranglarr_core::error::{Error, Result};
use wranglarr_core::http::ArrError;
use wranglarr_core::value::truthy;

pub const KEY: &str = "home:settings";

/// The home settings (state key `home:settings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeSettings {
    /// card details: "hover" or "always"
    pub details: String,
    /// each download client's queue under its card
    pub queue: bool,
    pub upcoming: bool,
    pub days: i64,
    pub unmonitored: bool,
    /// films, albums, books come out rarely: the next N of them...
    pub release_next: i64,
    /// ...within this many days
    pub release_days: i64,
    /// the arrs' missing items, Bazarr's wanted subtitles
    pub missing: bool,
    /// only what came out in the last N days (0: all of it)
    pub missing_days: i64,
    /// Seerr
    pub requests: bool,
    /// Prowlarr
    pub indexers: bool,
    /// items per list
    pub limit: i64,
}

impl Default for HomeSettings {
    fn default() -> Self {
        HomeSettings {
            details: "hover".into(),
            queue: true,
            upcoming: true,
            days: 7,
            unmonitored: false,
            release_next: 10,
            release_days: 90,
            missing: true,
            missing_days: 30,
            requests: true,
            indexers: true,
            limit: 25,
        }
    }
}

impl HomeSettings {
    /// Whether that kind of list is switched on.
    pub fn shows(&self, kind: FeedKind) -> bool {
        match kind {
            FeedKind::Upcoming => self.upcoming,
            FeedKind::Missing => self.missing,
            FeedKind::Indexers => self.indexers,
            FeedKind::Requests => self.requests,
        }
    }
}

impl HomeSettings {
    /// What the lists are asked for, as these settings say.
    pub fn query(&self) -> FeedQuery {
        let count = |n: i64| usize::try_from(n).unwrap_or(0);
        FeedQuery {
            limit: count(self.limit),
            days: self.days,
            unmonitored: self.unmonitored,
            release_next: count(self.release_next),
            release_days: self.release_days,
            missing_days: self.missing_days,
        }
    }
}

/// What a home page list is asked for: the parts of the [`HomeSettings`] that
/// shape a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedQuery {
    /// Items per list.
    pub limit: usize,
    /// Upcoming episodes: how many days ahead.
    pub days: i64,
    /// Upcoming: unmonitored ones too.
    pub unmonitored: bool,
    /// Upcoming releases (films, albums, books): the next this many ...
    pub release_next: usize,
    /// ... within this many days.
    pub release_days: i64,
    /// Missing: only what came out in the last this many days (0: all of it).
    pub missing_days: i64,
}

impl Default for FeedQuery {
    fn default() -> Self {
        HomeSettings::default().query()
    }
}

/// One home page list as an app's [`Feed`](super::Feed) makes it. The fields
/// that are None aren't sent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedPage {
    pub title: String,
    /// A line beside the title ("next 7 days", "2 failing").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The first [`FeedQuery::limit`] of them.
    pub items: Vec<Item>,
    /// How many there are.
    pub total: usize,
    /// Upcoming: how many days ahead it looked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<i64>,
    /// Upcoming releases: how many it shows at most.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<usize>,
    /// Missing: how many are missing, however long ago they came out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all: Option<usize>,
    /// The list's page in the app's UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Why there's nothing to list yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty: Option<String>,
}

impl FeedPage {
    /// A list of `items`, out of `total`.
    pub fn new(title: impl Into<String>, items: Vec<Item>, total: usize) -> FeedPage {
        FeedPage {
            title: title.into(),
            note: None,
            items,
            total,
            days: None,
            next: None,
            all: None,
            path: None,
            empty: None,
        }
    }
}

/// What a list looked up once and keeps (Seerr: `movie:<tmdb id>` -> title),
/// shared by every client built for the page.
#[derive(Default)]
pub struct Lookups {
    titles: Mutex<HashMap<String, String>>,
}

impl Lookups {
    /// A title looked up before.
    pub fn title(&self, key: &str) -> Option<String> {
        self.titles.lock().ok().and_then(|g| g.get(key).cloned())
    }

    /// Keep a title that was looked up.
    pub fn keep_title(&self, key: String, title: String) {
        if let Ok(mut g) = self.titles.lock() {
            g.insert(key, title);
        }
    }
}

/// The error for asking an app for a list it doesn't have.
pub fn no_list(app: &str, kind: FeedKind) -> ArrError {
    ArrError::new(format!("{app}: has no {} list", kind.key()))
}

// ---- reading the apps' JSON --------------------------------------------------------------

/// `str(v)` for a value that's there, "" otherwise (`v or ""`).
pub fn text(v: &Value) -> String {
    if !truthy(v) {
        return String::new();
    }
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        o => o.to_string(),
    }
}

/// `a or b`, as text.
pub fn or_text(a: &Value, b: &Value) -> String {
    if truthy(a) { text(a) } else { text(b) }
}

pub fn iso(d: DateTime<FixedOffset>) -> String {
    d.with_timezone(&Utc).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A date or time as the apps write it (RFC 3339 and its common relatives); a time
/// without a zone is UTC.
pub fn parse(s: &str) -> Option<DateTime<FixedOffset>> {
    if s.is_empty() {
        return None;
    }
    let z = s.replace('Z', "+00:00");
    if let Ok(d) = DateTime::parse_from_rfc3339(&z) {
        return Some(d);
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%dT%H:%M%:z", "%Y-%m-%d %H:%M%:z"] {
        if let Ok(d) = DateTime::parse_from_str(&z, f) {
            return Some(d);
        }
    }
    let utc = |n: NaiveDateTime| n.and_utc().fixed_offset();
    for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Some(utc(n));
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)).map(utc)
}

pub fn parse_v(v: &Value) -> Option<DateTime<FixedOffset>> {
    parse(&text(v))
}

/// One entry of a list. `day`: `when` is a date (a film or album out that day), not a
/// time (an episode airing at 20:00).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub title: String,
    pub sub: String,
    pub when: String,
    pub tag: String,
    pub state: String,
    pub path: String,
    pub day: bool,
}

pub fn item(title: &Value, sub: String, when: &Value, tag: String, state: &str, path: String, day: bool) -> Item {
    Item {
        title: if truthy(title) { text(title) } else { "?".into() },
        sub,
        when: text(when),
        tag,
        state: state.into(),
        path,
        day,
    }
}

/// The parts that are there, joined with " · ".
pub fn join(parts: &[String]) -> String {
    parts.iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

/// `prefix` and the value, or "" when there's no value.
pub fn some_path(prefix: &str, v: &Value) -> String {
    if truthy(v) { format!("{prefix}{}", text(v)) } else { String::new() }
}

/// Today (local midnight) and the next `days` days.
pub fn window(days: i64) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
    let now = Local::now().fixed_offset();
    let start =
        now.date_naive().and_hms_opt(0, 0, 0).and_then(|n| n.and_local_timezone(*now.offset()).single()).unwrap_or(now);
    (start, start + TimeDelta::days(days + 1))
}

/// `base` with these query parameters.
pub fn qs(base: &str, params: &[(&str, String)]) -> String {
    match reqwest::Url::parse_with_params(base, params) {
        Ok(u) => u.to_string(),
        Err(_) => base.to_string(),
    }
}

// ---- settings -----------------------------------------------------------------------------

/// A whole number from a settings body: a number (a fraction is cut off) or a string
/// holding one. Out-of-range values saturate; the caller clamps them anyway.
fn whole_number(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::String(s) => {
            s.trim().parse::<i128>().ok().map(|n| n.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
        }
        _ => None,
    }
}

/// Apply a PUT body to the settings; a bad value is a 400.
pub fn apply(mut s: HomeSettings, body: &Map<String, Value>) -> Result<HomeSettings> {
    if let Some(d) = body.get("details") {
        match d.as_str() {
            Some(v @ ("hover" | "always")) => s.details = v.into(),
            _ => return Err(Error::invalid("details: hover or always")),
        }
    }
    for (k, f) in [
        ("queue", &mut s.queue),
        ("upcoming", &mut s.upcoming),
        ("unmonitored", &mut s.unmonitored),
        ("missing", &mut s.missing),
        ("requests", &mut s.requests),
        ("indexers", &mut s.indexers),
    ] {
        if let Some(v) = body.get(k) {
            *f = truthy(v);
        }
    }
    for (k, f, lo, hi) in [
        ("days", &mut s.days, 1, 90),
        ("missing_days", &mut s.missing_days, 0, 3650),
        ("release_next", &mut s.release_next, 1, 200),
        ("release_days", &mut s.release_days, 1, 365),
        ("limit", &mut s.limit, 3, 200),
    ] {
        if let Some(v) = body.get(k) {
            let n = whole_number(v).ok_or_else(|| Error::invalid("the days and items per list must be numbers"))?;
            *f = n.clamp(lo, hi);
        }
    }
    Ok(s)
}
