// SPDX-License-Identifier: GPL-3.0-or-later
//! AuthTunnel sign-ins for apps whose API has no key, only a login (qBittorrent),
//! when `AUTH_QBITTORRENT_LOGIN` turns them on.
//!
//! The app's own login can't be passed through the gate: the app lets the gate's
//! network in without one. So wranglarr checks the username and password against
//! wranglarr's copy of the app's login itself, and hands out a session of its own,
//! which the gate then accepts like an API key.
//!
//! - Wrong logins have a limit of their own per address (the same five in 15
//!   minutes as wranglarr's sign-in), kept apart from wranglarr's own sign-in, so an
//!   app can't lock you out of wranglarr nor reset its count. A wrong password sent
//!   again counts once: a phone retrying an old one doesn't use up the limit.
//! - A weak app password is refused unless `AUTH_QBITTORRENT_ALLOW_WEAK` allows it.
//! - Sessions live in memory, only their SHA-256 is kept. Each lasts an hour from its
//!   last use and a day at most, ends when the app's login in wranglarr changes, and
//!   when wranglarr's own sessions are ended (sign out everywhere, a new password, a
//!   reset).
//! - Right and wrong logins take the same time.

use std::time::Duration;

use super::password::password_problem;
use super::support::{ct_eq_str, guard, sha, trim};
use super::{Auth, Mem};
use crate::error::Result;
use crate::ids::random_token;
use crate::time::unix_now;

/// How long an app session lasts after its last request ...
const IDLE_SECONDS: f64 = 3600.0;
/// ... and at most.
const MAX_SECONDS: f64 = 24.0 * 3600.0;
/// App sessions kept (the least recently used go first beyond this).
const MAX_SESSIONS: usize = 512;
/// Addresses whose wrong passwords are remembered.
const MAX_ADDRESSES: usize = 4096;
/// A wrong password is counted once within this window (the lockout's).
const WRONG_WINDOW: f64 = 15.0 * 60.0;
/// Every answer takes this long, right or wrong.
const ANSWER_DELAY: Duration = Duration::from_millis(400);
/// The shortest app password the gate takes without `AUTH_QBITTORRENT_ALLOW_WEAK`.
pub const MIN_APP_PASSWORD: usize = 16;

/// One app session.
pub(super) struct GateSession {
    idle_until: f64,
    ends: f64,
    /// [`fingerprint`] of the app's login it was opened with
    login: String,
}

/// What a sign-in through the gate came to.
#[derive(Debug, PartialEq, Eq)]
pub enum GateAnswer {
    /// The right login: a new session token.
    Session(String),
    /// A wrong login (counted against the address once per password).
    Wrong,
    /// The app's own password is empty or too weak to open the gate with; nothing was counted.
    Weak,
}

fn key(app: &str, token: &str) -> String {
    format!("{app}\n{}", sha(token))
}

/// The app's login, as kept with its sessions: they end when it changes.
fn fingerprint(real: (&str, &str)) -> String {
    sha(&format!("{}\n{}", real.0, real.1))
}

/// Where an app's sign-ins from `ip` are counted: apart from wranglarr's own.
fn bucket(app: &str, ip: &str) -> String {
    format!("{ip} ({app} app sign-in)")
}

/// Too weak to open the gate with: short, common, or containing the username.
pub fn weak_app_password(user: &str, password: &str) -> bool {
    password.chars().count() < MIN_APP_PASSWORD || password_problem(password, user).is_some()
}

fn sweep(m: &mut Mem, now: f64) {
    m.gate.retain(|_, s| s.idle_until > now && s.ends > now);
    trim(&mut m.gate, MAX_SESSIONS, |s| s.idle_until);
    m.gate_wrong.retain(|_, seen| {
        seen.retain(|(_, t)| now - t < WRONG_WINDOW);
        !seen.is_empty()
    });
    trim(&mut m.gate_wrong, MAX_ADDRESSES, |seen| seen.last().map_or(0.0, |(_, t)| *t));
}

