// SPDX-License-Identifier: GPL-3.0-or-later
//! The account's password: the stored scrypt hash (`scrypt$N$r$p$salt$hash`, a
//! fixed format, so an existing account keeps working), the bounded pool that
//! computes it, and the rules a new password and username must meet.

use std::sync::{Condvar, Mutex, PoisonError};

use super::support::{b64u, ct_eq, guard, unb64u};
use crate::error::{Error, Result};
use crate::ids::random_bytes;

const MIN_PASSWORD: usize = 12;
const COMMON: &[&str] = &[
    "password",
    "passwords",
    "password1",
    "password123",
    "123456789012",
    "qwertyuiopas",
    "letmein12345",
    "administrator",
    "wranglarr123",
    "changemeplease",
    "iloveyou1234",
    "welcome12345",
    "adminadmin12",
    "qwerty123456",
    "1q2w3e4r5t6y",
    "passw0rd1234",
    "trustno1trustno1",
    "abc123abc123",
];

const SCRYPT_LOG_N: u8 = 15;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
/// Parameters needing more than 64 MiB are refused (OpenSSL's `maxmem` for the same hash).
const SCRYPT_MAXMEM: u128 = 64 * 1024 * 1024;

/// scrypt computations at once (32 MiB each), and how many more may wait for one: a
/// burst of sign-ins (most of them unauthenticated) can't take the memory or the
/// blocking-thread pool with it.
const HASHING_AT_ONCE: usize = 2;
const HASHING_QUEUE: usize = 16;
const BUSY: &str = "wranglarr is busy checking other sign-ins; try again in a moment";

/// A counting semaphore for blocking threads, with a bounded queue.
struct HashGate {
    slots: Mutex<Slots>,
    freed: Condvar,
    at_once: usize,
    queue: usize,
}

struct Slots {
    running: usize,
    waiting: usize,
}

struct HashPermit<'a>(&'a HashGate);

impl HashGate {
    const fn new(at_once: usize, queue: usize) -> HashGate {
        HashGate { slots: Mutex::new(Slots { running: 0, waiting: 0 }), freed: Condvar::new(), at_once, queue }
    }

    /// Wait for a slot, or 429 when the queue is full too.
    fn enter(&self) -> Result<HashPermit<'_>> {
        let mut s = guard(&self.slots);
        if s.running >= self.at_once {
            if s.waiting >= self.queue {
                return Err(Error::TooMany(BUSY.into()));
            }
            s.waiting += 1;
            while s.running >= self.at_once {
                s = self.freed.wait(s).unwrap_or_else(PoisonError::into_inner);
            }
            s.waiting -= 1;
        }
        s.running += 1;
        Ok(HashPermit(self))
    }
}

impl Drop for HashPermit<'_> {
    fn drop(&mut self) {
        guard(&self.0.slots).running -= 1;
        self.0.freed.notify_one();
    }
}

static HASHING: HashGate = HashGate::new(HASHING_AT_ONCE, HASHING_QUEUE);

/// The derived key, or None for parameters it doesn't run (over
/// [`SCRYPT_MAXMEM`], or not valid scrypt parameters).
fn scrypt_raw(pw: &str, salt: &[u8], log_n: u8, r: u32, p: u32) -> Result<Option<[u8; 32]>> {
    // OpenSSL's memory estimate: B (p*128*r) + V (128*r*(N+2))
    let mem = 128u128 * u128::from(r) * (u128::from(p) + (1u128 << log_n) + 2);
    if mem > SCRYPT_MAXMEM {
        return Ok(None);
    }
    let Ok(params) = scrypt::Params::new(log_n, r, p) else { return Ok(None) };
    let _permit = HASHING.enter()?;
    let mut out = [0u8; 32];
    Ok(scrypt::scrypt(pw.as_bytes(), salt, &params, &mut out).ok().map(|()| out))
}

/// A new `scrypt$N$r$p$salt$hash`. An error, never a made-up hash, when it can't be computed.
pub fn hash_password(pw: &str) -> Result<String> {
    let salt = random_bytes(16);
    let h = scrypt_raw(pw, &salt, SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P)?
        .ok_or_else(|| Error::internal("the password couldn't be hashed"))?;
    Ok(format!("scrypt${}${SCRYPT_R}${SCRYPT_P}${}${}", 1u64 << SCRYPT_LOG_N, b64u(&salt), b64u(&h)))
}

/// Whether `pw` matches `stored` (false for a malformed hash). An error only when
/// the check couldn't run (too busy).
pub fn check_password(pw: &str, stored: &str) -> Result<bool> {
    let parts: Vec<&str> = stored.split('$').collect();
    let [_, n, r, p, salt, h] = parts[..] else { return Ok(false) };
    let (Ok(n), Ok(r), Ok(p)) = (n.trim().parse::<u64>(), r.trim().parse::<u32>(), p.trim().parse::<u32>()) else {
        return Ok(false);
    };
    if n < 2 || !n.is_power_of_two() {
        return Ok(false);
    }
    let (Some(salt), Some(want)) = (unb64u(salt), unb64u(h)) else { return Ok(false) };
    Ok(scrypt_raw(pw, &salt, n.trailing_zeros() as u8, r, p)?.is_some_and(|got| ct_eq(&got, &want)))
}

