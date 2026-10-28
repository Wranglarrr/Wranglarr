// SPDX-License-Identifier: GPL-3.0-or-later
//! Sessions and API keys. A session is a random token in an HttpOnly,
//! SameSite=Strict cookie; an API key is sent as X-Api-Key by scripts. Only their
//! SHA-256 is stored: sessions in the `sessions` table, keys in kv `auth:api_keys`.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use super::Auth;
use super::account::read_account;
use super::signin::Confirmed;
use chrono::Utc;

use super::support::{ct_eq, sha};
use crate::error::{Error, Result};
use crate::ids::{random_token, token_hex};
use crate::time::iso_z;

pub(super) const API_KEYS_KEY: &str = "auth:api_keys";

pub const SESSION_COOKIE: &str = "wranglarr_session";
const SESSION_DAYS_REMEMBER: i64 = 30;
const SESSION_HOURS: i64 = 24;

/// A row of the `sessions` table.
#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub token_hash: String,
    pub created: String,
    pub expires: String,
    pub last_seen: Option<String>,
    pub method: Option<String>,
    pub ip: Option<String>,
    pub agent: Option<String>,
    pub remember: bool,
}

fn session_row(r: &rusqlite::Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        token_hash: r.get("token_hash")?,
        created: r.get("created")?,
        expires: r.get("expires")?,
        last_seen: r.get("last_seen")?,
        method: r.get("method")?,
        ip: r.get("ip")?,
        agent: r.get("agent")?,
        remember: r.get::<_, Option<i64>>("remember")?.unwrap_or(0) != 0,
    })
}

/// Who a request is, from its session cookie or X-Api-Key ([`Auth::identify`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Identity {
    pub has_account: bool,
    /// (username, "session" | "api-key") when signed in.
    pub user: Option<(String, &'static str)>,
}

/// The objects in a stored `auth:api_keys`.
fn key_list(v: Option<Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.into_iter().filter(Value::is_object).collect(),
        _ => vec![],
    }
}

fn same_hash(entry: &Value, h: &str) -> bool {
    ct_eq(entry["hash"].as_str().unwrap_or("").as_bytes(), h.as_bytes())
}

impl Auth {
    /// -> (token, cookie max-age or None for a browser-session cookie)
    pub fn new_session(&self, method: &str, ip: &str, agent: &str, remember: bool) -> Result<(String, Option<i64>)> {
        let token = random_token(32);
        let now = Utc::now();
        let life = if remember {
            chrono::Duration::days(SESSION_DAYS_REMEMBER)
        } else {
            chrono::Duration::hours(SESSION_HOURS)
        };
        let agent: String = agent.chars().take(200).collect();
        let row = (sha(&token), iso_z(now), iso_z(now + life), method.to_owned(), ip.to_owned(), agent);
        self.sql(move |c| {
            let (hash, created, expires, method, ip, agent) = row;
            c.execute(
                "INSERT INTO sessions VALUES (?,?,?,?,?,?,?,?)",
                params![hash, created, expires, created, method, ip, agent, i64::from(remember)],
            )
        })?;
        tracing::info!("auth: signed in from {ip} ({method})");
        Ok((token, remember.then(|| life.num_seconds())))
    }

    /// The session for this cookie value, if it's current (its last use noted).
    pub fn session(&self, token: &str) -> Result<Option<SessionRow>> {
        if token.is_empty() {
            return Ok(None);
        }
        let h = sha(token);
        let now = Utc::now();
        self.sql(move |c| {
            let Some(row) = c.query_row("SELECT * FROM sessions WHERE token_hash=?", [&h], session_row).optional()?
            else {
                return Ok(None);
            };
            if row.expires < iso_z(now) {
                c.execute("DELETE FROM sessions WHERE token_hash=?", [&h])?;
                return Ok(None);
            }
            let stale = iso_z(now - chrono::Duration::minutes(5));
            if row.last_seen.as_deref().is_none_or(|l| l.is_empty() || l < stale.as_str()) {
                c.execute("UPDATE sessions SET last_seen=? WHERE token_hash=?", params![iso_z(now), h])?;
            }
            Ok(Some(row))
        })
    }

