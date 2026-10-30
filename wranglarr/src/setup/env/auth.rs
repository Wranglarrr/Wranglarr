// SPDX-License-Identifier: GPL-3.0-or-later
//! App logins: the checks, the AUTH_KEYS values, writing them into .env, and their next steps.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::Utc;
use regex::Regex;

use super::steps::default_user;
use super::{Answers, password_problem, q, valid_username};
use crate::error::{Error, Result};
use crate::service;

static HOST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$").expect("static regex"));
static ENV_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([A-Za-z_][A-Za-z0-9_]*)=").expect("static regex"));

pub fn check_auth(a: &Answers) -> Result<()> {
    if !["apps", "tunnel", "manual"].contains(&a.apps_auth.as_str()) {
        return Err(Error::invalid("app logins: pick one of the three"));
    }
    if a.apps_auth == "apps" {
        if !valid_username(a.apps_login_user.trim()) {
            return Err(Error::invalid("app login username: 1-64 letters, digits and . _ - @"));
        }
        if let Some(p) = password_problem(&a.apps_login_password, &a.apps_login_user) {
            return Err(Error::invalid(format!("app login password: {p}")));
        }
        if !["enabled", "disabledForLocalAddresses"].contains(&a.apps_auth_required.as_str()) {
            return Err(Error::invalid("app login: ask everywhere, or not on the local network"));
        }
    }
    if a.apps_auth == "tunnel" {
        let d = a.auth_cookie_domain.trim().trim_start_matches('.').to_lowercase();
        let by_ip = "by this machine's IP and port the apps already work, with this left blank";
        if d.parse::<std::net::IpAddr>().is_ok() {
            return Err(Error::invalid(format!(
                "cookie domain {d:?}: your domain, like example.com, not an IP: {by_ip}"
            )));
        }
        if !d.is_empty() && !HOST_RE.is_match(&d) {
            return Err(Error::invalid(format!("cookie domain {d:?}: a domain like example.com, no scheme or port")));
        }
        let u = a.auth_login_url.trim();
        if !u.is_empty()
            && (crate::links::check_url(u, "wranglarr's address").is_err()
                || crate::links::parse_url(u).is_none_or(|p| {
                    !p.username().is_empty()
                        || p.password().is_some()
                        || p.query().is_some()
                        || p.fragment().is_some()
                        || crate::url_base::check(p.path()).is_err()
                }))
        {
            return Err(Error::invalid(
                "wranglarr's address: like https://wranglarr.example.com or https://example.com/wranglarr, without credentials, query or fragment",
            ));
        }
        let host = crate::links::parse_url(u).as_ref().and_then(crate::links::url_host).unwrap_or_default();
        if host.parse::<std::net::IpAddr>().is_ok() {
            return Err(Error::invalid(format!(
                "wranglarr's address: its name on your reverse proxy, like https://wranglarr.example.com, not an IP: {by_ip}"
            )));
        }
        if !d.is_empty() && !host.is_empty() && host != d && !host.ends_with(&format!(".{d}")) {
            return Err(Error::invalid(format!(
                "wranglarr's address: {host} isn't under the cookie domain {d}, so the sign-in wouldn't reach the apps"
            )));
        }
        if !d.is_empty() && u.is_empty() {
            return Err(Error::invalid(
                "with a cookie domain, give wranglarr's address too (where the gate sends you to sign in)",
            ));
        }
    }
    Ok(())
}

/// Insertion-ordered KEY -> value pairs.
pub type Pairs = Vec<(&'static str, String)>;

fn set(p: &mut Pairs, k: &'static str, v: String) {
    match p.iter_mut().find(|(n, _)| *n == k) {
        Some(e) => e.1 = v,
        None => p.push((k, v)),
    }
}

pub fn get<'a>(p: &'a Pairs, k: &str) -> &'a str {
    p.iter().find(|(n, _)| *n == k).map_or("", |(_, v)| v.as_str())
}

pub(super) fn or<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() { b } else { a }
}

