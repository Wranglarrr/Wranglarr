// SPDX-License-Identifier: GPL-3.0-or-later
//! The wizard's answers, their checks, and what they become: `.env`, the
//! backup preferences in a new wranglarr.toml, and the next steps shown at the end.

mod answers;
mod auth;
mod render;
mod steps;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::config::env_quote;
use crate::error::{Error, Result};
use crate::service::{self, ServiceDef};

pub use answers::{Answers, VpnCheck};
pub use auth::{Pairs, auth_env, auth_steps, check_auth, get, set_env_keys};
pub use render::{mask_secrets, render_env};
pub use steps::next_steps;

/// The services the wizard offers, in order (the registry's, where it says so).
pub fn services() -> impl Iterator<Item = &'static ServiceDef> {
    service::ALL.iter().copied().filter(|d| d.wizard.is_some())
}

/// A wizard service's config file inside its folder (`config.xml`).
pub fn config_file(id: &str) -> Option<&'static str> {
    services().find(|d| d.id == id).and_then(|d| d.wizard).and_then(|w| w.config)
}

/// The wizard's services as the page gets them.
pub fn services_json() -> Value {
    Value::Array(
        services()
            .map(|d| {
                let w = d.wizard.as_ref();
                let mut v = json!({"id": d.id, "name": d.label, "group": w.map_or("", |w| w.group),
                                   "desc": d.description, "port": d.container.ui_port(), "db": d.database.is_some()});
                if let Some(c) = w.and_then(|w| w.config) {
                    v["config"] = json!(c);
                }
                if !d.container.vpn {
                    v["vpn"] = json!(false);
                }
                v
            })
            .collect(),
    )
}

pub const ENV_KEYS_SECRET: &[&str] = &[
    "POSTGRES_PASSWORD",
    "GLUETUN_API_KEY",
    "QBITTORRENT_PASSWORD",
    "WIREGUARD_PRIVATE_KEY",
    "WIREGUARD_PRESHARED_KEY",
    "VPN_PASSWORD",
    "NZBGET_PASS",
    "VPN_PIA_PASS",
    "APPS_LOGIN_PASSWORD",
];

/// Secrets already in .env never go back to the browser: it gets KEEP instead, and KEEP coming back
/// means "leave it as it is" (long enough for the wizard's 12-character password check).
pub const KEEP: &str = "•••••• unchanged ••••••";

/// What `wranglarr-setup --auth` may change in .env.
pub const AUTH_KEYS: &[&str] = &[
    "APPS_AUTH",
    "APPS_LOGIN_USER",
    "APPS_LOGIN_PASSWORD",
    "APPS_AUTH_REQUIRED",
    "AUTH_COOKIE_DOMAIN",
    "AUTH_LOGIN_URL",
    "QBITTORRENT_USER",
    "QBITTORRENT_PASSWORD",
    "NZBGET_USER",
    "NZBGET_PASS",
];

static HEAD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\[").expect("static regex"));
static TOML_KEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\w+)\s*=").expect("static regex"));

// ---- passwords ----------------------------------------------------------------------------
// the same rules as the login itself
pub use crate::auth::{password_problem, valid_username};

// ---- helpers ---------------------------------------------------------------------------------

/// The KEY=value lines of a .env (quotes around a value dropped).
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let (k, v) = line.split_once('=').unwrap_or((line, ""));
        let mut v = v.trim();
        let b = v.as_bytes();
        if b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'') {
            v = &v[1..v.len() - 1];
        }
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

/// `.env` read as [`parse_env`]; none yet is empty.
pub fn read_env(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    Ok(parse_env(&std::fs::read_to_string(path)?))
}

/// env_quote, or a 400 for a value .env can't hold.
pub fn q(v: &str) -> Result<String> {
    env_quote(v).map_err(|e| Error::invalid(format!("a value {e}")))
}

/// Name the field when a value can't be written to .env (q() alone can't say which).
pub fn check_values(a: &Answers) -> Result<()> {
    let all = a
        .strings()
        .into_iter()
        .map(|(f, v)| ("", f, v))
        .chain(a.vpn.strings().into_iter().map(|(f, v)| ("VPN ", f, v)));
    for (pre, f, v) in all {
        if let Err(e) = env_quote(v) {
            return Err(Error::invalid(format!("{pre}{}: {e}", f.replace('_', " "))));
        }
    }
    Ok(())
}

pub fn check_backup_prefs(a: &Answers) -> Result<()> {
    let cron = a.backup_cron.trim();
    if !cron.is_empty()
        && let Err(e) = crate::sched::parse(cron)
    {
        return Err(Error::invalid(format!("backup schedule {:?}: {e}", a.backup_cron)));
    }
    if a.keep_last < 0 || a.keep_days < 0 {
        return Err(Error::invalid("backup retention: 0 or more"));
    }
    Ok(())
}

