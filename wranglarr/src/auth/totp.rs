// SPDX-License-Identifier: GPL-3.0-or-later
//! Two-factor sign-in: TOTP codes (RFC 6238, as every authenticator app makes
//! them), the single-use recovery codes, the QR code for setting it up, and the
//! account's TOTP settings.

use data_encoding::BASE32;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};

use super::account::Totp;
use super::signin::Confirmed;
use super::support::{ct_eq, sha};
use super::{Auth, ISSUER};
use crate::error::{Error, Result};
use crate::format::{quote, quote_path};
use crate::ids::random_bytes;
use crate::time::unix_now;

/// The 6-digit code for one 30 s step (HMAC-SHA1, like every authenticator app).
pub fn totp_at(secret_b32: &str, counter: i64) -> Option<String> {
    let pad = (8 - secret_b32.len() % 8) % 8;
    let key = BASE32.decode(format!("{secret_b32}{}", "=".repeat(pad)).as_bytes()).ok()?;
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&key).ok()?;
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = usize::from(h[h.len() - 1] & 0x0F);
    let v = u32::from_be_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]) & 0x7FFF_FFFF;
    Some(format!("{:06}", v % 1_000_000))
}

/// The time step the code is for (one step of clock drift either way), if it's
/// valid and newer than `after` (so a code can't be used twice).
pub fn totp_match(secret_b32: &str, code: &str, after: i64, now: Option<f64>) -> Option<i64> {
    let code: String = code.chars().filter(char::is_ascii_digit).collect();
    if code.len() != 6 {
        return None;
    }
    let t = (now.unwrap_or_else(unix_now) / 30.0).floor() as i64;
    (t - 1..=t + 1)
        .find(|&c| c > after && totp_at(secret_b32, c).is_some_and(|want| ct_eq(want.as_bytes(), code.as_bytes())))
}

