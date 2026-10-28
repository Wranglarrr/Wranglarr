// SPDX-License-Identifier: GPL-3.0-or-later
//! The *arr family: one API, six apps. What differs between them is data, here.

use crate::AnyClient;
use crate::AppConfigExt;
use crate::KeyFile;
use serde_json::{Map, Value, json};
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;

use super::{
    ARR_COPY, ARR_URL_BASE, BackupMode, Container, CopyRules, Credentials, Database, FeedKind, HomeGroup, Port,
    ProxyTraits, Roles, ServiceDef, Wizard,
};

pub mod client;
pub mod connection;
pub mod lidarr;
mod login;
pub mod prowlarr;
pub mod radarr;
pub mod readarr;
pub mod shared;
pub mod sonarr;
pub mod whisparr;

pub use client::{ArrClient, shape};

/// What differs between the arrs. Each arr's file fills one in; the shared
/// implementation ([`ArrClient`]) and the modules read it rather than ask which
/// arr they have.
#[derive(Debug)]
pub struct ArrDef {
    /// The API version in its path: `/api/v3`.
    pub api_version: &'static str,
    /// Its application implementation in Prowlarr (None: Prowlarr itself).
    pub prowlarr_impl: Option<&'static str>,
    /// The Newznab/Torznab categories it searches (its own defaults).
    pub categories: &'static [i64],
    /// Anime categories, for the arrs that have them.
    pub anime_categories: Option<&'static [i64]>,
    /// Hand-made download categories that mean this arr ("movies", "tv").
    pub category_aliases: &'static [&'static str],

    // ---- the backup's overview (manifest keys: persisted) -------------------------
    /// What its collection counts (the manifest's `collection`).
    pub collection: Collection,
    /// The settings lists its scope counts (the manifest's `scope`), besides the
    /// root folders every arr adds.
    pub scope: &'static [&'static str],
    /// Its custom formats count towards the scope too (`customformat`).
    pub scope_custom_formats: bool,

    // ---- the home page, the watcher, notifications ----------------------------------
    /// Its calendar and Wanted → Missing (the library arrs).
    pub lists: Option<Lists>,
    /// The queue's switches that take in downloads it can't match to its library
    /// (`includeUnknownMovieItems`): the watcher looks at those too.
    pub queue_unknown: &'static [&'static str],
    /// The notification flags only it has.
    pub events: Events,

    /// Takes community configs (quality profiles, custom formats, …).
    pub community: Option<CommunityDef>,
}

/// What an arr's collection counts.
#[derive(Debug)]
pub enum Collection {
    /// A media library.
    Library(Library),
    /// The length of each of these lists: `(key, endpoint)`.
    Counts(&'static [(&'static str, &'static str)]),
}

/// A media library, and how its collection counts it.
#[derive(Debug)]
pub struct Library {
    /// The list of its items ("movie").
    pub endpoint: &'static str,
    /// The key the number of items goes under ("movies").
    pub count: &'static str,
    /// The other counts, in order: `(key, count)`.
    pub totals: &'static [(&'static str, Total)],
}

impl Library {
    /// The collection, from what its list endpoint answered.
    pub fn counts(&self, items: &[Value]) -> Value {
        let mut out = Map::new();
        out.insert(self.count.into(), json!(items.len()));
        for (key, total) in self.totals {
            out.insert((*key).into(), json!(total.of(items)));
        }
        Value::Object(out)
    }
}