    pub fn end_session(&self, token: &str) -> Result<()> {
        let h = sha(token);
        self.sql(move |c| c.execute("DELETE FROM sessions WHERE token_hash=?", [h]))?;
        Ok(())
    }

    pub fn sessions(&self, current: &str) -> Result<Vec<Value>> {
        let now = iso_z(Utc::now());
        let rows = self.sql(move |c| {
            let mut st = c.prepare("SELECT * FROM sessions WHERE expires >= ? ORDER BY last_seen DESC")?;
            st.query_map([now], session_row)?.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        let cur = sha(current);
        Ok(rows
            .into_iter()
            .map(|r| {
                json!({"id": r.token_hash.get(..16).unwrap_or(&r.token_hash), "created": r.created,
                       "last_seen": r.last_seen, "expires": r.expires, "method": r.method, "ip": r.ip,
                       "agent": r.agent, "current": r.token_hash == cur})
            })
            .collect())
    }

    pub fn end_other_sessions(&self, current: &str) -> Result<usize> {
        let keep = sha(current);
        self.sql(move |c| c.execute("DELETE FROM sessions WHERE token_hash != ?", [keep]))
    }

    pub fn end_session_by_id(&self, sid: &str) -> Result<()> {
        let sid = sid.to_owned();
        let n = self.sql(move |c| c.execute("DELETE FROM sessions WHERE substr(token_hash, 1, 16) = ?", [sid]))?;
        if n == 0 {
            return Err(Error::not_found("no such session"));
        }
        Ok(())
    }

    /// Who a request is: its session cookie, else its X-Api-Key. Neither counts
    /// while there is no account, so a key made before a reset doesn't outlive it.
    pub fn identify(&self, session_token: &str, api_key: &str) -> Result<Identity> {
        let Some(acct) = self.account()? else { return Ok(Identity::default()) };
        let user = if acct.username.is_empty() { "?".to_string() } else { acct.username };
        let via = if self.session(session_token)?.is_some() {
            Some("session")
        } else if self.check_api_key(api_key)?.is_some() {
            Some("api-key")
        } else {
            None
        };
        Ok(Identity { has_account: true, user: via.map(|v| (user, v)) })
    }

    // ---- API keys ----------------------------------------------------------------------
    fn keys(&self) -> Result<Vec<Value>> {
        Ok(key_list(self.stored(API_KEYS_KEY)?))
    }

    pub fn api_keys(&self) -> Result<Vec<Value>> {
        Ok(self
            .keys()?
            .into_iter()
            .map(|mut k| {
                if let Some(o) = k.as_object_mut() {
                    o.remove("hash");
                }
                k
            })
            .collect())
    }

    pub fn create_api_key(&self, ok: &Confirmed, name: &str) -> Result<Value> {
        let key = format!("wr_{}", random_token(30));
        let name: String = name.trim().chars().take(60).collect();
        let entry = json!({"id": token_hex(6), "name": if name.is_empty() { "API key".to_string() } else { name },
                           "hash": sha(&key), "prefix": &key[..7], "created": iso_z(Utc::now()), "last_used": null});
        let (ok, stored) = (ok.clone(), entry.clone());
        self.write(move |tx| {
            ok.check(&read_account(tx)?.ok_or_else(|| Error::not_found("no account"))?)?;
            let mut keys = key_list(tx.get_value(API_KEYS_KEY)?);
            keys.push(stored);
            tx.put(API_KEYS_KEY, &keys)
        })?;
        let mut out = entry;
        if let Some(o) = out.as_object_mut() {
            o.remove("hash");
            o.insert("key".into(), json!(key));
        }
        Ok(out)
    }

    pub fn revoke_api_key(&self, kid: &str) -> Result<()> {
        let kid = kid.to_owned();
        self.write(move |tx| {
            let keys = key_list(tx.get_value(API_KEYS_KEY)?);
            let n = keys.len();
            let left: Vec<Value> = keys.into_iter().filter(|k| k["id"].as_str() != Some(kid.as_str())).collect();
            if left.len() == n {
                return Err(Error::not_found("no such API key"));
            }
            tx.put(API_KEYS_KEY, &left)
        })
    }

    /// The stored entry for this key, if it's one of ours (its last use noted).
    pub fn check_api_key(&self, key: &str) -> Result<Option<Value>> {
        if key.is_empty() {
            return Ok(None);
        }
        let h = sha(key);
        let Some(mut entry) = self.keys()?.into_iter().find(|k| same_hash(k, &h)) else { return Ok(None) };
        let now = iso_z(Utc::now());
        let stale = iso_z(Utc::now() - chrono::Duration::minutes(5));
        if entry["last_used"].as_str().unwrap_or("") < stale.as_str() {
            // re-read in the transaction: a key revoked meanwhile must stay revoked
            let used = now.clone();
            let noted = self.write(move |tx| {
                let mut keys = key_list(tx.get_value(API_KEYS_KEY)?);
                let Some(k) = keys.iter_mut().find(|k| same_hash(k, &h)) else { return Ok(false) };
                k["last_used"] = json!(used);
                tx.put(API_KEYS_KEY, &keys)?;
                Ok(true)
            });
            match noted {
                Ok(false) => return Ok(None),
                Ok(true) => entry["last_used"] = json!(now),
                Err(e) => tracing::warn!("auth: could not note the API key's use: {e}"),
            }
        }
        Ok(Some(entry))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::auth::account::ACCOUNT_KEY;
    use crate::auth::testing::with_account;

    #[test]
    fn sessions_and_keys() {
        let a = with_account();
        let (t1, age) = a.new_session("password", "ip", "agent", true).unwrap();
        assert_eq!(age, Some(30 * 86400));
        let (t2, age2) = a.new_session("password", "ip", "agent", false).unwrap();
        assert_eq!(age2, None);
        assert!(a.session(&t1).unwrap().is_some() && a.session("nope").unwrap().is_none());
        let list = a.sessions(&t1).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);
        assert_eq!(a.end_other_sessions(&t1).unwrap(), 1);
        assert!(a.session(&t2).unwrap().is_none());
        let ok = a.reconfirm("ip", "correct horse battery").unwrap();
        let k = a.create_api_key(&ok, "  ").unwrap();
        assert_eq!(k["name"], "API key");
        let key = k["key"].as_str().unwrap();
        assert!(key.starts_with("wr_") && key.len() == 43 && k["prefix"] == key[..7]);
        assert!(a.check_api_key(key).unwrap().is_some() && a.check_api_key("wr_x").unwrap().is_none());
        assert!(a.api_keys().unwrap()[0].get("hash").is_none());
        assert!(a.api_keys().unwrap()[0]["last_used"].is_string());
        assert_eq!(a.identify("", key).unwrap().user, Some(("admin".into(), "api-key")));
        assert_eq!(a.identify(&t1, "").unwrap().user, Some(("admin".into(), "session")));
        a.revoke_api_key(k["id"].as_str().unwrap()).unwrap();
        assert!(a.check_api_key(key).unwrap().is_none());
        assert!(matches!(a.revoke_api_key("x"), Err(Error::NotFound(_))));
    }

    #[test]
    fn keys_and_sessions_need_an_account() {
        // e.g. the account row deleted by hand, or by an older wranglarr-auth reset
        let a = with_account();
        let ok = a.reconfirm("ip", "correct horse battery").unwrap();
        let key = a.create_api_key(&ok, "ha").unwrap()["key"].as_str().unwrap().to_string();
        let (token, _) = a.new_session("password", "ip", "", false).unwrap();
        a.state.call_blocking(|db| db.delete(ACCOUNT_KEY)).unwrap();
        assert!(a.check_api_key(&key).unwrap().is_some(), "the key is still stored");
        assert_eq!(a.identify(&token, &key).unwrap(), Identity { has_account: false, user: None });
    }
}
