// SPDX-License-Identifier: GPL-3.0-or-later
//! wranglarr's login: one admin account, built in, no identity provider.
//!
//! - Password: scrypt-hashed (`scrypt$N$r$p$salt$hash`, a fixed
//!   format, so an existing account keeps working). The first visit creates
//!   the account, and needs a one-time setup code printed in wranglarr's log,
//!   so nobody else on the network can claim it first.
//! - Two-factor: optional TOTP (any authenticator app), with single-use
//!   recovery codes.
//! - Passkeys: WebAuthn with wranglarr as the relying party ([`passkey`]). A
//!   passkey signs in without the password and counts as two factors (user
//!   verification is required). Browsers allow them only over HTTPS on a host
//!   name, or on localhost.
//! - Sessions: random tokens in an HttpOnly, SameSite=Strict cookie; only their
//!   SHA-256 is stored.
//! - API keys: for scripts, sent as the X-Api-Key header; only hashes stored.
//!
//! State lives in wranglarr.db (kv `auth:account`, `auth:api_keys`; table
//! `sessions`). `wranglarr auth` ([`cli`]) resets things from a shell, so every
//! read-modify-write is one SQLite write transaction ([`Db::write`]), never
//! a snapshot saved back over someone else's change.
//!
//! [`Auth`] is synchronous: password hashing is slow on purpose, so the web
//! layer calls it on a blocking thread, and the CLI has no runtime. Its
//! database requests wait with [`State::call_blocking`], which refuses an
//! async runtime thread. Hashing happens before a request is sent, never
//! inside one.
//!
//! [`Db::write`]: crate::state::Db::write
//!
//! The parts: [`Auth`] and its options here; the account (`account`), its
//! password (`password`), signing in and its limits (`signin`), TOTP and recovery
//! codes (`totp`), sessions and API keys (`session`), passkeys ([`passkey`]) and
//! the shell commands ([`cli`]).

pub mod cli;
pub mod passkey;

mod account;
mod password;
mod session;
mod signin;
mod support;
mod totp;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, RwLock};

use rusqlite::Connection;
use serde_json::Value;

use crate::app::App;
use crate::error::Result;
use crate::state::{State, Tx};

pub use account::{Account, Totp};
pub use password::{password_problem, valid_username};
pub use session::{Identity, SESSION_COOKIE, SessionRow};
pub use signin::{Attempt, Confirmed, SignIn};
pub use support::ct_eq_str;
pub use totp::totp_at;

use crate::time::iso_z;
use chrono::Utc;
use signin::Pending;
use support::guard;

/// The issuer shown in authenticator apps.
const ISSUER: &str = "wranglarr";
/// How long a passkey ceremony may take.
const CHALLENGE_SECONDS: f64 = 300.0;

// ---- options from the environment ------------------------------------------------------

/// What the environment says about the login (read once at start).
#[derive(Debug, Clone)]
pub struct Options {
    /// WRANGLARR_AUTH isn't off/false/0/no.
    pub enabled: bool,
    /// APPS_AUTH=tunnel (AuthTunnel, wiki/App-Logins.md): wranglarr-gate asks
    /// /api/auth/gate before every request to an app, so the sign-in cookie has
    /// to reach the apps' addresses too.
    pub tunnel: bool,
    /// AUTH_COOKIE_DOMAIN (tunnel mode only), without a leading dot.
    pub cookie_domain: String,
    /// AUTH_LOGIN_URL, without a trailing slash.
    pub login_url: String,
    /// WRANGLARR_PORT (8787): where the gate sends browsers without AUTH_LOGIN_URL.
    pub port: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            enabled: true,
            tunnel: false,
            cookie_domain: String::new(),
            login_url: String::new(),
            port: "8787".into(),
        }
    }
}

impl Options {
    /// The options in the environment `env` (part of [`crate::deployment::Deployment`]).
    pub fn read(env: crate::config::Lookup) -> Options {
        let env = |k: &str| env(k).unwrap_or_default().trim().to_string();
        // on unless clearly turned off: a typo must not open the service
        let enabled = crate::value::flag_word(&env("WRANGLARR_AUTH")) != Some(false);
        let tunnel = env("APPS_AUTH").to_lowercase() == "tunnel";
        let cookie_domain =
            if tunnel { env("AUTH_COOKIE_DOMAIN").trim_start_matches('.').to_lowercase() } else { String::new() };
        let port = env("WRANGLARR_PORT");
        Options {
            enabled,
            tunnel,
            cookie_domain,
            login_url: env("AUTH_LOGIN_URL").trim_end_matches('/').to_string(),
            port: if port.is_empty() { "8787".into() } else { port },
        }
    }
}

// ---- the service -----------------------------------------------------------------------