/// One count over a library's items.
#[derive(Clone, Copy, Debug)]
pub enum Total {
    /// The items whose field is `true`.
    Flagged(&'static str),
    /// Monitored items that are out (`isAvailable`, true when absent) and have no
    /// file: what Wanted → Missing would grab now.
    Wanted,
    /// The sum of a field of each item.
    Sum(&'static str),
    /// The sum of a field of each item's `statistics`.
    Stat(&'static str),
    /// The sum of `a - b` (at least 0) over each item's `statistics`: the ones
    /// without a file.
    StatGap(&'static str, &'static str),
}

impl Total {
    /// This count over `items`.
    pub fn of(self, items: &[Value]) -> i64 {
        let n = |v: &Value, k: &str| v[k].as_i64().unwrap_or(0);
        let stats = |f: &dyn Fn(&Value) -> i64| items.iter().map(|i| f(&i["statistics"])).sum();
        let count = |f: &dyn Fn(&Value) -> bool| items.iter().filter(|i| f(i)).count() as i64;
        match self {
            Total::Flagged(k) => count(&|i| i[k] == true),
            Total::Wanted => {
                count(&|i| i["monitored"] == true && i["hasFile"] != true && i["isAvailable"].as_bool().unwrap_or(true))
            }
            Total::Sum(k) => items.iter().map(|i| n(i, k)).sum(),
            Total::Stat(k) => stats(&|s| n(s, k)),
            Total::StatGap(a, b) => stats(&|s| (n(s, a) - n(s, b)).max(0)),
        }
    }
}

/// The settings lists every library arr's scope counts.
pub const LIBRARY_SCOPE: &[&str] =
    &["indexer", "downloadclient", "qualityprofile", "importlist", "notification", "tag"];

/// How an arr's calendar reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Calendar {
    /// Dated releases (a film's cinema/digital/disc dates, an album, a book): the
    /// next few, months ahead.
    Releases,
    /// Weekly episodes: all of the next days.
    Episodes,
}

/// A library arr's calendar and Wanted → Missing.
#[derive(Debug)]
pub struct Lists {
    pub calendar: Calendar,
    /// What its records are, in the order they're told apart by their keys
    /// ([`Record::marks`]); a record with none of those keys is the last one.
    pub records: &'static [Record],
    /// What Wanted → Missing sorts by (newest first).
    pub missing_sort: &'static str,
}

impl Lists {
    /// What this calendar or Wanted → Missing record is.
    pub fn record(&self, r: &Value) -> Record {
        let marked = |k: &&Record| k.marks().iter().any(|m| r.get(m).is_some());
        self.records.iter().find(marked).or(self.records.last()).copied().unwrap_or(Record::Movie)
    }
}

/// What a calendar or Wanted → Missing record describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// An episode (Sonarr, Whisparr v2).
    Episode,
    /// An album (Lidarr).
    Album,
    /// A book (Readarr).
    Book,
    /// A film (Radarr, Whisparr v3).
    Movie,
}

impl Record {
    /// The keys that tell this record apart (any one of them).
    pub const fn marks(self) -> &'static [&'static str] {
        match self {
            Record::Episode => &["series", "seriesId"],
            Record::Album => &["artist", "artistId"],
            Record::Book => &["author", "authorId"],
            // no key of its own: a film is what the others aren't
            Record::Movie => &[],
        }
    }

    /// Its release dates when it has several, `(field, label)`: the calendar
    /// lists it once per date in its window.
    pub const fn dates(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Record::Movie => {
                &[("inCinemas", "in cinemas"), ("digitalRelease", "digital release"), ("physicalRelease", "on disc")]
            }
            Record::Episode | Record::Album | Record::Book => &[],
        }
    }
}

/// The notification flags that differ between the arrs.
#[derive(Debug)]
pub struct Events {
    /// Set on an import, besides the shared `onDownload` and `onImportComplete`.
    pub import: &'static [&'static str],
    /// Set when something is added to its library.
    pub added: Option<&'static str>,
}

/// What community configs need to know about an arr that takes them.
#[derive(Debug)]
pub struct CommunityDef {
    /// The quality profiles it ships with (lower case): changing one changes its defaults.
    pub quality_profiles: &'static [&'static str],
    /// It has metadata profiles (Lidarr): the ones it ships with.
    pub metadata_profiles: Option<&'static [&'static str]>,
    /// The library lists that say what uses a profile, tried in order: `(endpoint, noun)`.
    pub library: &'static [(&'static str, &'static str)],
    /// What its auto tagging tags, for the plan's notes ("movies").
    pub auto_tag_noun: &'static str,
    /// The type of the auto tagging fields that hold tags ("movieTag").
    pub tag_field: Option<&'static str>,
    /// Its items' list in `tag/detail`, and what one is called: `("movieIds", "movie")`.
    pub tag_use: Option<(&'static str, &'static str)>,
    /// Where `parse` puts what it read from a release name ("parsedMovieInfo").
    pub parsed_info: Option<&'static str>,
    /// Its own media management options, besides the ones every arr has.
    pub media_fields: &'static [&'static str],
}

