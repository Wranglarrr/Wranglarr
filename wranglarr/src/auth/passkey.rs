// SPDX-License-Identifier: GPL-3.0-or-later
//! Passkeys: WebAuthn with wranglarr as the relying party, through webauthn-rs.
//!
//! The relying party id is the host name the browser used ([`crate::web::rp_id`]),
//! so it's chosen per request. Each ceremony keeps the origin its options were
//! issued for (scheme, host and port, as wranglarr saw that request), and its
//! finishing step must come from exactly that origin: the request's own, the
//! browser's Origin header and the signed client data all have to match it. Not
//! a sibling name under the rp id, not another port on the same host.
//!
//! Each passkey row in `auth:account.passkeys` has the listing fields (`id`, `name`, `created`, `last_used`, `rp_id`, `backed_up`,
//! `transports`) plus `passkey`, the credential in webauthn-rs's own format.
//! Rows without it (from before 0.2.0) are not listed and can't sign in;
//! registering a new passkey drops them.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use webauthn_rs::prelude::{
    Credential, DiscoverableAuthentication, DiscoverableKey, Passkey, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
};
use webauthn_rs_proto::{AuthenticatorSelectionCriteria, ResidentKeyRequirement, UserVerificationPolicy};

use super::signin::stamp;
use chrono::Utc;

use super::support::{ct_eq_str, guard, trim, unb64u};
use super::{Account, Auth, CHALLENGE_SECONDS, Confirmed};
use crate::error::{Error, Result};
use crate::ids::random_token;
use crate::time::{iso_z, unix_now};

/// Ceremonies in progress, in all ...
pub const MAX_CEREMONIES: usize = 1024;
/// ... and from one address (its oldest give way first): the sign-in options are
/// public, so a flood can't grow the map or crowd out everyone else's.
pub const MAX_CEREMONIES_PER_ADDRESS: usize = 8;

const EXPIRED: &str = "that passkey request expired; try again";
const NOT_REGISTERED: &str = "this passkey isn't registered with wranglarr";

pub(super) enum Ceremony {
    Register(Box<PasskeyRegistration>),
    Login(Box<DiscoverableAuthentication>),
}

pub(super) struct Challenge {
    ceremony: Ceremony,
    rp_id: String,
    /// Where the options were asked for: the only origin that may finish it.
    origin: Url,
    /// The account's [`stamp`] then: a password change or a reset ends the ceremony.
    stamp: String,
    /// Who asked (for the per-address cap).
    ip: String,
    pub(super) expires: f64,
}

/// A passkey row as stored (the webauthn-rs format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasskeyRow {
    pub id: String,
    pub name: String,
    pub created: String,
    pub last_used: Option<String>,
    pub rp_id: String,
    #[serde(default)]
    pub backed_up: bool,
    #[serde(default)]
    pub transports: Vec<Value>,
    pub passkey: Passkey,
}

/// The rows that hold a usable credential (others are skipped).
pub fn rows(acct: &Account) -> Vec<PasskeyRow> {
    acct.passkeys.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect()
}

/// What the account page lists.
pub fn listing(acct: &Account) -> Vec<Value> {
    rows(acct)
        .into_iter()
        .map(|p| {
            json!({"id": p.id, "name": p.name, "created": p.created, "last_used": p.last_used,
                   "rp_id": p.rp_id, "backed_up": p.backed_up})
        })
        .collect()
}

fn to_values(rows: &[PasskeyRow]) -> Result<Vec<Value>> {
    rows.iter().map(|r| serde_json::to_value(r).map_err(Error::from)).collect()
}

/// The WebAuthn user handle: the first 16 bytes of the account's `user_handle`.
fn user_uuid(acct: &Account) -> Uuid {
    match unb64u(&acct.user_handle) {
        Some(b) if b.len() >= 16 => Uuid::from_slice(&b[..16]).unwrap_or_default(),
        _ => {
            use sha2::Digest;
            let h = sha2::Sha256::digest(acct.username.as_bytes());
            Uuid::from_slice(&h[..16]).unwrap_or_default()
        }
    }
}

/// A relying party that accepts exactly `origin` (webauthn-rs allows neither
/// subdomains nor other ports unless asked to).
fn webauthn(rp_id: &str, origin: &Url) -> Result<Webauthn> {
    WebauthnBuilder::new(rp_id, origin)
        .and_then(|b| b.rp_name("wranglarr").build())
        .map_err(|e| Error::invalid(format!("passkeys can't use this address ({rp_id}): {e}")))
}