#[derive(Default)]
struct Mem {
    /// ip -> failure times (within the window)
    fails: HashMap<String, Vec<f64>>,
    /// ip -> locked until
    locked: HashMap<String, f64>,
    /// ip -> password checks running now: they count against the limit before they finish
    checking: HashMap<String, usize>,
    /// the MFA step after the password
    pending: HashMap<String, Pending>,
    /// passkey ceremonies
    challenges: HashMap<String, passkey::Challenge>,
    setup_code: String,
}

type LockoutHook = Box<dyn Fn(&str, u64) + Send + Sync>;

/// The login: the account, its sign-ins, sessions and API keys (see the module docs).
pub struct Auth {
    state: State,
    opts: RwLock<Options>,
    mem: Mutex<Mem>,
    on_lockout: Mutex<Option<LockoutHook>>,
}

impl Auth {
    /// The login store on the state database at `path`, with a connection of
    /// its own (the CLI). Blocks.
    pub fn open(path: &Path, opts: Options) -> Result<Auth> {
        Auth::with_state(State::open(path)?, opts)
    }

    /// The login store on an open state database. Blocks: call it off the runtime.
    pub fn with_state(state: State, opts: Options) -> Result<Auth> {
        let now = iso_z(Utc::now());
        state.call_blocking(move |db| {
            db.conn().execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
                  token_hash TEXT PRIMARY KEY, created TEXT NOT NULL, expires TEXT NOT NULL, last_seen TEXT,
                  method TEXT, ip TEXT, agent TEXT, remember INTEGER NOT NULL DEFAULT 0);",
            )?;
            db.conn().execute("DELETE FROM sessions WHERE expires < ?", [now])?;
            Ok(())
        })?;
        Ok(Auth { state, opts: RwLock::new(opts), mem: Mutex::new(Mem::default()), on_lockout: Mutex::new(None) })
    }

    /// SQL on the auth tables, waited for on this (blocking) thread.
    fn sql<T: Send + 'static>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static) -> Result<T> {
        self.state.call_blocking(move |db| Ok(f(db.conn())?))
    }

    /// One read-modify-write transaction (see [`crate::state::Db::write`]).
    fn write<T: Send + 'static>(&self, f: impl FnOnce(&Tx) -> Result<T> + Send + 'static) -> Result<T> {
        self.state.call_blocking(move |db| db.write(f))
    }

    /// A kv value.
    fn stored(&self, key: &'static str) -> Result<Option<Value>> {
        self.state.call_blocking(move |db| db.get_value(key))
    }

    pub fn enabled(&self) -> bool {
        self.options().enabled
    }

    pub fn options(&self) -> Options {
        self.opts.read().map(|o| o.clone()).unwrap_or_default()
    }

    /// Replace the options (tests; the environment is read once at start).
    pub fn set_options(&self, o: Options) {
        if let Ok(mut g) = self.opts.write() {
            *g = o;
        }
    }

    /// Called with (ip, minutes) when an address gets locked out, from a
    /// blocking thread.
    pub fn set_on_lockout(&self, f: impl Fn(&str, u64) + Send + Sync + 'static) {
        *guard(&self.on_lockout) = Some(Box::new(f));
    }

    /// Drop every sign-in waiting for its code and every passkey ceremony.
    fn forget_flows(&self) {
        let mut m = guard(&self.mem);
        m.pending.clear();
        m.challenges.clear();
    }
}

// ---- the service hooks --------------------------------------------------------------------

/// Start-up: print the setup code while there is no account, and hand lockouts
/// to [`App::auth_lockout`].
pub async fn start(app: &App) {
    if app.auth().enabled()
        && let Err(e) = app.blocking(|app| app.auth().ensure_setup_code()).await
    {
        tracing::error!("auth: can't read the account: {e}");
    }
    let a = app.clone();
    app.auth().set_on_lockout(move |ip, minutes| a.auth_lockout(ip, minutes));
}

impl App {
    /// An address was just locked out after too many failed sign-ins.
    /// Called from a blocking thread: the notifier sends on its runtime.
    pub fn auth_lockout(&self, ip: &str, minutes: u64) {
        self.notifier().lockout(self, ip, i64::try_from(minutes).unwrap_or(i64::MAX));
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::{Auth, Options};
    use crate::state::State;

    /// A login store in memory, without an account.
    pub(crate) fn auth() -> Auth {
        Auth::with_state(State::memory().expect("state"), Options::default()).expect("auth")
    }

    /// A login store with the account admin / "correct horse battery".
    pub(crate) fn with_account() -> Auth {
        let a = auth();
        let code = a.ensure_setup_code().expect("code");
        a.create_account(&code, "admin", "correct horse battery").expect("account");
        a
    }
}
