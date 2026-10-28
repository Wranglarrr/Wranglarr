// SPDX-License-Identifier: GPL-3.0-or-later
//! Radarr: movies.

use super::{
    ArrDef, Calendar, Collection, CommunityDef, Events, LIBRARY_SCOPE, Library, Lists, Port, Record, ServiceDef, Total,
    def, with_ui,
};

/// A film library (Whisparr v3's too).
pub(super) const FILMS: Library = Library {
    endpoint: "movie",
    count: "movies",
    totals: &[
        ("with_file", Total::Flagged("hasFile")),
        ("missing", Total::Wanted),
        ("size_on_disk", Total::Sum("sizeOnDisk")),
    ],
};

/// The quality profiles Radarr ships with (Sonarr's and Whisparr's are the same).
pub(super) const QUALITY_PROFILES: &[&str] = &["any", "sd", "hd-720p", "hd-1080p", "ultra-hd", "hd - 720p/1080p"];

static ARR: ArrDef = ArrDef {
    api_version: "v3",
    prowlarr_impl: Some("Radarr"),
    categories: &[2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060, 2070, 2080],
    anime_categories: None,
    category_aliases: &["movies", "movie", "film", "films"],
    collection: Collection::Library(FILMS),
    scope: LIBRARY_SCOPE,
    scope_custom_formats: true,
    lists: Some(Lists { calendar: Calendar::Releases, records: &[Record::Movie], missing_sort: "releaseDate" }),
    queue_unknown: &["includeUnknownMovieItems"],
    events: Events { import: &[], added: Some("onMovieAdded") },
    community: Some(CommunityDef {
        quality_profiles: QUALITY_PROFILES,
        metadata_profiles: None,
        library: &[("movie", "movies")],
        auto_tag_noun: "movies",
        tag_field: Some("movieTag"),
        tag_use: Some(("movieIds", "movie")),
        parsed_info: Some("parsedMovieInfo"),
        media_fields: &["autoUnmonitorPreviouslyDownloadedMovies", "createEmptyMovieFolders"],
    }),
};

pub static DEF: ServiceDef = with_ui(
    def("radarr", "Radarr", "Movies", "lscr.io/linuxserver/radarr:latest", &[Port::tcp(7878, 7878)], &ARR, true),
    "#ffc230",
    "movies",
);