/// The wizard's schedule goes on the first suite (media); the others keep their
/// staggered times unless the schedule is blank (no automatic backups at all).
/// Retention goes on every suite.
pub fn apply_backup_prefs(text: &str, a: &Answers) -> String {
    let cron = a.backup_cron.trim();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let heads: Vec<usize> = (0..lines.len()).filter(|&n| HEAD_RE.is_match(&lines[n])).collect();
    let suites: Vec<usize> = heads.iter().copied().filter(|&x| lines[x].trim() == "[[suite]]").collect();
    let mut out_of_order: Vec<(usize, Vec<String>)> = vec![]; // (index after which to insert, lines)
    for (si, &n) in suites.iter().enumerate() {
        let end = heads.iter().copied().find(|&h| h > n).unwrap_or(lines.len());
        let mut want: Vec<(&str, String)> =
            vec![("keep_last", a.keep_last.to_string()), ("keep_days", a.keep_days.to_string())];
        if si == 0 || cron.is_empty() {
            want.push(("schedule", crate::standalone::q(cron)));
        }
        let mut last_key = n;
        for (k, line) in lines.iter_mut().enumerate().take(end).skip(n + 1) {
            let Some(name) = TOML_KEY_RE.captures(line).map(|c| c[1].to_string()) else { continue };
            last_key = k;
            if let Some(i) = want.iter().position(|(w, _)| *w == name) {
                let (_, v) = want.remove(i);
                let note = if name == "schedule" && si == 0 { "   # from the setup wizard" } else { "" };
                *line = format!("{name} = {v}{note}");
            }
        }
        if !want.is_empty() {
            out_of_order.push((last_key, want.iter().map(|(k, v)| format!("{k} = {v}")).collect()));
        }
    }
    for (at, add) in out_of_order.into_iter().rev() {
        lines.splice(at + 1..at + 1, add);
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_parse() {
        let e = parse_env("# c\nA=1\n B = 'x y' \nC=\"q\"\nD='\nnoeq\nA=2\n");
        assert_eq!(e.get("A").map(String::as_str), Some("2"));
        assert_eq!(e.get("B").map(String::as_str), Some("x y"));
        assert_eq!(e.get("C").map(String::as_str), Some("q"));
        assert_eq!(e.get("D").map(String::as_str), Some("'"));
        assert!(!e.contains_key("noeq"));
    }

    #[test]
    fn values_name_the_field() {
        let a = Answers { postgres_password: "pa$$'word".into(), ..Default::default() };
        assert_eq!(
            check_values(&a).expect_err("bad").to_string(),
            "postgres password: can't contain both ' and one of $ \" \\"
        );
        let mut a = Answers::default();
        a.vpn.lan_network = "a\nb".into();
        assert_eq!(check_values(&a).expect_err("bad").to_string(), "VPN lan network: can't contain a line break");
    }

    #[test]
    fn backup_prefs() {
        let a = Answers { backup_cron: "nonsense".into(), ..Default::default() };
        assert!(check_backup_prefs(&a).expect_err("bad").to_string().starts_with(r#"backup schedule "nonsense": "#));
        let a = Answers { keep_days: -1, ..Default::default() };
        assert_eq!(check_backup_prefs(&a).expect_err("bad").to_string(), "backup retention: 0 or more");
        assert!(check_backup_prefs(&Answers { backup_cron: " ".into(), ..Default::default() }).is_ok());
        let text = "[server]\nport = 1\n[[suite]]\nname = \"a\"\nschedule = \"x\"  # c\nkeep_last = 3\n  [[suite.app]]\n\
                    name = \"r\"\n[[suite]]\nname = \"b\"\nschedule = \"y\"\n";
        let a = Answers { backup_cron: "0 2 * * *".into(), keep_last: 5, keep_days: 9, ..Default::default() };
        assert_eq!(
            apply_backup_prefs(text, &a),
            "[server]\nport = 1\n[[suite]]\nname = \"a\"\nschedule = \"0 2 * * *\"   # from the setup wizard\nkeep_last = 5\n\
             keep_days = 9\n  [[suite.app]]\nname = \"r\"\n[[suite]]\nname = \"b\"\nschedule = \"y\"\nkeep_last = 5\nkeep_days = 9\n"
        );
        let a = Answers { backup_cron: String::new(), ..Default::default() };
        assert!(apply_backup_prefs(text, &a).contains("name = \"b\"\nschedule = \"\"\n"));
    }
}
