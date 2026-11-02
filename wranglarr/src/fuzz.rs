// SPDX-License-Identifier: GPL-3.0-or-later
//! Entry points for the fuzz targets in `fuzz/` (the `fuzzing` feature): the
//! parsers of text wranglarr gets from outside that aren't public otherwise.

/// A community config as pasted or fetched; true when it was accepted.
pub fn community_config(text: &str) -> bool {
    crate::community::fuzz_parse(text)
}

/// Jackett's list of configured trackers.
pub fn jackett(xml: &str) -> Option<usize> {
    crate::indexers::parse_jackett("jackett", xml).ok().map(|t| t.len())
}

/// A schedule from wranglarr.toml, and when it next fires.
pub fn cron(expr: &str) {
    if let Ok(cron) = crate::sched::parse(expr) {
        // a fixed moment, so a run can be replayed
        use chrono::TimeZone;
        if let Some(at) = chrono::Local.timestamp_opt(1_800_000_000, 0).single() {
            let _ = crate::sched::next_after(&cron, at);
        }
    }
}