pub fn new_recovery_codes(n: usize) -> Vec<String> {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    (0..n)
        .map(|_| {
            (0..3)
                .map(|_| (0..4).map(|_| ALPHABET[rand::random_range(0..ALPHABET.len())] as char).collect::<String>())
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect()
}

pub fn norm_recovery(code: &str) -> String {
    code.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// An `otpauth://` QR code as inline SVG (5 px modules, a 2-module border).
pub fn qr_svg(data: &str) -> Option<String> {
    let code = qrcode::QrCode::with_error_correction_level(data.as_bytes(), qrcode::EcLevel::M).ok()?;
    let w = code.width();
    let colors = code.to_colors();
    let (border, scale) = (2usize, 5usize);
    let size = (w + 2 * border) * scale;
    let mut d = String::new();
    for y in 0..w {
        let mut x = 0;
        while x < w {
            if colors[y * w + x] == qrcode::Color::Dark {
                let start = x;
                while x < w && colors[y * w + x] == qrcode::Color::Dark {
                    x += 1;
                }
                d.push_str(&format!("M{} {}h{}v1h-{}z", start + border, y + border, x - start, x - start));
            } else {
                x += 1;
            }
        }
    }
    let n = w + 2 * border;
    Some(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {n} {n}" class="segno" shape-rendering="crispEdges"><rect width="{n}" height="{n}" fill="#fff"/><path fill="#000" d="{d}"/></svg>"##
    ))
}

impl Auth {
    pub fn totp_begin(&self, ok: &Confirmed) -> Result<Value> {
        let secret = BASE32.encode(&random_bytes(20)).trim_end_matches('=').to_string();
        let (ok, pending) = (ok.clone(), secret.clone());
        let username = self.update_account(move |_, acct| {
            ok.check(acct)?;
            acct.totp_pending = Some(pending);
            Ok(acct.username.clone())
        })?;
        let uri = format!(
            "otpauth://totp/{}:{}?secret={secret}&issuer={}&algorithm=SHA1&digits=6&period=30",
            quote(ISSUER),
            quote_path(&username),
            quote(ISSUER)
        );
        let svg = qr_svg(&uri).unwrap_or_default();
        let grouped: Vec<String> =
            secret.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
        Ok(json!({"secret": grouped.join(" "), "uri": uri, "qr_svg": svg}))
    }

    pub fn totp_enable(&self, code: &str) -> Result<Vec<String>> {
        let codes = new_recovery_codes(10);
        let (code, recovery) = (code.to_owned(), recovery_hashes(&codes));
        self.update_account(move |_, acct| {
            let Some(secret) = acct.totp_pending.clone() else {
                return Err(Error::invalid("start the setup again"));
            };
            let Some(step) = totp_match(&secret, &code, 0, None) else {
                return Err(Error::invalid("that code isn't right; check the time on your phone"));
            };
            acct.totp = Totp { enabled: true, secret, last: step };
            acct.recovery = recovery;
            acct.totp_pending = None;
            Ok(())
        })?;
        tracing::info!("auth: two-factor (TOTP) turned on");
        Ok(codes)
    }

    pub fn totp_disable(&self, ok: &Confirmed) -> Result<()> {
        let ok = ok.clone();
        self.update_account(move |_, acct| {
            ok.check(acct)?;
            acct.totp = Totp::default();
            acct.recovery = vec![];
            Ok(())
        })?;
        tracing::info!("auth: two-factor (TOTP) turned off");
        Ok(())
    }

    /// Turn two-factor off without the password (`wranglarr auth disable-2fa`).
    pub fn force_totp_off(&self) -> Result<()> {
        self.update_account(|_, acct| {
            acct.totp = Totp::default();
            acct.recovery = vec![];
            Ok(())
        })
    }

    pub fn new_recovery(&self, ok: &Confirmed) -> Result<Vec<String>> {
        let codes = new_recovery_codes(10);
        let (ok, recovery) = (ok.clone(), recovery_hashes(&codes));
        self.update_account(move |_, acct| {
            ok.check(acct)?;
            if !acct.totp.enabled {
                return Err(Error::invalid("turn on two-factor first"));
            }
            acct.recovery = recovery;
            Ok(())
        })?;
        Ok(codes)
    }
}

/// What the account stores of its recovery codes.
fn recovery_hashes(codes: &[String]) -> Vec<String> {
    codes.iter().map(|c| sha(&norm_recovery(c))).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn totp_rfc6238_vectors() {
        // RFC 6238 appendix B (SHA-1), last six digits
        let secret = BASE32.encode(b"12345678901234567890").trim_end_matches('=').to_string();
        for (t, want) in [
            (59i64, "287082"),
            (1111111109, "081804"),
            (1111111111, "050471"),
            (1234567890, "005924"),
            (2000000000, "279037"),
            (20000000000, "353130"),
        ] {
            assert_eq!(totp_at(&secret, t / 30).unwrap(), want, "t={t}");
        }
        let now = 1111111111.0;
        let step = 1111111111 / 30;
        assert_eq!(totp_match(&secret, "050 471", 0, Some(now)), Some(step));
        assert_eq!(totp_match(&secret, "050471", step, Some(now)), None, "no replay");
        let next = totp_at(&secret, step + 1).unwrap();
        assert_eq!(totp_match(&secret, &next, step, Some(now)), Some(step + 1), "one step of drift");
        let far = totp_at(&secret, step + 2).unwrap();
        assert_eq!(totp_match(&secret, &far, 0, Some(now)), None);
        assert_eq!(totp_match(&secret, "12345", 0, Some(now)), None);
    }

    #[test]
    fn recovery_codes() {
        let c = new_recovery_codes(10);
        assert_eq!(c.len(), 10);
        assert!(c.iter().all(|c| c.len() == 14 && c.matches('-').count() == 2));
        assert_eq!(norm_recovery("ABCD-2345 wxyz"), "abcd2345wxyz");
        // SHA-256 of "abcd2345wxyz", hex
        assert_eq!(sha("abcd2345wxyz"), "b337444efd14a226c60c1893e4ee99404c1b869e8d1add9bb6d397d609623c5d");
    }
}
