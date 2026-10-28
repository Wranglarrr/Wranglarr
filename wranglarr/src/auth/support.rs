// SPDX-License-Identifier: GPL-3.0-or-later
//! Small pieces the auth modules share: base64url, SHA-256, constant-time
//! comparison, and lock and map helpers. The clock is `crate::time`, random
//! tokens `crate::ids`.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use data_encoding::BASE64URL_NOPAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// URL-safe base64 without padding.
pub fn b64u(b: &[u8]) -> String {
    BASE64URL_NOPAD.encode(b)
}

pub fn unb64u(s: &str) -> Option<Vec<u8>> {
    BASE64URL_NOPAD.decode(s.trim_end_matches('=').as_bytes()).ok()
}

/// Hex SHA-256 of a string.
pub fn sha(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub(super) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// Constant-time string comparison, for secrets.
pub fn ct_eq_str(a: &str, b: &str) -> bool {
    ct_eq(a.as_bytes(), b.as_bytes())
}

/// Keep at most `keep` entries, dropping those with the smallest `when` first.
pub(super) fn trim<V>(m: &mut HashMap<String, V>, keep: usize, when: impl Fn(&V) -> f64) {
    while m.len() > keep {
        let Some(k) = m.iter().min_by(|a, b| when(a.1).total_cmp(&when(b.1))).map(|(k, _)| k.clone()) else { break };
        m.remove(&k);
    }
}

/// A lock's contents, also after a panic elsewhere held it.
pub(super) fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