/// The ceremony's start doesn't depend on the origin.
fn webauthn_for(rp_id: &str) -> Result<Webauthn> {
    let u = Url::parse(&format!("https://{rp_id}"))
        .map_err(|_| Error::invalid(format!("passkeys can't use this address ({rp_id})")))?;
    webauthn(rp_id, &u)
}

/// The origin of a request, as given (`scheme://host[:port]`).
fn parse_origin(here: &str, rp_id: &str) -> Result<Url> {
    Url::parse(here)
        .ok()
        .filter(|u| u.host_str().is_some())
        .ok_or_else(|| Error::invalid(format!("passkeys can't use this address ({rp_id})")))
}

impl Auth {
    /// Passkeys need a secure origin whose host is the relying party itself.
    pub fn check_origin(origin: &str, rp_id: &str) -> Result<Url> {
        let u = Url::parse(origin).ok().filter(|u| u.host_str().is_some_and(|h| !h.is_empty()));
        let Some(u) = u else {
            return Err(Error::invalid("the browser didn't say where this came from (no Origin)"));
        };
        let host = u.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_lowercase();
        if host != rp_id {
            return Err(Error::invalid(format!("this page ({host}) isn't the passkey's site ({rp_id})")));
        }
        if u.scheme() != "https" && host != "localhost" {
            return Err(Error::invalid("passkeys need HTTPS (or http://localhost); see the wiki's Login page"));
        }
        Ok(u)
    }

    /// The origin the ceremony may finish from: the browser's Origin (`sent`) and
    /// this request's own (`here`) must both be the one its options were issued for.
    fn finishing_origin(c: &Challenge, sent: &str, here: &str) -> Result<Url> {
        let sent = Self::check_origin(sent, &c.rp_id)?;
        let here = Url::parse(here).map(|u| u.origin());
        if sent.origin() != c.origin.origin() || here != Ok(c.origin.origin()) {
            return Err(Error::invalid(format!(
                "this page ({}) isn't where the passkey request started ({}); try again",
                sent.origin().ascii_serialization(),
                c.origin.origin().ascii_serialization()
            )));
        }
        Ok(c.origin.clone())
    }

