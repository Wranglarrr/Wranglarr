// SPDX-License-Identifier: GPL-3.0-or-later
//! Lidarr: music.

use super::{
    ArrDef, Calendar, Collection, CommunityDef, Events, LIBRARY_SCOPE, Library, Lists, Port, Record, ServiceDef, Total,
    def, with_ui,
};

static ARR: ArrDef = ArrDef {
    api_version: "v1",
    prowlarr_impl: Some("Lidarr"),
    categories: &[3000, 3010, 3030, 3040, 3050, 3060],
    anime_categories: None,
    category_aliases: &["music", "audio"],
    collection: Collection::Library(Library {
        endpoint: "artist",
        count: "artists",
        totals: &[
            ("albums", Total::Stat("albumCount")),
            ("tracks", Total::Stat("trackCount")),
            ("track_files", Total::Stat("trackFileCount")),
            ("size_on_disk", Total::Stat("sizeOnDisk")),
        ],
    }),
    scope: LIBRARY_SCOPE,
    scope_custom_formats: false,
    lists: Some(Lists { calendar: Calendar::Releases, records: &[Record::Album], missing_sort: "releaseDate" }),
    queue_unknown: &[],
    events: Events { import: &["onReleaseImport"], added: Some("onArtistAdd") },
    community: Some(CommunityDef {
        quality_profiles: &["any", "lossless", "standard"],
        metadata_profiles: Some(&["standard", "none"]),
        library: &[("artist", "artists")],
        auto_tag_noun: "artists",
        tag_field: Some("artistTag"),
        tag_use: Some(("artistIds", "artist")),
        parsed_info: Some("parsedAlbumInfo"),
        media_fields: &["autoUnmonitorPreviouslyDownloadedTracks", "createEmptyArtistFolders"],
    }),
};

pub static DEF: ServiceDef = with_ui(
    def("lidarr", "Lidarr", "Music", "lscr.io/linuxserver/lidarr:latest", &[Port::tcp(8686, 8686)], &ARR, true),
    "#00a65b",
    "artists",
);
