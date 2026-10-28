// SPDX-License-Identifier: GPL-3.0-or-later
//! Prowlarr: indexers, pushed to the library arrs.

use super::{ARR_COPY, ArrDef, Collection, CopyRules, Events, Port, ServiceDef, def, with_copy, with_ui};

static ARR: ArrDef = ArrDef {
    api_version: "v1",
    prowlarr_impl: None,
    categories: &[],
    anime_categories: None,
    category_aliases: &[],
    collection: Collection::Counts(&[("indexers", "indexer"), ("applications", "applications")]),
    scope: &["indexer", "applications", "downloadclient", "notification", "tag", "indexerproxy"],
    scope_custom_formats: false,
    lists: None,
    queue_unknown: &[],
    events: Events { import: &[], added: None },
    community: None,
};

pub static DEF: ServiceDef = with_copy(
    with_ui(
        def(
            "prowlarr",
            "Prowlarr",
            "Indexers",
            "lscr.io/linuxserver/prowlarr:latest",
            &[Port::tcp(9696, 9696)],
            &ARR,
            true,
        ),
        "#e66000",
        "indexers",
    ),
    // indexer definitions are fetched on start; Definitions/Custom is kept
    CopyRules { skip_files_in: &["Definitions"], ..ARR_COPY },
);
