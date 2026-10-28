// SPDX-License-Identifier: GPL-3.0-or-later
//! Random ids and tokens: ids for the things wranglarr stores (channels, media
//! servers, indexer sources, history entries, API keys) and secrets it hands
//! out (session tokens, setup codes). All come from the OS's random source.

use data_encoding::BASE64URL_NOPAD;

/// `n` random bytes (salts, secrets, handles).
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    rand::fill(&mut b[..]);
    b
}

/// `n` random bytes as lower-case hex: `token_hex(4)` is 8 characters.
pub fn token_hex(n: usize) -> String {
    hex::encode(random_bytes(n))
}

/// `n` random bytes as URL-safe base64 without padding: `random_token(18)` is 24 characters.
pub fn random_token(n: usize) -> String {
    BASE64URL_NOPAD.encode(&random_bytes(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_of_the_length_asked() {
        let t = token_hex(4);
        assert_eq!(t.len(), 8);
        assert!(t.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(token_hex(8), token_hex(8));
    }

    #[test]
    fn url_safe_tokens() {
        let t = random_token(18);
        assert_eq!(t.len(), 24);
        assert!(t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(random_token(18), random_token(18));
    }
}
