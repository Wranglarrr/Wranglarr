// SPDX-License-Identifier: GPL-3.0-or-later
//! Signing in: the password step, the second factor, the password typed again
//! before a sensitive change, and the per-address limits all of them share
//! (five failures within 15 minutes lock an address out for 15 minutes).

use std::time::Duration;

use super::password::{DUMMY_HASH, check_password};
use super::support::{ct_eq, ct_eq_str, guard, sha, trim};
use super::totp::{norm_recovery, totp_match};
use super::{Account, Auth, Mem};
use crate::error::{Error, Result};
use crate::ids::random_token;
use crate::time::unix_now;

/// How long a sign-in waits for its TOTP code.
const PENDING_SECONDS: f64 = 300.0;

/// Failed sign-ins from one address ...
const LOCK_AFTER: usize = 5;
/// ... within this window lock it out ...
const LOCK_WINDOW: f64 = 15.0 * 60.0;
/// ... for this long.
const LOCK_FOR: f64 = 15.0 * 60.0;

pub(super) const WRONG_PASSWORD: &str = "the password is wrong";
pub(super) const EXPIRED: &str = "that sign-in expired; start again";
const AT_ONCE: &str = "too many sign-in attempts at once from this address; try again in a moment";

/// Addresses whose failures and lockouts are kept (the oldest go first beyond this).
const MAX_ADDRESSES: usize = 4096;
/// Sign-ins waiting for their code (each needed the right password).
const MAX_PENDING: usize = 256;

/// Password accepted, waiting for the TOTP code.
pub(super) struct Pending {
    expires: f64,
    remember: bool,
    tries: u32,
    /// [`stamp`] of the account when the password was checked.
    stamp: String,
}

/// A successful sign-in step: what the session is made with.
#[derive(Debug, Clone, PartialEq)]
pub enum SignIn {
    /// Signed in: (method, remember).
    Done { method: &'static str, remember: bool },
    /// The password was right; the TOTP step needs this token (the MFA cookie).
    Pending(String),
}

/// The account's password, just typed again ([`Auth::reconfirm`]). Holds the hash
/// it matched, so a password change that lands in between is noticed.
#[derive(Clone)]
pub struct Confirmed {
    hash: String,
}

impl Confirmed {
    pub(super) fn check(&self, acct: &Account) -> Result<()> {
        if acct.password.is_empty() || !ct_eq_str(&acct.password, &self.hash) {
            return Err(Error::Forbidden(WRONG_PASSWORD.into()));
        }
        Ok(())
    }
}

/// Changes whenever the password does, or the account is reset and made again. A
/// sign-in step or passkey ceremony keeps the one it started with and is refused
/// once it differs, even after a change by `wranglarr-auth` in another process.
pub(super) fn stamp(acct: Option<&Account>) -> String {
    acct.map(|a| sha(&format!("{}\n{}", a.password, a.user_handle))).unwrap_or_default()
}

/// A password check from one address, counted against its sign-in limit while it
/// runs ([`Auth::admit`]).
pub struct Attempt<'a> {
    auth: &'a Auth,
    ip: String,
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        let mut m = guard(&self.auth.mem);
        if let Some(n) = m.checking.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.checking.remove(&self.ip);
            }
        }
    }
}

impl Auth {
    /// The account's password, typed again before a sensitive change (403 when
    /// wrong). It shares the sign-in limit: a wrong one counts as a failed sign-in
    /// from `ip`, so a stolen session can't guess its way to an API key.
    pub fn reconfirm(&self, ip: &str, password: &str) -> Result<Confirmed> {
        let _attempt = self.admit(ip)?;
        let acct = self.account()?.unwrap_or_default();
        let stored = if acct.password.is_empty() { DUMMY_HASH } else { acct.password.as_str() };
        if !check_password(password, stored)? || acct.password.is_empty() {
            self.failed(ip);
            return Err(Error::Forbidden(WRONG_PASSWORD.into()));
        }
        self.succeeded(ip);
        Ok(Confirmed { hash: acct.password })
    }