/// The AUTH_KEYS values. apps: qBittorrent and NZBGet get the same login. tunnel: they keep
/// theirs (wranglarr, the arrs and the gate use it; you never type it) or get a random one.
pub fn auth_env(a: &Answers, old: &BTreeMap<String, String>) -> Result<Pairs> {
    let mut out: Pairs = vec![
        ("APPS_AUTH", a.apps_auth.clone()),
        ("APPS_LOGIN_USER", String::new()),
        ("APPS_LOGIN_PASSWORD", String::new()),
        ("APPS_AUTH_REQUIRED", "enabled".into()),
        ("AUTH_COOKIE_DOMAIN", String::new()),
        ("AUTH_LOGIN_URL", String::new()),
    ];
    let old = |k: &str| old.get(k).map_or("", String::as_str);
    let (mut qu, mut qp) =
        (or(&a.qbittorrent_user, default_user(&service::qbittorrent::DEF)).to_string(), a.qbittorrent_password.clone());
    let (mut nu, mut np) = (or(&a.nzbget_user, default_user(&service::nzbget::DEF)).to_string(), a.nzbget_pass.clone());
    if a.apps_auth == "apps" {
        let u = a.apps_login_user.trim().to_string();
        set(&mut out, "APPS_LOGIN_USER", u.clone());
        set(&mut out, "APPS_LOGIN_PASSWORD", a.apps_login_password.clone());
        set(&mut out, "APPS_AUTH_REQUIRED", a.apps_auth_required.clone());
        qu.clone_from(&u);
        nu = u;
        qp.clone_from(&a.apps_login_password);
        np.clone_from(&a.apps_login_password);
    } else if a.apps_auth == "tunnel" {
        set(&mut out, "AUTH_COOKIE_DOMAIN", a.auth_cookie_domain.trim().trim_start_matches('.').to_lowercase());
        set(&mut out, "AUTH_LOGIN_URL", a.auth_login_url.trim().trim_end_matches('/').to_string());
        qu = or(or(&qu, old("QBITTORRENT_USER")), default_user(&service::qbittorrent::DEF)).to_string();
        qp = if !qp.is_empty() {
            qp
        } else if !old("QBITTORRENT_PASSWORD").is_empty() {
            old("QBITTORRENT_PASSWORD").to_string()
        } else {
            crate::ids::random_token(18)
        };
        nu = or(or(&nu, old("NZBGET_USER")), default_user(&service::nzbget::DEF)).to_string();
        np = if !np.is_empty() {
            np
        } else if !old("NZBGET_PASS").is_empty() {
            old("NZBGET_PASS").to_string()
        } else {
            crate::ids::random_token(18)
        };
    }
    set(&mut out, "QBITTORRENT_USER", qu);
    set(&mut out, "QBITTORRENT_PASSWORD", qp);
    set(&mut out, "NZBGET_USER", nu);
    set(&mut out, "NZBGET_PASS", np);
    Ok(out)
}

/// Replace each key's last uncommented line in .env; append the ones that aren't there.
pub fn set_env_keys(text: &str, values: &Pairs) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut last: Vec<(&'static str, usize)> = vec![];
    for (i, line) in lines.iter().enumerate() {
        if let Some(c) = ENV_KEY_RE.captures(line)
            && let Some((k, _)) = values.iter().find(|(k, _)| *k == &c[1])
        {
            match last.iter_mut().find(|(n, _)| n == k) {
                Some(e) => e.1 = i,
                None => last.push((k, i)),
            }
        }
    }
    for (k, i) in &last {
        lines[*i] = format!("{k}={}", q(get(values, k))?);
    }
    let missing: Vec<&(&'static str, String)> =
        values.iter().filter(|(k, _)| !last.iter().any(|(n, _)| n == k)).collect();
    if !missing.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "# App logins (wranglarr-setup --auth, {}); applied by ./scripts/init.sh --auth",
            Utc::now().format("%Y-%m-%d")
        ));
        for (k, v) in missing {
            lines.push(format!("{k}={}", q(v)?));
        }
    }
    Ok(lines.join("\n") + "\n")
}

