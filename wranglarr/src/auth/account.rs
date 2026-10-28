// SPDX-License-Identifier: GPL-3.0-or-later
//! The account: its stored shape (kv `auth:account`, read leniently and written
//! back with unknown keys kept), and creating, resetting and changing it. Every
//! change is one write transaction, so `wranglarr auth` in another process can't
//! be overwritten by a stale copy.

use serde_json::{Map, Value, json};

use super::password::{hash_password, password_problem, valid_username};
use super::session::API_KEYS_KEY;
use chrono::Utc;

use super::support::{b64u, ct_eq, guard, sha};
use super::{Auth, passkey};
use crate::error::{Error, Result};
use crate::ids::{random_bytes, token_hex};
use crate::state::Tx;
use crate::time::iso_z;
use crate::value::truthy;

pub(super) const ACCOUNT_KEY: &str = "auth:account";

/// `auth:account`. Read leniently (missing or null fields take their
/// defaults), and unknown keys are kept.
#[derive(Debug, Clone, Default)]
pub struct Account {
    pub username: String,
    pub password: String,
    pub created: Option<String>,
    /// 32 random bytes, base64url: the WebAuthn user handle's source.
    pub user_handle: String,
    pub totp: Totp,
    /// SHA-256 of the normalised unused recovery codes.
    pub recovery: Vec<String>,
    /// Passkey rows (see [`passkey`]); rows without a credential are ignored.
    pub passkeys: Vec<Value>,
    pub totp_pending: Option<String>,
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totp {
    pub enabled: bool,
    pub secret: String,
    pub last: i64,
}

fn take_str(m: &mut Map<String, Value>, k: &str) -> Option<String> {
    m.remove(k).and_then(|v| v.as_str().map(str::to_string))
}

impl Account {
    pub fn from_value(v: Value) -> Option<Account> {
        let Value::Object(mut m) = v else { return None };
        if m.is_empty() {
            return None;
        }
        let totp = match m.remove("totp") {
            Some(Value::Object(t)) => Totp {
                enabled: t.get("enabled").is_some_and(truthy),
                secret: t.get("secret").and_then(Value::as_str).unwrap_or("").to_string(),
                last: t.get("last").and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0),
            },
            _ => Totp::default(),
        };
        let recovery = match m.remove("recovery") {
            Some(Value::Array(a)) => a.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => vec![],
        };
        let passkeys = match m.remove("passkeys") {
            Some(Value::Array(a)) => a,
            _ => vec![],
        };
        Some(Account {
            username: take_str(&mut m, "username").unwrap_or_default(),
            password: take_str(&mut m, "password").unwrap_or_default(),
            created: take_str(&mut m, "created"),
            user_handle: take_str(&mut m, "user_handle").unwrap_or_default(),
            totp,
            recovery,
            passkeys,
            totp_pending: take_str(&mut m, "totp_pending").filter(|s| !s.is_empty()),
            extra: m,
        })
    }

    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("username".into(), json!(self.username));
        m.insert("password".into(), json!(self.password));
        m.insert("created".into(), json!(self.created));
        m.insert("user_handle".into(), json!(self.user_handle));
        m.insert(
            "totp".into(),
            json!({"enabled": self.totp.enabled, "secret": self.totp.secret, "last": self.totp.last}),
        );
        m.insert("recovery".into(), json!(self.recovery));
        m.insert("passkeys".into(), Value::Array(self.passkeys.clone()));
        if let Some(p) = &self.totp_pending {
            m.insert("totp_pending".into(), json!(p));
        }
        for (k, v) in &self.extra {
            m.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Value::Object(m)
    }
}

pub(super) fn read_account(tx: &Tx) -> Result<Option<Account>> {
    Ok(tx.get_value(ACCOUNT_KEY)?.and_then(Account::from_value))
}

impl Auth {
    /// The account, if there is one. A database error is an error, not "no account".
    pub fn account(&self) -> Result<Option<Account>> {
        Ok(self.stored(ACCOUNT_KEY)?.and_then(Account::from_value))
    }

    pub fn has_account(&self) -> Result<bool> {
        Ok(self.account()?.is_some())
    }

    /// The account, or 404 "no account".
    pub(super) fn need_account(&self) -> Result<Account> {
        self.account()?.ok_or_else(|| Error::not_found("no account"))
    }