/// Every arr's [`ArrDef`], in the registry's order.
pub fn all() -> impl Iterator<Item = &'static ArrDef> {
    super::ALL.iter().filter_map(|d| d.arr)
}

/// Every arr's client: the one arr implementation.
fn build(app: &AppConfig) -> Result<AnyClient> {
    Ok(AnyClient::new(ArrClient::new(&app.name, &app.kind, &app.url, &app.api_key_blocking()?, app.snapshot_source())?))
}

/// The parts every arr shares; each arr's file fills in the rest.
pub(crate) const fn def(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    image: &'static str,
    ports: &'static [Port],
    arr: &'static ArrDef,
    wizard: bool,
) -> ServiceDef {
    let library = arr.prowlarr_impl.is_some();
    ServiceDef {
        id,
        label,
        description,
        roles: if library { Roles::ARR.union(Roles::LIBRARY) } else { Roles::ARR.union(Roles::INDEXERS) },
        home_group: if library { HomeGroup::Media } else { HomeGroup::Metadata },
        color: "",
        primary_count: "",
        logo_paths: &["/Content/Images/logo.svg", "/Content/Images/Icons/android-chrome-192x192.png"],
        status_page: Some("/system/status"),
        container: Container {
            image,
            ports,
            config_mount: "/config",
            env: &[],
            run_as_user: false,
            data: true,
            vpn: true,
        },
        copy: ARR_COPY,
        credentials: Credentials::ApiKey { file: KeyFile::XmlTag("ApiKey") },
        backup: BackupMode::ApiZip,
        database: Some(Database::IN_CONFIG),
        proxy: ProxyTraits {
            url_base: Some(ARR_URL_BASE),
            own_name: false,
            own_login: false,
            public_preset: true,
            host_check: false,
            public_note: None,
        },
        wizard: if wizard { Some(Wizard { group: "media", config: Some("config.xml") }) } else { None },
        takes: None,
        arr: Some(arr),
        downloads: None,
        login: Some(super::login::Login::Managed(login::LOGIN)),
        feeds: if library { &[FeedKind::Upcoming, FeedKind::Missing] } else { &[FeedKind::Indexers] },
        build,
    }
}

/// `def` with this arr's colour and headline count.
pub(crate) const fn with_ui(mut d: ServiceDef, color: &'static str, primary_count: &'static str) -> ServiceDef {
    d.color = color;
    d.primary_count = primary_count;
    d
}

/// `def` with its own copy rules.
pub(crate) const fn with_copy(mut d: ServiceDef, copy: CopyRules) -> ServiceDef {
    d.copy = copy;
    d
}

