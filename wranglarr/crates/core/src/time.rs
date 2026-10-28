// SPDX-License-Identifier: GPL-3.0-or-later
//! Timestamps as wranglarr writes them. There are three spellings, and each is
//! kept where it is stored or compared, so a value never changes shape:
//!
//! - [`iso`]: RFC 3339 to the second, `2026-10-26T09:35:20+00:00`. What the API
//!   shows and the saved settings, channels and logs hold.
//! - [`iso_micros`]: the same with microseconds. Where runs within one second must
//!   still sort: a backup manifest's `started`, update checks, overview fetches.
//! - [`iso_z`]: `2026-10-26T09:35:20Z`. The state database's own columns (runs,
//!   sessions, settings rows, the Community history). Those are compared as text,
//!   so every row has to use the same width.

use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};

/// `t` in RFC 3339 to the second, with its offset (`+00:00` for UTC).
pub fn iso<Tz: TimeZone>(t: &DateTime<Tz>) -> String
where
    Tz::Offset: Display,
{
    t.to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// UTC now, as [`iso`].
pub fn now_iso() -> String {
    iso(&Utc::now())
}

/// `t` in RFC 3339 with microseconds.
pub fn iso_micros<Tz: TimeZone>(t: &DateTime<Tz>) -> String
where
    Tz::Offset: Display,
{
    t.to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// UTC now, as [`iso_micros`].
pub fn now_iso_micros() -> String {
    iso_micros(&Utc::now())
}

/// `t` as `2026-10-26T09:35:20Z`, the state database's spelling.
pub fn iso_z(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// UTC now, as [`iso_z`].
pub fn now_iso_z() -> String {
    iso_z(Utc::now())
}

/// Seconds since the Unix epoch, with the fraction.
pub fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Seconds since the Unix epoch (the fraction dropped) as [`iso`]; "" when out of range.
pub fn unix_iso(secs: f64) -> String {
    DateTime::from_timestamp(secs as i64, 0).map(|d| iso(&d)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings() {
        let t =
            Utc.with_ymd_and_hms(2026, 10, 26, 9, 35, 20).single().expect("time") + chrono::Duration::microseconds(42);
        assert_eq!(iso(&t), "2026-10-26T09:35:20+00:00");
        assert_eq!(iso_micros(&t), "2026-10-26T09:35:20.000042+00:00");
        assert_eq!(iso_z(t), "2026-10-26T09:35:20Z");
        assert_eq!(unix_iso(1_793_007_320.9), "2026-10-26T09:35:20+00:00");
        assert_eq!(now_iso_z().len(), 20);
        assert!(now_iso().ends_with("+00:00") && now_iso_micros().ends_with("+00:00"));
    }
}
