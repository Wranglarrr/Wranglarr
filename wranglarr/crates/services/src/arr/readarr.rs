// SPDX-License-Identifier: GPL-3.0-or-later
//! Readarr: books. Not offered by the setup wizard (retired upstream; its
//! default port is wranglarr's own).

use super::{
    ArrDef, Calendar, Collection, Events, LIBRARY_SCOPE, Library, Lists, Port, Record, ServiceDef, Total, def, with_ui,
};

static ARR: ArrDef = ArrDef {
    api_version: "v1",
    prowlarr_impl: Some("Readarr"),
    categories: &[3030, 7000, 7010, 7020, 7030, 7040, 7050, 7060],
    anime_categories: None,
    category_aliases: &["books", "ebooks", "audiobooks"],
    collection: Collection::Library(Library {
        endpoint: "author",
        count: "authors",
        totals: &[
            ("books", Total::Stat("bookCount")),
            ("book_files", Total::Stat("bookFileCount")),
            ("size_on_disk", Total::Stat("sizeOnDisk")),
        ],
    }),
    scope: LIBRARY_SCOPE,
    scope_custom_formats: false,
    lists: Some(Lists { calendar: Calendar::Releases, records: &[Record::Book], missing_sort: "releaseDate" }),
    queue_unknown: &[],
    events: Events { import: &["onBookFileImport"], added: Some("onAuthorAdded") },
    community: None,
};

pub static DEF: ServiceDef = with_ui(
    def("readarr", "Readarr", "Books", "lscr.io/linuxserver/readarr:develop", &[Port::tcp(8787, 8787)], &ARR, false),
    "#8e2222",
    "authors",
);