    // ---- throttling --------------------------------------------------------------------
    /// Drop what has expired, and keep the per-address maps bounded.
    pub(super) fn sweep(m: &mut Mem, now: f64) {
        m.pending.retain(|_, v| v.expires >= now);
        m.challenges.retain(|_, v| v.expires >= now);
        m.locked.retain(|_, until| *until > now);
        m.fails.retain(|_, times| {
            times.retain(|t| now - t < LOCK_WINDOW);
            !times.is_empty()
        });
        trim(&mut m.fails, MAX_ADDRESSES, |t| t.last().copied().unwrap_or(0.0));
        trim(&mut m.locked, MAX_ADDRESSES, |until| *until);
    }

    fn lockout(m: &Mem, ip: &str, now: f64) -> Result<()> {
        let until = m.locked.get(ip).copied().unwrap_or(0.0);
        if until > now {
            return Err(Error::TooMany(format!(
                "too many failed sign-ins; try again in {} min (or restart wranglarr to clear it)",
                ((until - now) / 60.0).floor() as i64 + 1
            )));
        }
        Ok(())
    }

    pub fn check_throttle(&self, ip: &str) -> Result<()> {
        let now = unix_now();
        let mut m = guard(&self.mem);
        Self::sweep(&mut m, now);
        Self::lockout(&m, ip, now)
    }

