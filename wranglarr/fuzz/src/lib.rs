// SPDX-License-Identifier: GPL-3.0-or-later
//! What each fuzz target checks, as plain functions: the targets in
//! `fuzz_targets/` call these with libFuzzer's input, and `tests/targets.rs`
//! replays every saved input (`corpus/<target>/`) through them on stable Rust.
//!
//! A new target goes in [`TARGETS`], `Cargo.toml` (`[[bin]]`), `fuzz_targets/`
//! and `corpus/` (at least one real example); the test fails until all four agree.

use wranglarr_core::manifest::Manifest;

/// What a target runs on one input.
pub type Check = fn(&[u8]);

/// Every target: its name (the `[[bin]]`, the `fuzz_targets/` file and the
/// `corpus/` folder) and what it runs.
pub const TARGETS: &[(&str, Check)] = &[
    ("community_config", community_config),
    ("jackett_xml", jackett_xml),
    ("backup_manifest", backup_manifest),
    ("wranglarr_toml", wranglarr_toml),
    ("small_parsers", small_parsers),
];

fn text(data: &[u8]) -> Option<&str> {
    std::str::from_utf8(data).ok()
}

/// A community config pasted on the page or fetched from a provider: checking
/// one never panics, whatever the text.
pub fn community_config(data: &[u8]) {
    if let Some(t) = text(data) {
        let _ = wranglarr::fuzz::community_config(t);
    }
}

/// Jackett's answer listing its trackers (XML from another container).
pub fn jackett_xml(data: &[u8]) {
    if let Some(t) = text(data) {
        let _ = wranglarr::fuzz::jackett(t);
    }
}

/// A backup's manifest.json, which may come from another machine or an older
/// wranglarr: reading it and asking it questions never panics (sizes included),
/// and one that reads writes back to the same thing.
pub fn backup_manifest(data: &[u8]) {
    let Ok(m) = serde_json::from_slice::<Manifest>(data) else { return };
    let _ = (m.bytes(), m.pre_update_kept_apart());
    for app in m.apps.values() {
        let _ = (app.ok(), app.restorable(), app.database_bytes());
    }
    let written = serde_json::to_string(&m).expect("a manifest that was read can be written");
    let again: Manifest = serde_json::from_str(&written).expect("what was written reads back");
    assert_eq!(serde_json::to_value(&again).ok(), serde_json::to_value(&m).ok());
}

/// wranglarr.toml as a user writes it, with `${VAR}` references left unset.
pub fn wranglarr_toml(data: &[u8]) {
    if let Some(t) = text(data) {
        let _ = wranglarr_services::apps::parse(t, |_| None);
    }
}

/// The short strings wranglarr reads from elsewhere; the first byte picks which.
pub fn small_parsers(data: &[u8]) {
    let Some((&which, rest)) = data.split_first() else { return };
    let Some(s) = text(rest) else { return };
    match which % 4 {
        // an image reference from a compose file or the registry
        0 => {
            let _ = wranglarr_core::registry::parse(s);
        }
        // a date in an app's API answer or feed
        1 => {
            let _ = wranglarr_services::feed::parse(s);
        }
        // a schedule in wranglarr.toml
        2 => wranglarr::fuzz::cron(s),
        // the stack's .env
        _ => {
            let _ = wranglarr::setup::env::parse_env(s);
        }
    }
}