    /// Change the account in one write transaction (404 without one). It's written
    /// back only if `f` changed it; an error from `f` changes nothing.
    pub(super) fn update_account<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Tx, &mut Account) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.write(move |tx| {
            let mut acct = read_account(tx)?.ok_or_else(|| Error::not_found("no account"))?;
            let before = acct.to_value();
            let out = f(tx, &mut acct)?;
            let after = acct.to_value();
            if after != before {
                tx.put(ACCOUNT_KEY, &after)?;
            }
            Ok(out)
        })
    }

    /// Delete the account, every API key and every session in one transaction
    /// (`wranglarr auth reset`); the next visit sets it up again.
    pub fn reset(&self) -> Result<()> {
        self.write(|tx| {
            tx.delete(ACCOUNT_KEY)?;
            tx.delete(API_KEYS_KEY)?;
            tx.conn().execute("DELETE FROM sessions", [])?;
            Ok(())
        })?;
        self.forget_flows();
        tracing::warn!("auth: account reset: API keys and sessions deleted");
        Ok(())
    }

    /// The current setup code ("" when there is none yet, or an account exists).
    pub fn setup_code(&self) -> String {
        guard(&self.mem).setup_code.clone()
    }

    /// The one-time code the first visit must type. Printed in the log.
    pub fn ensure_setup_code(&self) -> Result<String> {
        if self.has_account()? {
            guard(&self.mem).setup_code.clear();
            return Ok(String::new());
        }
        let mut m = guard(&self.mem);
        if m.setup_code.is_empty() {
            m.setup_code = (0..3).map(|_| token_hex(2).to_uppercase()).collect::<Vec<_>>().join("-");
            let code = format!("    {}", m.setup_code);
            // straight to stderr like the other boxed warnings: the log formatter would escape the colours
            eprintln!(
                "\n{}",
                crate::owner::amber(&[
                    "wranglarr has no account yet.",
                    "",
                    "Open wranglarr in your browser and create one. It asks for this",
                    "one-time setup code, so nobody else on your network can claim it:",
                    "",
                    &code,
                    "",
                    "The code changes each time wranglarr starts, until the account exists.",
                ])
            );
        }
        Ok(m.setup_code.clone())
    }

    /// The first account. Hashes the password: take an [`Auth::admit`] first.
    pub fn create_account(&self, setup_code: &str, username: &str, password: &str) -> Result<()> {
        let exists = || Error::conflict("the account already exists");
        if self.has_account()? {
            return Err(exists());
        }
        // e.g. a setup page left open across a restart or a `wranglarr auth reset`:
        // print a fresh code now, so "it's in the log" is true
        let code = self.ensure_setup_code()?;
        let typed = setup_code.trim().to_uppercase().replace(' ', "");
        if !ct_eq(typed.as_bytes(), code.replace(' ', "").as_bytes()) {
            return Err(Error::Forbidden(
                "wrong setup code: it's in wranglarr's log (docker compose logs wranglarr)".into(),
            ));
        }
        let username = username.trim();
        if !valid_username(username) {
            return Err(Error::invalid("username: 1-64 letters, digits and . _ - @"));
        }
        if let Some(p) = password_problem(password, username) {
            return Err(Error::invalid(p));
        }
        let acct = Account {
            username: username.to_string(),
            password: hash_password(password)?,
            created: Some(iso_z(Utc::now())),
            user_handle: b64u(&random_bytes(32)),
            ..Account::default()
        };
        let stored = acct.to_value();
        self.write(move |tx| {
            if read_account(tx)?.is_some() {
                return Err(exists());
            }
            tx.put(ACCOUNT_KEY, &stored)
        })?;
        guard(&self.mem).setup_code.clear();
        tracing::info!("auth: account '{username}' created");
        Ok(())
    }

    /// A new password, the current one typed again. Every other session ends, and
    /// every sign-in and passkey ceremony in progress: they began under the old one.
    /// Returns how many sessions ended.
    pub fn change_password(&self, ip: &str, current: &str, new: &str, keep_session: &str) -> Result<usize> {
        let wrong = || Error::Forbidden("the current password is wrong".into());
        let ok = self.reconfirm(ip, current).map_err(|e| if matches!(e, Error::Forbidden(_)) { wrong() } else { e })?;
        let username = self.need_account()?.username;
        if let Some(p) = password_problem(new, &username) {
            return Err(Error::invalid(p));
        }
        let hash = hash_password(new)?;
        let keep = sha(keep_session);
        let ended = self.update_account(move |tx, acct| {
            ok.check(acct).map_err(|_| wrong())?;
            acct.password = hash;
            Ok(tx.conn().execute("DELETE FROM sessions WHERE token_hash != ?", [keep])?)
        })?;
        self.forget_flows();
        tracing::info!("auth: password changed; {ended} other session(s) ended");
        Ok(ended)
    }

    /// Set the password without the current one (`wranglarr auth set-password`).
    /// Every session ends. Returns how many.
    pub fn set_password(&self, new: &str) -> Result<usize> {
        self.need_account()?;
        let hash = hash_password(new)?;
        let ended = self.update_account(move |tx, acct| {
            acct.password = hash;
            Ok(tx.conn().execute("DELETE FROM sessions", [])?)
        })?;
        self.forget_flows();
        Ok(ended)
    }

    pub fn summary(&self) -> Result<Value> {
        let acct = self.account()?.unwrap_or_default();
        Ok(json!({"username": acct.username, "created": acct.created, "totp": acct.totp.enabled,
                  "recovery_left": acct.recovery.len(), "passkeys": passkey::listing(&acct)}))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::auth::signin::{EXPIRED, SignIn};
    use crate::auth::testing::{auth, with_account};
    use crate::auth::totp::totp_at;
    use crate::auth::{Identity, Options};
    use crate::time::unix_now;

    #[test]
    fn account_roundtrip_keeps_unknown_keys() {
        let v = json!({"username": "admin", "password": "x", "totp": null, "recovery": null, "future": 1,
                       "passkeys": [{"id": "old", "public_key": "…"}]});
        let a = Account::from_value(v).unwrap();
        assert_eq!(a.totp, Totp::default());
        let back = a.to_value();
        assert_eq!(back["future"], 1);
        assert_eq!(back["passkeys"][0]["id"], "old");
        assert!(Account::from_value(json!({})).is_none());
    }

    #[test]
    fn a_broken_store_is_an_error_not_no_account() {
        let a = auth();
        a.sql(|c| c.execute("INSERT INTO kv VALUES ('auth:account', 'not json', '')", [])).unwrap();
        assert!(a.account().is_err());
        assert!(a.has_account().is_err());
        assert!(a.identify("", "wr_x").is_err(), "the guard answers 500, not 'set it up'");
    }

    #[test]
    fn changing_the_password_ends_everything_else() {
        let a = with_account();
        let (mine, _) = a.new_session("password", "ip", "", false).unwrap();
        let (other, _) = a.new_session("password", "ip", "", false).unwrap();
        let ok = a.reconfirm("ip", "correct horse battery").unwrap();
        let b = a.totp_begin(&ok).unwrap();
        let secret = b["secret"].as_str().unwrap().replace(' ', "");
        let step = (unix_now() / 30.0).floor() as i64;
        a.totp_enable(&totp_at(&secret, step).unwrap()).unwrap();
        let SignIn::Pending(p) = a.login_password("ip", "admin", "correct horse battery", false).unwrap() else {
            panic!("expected the code step")
        };
        let e = a.change_password("ip", "wrong", "another long passphrase", &mine).unwrap_err();
        assert_eq!(e.message(), "the current password is wrong");
        assert_eq!(a.change_password("ip", "correct horse battery", "another long passphrase", &mine).unwrap(), 1);
        assert!(a.session(&mine).unwrap().is_some(), "this session stays");
        assert!(a.session(&other).unwrap().is_none(), "the others end");
        let e = a.login_second_factor("ip", &p, &totp_at(&secret, step + 1).unwrap()).unwrap_err();
        assert_eq!(e.message(), EXPIRED, "a sign-in started with the old password can't finish");
    }

    #[test]
    fn a_password_set_elsewhere_ends_pending_sign_ins() {
        // the server and wranglarr-auth: two processes, one database
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("wranglarr.db");
        let server = Auth::open(&db, Options::default()).unwrap();
        let code = server.ensure_setup_code().unwrap();
        server.create_account(&code, "admin", "correct horse battery").unwrap();
        let ok = server.reconfirm("ip", "correct horse battery").unwrap();
        let b = server.totp_begin(&ok).unwrap();
        let secret = b["secret"].as_str().unwrap().replace(' ', "");
        let step = (unix_now() / 30.0).floor() as i64;
        server.totp_enable(&totp_at(&secret, step).unwrap()).unwrap();
        let SignIn::Pending(p) = server.login_password("ip", "admin", "correct horse battery", false).unwrap() else {
            panic!("expected the code step")
        };
        let (token, _) = server.new_session("password", "ip", "", false).unwrap();
        let cli = Auth::open(&db, Options::default()).unwrap();
        assert_eq!(cli.set_password("another long passphrase").unwrap(), 1, "every session ends");
        assert!(server.session(&token).unwrap().is_none());
        let e = server.login_second_factor("ip", &p, &totp_at(&secret, step + 1).unwrap()).unwrap_err();
        assert_eq!(e.message(), EXPIRED);
    }

    #[test]
    fn reset_takes_keys_and_sessions_with_it() {
        let a = with_account();
        let ok = a.reconfirm("ip", "correct horse battery").unwrap();
        let key = a.create_api_key(&ok, "ha").unwrap()["key"].as_str().unwrap().to_string();
        let (token, _) = a.new_session("password", "ip", "", false).unwrap();
        a.reset().unwrap();
        assert!(!a.has_account().unwrap());
        assert!(a.api_keys().unwrap().is_empty() && a.sessions("").unwrap().is_empty());
        assert_eq!(a.identify(&token, &key).unwrap(), Identity::default());
    }
}
