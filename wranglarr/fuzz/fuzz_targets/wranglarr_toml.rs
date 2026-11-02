// SPDX-License-Identifier: GPL-3.0-or-later
//! See [`wranglarr_fuzz::wranglarr_toml`] for what this checks.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| wranglarr_fuzz::wranglarr_toml(data));