impl Auth {
    /// A sign-in to `app` through the gate from `ip`, checked against wranglarr's copy
    /// of the app's login (`real`). An error when the address is locked out. Blocks
    /// (every answer waits the same time): call it off the async threads.
    pub fn gate_login(
        &self,
        ip: &str,
        app: &str,
        sent: (&str, &str),
        real: (&str, &str),
        allow_weak: bool,
    ) -> Result<GateAnswer> {
        let b = bucket(app, ip);
        let _attempt = self.admit(&b)?;
        let answer = self.gate_check(&b, app, sent, real, allow_weak);
        std::thread::sleep(ANSWER_DELAY);
        Ok(answer)
    }

    fn gate_check(&self, b: &str, app: &str, sent: (&str, &str), real: (&str, &str), allow_weak: bool) -> GateAnswer {
        let now = unix_now();
        if real.1.is_empty() || (!allow_weak && weak_app_password(real.0, real.1)) {
            let mut m = guard(&self.mem);
            if now - m.gate_weak_warned > 600.0 {
                m.gate_weak_warned = now;
                tracing::warn!(
                    "auth: {app} app sign-ins through the gate are refused: its password in .env is empty or weak \
                     (it needs {MIN_APP_PASSWORD} characters or more, not a common one); set a longer one, or \
                     AUTH_QBITTORRENT_ALLOW_WEAK=on"
                );
            }
            return GateAnswer::Weak;
        }
        // both compared every time
        let user_ok = ct_eq_str(sent.0, real.0);
        let pass_ok = ct_eq_str(sent.1, real.1);
        if !(user_ok && pass_ok) {
            let wrong = sha(&format!("{b}\n{}\n{}", sent.0, sent.1));
            let again = {
                let mut m = guard(&self.mem);
                sweep(&mut m, now);
                let seen = m.gate_wrong.entry(b.to_string()).or_default();
                let again = seen.iter().any(|(h, _)| *h == wrong);
                if !again {
                    seen.push((wrong, now));
                }
                again
            };
            if !again {
                self.record_failure(b);
            }
            return GateAnswer::Wrong;
        }
        self.succeeded(b);
        let token = random_token(24);
        let mut m = guard(&self.mem);
        sweep(&mut m, now);
        m.gate_wrong.remove(b);
        let login = fingerprint(real);
        m.gate.insert(key(app, &token), GateSession { idle_until: now + IDLE_SECONDS, ends: now + MAX_SECONDS, login });
        GateAnswer::Session(token)
    }

    /// `token` is a live session for `app`, opened with the app's current login
    /// (`real`); using it keeps it alive for another hour, up to a day in all.
    pub fn gate_session(&self, app: &str, token: &str, real: (&str, &str)) -> bool {
        if token.is_empty() {
            return false;
        }
        let now = unix_now();
        let login = fingerprint(real);
        let mut m = guard(&self.mem);
        sweep(&mut m, now);
        let k = key(app, token);
        match m.gate.get_mut(&k) {
            Some(s) if ct_eq_str(&s.login, &login) => {
                s.idle_until = (now + IDLE_SECONDS).min(s.ends);
                true
            }
            Some(_) => {
                m.gate.remove(&k); // opened with a login that has changed since
                false
            }
            None => false,
        }
    }