    pub fn rp_id_problem(rp_id: &str) -> Option<&'static str> {
        if rp_id.is_empty() {
            return Some("no host name");
        }
        if rp_id.trim_matches(|c| c == '[' || c == ']').parse::<std::net::IpAddr>().is_ok() {
            return Some("passkeys need a host name, not an IP address");
        }
        None
    }

    fn challenge(&self, ceremony: Ceremony, rp_id: &str, origin: Url, stamp: String, ip: &str) -> String {
        let now = unix_now();
        let mut m = guard(&self.mem);
        Self::sweep(&mut m, now);
        let mut mine: Vec<(String, f64)> =
            m.challenges.iter().filter(|(_, c)| c.ip == ip).map(|(k, c)| (k.clone(), c.expires)).collect();
        if mine.len() >= MAX_CEREMONIES_PER_ADDRESS {
            mine.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in &mine[..=mine.len() - MAX_CEREMONIES_PER_ADDRESS] {
                m.challenges.remove(k);
            }
        }
        let flow = random_token(18);
        let c = Challenge {
            ceremony,
            rp_id: rp_id.to_string(),
            origin,
            stamp,
            ip: ip.to_string(),
            expires: now + CHALLENGE_SECONDS,
        };
        m.challenges.insert(flow.clone(), c);
        trim(&mut m.challenges, MAX_CEREMONIES, |c| c.expires);
        flow
    }

    fn take(&self, flow: &str, register: bool) -> Result<Challenge> {
        let c = guard(&self.mem).challenges.remove(flow);
        match c {
            Some(c) if matches!(c.ceremony, Ceremony::Register(_)) == register && c.expires >= unix_now() => Ok(c),
            _ => Err(Error::invalid(EXPIRED)),
        }
    }

    /// `here`: this request's origin as wranglarr sees it (the web layer's `request_origin`).
    pub fn passkey_register_options(&self, ok: &Confirmed, rp_id: &str, here: &str, ip: &str) -> Result<Value> {
        if let Some(p) = Self::rp_id_problem(rp_id) {
            return Err(Error::invalid(p));
        }
        let origin = parse_origin(here, rp_id)?;
        let acct = self.need_account()?;
        ok.check(&acct)?;
        let exclude: Vec<_> =
            rows(&acct).iter().filter(|p| p.rp_id == rp_id).map(|p| p.passkey.cred_id().clone()).collect();
        let (mut ccr, state) = webauthn_for(rp_id)?
            .start_passkey_registration(user_uuid(&acct), &acct.username, &acct.username, Some(exclude))
            .map_err(|e| Error::invalid(format!("the passkey request failed: {e}")))?;
        // a discoverable credential (resident key required), so the sign-in needs no username
        ccr.public_key.authenticator_selection = Some(AuthenticatorSelectionCriteria {
            authenticator_attachment: None,
            resident_key: Some(ResidentKeyRequirement::Required),
            require_resident_key: true,
            user_verification: UserVerificationPolicy::Required,
        });
        let flow = self.challenge(Ceremony::Register(Box::new(state)), rp_id, origin, stamp(Some(&acct)), ip);
        Ok(json!({"flow": flow, "options": ccr.public_key}))
    }

    /// `sent`: the browser's Origin header; `here`: this request's origin.
    pub fn passkey_register(
        &self,
        flow: &str,
        credential: &Value,
        name: &str,
        sent: &str,
        here: &str,
    ) -> Result<Value> {
        let c = self.take(flow, true)?;
        let origin = Self::finishing_origin(&c, sent, here)?;
        let Ceremony::Register(state) = c.ceremony else {
            return Err(Error::invalid(EXPIRED));
        };
        let verify = || -> std::result::Result<Passkey, String> {
            let reg: RegisterPublicKeyCredential =
                serde_json::from_value(credential.clone()).map_err(|e| e.to_string())?;
            webauthn(&c.rp_id, &origin)
                .map_err(|e| e.to_string())?
                .finish_passkey_registration(&reg, &state)
                .map_err(|e| e.to_string())
        };
        let pk = verify().map_err(|e| Error::invalid(format!("the passkey couldn't be verified: {e}")))?;
        let id = super::support::b64u(pk.cred_id().as_ref());
        let name: String = name.trim().chars().take(60).collect();
        let row = PasskeyRow {
            id: id.clone(),
            name: if name.is_empty() { "passkey".into() } else { name },
            created: iso_z(Utc::now()),
            last_used: None,
            rp_id: c.rp_id.clone(),
            backed_up: Credential::from(pk.clone()).backup_state,
            transports: credential
                .pointer("/response/transports")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            passkey: pk,
        };
        let (started, stored) = (c.stamp.clone(), row.clone());
        self.update_account(move |_, acct| {
            if !ct_eq_str(&stamp(Some(acct)), &started) {
                return Err(Error::invalid(EXPIRED));
            }
            // rows without a credential (and a re-registered id) go
            let mut keep: Vec<PasskeyRow> = rows(acct).into_iter().filter(|p| p.id != id).collect();
            keep.push(stored);
            acct.passkeys = to_values(&keep)?;
            Ok(())
        })?;
        tracing::info!("auth: passkey '{}' added for {}", row.name, row.rp_id);
        Ok(json!({"id": row.id, "name": row.name, "created": row.created, "rp_id": row.rp_id}))
    }

    pub fn passkey_login_options(&self, rp_id: &str, here: &str, ip: &str) -> Result<Value> {
        if let Some(p) = Self::rp_id_problem(rp_id) {
            return Err(Error::invalid(p));
        }
        let origin = parse_origin(here, rp_id)?;
        let (rcr, state) = webauthn_for(rp_id)?
            .start_discoverable_authentication()
            .map_err(|e| Error::invalid(format!("the passkey request failed: {e}")))?;
        let now = stamp(self.account()?.as_ref());
        let flow = self.challenge(Ceremony::Login(Box::new(state)), rp_id, origin, now, ip);
        // without rcr.mediation (conditional UI): the page asks with the usual dialog
        Ok(json!({"flow": flow, "options": rcr.public_key}))
    }

    pub fn passkey_login(&self, ip: &str, flow: &str, credential: &Value, sent: &str, here: &str) -> Result<()> {
        self.check_throttle(ip)?;
        let c = self.take(flow, false)?;
        let origin = Self::finishing_origin(&c, sent, here)?;
        let Ceremony::Login(state) = c.ceremony else {
            return Err(Error::invalid(EXPIRED));
        };
        let acct = self.account()?.unwrap_or_default();
        let raw_id = credential.get("rawId").or_else(|| credential.get("id")).and_then(Value::as_str).unwrap_or("");
        let raw_id = raw_id.trim_end_matches('=');
        let all = rows(&acct);
        let handle = credential.pointer("/response/userHandle").and_then(Value::as_str).filter(|h| !h.is_empty());
        let handle_ok = handle
            .is_none_or(|h| unb64u(h).and_then(|b| Uuid::from_slice(&b).ok()).is_some_and(|u| u == user_uuid(&acct)));
        let Some(row) = all.iter().find(|p| p.id == raw_id && p.rp_id == c.rp_id).filter(|_| handle_ok) else {
            self.failed(ip);
            return Err(Error::Unauthorized(NOT_REGISTERED.into()));
        };
        let verify = || -> std::result::Result<_, String> {
            let cred: PublicKeyCredential = serde_json::from_value(credential.clone()).map_err(|e| e.to_string())?;
            webauthn(&c.rp_id, &origin)
                .map_err(|e| e.to_string())?
                .finish_discoverable_authentication(&cred, *state, &[DiscoverableKey::from(&row.passkey)])
                .map_err(|e| e.to_string())
        };
        let res = match verify() {
            Ok(r) => r,
            Err(e) => {
                self.failed(ip);
                return Err(Error::Unauthorized(format!("the passkey couldn't be verified: {e}")));
            }
        };
        // note the use on the account as it is now, not the copy read above
        let (started, rp_id, raw_id) = (c.stamp.clone(), c.rp_id.clone(), raw_id.to_owned());
        let saved = self.update_account(move |_, acct| {
            if !ct_eq_str(&stamp(Some(acct)), &started) {
                return Err(Error::invalid(EXPIRED));
            }
            let mut all = rows(acct);
            let Some(p) = all.iter_mut().find(|p| p.id == raw_id && p.rp_id == rp_id) else {
                return Err(Error::Unauthorized(NOT_REGISTERED.into()));
            };
            p.passkey.update_credential(&res);
            p.last_used = Some(iso_z(Utc::now()));
            acct.passkeys = to_values(&all)?;
            Ok(())
        });
        match saved {
            Err(Error::NotFound(_)) => return Err(Error::Unauthorized(NOT_REGISTERED.into())),
            other => other?,
        }
        self.succeeded(ip);
        Ok(())
    }

    pub fn passkey_rename(&self, pid: &str, name: &str) -> Result<()> {
        let name: String = name.trim().chars().take(60).collect();
        let pid = pid.to_owned();
        self.update_account(move |_, acct| {
            let mut all = rows(acct);
            let Some(p) = all.iter_mut().find(|p| p.id == pid) else {
                return Err(Error::not_found("no such passkey"));
            };
            if !name.is_empty() {
                p.name = name;
            }
            acct.passkeys = to_values(&all)?;
            Ok(())
        })
    }

    pub fn passkey_delete(&self, pid: &str) -> Result<()> {
        let pid = pid.to_owned();
        self.update_account(move |_, acct| {
            let all = rows(acct);
            let before = all.len();
            let left: Vec<PasskeyRow> = all.into_iter().filter(|p| p.id != pid).collect();
            if left.len() == before {
                return Err(Error::not_found("no such passkey"));
            }
            acct.passkeys = to_values(&left)?;
            Ok(())
        })
    }

    /// Delete every passkey (`wranglarr auth remove-passkeys`): how many rows went.
    pub fn remove_passkeys(&self) -> Result<usize> {
        self.update_account(|_, acct| {
            let n = acct.passkeys.len();
            acct.passkeys.clear();
            Ok(n)
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::auth::Options;
    use crate::state::State;

    #[test]
    fn origins() {
        assert!(Auth::check_origin("https://wranglarr.lan:8787", "wranglarr.lan").is_ok());
        assert!(Auth::check_origin("http://localhost:8787", "localhost").is_ok());
        let e = Auth::check_origin("http://wranglarr.lan:8787", "wranglarr.lan").unwrap_err();
        assert!(e.message().contains("HTTPS"));
        let e = Auth::check_origin("https://evilwranglarr.lan", "wranglarr.lan").unwrap_err();
        assert_eq!(e.message(), "this page (evilwranglarr.lan) isn't the passkey's site (wranglarr.lan)");
        // a name under the rp id is another site: it could ask for wranglarr.lan's passkey and relay it
        let e = Auth::check_origin("https://a.wranglarr.lan", "wranglarr.lan").unwrap_err();
        assert_eq!(e.message(), "this page (a.wranglarr.lan) isn't the passkey's site (wranglarr.lan)");
        assert!(Auth::check_origin("", "x").unwrap_err().message().contains("no Origin"));
        assert_eq!(Auth::rp_id_problem("192.168.1.5"), Some("passkeys need a host name, not an IP address"));
        assert_eq!(Auth::rp_id_problem("[::1]"), Some("passkeys need a host name, not an IP address"));
        assert_eq!(Auth::rp_id_problem(""), Some("no host name"));
        assert_eq!(Auth::rp_id_problem("wranglarr.lan"), None);
    }

    #[test]
    fn a_ceremony_finishes_only_where_it_started() {
        let a = Auth::with_state(State::memory().unwrap(), Options::default()).unwrap();
        let o = a.passkey_login_options("wranglarr.lan", "https://wranglarr.lan", "ip").unwrap();
        let flow = o["flow"].as_str().unwrap();
        let c = a.take(flow, false).unwrap();
        let ok = |sent: &str, here: &str| Auth::finishing_origin(&c, sent, here).is_ok();
        assert!(ok("https://wranglarr.lan", "https://wranglarr.lan"));
        assert!(ok("https://wranglarr.lan:443", "https://WRANGLARR.lan"), "the same origin, written differently");
        // another HTTPS service on this host, relaying with a forged Host header
        assert!(!ok("https://wranglarr.lan:8443", "https://wranglarr.lan:8443"));
        assert!(!ok("https://wranglarr.lan", "https://wranglarr.lan:8443"), "the request's own origin counts too");
        assert!(!ok("https://wranglarr.lan", "http://wranglarr.lan"));
        assert!(!ok("https://a.wranglarr.lan", "https://a.wranglarr.lan"));
        let e = Auth::finishing_origin(&c, "https://wranglarr.lan:8443", "https://wranglarr.lan:8443").unwrap_err();
        assert_eq!(
            e.message(),
            "this page (https://wranglarr.lan:8443) isn't where the passkey request started (https://wranglarr.lan); try again"
        );
    }

    #[test]
    fn ceremonies_are_capped() {
        let a = Auth::with_state(State::memory().unwrap(), Options::default()).unwrap();
        let here = "https://wranglarr.lan";
        let first =
            a.passkey_login_options("wranglarr.lan", here, "flood").unwrap()["flow"].as_str().unwrap().to_string();
        let keep = a.passkey_login_options("wranglarr.lan", here, "me").unwrap()["flow"].as_str().unwrap().to_string();
        for _ in 0..MAX_CEREMONIES_PER_ADDRESS + 5 {
            a.passkey_login_options("wranglarr.lan", here, "flood").unwrap();
        }
        {
            let m = guard(&a.mem);
            assert_eq!(m.challenges.values().filter(|c| c.ip == "flood").count(), MAX_CEREMONIES_PER_ADDRESS);
            assert!(!m.challenges.contains_key(&first), "the flood's oldest went");
            assert!(m.challenges.contains_key(&keep), "someone else's stays");
        }
        for i in 0..MAX_CEREMONIES + 10 {
            a.passkey_login_options("wranglarr.lan", here, &format!("10.0.{i}")).unwrap();
        }
        assert_eq!(guard(&a.mem).challenges.len(), MAX_CEREMONIES);
        // and they expire
        guard(&a.mem).challenges.values_mut().for_each(|c| c.expires = unix_now() - 1.0);
        a.check_throttle("x").unwrap();
        assert!(guard(&a.mem).challenges.is_empty());
    }

    #[test]
    fn rows_without_a_credential_are_ignored() {
        let acct = Account {
            passkeys: vec![json!({"id": "abc", "public_key": "pk", "sign_count": 0, "name": "old", "rp_id": "x",
                                  "created": "2026-01-01T00:00:00Z", "last_used": null})],
            ..Account::default()
        };
        assert!(listing(&acct).is_empty());
    }
}