pub fn auth_steps(a: &Answers, auth_only: bool) -> Vec<String> {
    let mut s = vec![];
    if auth_only {
        s.push("Apply it, in the stack directory: ./scripts/init.sh --auth  (it sets each app's login, and starts or removes wranglarr-gate for AuthTunnel).".to_string());
    }
    if a.apps_auth == "apps" {
        s.push(format!("Every app's login is {:?} with the password you chose. init.sh sets it in each app and signs in with it to check; the API keys don't change.", a.apps_login_user.trim()));
    } else if a.apps_auth == "tunnel" {
        let where_ = if a.auth_cookie_domain.trim().is_empty() {
            "sign in to wranglarr at the same address you open the apps at".to_string()
        } else {
            format!(
                "through the proxy you sign in at {} and the cookie covers *.{}; by IP and port, at wranglarr on that address",
                a.auth_login_url.trim().trim_end_matches('/'),
                a.auth_cookie_domain.trim().trim_start_matches('.').to_lowercase()
            )
        };
        s.push(format!("AuthTunnel: the apps' ports now belong to wranglarr-gate, which asks for your wranglarr sign-in; {where_}. Point your reverse proxy at the same ports as before (wiki: Reverse-Proxy)."));
    } else {
        s.push(
            "Manual: wranglarr leaves the apps' logins alone; each app asks you to create one on first visit.".into(),
        );
        if a.services.iter().any(|x| x == service::qbittorrent::DEF.id) && a.qbittorrent_password.is_empty() {
            s.push("qBittorrent prints a temporary admin password on first start: docker compose logs qbittorrent. Set a permanent one in its UI, then put it in .env as QBITTORRENT_PASSWORD and run: docker compose up -d wranglarr.".into());
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::super::render_env;
    use super::*;

    #[test]
    fn logins() {
        let a = Answers { apps_login_password: "short".into(), ..Default::default() };
        assert_eq!(check_auth(&a).expect_err("bad").to_string(), "app login password: use at least 12 characters");
        let a = Answers { apps_login_password: "a-long-Enough-pass".into(), ..Default::default() };
        assert!(check_auth(&a).is_ok());
        let a = Answers { apps_auth: "tunnel".into(), auth_cookie_domain: ".Example.COM".into(), ..Default::default() };
        assert!(check_auth(&a).expect_err("bad").to_string().starts_with("with a cookie domain"));
        let a = Answers { apps_auth: "tunnel".into(), auth_cookie_domain: "http://x".into(), ..Default::default() };
        assert_eq!(
            check_auth(&a).expect_err("bad").to_string(),
            r#"cookie domain "http://x": a domain like example.com, no scheme or port"#
        );
        // the machine's IP where the proxy's names go: by IP it works without them
        let tunnel = |d: &str, u: &str| Answers {
            apps_auth: "tunnel".into(),
            auth_cookie_domain: d.into(),
            auth_login_url: u.into(),
            ..Default::default()
        };
        let e = check_auth(&tunnel("example.com", "http://192.168.1.10:8787")).expect_err("ip").to_string();
        assert!(e.starts_with("wranglarr's address: its name on your reverse proxy") && e.contains("not an IP"), "{e}");
        let e = check_auth(&tunnel("192.168.1.10", "https://wranglarr.example.com")).expect_err("ip").to_string();
        assert!(e.starts_with(r#"cookie domain "192.168.1.10": your domain"#), "{e}");
        assert_eq!(
            check_auth(&tunnel("example.com", "https://wranglarr.other.net")).expect_err("elsewhere").to_string(),
            "wranglarr's address: wranglarr.other.net isn't under the cookie domain example.com, so the sign-in wouldn't reach the apps"
        );
        assert!(check_auth(&tunnel("example.com", "https://wranglarr.example.com")).is_ok());
        let a = Answers {
            apps_auth: "tunnel".into(),
            auth_cookie_domain: ".Example.COM".into(),
            auth_login_url: "https://w.example.com/".into(),
            ..Default::default()
        };
        let e = auth_env(&a, &BTreeMap::new()).expect("env");
        assert_eq!(get(&e, "AUTH_COOKIE_DOMAIN"), "example.com");
        assert_eq!(get(&e, "AUTH_LOGIN_URL"), "https://w.example.com");
        assert_eq!(get(&e, "QBITTORRENT_PASSWORD").len(), 24, "a random one");
        let old = BTreeMap::from([("NZBGET_PASS".to_string(), "kept".to_string())]);
        assert_eq!(get(&auth_env(&a, &old).expect("env"), "NZBGET_PASS"), "kept");
    }

    #[test]
    fn env_keys_replaced() {
        let vals: Pairs = vec![("A", "x y".into()), ("B", "2".into())];
        let out = set_env_keys("A=1\n# A=0\nA=3\nC=4", &vals).expect("ok");
        assert!(out.starts_with("A=1\n# A=0\nA='x y'\nC=4\n\n# App logins (wranglarr-setup --auth, "), "{out}");
        assert!(out.ends_with("); applied by ./scripts/init.sh --auth\nB=2\n"), "{out}");
    }

    #[test]
    fn login_addresses_may_include_the_url_base() {
        let tunnel = |url: &str| Answers {
            apps_auth: "tunnel".into(),
            auth_cookie_domain: "example.com".into(),
            auth_login_url: url.into(),
            ..Default::default()
        };
        check_auth(&tunnel("https://example.com/wranglarr")).expect("login at a path");
        for bad in [
            "https://example.com/w?x=1",
            "https://example.com/w#x",
            "https://u:p@example.com/w",
            "https://example.com/api",
        ] {
            assert!(check_auth(&tunnel(bad)).is_err(), "{bad}");
        }
        let a = Answers { wranglarr_url_base: "/w/".into(), postgres_password: "x".into(), ..Default::default() };
        assert!(render_env(&a, "img").expect("env").contains("WRANGLARR_URL_BASE=/w\n"));
    }
}