    /// End `token`'s session for `app` (the app logged out).
    pub fn gate_logout(&self, app: &str, token: &str) {
        guard(&self.mem).gate.remove(&key(app, token));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{GateAnswer, GateSession};
    use crate::auth::support::guard;
    use crate::auth::testing::{auth, with_account};

    const REAL: (&str, &str) = ("admin", "a-long-qbittorrent-secret");
    const APP: &str = "qbittorrent";

    fn token(a: GateAnswer) -> String {
        match a {
            GateAnswer::Session(t) => t,
            other => panic!("no session: {other:?}"),
        }
    }

    #[test]
    fn the_right_login_opens_a_session_for_that_app_only() {
        let a = auth();
        let t = token(a.gate_login("10.0.0.5", APP, REAL, REAL, false).unwrap());
        assert!(a.gate_session(APP, &t, REAL));
        assert!(!a.gate_session("qb2", &t, REAL), "another app's gate doesn't take it");
        assert!(!a.gate_session(APP, "made-up", REAL));
        assert!(!a.gate_session(APP, "", REAL));
        a.gate_logout(APP, &t);
        assert!(!a.gate_session(APP, &t, REAL), "logged out");
    }

    #[test]
    fn five_different_wrong_logins_lock_the_address_out_of_app_sign_ins_only() {
        let a = auth();
        for i in 0..5 {
            let sent = format!("nope{i}");
            assert_eq!(a.gate_login("10.0.0.6", APP, ("admin", &sent), REAL, false).unwrap(), GateAnswer::Wrong);
        }
        assert!(a.gate_login("10.0.0.6", APP, REAL, REAL, false).is_err(), "locked out, even with the right one");
        assert!(a.check_throttle("10.0.0.6").is_ok(), "wranglarr's own sign-in isn't");
        assert!(matches!(a.gate_login("10.0.0.7", APP, REAL, REAL, false).unwrap(), GateAnswer::Session(_)));
    }

    #[test]
    fn an_old_password_sent_again_counts_once() {
        let a = auth();
        for _ in 0..8 {
            let old = ("admin", "an-old-password");
            assert_eq!(a.gate_login("10.0.0.8", APP, old, REAL, false).unwrap(), GateAnswer::Wrong);
        }
        assert!(matches!(a.gate_login("10.0.0.8", APP, REAL, REAL, false).unwrap(), GateAnswer::Session(_)));
    }

    #[test]
    fn a_right_app_login_doesnt_reset_wranglarrs_own_count() {
        let a = with_account();
        for _ in 0..4 {
            assert!(a.login_password("10.0.0.9", "admin", "wrong", false).is_err());
        }
        assert!(matches!(a.gate_login("10.0.0.9", APP, REAL, REAL, false).unwrap(), GateAnswer::Session(_)));
        assert!(a.login_password("10.0.0.9", "admin", "wrong", false).is_err());
        assert!(a.check_throttle("10.0.0.9").is_err(), "the fifth wrong admin password still locks it");
    }

    #[test]
    fn a_weak_or_empty_app_password_opens_nothing_unless_allowed() {
        let a = auth();
        let weak = ("admin", "short-one");
        assert_eq!(a.gate_login("10.0.0.10", APP, weak, weak, false).unwrap(), GateAnswer::Weak);
        assert!(matches!(a.gate_login("10.0.0.10", APP, weak, weak, true).unwrap(), GateAnswer::Session(_)));
        let empty = ("admin", "");
        assert_eq!(a.gate_login("10.0.0.10", APP, empty, empty, true).unwrap(), GateAnswer::Weak);
        let common = ("admin", "aaaaaaaaaaaaaaaaaaaa");
        assert_eq!(a.gate_login("10.0.0.10", APP, common, common, false).unwrap(), GateAnswer::Weak);
    }

    #[test]
    fn a_session_ends_when_the_login_changes() {
        let a = auth();
        let t = token(a.gate_login("10.0.0.11", APP, REAL, REAL, false).unwrap());
        assert!(!a.gate_session(APP, &t, ("admin", "the-new-long-password")), "the old login's session");
        assert!(!a.gate_session(APP, &t, REAL), "and it's gone");
    }

    #[test]
    fn sessions_end_after_an_idle_hour_and_after_a_day_in_all() {
        let a = auth();
        let now = crate::time::unix_now();
        for (idle_until, ends) in [(now - 1.0, now + 100.0), (now + 100.0, now - 1.0)] {
            let t = token(a.gate_login("10.0.0.12", APP, REAL, REAL, false).unwrap());
            let k = super::key(APP, &t);
            let s = GateSession { idle_until, ends, login: super::fingerprint(REAL) };
            guard(&a.mem).gate.insert(k.clone(), s);
            assert!(!a.gate_session(APP, &t, REAL));
            assert!(!guard(&a.mem).gate.contains_key(&k), "and swept");
        }
    }

    #[test]
    fn ending_wranglarrs_sessions_ends_the_apps_too() {
        let a = with_account();
        let t = token(a.gate_login("10.0.0.13", APP, REAL, REAL, false).unwrap());
        a.end_other_sessions("").unwrap();
        assert!(!a.gate_session(APP, &t, REAL), "sign out everywhere");
        let t = token(a.gate_login("10.0.0.13", APP, REAL, REAL, false).unwrap());
        a.set_password("another correct horse").unwrap();
        assert!(!a.gate_session(APP, &t, REAL), "a new password");
    }
}
