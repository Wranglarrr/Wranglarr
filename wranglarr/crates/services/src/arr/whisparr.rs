// SPDX-License-Identifier: GPL-3.0-or-later
//! Whisparr: adult films. Not offered by the setup wizard.
//!
//! v3 is built on Radarr (films), v2 on Sonarr (episodes): its calendar and
//! Wanted → Missing can hold either, and community configs look in both libraries.

use super::{
    ArrDef, Calendar, Collection, CommunityDef, Events, LIBRARY_SCOPE, Lists, Port, Record, ServiceDef, def, radarr,
    with_ui,
};

static ARR: ArrDef = ArrDef {
    api_version: "v3",
    prowlarr_impl: Some("Whisparr"),
    categories: &[6000, 6010, 6020, 6030, 6040, 6045, 6050, 6070, 6080, 6090],
    anime_categories: None,
    category_aliases: &["xxx"],
    collection: Collection::Library(radarr::FILMS),
    scope: LIBRARY_SCOPE,
    scope_custom_formats: true,
    lists: Some(Lists {
        calendar: Calendar::Episodes,
        records: &[Record::Episode, Record::Movie],
        missing_sort: "episodes.airDateUtc",
    }),
    queue_unknown: &["includeUnknownMovieItems", "includeUnknownSeriesItems"],
    events: Events { import: &[], added: None },
    community: Some(CommunityDef {
        quality_profiles: radarr::QUALITY_PROFILES,
        metadata_profiles: None,
        library: &[("movie", "movies"), ("series", "series")],
        auto_tag_noun: "items",
        tag_field: None,
        tag_use: None,
        parsed_info: None,
        media_fields: &[],
    }),
};

pub static DEF: ServiceDef = with_ui(
    def("whisparr", "Whisparr", "Adult", "ghcr.io/hotio/whisparr:latest", &[Port::tcp(6969, 6969)], &ARR, false),
    "#ff69b4",
    "movies",
);