/// The arr's API version (`v3`); None if `kind` isn't an arr.
pub fn api_version(kind: &str) -> Option<&'static str> {
    Some(crate::get(kind)?.arr?.api_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(id: &str) -> &'static ArrDef {
        super::super::get(id).and_then(|d| d.arr).expect("an arr")
    }

    fn library(id: &str) -> &'static Library {
        match &arr(id).collection {
            Collection::Library(l) => l,
            Collection::Counts(_) => panic!("{id} has no library"),
        }
    }

    fn lists(id: &str) -> &'static Lists {
        arr(id).lists.as_ref().expect("lists")
    }

    #[test]
    fn the_family_is_consistent() {
        for d in super::super::with(Roles::ARR) {
            let a = d.arr.expect("arr data");
            let lib = d.has(Roles::LIBRARY);
            assert_eq!(lib, matches!(a.collection, Collection::Library(_)), "{}: a library is counted as one", d.id);
            assert_eq!(lib, a.lists.is_some(), "{}: a library has a calendar", d.id);
            assert_eq!(lib, d.feeds == [FeedKind::Upcoming, FeedKind::Missing], "{}", d.id);
            assert_eq!(!lib, d.feeds == [FeedKind::Indexers], "{}", d.id);
            if let Some(l) = &a.lists {
                assert!(!l.records.is_empty(), "{}", d.id);
            }
            if let Some(c) = &a.community {
                assert!(lib, "{}: configs go to a library", d.id);
                assert!(!c.library.is_empty() && !c.quality_profiles.is_empty(), "{}", d.id);
            }
            assert!(!a.scope.is_empty() && !a.scope.contains(&"customformat"), "{}: that's scope_custom_formats", d.id);
        }
        assert_eq!(all().count(), 6);
    }

    #[test]
    fn libraries_count_like_the_arrs() {
        let films = [
            json!({"hasFile": true, "monitored": true, "sizeOnDisk": 10}),
            json!({"hasFile": false, "monitored": true, "isAvailable": true}),
            json!({"monitored": true, "isAvailable": false}),
            json!({"monitored": false, "sizeOnDisk": 5}),
            json!({"monitored": true}),
        ];
        assert_eq!(
            library("radarr").counts(&films).to_string(),
            r#"{"movies":5,"with_file":1,"missing":2,"size_on_disk":15}"#,
            "keys in order; not out yet isn't missing"
        );
        assert_eq!(
            library("whisparr").counts(&[]),
            json!({"movies": 0, "with_file": 0, "missing": 0, "size_on_disk": 0})
        );
        let shows = [
            json!({"statistics": {"episodeCount": 10, "episodeFileCount": 4, "sizeOnDisk": 100}}),
            json!({"statistics": {"episodeCount": 1, "episodeFileCount": 3}}),
            json!({}),
        ];
        assert_eq!(
            library("sonarr").counts(&shows).to_string(),
            r#"{"series":3,"episodes":11,"episode_files":7,"missing":6,"size_on_disk":100}"#,
            "more files than episodes is no missing ones"
        );
        let artists =
            [json!({"statistics": {"albumCount": 2, "trackCount": 20, "trackFileCount": 18, "sizeOnDisk": 7}})];
        assert_eq!(
            library("lidarr").counts(&artists).to_string(),
            r#"{"artists":1,"albums":2,"tracks":20,"track_files":18,"size_on_disk":7}"#
        );
        let authors = [json!({"statistics": {"bookCount": 3, "bookFileCount": 1}})];
        assert_eq!(
            library("readarr").counts(&authors).to_string(),
            r#"{"authors":1,"books":3,"book_files":1,"size_on_disk":0}"#
        );
        assert!(matches!(arr("prowlarr").collection, Collection::Counts([("indexers", "indexer"), ..])));
    }

    #[test]
    fn records_are_told_apart_by_their_keys() {
        let ep = json!({"series": {"title": "S"}});
        let film = json!({"title": "F", "year": 2026});
        assert_eq!(lists("sonarr").record(&ep), Record::Episode);
        assert_eq!(lists("sonarr").record(&json!({"seriesId": 3})), Record::Episode);
        assert_eq!(lists("radarr").record(&film), Record::Movie);
        assert_eq!(lists("lidarr").record(&json!({"artistId": 1})), Record::Album);
        assert_eq!(lists("lidarr").record(&json!({})), Record::Album, "none of the keys: its only kind");
        assert_eq!(lists("readarr").record(&json!({"author": {}})), Record::Book);
        // Whisparr: v2 episodes, v3 films
        assert_eq!(lists("whisparr").record(&ep), Record::Episode);
        assert_eq!(lists("whisparr").record(&json!({"seriesId": 3})), Record::Episode);
        assert_eq!(lists("whisparr").record(&film), Record::Movie);
    }

    #[test]
    fn what_differs_in_the_lists() {
        let dated: Vec<&str> = all()
            .filter(|a| a.lists.as_ref().is_some_and(|l| l.calendar == Calendar::Releases))
            .filter_map(|a| a.prowlarr_impl)
            .collect();
        assert_eq!(dated, ["Radarr", "Lidarr", "Readarr"]);
        assert_eq!(lists("sonarr").missing_sort, "episodes.airDateUtc");
        assert_eq!(lists("whisparr").missing_sort, "episodes.airDateUtc");
        assert_eq!(lists("radarr").missing_sort, "releaseDate");
        let dates: Vec<&str> = Record::Movie.dates().iter().map(|d| d.0).collect();
        assert_eq!(dates, ["inCinemas", "digitalRelease", "physicalRelease"]);
        assert!(Record::Episode.dates().is_empty() && Record::Album.dates().is_empty());
    }
}