    /// Room for one more password check from `ip`, taken before it hashes: its recent
    /// failures plus the checks still running stay under the lockout limit, so
    /// concurrent guesses can't all start before the first one fails.
    pub fn admit(&self, ip: &str) -> Result<Attempt<'_>> {
        let now = unix_now();
        let mut m = guard(&self.mem);
        Self::sweep(&mut m, now);
        Self::lockout(&m, ip, now)?;
        let recent = m.fails.get(ip).map_or(0, Vec::len);
        let running = m.checking.get(ip).copied().unwrap_or(0);
        if recent + running >= LOCK_AFTER {
            return Err(Error::TooMany(AT_ONCE.into()));
        }
        m.checking.insert(ip.to_string(), running + 1);
        Ok(Attempt { auth: self, ip: ip.to_string() })
    }

    /// A failed sign-in from `ip`: the fifth within the window locks it out.
    /// Sleeps 0.4 s (call it from a blocking thread).
    pub fn failed(&self, ip: &str) {
        let now = unix_now();
        let locked = {
            let mut m = guard(&self.mem);
            Self::sweep(&mut m, now);
            let times = m.fails.entry(ip.to_string()).or_default();
            times.push(now);
            if times.len() >= LOCK_AFTER {
                m.fails.remove(ip);
                m.locked.insert(ip.to_string(), now + LOCK_FOR);
                true
            } else {
                false
            }
        };
        if locked {
            let minutes = (LOCK_FOR / 60.0) as u64;
            tracing::warn!(
                "auth: {ip} locked out for {minutes} min after {LOCK_AFTER} failed sign-ins (restarting wranglarr clears it)"
            );
            if let Some(hook) = guard(&self.on_lockout).as_ref() {
                hook(ip, minutes);
            }
        }
        std::thread::sleep(Duration::from_millis(400));
    }

    pub fn succeeded(&self, ip: &str) {
        guard(&self.mem).fails.remove(ip);
    }

    // ---- password sign-in --------------------------------------------------------------
    pub fn login_password(&self, ip: &str, username: &str, password: &str, remember: bool) -> Result<SignIn> {
        let _attempt = self.admit(ip)?;
        let acct = self.account()?;
        let ok_user = acct
            .as_ref()
            .is_some_and(|a| ct_eq(username.trim().to_lowercase().as_bytes(), a.username.to_lowercase().as_bytes()));
        let stored = match &acct {
            Some(a) if ok_user => a.password.as_str(),
            _ => DUMMY_HASH,
        };
        let ok = check_password(password, stored)? && ok_user;
        let acct = match acct {
            Some(a) if ok => a,
            _ => {
                self.failed(ip);
                return Err(Error::Unauthorized("wrong username or password".into()));
            }
        };
        if acct.totp.enabled {
            let token = random_token(24);
            let now = unix_now();
            let mut m = guard(&self.mem);
            Self::sweep(&mut m, now);
            let p = Pending { expires: now + PENDING_SECONDS, remember, tries: 0, stamp: stamp(Some(&acct)) };
            m.pending.insert(sha(&token), p);
            trim(&mut m.pending, MAX_PENDING, |p| p.expires);
            return Ok(SignIn::Pending(token));
        }
        self.succeeded(ip);
        Ok(SignIn::Done { method: "password", remember })
    }

    pub fn login_second_factor(&self, ip: &str, pending: &str, code: &str) -> Result<SignIn> {
        self.check_throttle(ip)?;
        let key = sha(pending);
        let (remember, started) = {
            let m = guard(&self.mem);
            match m.pending.get(&key) {
                Some(p) if p.expires >= unix_now() => (p.remember, p.stamp.clone()),
                _ => return Err(Error::Unauthorized(EXPIRED.into())),
            }
        };
        let code = code.to_owned();
        let checked = self.update_account(move |_, acct| {
            if !ct_eq_str(&stamp(Some(acct)), &started) {
                return Ok(None); // the password changed, or the account was made again
            }
            let t = &acct.totp;
            let step = if t.enabled { totp_match(&t.secret, &code, t.last, None) } else { None };
            if let Some(step) = step {
                acct.totp.last = step;
                return Ok(Some("password+totp"));
            }
            let h = sha(&norm_recovery(&code));
            if let Some(i) = acct.recovery.iter().position(|r| *r == h) {
                acct.recovery.remove(i);
                tracing::warn!("auth: recovery code used, {} left", acct.recovery.len());
                return Ok(Some("password+recovery"));
            }
            Ok(Some(""))
        });
        let method = match checked {
            Err(Error::NotFound(_)) | Ok(None) => {
                guard(&self.mem).pending.remove(&key);
                return Err(Error::Unauthorized(EXPIRED.into()));
            }
            Err(e) => return Err(e),
            Ok(Some(m)) => m,
        };
        if method.is_empty() {
            {
                let mut m = guard(&self.mem);
                let drop = m.pending.get_mut(&key).map(|p| {
                    p.tries += 1;
                    p.tries >= 5
                });
                if drop == Some(true) {
                    m.pending.remove(&key);
                }
            }
            self.failed(ip);
            return Err(Error::Unauthorized("that code isn't right".into()));
        }
        guard(&self.mem).pending.remove(&key);
        self.succeeded(ip);
        Ok(SignIn::Done { method, remember })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::auth::testing::{auth, with_account};
    use crate::auth::totp::totp_at;

    #[test]
    fn setup_then_login_and_lockout() {
        let a = auth();
        let code = a.ensure_setup_code().unwrap();
        assert_eq!(code.len(), 14);
        assert!(matches!(
            a.create_account("AAAA-BBBB-CCCC", "admin", "correct horse battery"),
            Err(Error::Forbidden(_))
        ));
        a.create_account(&code.to_lowercase(), " admin ", "correct horse battery").unwrap();
        assert!(matches!(a.create_account(&code, "x", "correct horse battery"), Err(Error::Conflict(_))));
        assert_eq!(a.setup_code(), "");
        assert_eq!(
            a.login_password("1.2.3.4", "ADMIN", "correct horse battery", true).unwrap(),
            SignIn::Done { method: "password", remember: true }
        );
        let hits = std::sync::Arc::new(Mutex::new(vec![]));
        let h2 = hits.clone();
        a.set_on_lockout(move |ip, m| h2.lock().unwrap().push((ip.to_string(), m)));
        for _ in 0..LOCK_AFTER {
            assert!(matches!(a.login_password("1.2.3.4", "admin", "nope", false), Err(Error::Unauthorized(_))));
        }
        assert_eq!(*hits.lock().unwrap(), vec![("1.2.3.4".to_string(), 15)]);
        let e = a.login_password("1.2.3.4", "admin", "correct horse battery", false).unwrap_err();
        assert!(matches!(e, Error::TooMany(ref m) if m.contains("try again in 15 min")), "{e}");
        assert!(a.login_password("5.6.7.8", "admin", "correct horse battery", false).is_ok(), "per address");
    }

    #[test]
    fn concurrent_guesses_count_before_they_hash() {
        let a = with_account();
        // three failures on record: room for two more checks, however many arrive at once
        for _ in 0..3 {
            a.failed("9.9.9.9");
        }
        let one = a.admit("9.9.9.9").unwrap();
        let two = a.admit("9.9.9.9").unwrap();
        let e = a.admit("9.9.9.9").err().unwrap();
        assert!(matches!(e, Error::TooMany(ref m) if m == AT_ONCE), "{e}");
        assert!(a.admit("8.8.8.8").is_ok(), "other addresses aren't affected");
        drop(one);
        let three = a.admit("9.9.9.9").unwrap();
        drop((two, three));
        assert!(guard(&a.mem).checking.is_empty(), "finished checks give their room back");
        // and it's what sign-ins go through
        let _a = a.admit("9.9.9.9").unwrap();
        let _b = a.admit("9.9.9.9").unwrap();
        let e = a.login_password("9.9.9.9", "admin", "correct horse battery", false).unwrap_err();
        assert!(matches!(e, Error::TooMany(ref m) if m == AT_ONCE), "{e}");
    }

    #[test]
    fn address_state_expires_and_stays_bounded() {
        let a = auth();
        let now = unix_now();
        {
            let mut m = guard(&a.mem);
            m.fails.insert("old".into(), vec![now - LOCK_WINDOW - 1.0]);
            m.fails.insert("new".into(), vec![now - LOCK_WINDOW - 1.0, now]);
            m.locked.insert("was".into(), now - 1.0);
            m.locked.insert("is".into(), now + 60.0);
            for i in 0..MAX_ADDRESSES + 10 {
                m.fails.insert(format!("10.0.{i}"), vec![now - 100.0 + i as f64 * 0.001]);
            }
        }
        a.check_throttle("x").unwrap();
        let m = guard(&a.mem);
        assert!(!m.fails.contains_key("old") && m.fails["new"] == vec![now], "old failures are forgotten");
        assert!(!m.locked.contains_key("was") && m.locked.contains_key("is"));
        assert_eq!(m.fails.len(), MAX_ADDRESSES);
        assert!(m.fails.contains_key("new") && !m.fails.contains_key("10.0.0"), "the oldest go first");
    }

    #[test]
    fn totp_login_and_recovery() {
        let a = with_account();
        let ok = a.reconfirm("ip", "correct horse battery").unwrap();
        let b = a.totp_begin(&ok).unwrap();
        assert!(b["uri"].as_str().unwrap().starts_with("otpauth://totp/wranglarr:admin?secret="));
        assert!(b["qr_svg"].as_str().unwrap().starts_with("<svg"));
        let secret = b["secret"].as_str().unwrap().replace(' ', "");
        let now_step = (unix_now() / 30.0).floor() as i64;
        let rec = a.totp_enable(&totp_at(&secret, now_step).unwrap()).unwrap();
        let SignIn::Pending(p) = a.login_password("ip", "admin", "correct horse battery", false).unwrap() else {
            panic!("expected the code step")
        };
        assert!(a.login_second_factor("ip", &p, &totp_at(&secret, now_step).unwrap()).is_err(), "replay");
        let r = a.login_second_factor("ip", &p, &rec[0].to_uppercase()).unwrap();
        assert_eq!(r, SignIn::Done { method: "password+recovery", remember: false });
        assert!(a.login_second_factor("ip", &p, &rec[1]).is_err(), "the pending sign-in is used up");
        assert_eq!(a.summary().unwrap()["recovery_left"], 9);
    }

    #[test]
    fn reconfirming_shares_the_sign_in_limit() {
        let a = with_account();
        for _ in 0..LOCK_AFTER {
            let e = a.reconfirm("6.6.6.6", "guess").err().unwrap();
            assert!(matches!(e, Error::Forbidden(ref m) if m == WRONG_PASSWORD), "{e}");
        }
        let e = a.reconfirm("6.6.6.6", "correct horse battery").err().unwrap();
        assert!(matches!(e, Error::TooMany(_)), "locked out like a sign-in: {e}");
        let e = a.login_password("6.6.6.6", "admin", "correct horse battery", false).unwrap_err();
        assert!(matches!(e, Error::TooMany(_)), "and the sign-in too: {e}");
    }

    #[test]
    fn a_confirmation_is_for_the_password_it_matched() {
        let a = with_account();
        let ok = a.reconfirm("ip", "correct horse battery").unwrap();
        a.set_password("another long passphrase").unwrap();
        let e = a.create_api_key(&ok, "x").unwrap_err();
        assert!(matches!(e, Error::Forbidden(ref m) if m == WRONG_PASSWORD), "{e}");
        assert!(a.api_keys().unwrap().is_empty());
    }
}
