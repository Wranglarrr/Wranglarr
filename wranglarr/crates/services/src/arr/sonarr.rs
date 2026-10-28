// SPDX-License-Identifier: GPL-3.0-or-later
//! Sonarr: TV.

use super::{
    ArrDef, Calendar, Collection, CommunityDef, Events, LIBRARY_SCOPE, Library, Lists, Port, Record, ServiceDef, Total,
    def, radarr, with_ui,
};

static ARR: ArrDef = ArrDef {
    api_version: "v3",
    prowlarr_impl: Some("Sonarr"),
    categories: &[5000, 5010, 5020, 5030, 5040, 5045, 5050, 5090],
    anime_categories: Some(&[5070]),
    category_aliases: &["tv", "series", "shows", "anime"],
    collection: Collection::Library(Library {
        endpoint: "series",
        count: "series",
        totals: &[
            ("episodes", Total::Stat("episodeCount")),
            ("episode_files", Total::Stat("episodeFileCount")),
            ("missing", Total::StatGap("episodeCount", "episodeFileCount")),
            ("size_on_disk", Total::Stat("sizeOnDisk")),
        ],
    }),
    scope: LIBRARY_SCOPE,
    scope_custom_formats: true,
    lists: Some(Lists {
        calendar: Calendar::Episodes,
        records: &[Record::Episode],
        missing_sort: "episodes.airDateUtc",
    }),
    queue_unknown: &["includeUnknownSeriesItems"],
    events: Events { import: &[], added: Some("onSeriesAdd") },
    community: Some(CommunityDef {
        quality_profiles: radarr::QUALITY_PROFILES,
        metadata_profiles: None,
        library: &[("series", "series")],
        auto_tag_noun: "series",
        tag_field: Some("seriesTag"),
        tag_use: Some(("seriesIds", "series")),
        parsed_info: Some("parsedEpisodeInfo"),
        media_fields: &[
            "autoUnmonitorPreviouslyDownloadedEpisodes",
            "createEmptySeriesFolders",
            "episodeTitleRequired",
        ],
    }),
};

pub static DEF: ServiceDef = with_ui(
    def("sonarr", "Sonarr", "TV", "lscr.io/linuxserver/sonarr:latest", &[Port::tcp(8989, 8989)], &ARR, true),
    "#35c5f4",
    "series",
);