/// Checked when there's no such user, so that is as slow as a wrong password. Well
/// formed with the usual parameters; nothing hashes to it.
pub(super) const DUMMY_HASH: &str =
    "scrypt$32768$8$1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

pub fn password_problem(pw: &str, username: &str) -> Option<String> {
    let n = pw.chars().count();
    if n < MIN_PASSWORD {
        return Some(format!("use at least {MIN_PASSWORD} characters"));
    }
    if n > 1024 {
        return Some("that's too long (1024 characters at most)".into());
    }
    let low = pw.to_lowercase();
    let distinct: std::collections::HashSet<char> = pw.chars().collect();
    if COMMON.contains(&low.as_str()) || distinct.len() < 5 {
        return Some("that password is too easy to guess".into());
    }
    if !username.is_empty() && low.contains(&username.to_lowercase()) {
        return Some("don't put the username in the password".into());
    }
    None
}

pub fn valid_username(u: &str) -> bool {
    let n = u.chars().count();
    (1..=64).contains(&n) && u.chars().all(|c| c.is_alphanumeric() || "._-@".contains(c))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stored_hashes_check() {
        // the stored format: scrypt$N$r$p$<salt>$<key>, base64url without padding, 32-byte key
        let h = "scrypt$32768$8$1$AAECAwQFBgcICQoLDA0ODw$xy6UCICz8Vv_P6JqOkkUD4DOmhxN0tXSe6sQAUBxupQ";
        assert!(check_password("correct horse battery", h).unwrap());
        assert!(!check_password("correct horse batterz", h).unwrap());
        let h2 = "scrypt$16384$8$2$__7__v_-__7__v_-__7__g$ABipokUeISudlGM5pN3Ob87GxwlInd4M5AYN95fC5tg";
        assert!(check_password("pässwörd ünïcode 🔑", h2).unwrap());
        assert!(!check_password("x", "scrypt$1000$8$1$AAAA$AAAA").unwrap(), "N not a power of two");
        assert!(!check_password("x", "garbage").unwrap());
        assert!(!check_password("x", "scrypt$1048576$8$1$AAAA$AAAA").unwrap(), "over maxmem");
        // the stand-in for "no such user" is well formed (so it costs a real check) and matches nothing
        assert!(unb64u(DUMMY_HASH.split('$').nth(4).unwrap()).is_some_and(|s| s.len() == 16));
        assert!(!check_password("", DUMMY_HASH).unwrap());
    }

    #[test]
    fn new_hashes_have_the_stored_format() {
        let h = hash_password("a long passphrase").unwrap();
        let p: Vec<&str> = h.split('$').collect();
        assert_eq!(&p[..4], &["scrypt", "32768", "8", "1"]);
        assert_eq!(p[4].len(), 22);
        assert_eq!(p[5].len(), 43);
        assert!(check_password("a long passphrase", &h).unwrap());
    }

    #[test]
    fn hashing_is_bounded() {
        let gate = std::sync::Arc::new(HashGate::new(2, 3));
        let (running, most) = (std::sync::Arc::new(Mutex::new(0usize)), std::sync::Arc::new(Mutex::new(0usize)));
        let threads: Vec<_> = (0..5)
            .map(|_| {
                let (gate, running, most) = (gate.clone(), running.clone(), most.clone());
                std::thread::spawn(move || {
                    let _p = gate.enter().unwrap();
                    let n = {
                        let mut r = running.lock().unwrap();
                        *r += 1;
                        *r
                    };
                    let mut m = most.lock().unwrap();
                    *m = (*m).max(n);
                    drop(m);
                    std::thread::sleep(Duration::from_millis(50));
                    *running.lock().unwrap() -= 1;
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(*most.lock().unwrap(), 2, "never more than two at once");
        // a full queue answers 429 instead of waiting
        let full = HashGate::new(1, 0);
        let _held = full.enter().unwrap();
        assert!(matches!(full.enter(), Err(Error::TooMany(ref m)) if m == BUSY));
    }

    #[test]
    fn password_rules() {
        assert!(password_problem("short", "").unwrap().contains("12"));
        assert_eq!(password_problem("aaaaaaaaaaaaaaab", "").unwrap(), "that password is too easy to guess");
        assert_eq!(password_problem("Password123", "").unwrap(), "use at least 12 characters");
        assert_eq!(password_problem("my admin passphrase", "admin").unwrap(), "don't put the username in the password");
        assert!(password_problem("correct horse battery", "admin").is_none());
        assert!(valid_username("admin@home.lan") && !valid_username("") && !valid_username("a b"));
    }
}
