// SPDX-License-Identifier: GPL-3.0-or-later
//! A report of a failed update: what was tried, how it failed, and the failed
//! container's last log lines, with secrets, addresses and domains taken out.
//!
//! Each report has a fingerprint, `wf-` and ten hex digits, made from what the
//! failure looks like rather than when or where it happened. People who hit
//! the same failure get the same fingerprint, so one search of the issues
//! finds the report someone already filed.

use std::collections::HashSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::recovery::{DataRecovery, Outcome, Recovery, RecoveryPhase};
use super::recreate::SwapFacts;
use super::{OLD_SUFFIX, Target, Updater};
use crate::jobs::RunLog;
use crate::service::ReportTo;
use crate::value::head;

/// The state key the reports are kept under, newest first.
pub(crate) const KEY: &str = "updates:reports";
/// How many are kept.
const KEEP: usize = 25;
/// The issue form in the repo the report goes to.
pub(crate) const TEMPLATE: &str = "update-failure.yml";
/// Log lines a report keeps at most.
const LOG_LINES: usize = 60;
/// A GitHub new-issue link longer than this is cut by browsers or GitHub: the
/// report is then pasted by hand.
const URL_MAX: usize = 7000;

/// What a failed container looked like just before it was removed.
#[derive(Debug, Clone, Default)]
pub(super) struct Captured {
    pub(crate) log: String,
    /// Its `State` from inspect: exit code, OOM, health checks.
    pub(crate) state: Value,
}

/// One failed update of one container.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Report {
    /// This failure here: `<run>-<container>`. Its page and API go by it.
    pub(crate) key: String,
    /// The managed container as named here, for finding its reports. Only the
    /// cleaned `container` goes into what's exported.
    pub(crate) target: String,
    /// The fingerprint: the same failure anywhere has the same one.
    pub(crate) id: String,
    pub(crate) container: String,
    pub(crate) kind: String,
    pub(crate) label: String,
    /// The image without its tag or digest.
    pub(crate) image: String,
    pub(crate) old_version: String,
    pub(crate) new_version: String,
    pub(crate) old_image: String,
    pub(crate) new_image: String,
    pub(crate) error: String,
    pub(crate) exit_code: Option<i64>,
    pub(crate) oom: bool,
    pub(crate) health: Vec<String>,
    pub(crate) state: Recovery,
    pub(crate) blocked: bool,
    /// What happened to the app's data.
    pub(crate) data: String,
    pub(crate) data_recovery: DataRecovery,
    pub(crate) recovery_phase: Option<RecoveryPhase>,
    pub(crate) log: Vec<String>,
    pub(crate) wranglarr: String,
    /// `owner/name` on GitHub; "" when nobody is known to report it to.
    pub(crate) repo: String,
    /// The repo is wranglarr's or an official lariat's.
    pub(crate) curated: bool,
    pub(crate) at: String,
    pub(crate) run_id: i64,
}

/// What [`Report::new`] is made from.
pub(super) struct Failure<'a> {
    pub(crate) container: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) label: &'a str,
    pub(crate) facts: &'a SwapFacts,
    pub(crate) error: &'a str,
    pub(crate) captured: Option<&'a Captured>,
    pub(crate) outcome: &'a Outcome,
    pub(crate) report_to: ReportTo,
    pub(crate) run_id: i64,
}

impl Report {
    /// The report of `f`, every field of it through `r`: it's made to be sent.
    pub(super) fn new(f: &Failure<'_>, r: &Redactor, now: &str) -> Report {
        let st = f.captured.map(|c| &c.state).unwrap_or(&Value::Null);
        let raw_log = f.captured.map(|c| c.log.as_str()).unwrap_or("");
        let log = pick_lines(&r.clean(raw_log));
        let error = r.clean(f.error);
        let health: Vec<String> = st["Health"]["Log"]
            .as_array()
            .map(|l| {
                let outs: Vec<String> = l
                    .iter()
                    .filter_map(|h| h["Output"].as_str())
                    .map(|o| head(r.clean(o).trim(), 300))
                    .filter(|o| !o.is_empty())
                    .collect();
                outs[outs.len().saturating_sub(3)..].to_vec()
            })
            .unwrap_or_default();
        // a private registry's name is as much yours as your domain
        let image = r.clean(&repo_of(&f.facts.reference));
        let (old_version, new_version) = (r.clean(&f.facts.old_version), r.clean(&f.facts.new_version));
        let first_error = log.iter().find(|l| ERROR_LINE.is_match(l)).map_or("", String::as_str);
        let id = fingerprint(f.kind, &image, &new_version, &error, first_error);
        Report {
            key: format!("{}-{}", f.run_id, f.container),
            target: f.container.to_owned(),
            id,
            container: r.clean(f.container),
            kind: f.kind.to_owned(),
            label: r.clean(f.label),
            image,
            old_version,
            new_version,
            old_image: super::short(&f.facts.old_image),
            new_image: super::short(&f.facts.new_image),
            error,
            exit_code: st["ExitCode"].as_i64().filter(|c| *c != 0),
            oom: st["OOMKilled"].as_bool().unwrap_or(false),
            health,
            state: f.outcome.state,
            blocked: f.outcome.blocked,
            data: r.clean(&f.outcome.data),
            data_recovery: DataRecovery {
                completed: f.outcome.data_recovery.completed.iter().map(|part| r.clean(part)).collect(),
                ..f.outcome.data_recovery.clone()
            },
            recovery_phase: f.outcome.recovery_phase,
            log,
            wranglarr: crate::VERSION.to_owned(),
            repo: f.report_to.repo.clone(),
            curated: f.report_to.curated,
            at: now.to_owned(),
            run_id: f.run_id,
        }
    }

    pub(super) fn version(v: &str, image: &str) -> String {
        if v.is_empty() { format!("image {image}") } else { v.to_owned() }
    }

    /// Where the container was left, in a sentence or two: never more than
    /// was found.
    pub(crate) fn outcome(&self) -> String {
        let (c, old, new) = (
            &self.container,
            Self::version(&self.old_version, &self.old_image),
            Self::version(&self.new_version, &self.new_image),
        );
        let state = match self.state {
            Recovery::Running => format!("{c} is back on {old}."),
            Recovery::Unhealthy => format!("{c} was put back on {old}, but it didn't come up healthy: check it."),
            Recovery::Stopped => format!("{c} was put back on {old}, stopped as it was."),
            Recovery::Parked => format!(
                "{c} is stopped: the old container is parked as {c}{OLD_SUFFIX} for recovery by hand (see the run log)."
            ),
            Recovery::NotStarted => format!("{c} was put back on {old}, but couldn't be started: start it by hand."),
            Recovery::Rejected => format!(
                "{c} is stopped: the new container couldn't be removed, and the old one is parked as \
                 {c}{OLD_SUFFIX} for recovery by hand (see the run log)."
            ),
            Recovery::RejectedRunning => format!(
                "The new container {c} couldn't be removed or stopped and may still be running: stop it before \
                 anything else, then recover {c}{OLD_SUFFIX} by hand (see the run log)."
            ),
            Recovery::Unknown => format!(
                "wranglarr couldn't tell how {c} was left: check {c} and {c}{OLD_SUFFIX} in Docker before \
                 starting either (see the run log)."
            ),
        };
        let block = if self.blocked {
            format!("{new} is skipped from now on.")
        } else {
            format!("wranglarr couldn't record that {new} failed, so it may be tried again: hold {c} until it's fixed.")
        };
        [state, block, self.data.clone()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// The issue title: it carries the fingerprint so a search finds it.
    pub(crate) fn title(&self) -> String {
        format!(
            "Update failed: {} {} → {} [{}]",
            self.label,
            Self::version(&self.old_version, &self.old_image),
            Self::version(&self.new_version, &self.new_image),
            self.id
        )
    }

    /// The report as Markdown, for the issue body.
    pub(crate) fn markdown(&self) -> String {
        let mut m = String::new();
        let line = |m: &mut String, k: &str, v: &str| m.push_str(&format!("- **{k}:** {v}\n"));
        m.push_str(&format!("### Update failure `{}`\n\n", self.id));
        line(&mut m, "App", &format!("{} (`{}`)", self.label, self.kind));
        line(&mut m, "Image", &format!("`{}`", self.image));
        line(
            &mut m,
            "From → to",
            &format!(
                "{} (`{}`) → {} (`{}`)",
                Self::version(&self.old_version, &self.old_image),
                self.old_image,
                Self::version(&self.new_version, &self.new_image),
                self.new_image
            ),
        );
        line(&mut m, "wranglarr", &self.wranglarr);
        line(&mut m, "What happened", &self.error);
        if let Some(c) = self.exit_code {
            line(&mut m, "Exit code", &c.to_string());
        }
        if self.oom {
            line(&mut m, "Out of memory", "yes, the kernel killed it");
        }
        let after = self.outcome();
        if !after.is_empty() {
            line(&mut m, "Afterwards", &after);
        }
        if !self.health.is_empty() {
            m.push_str("\n#### Health check\n\n```\n");
            m.push_str(&self.health.join("\n"));
            m.push_str("\n```\n");
        }
        m.push_str("\n#### Log\n\n");
        if self.log.is_empty() {
            m.push_str("The container left no log.\n");
        } else {
            m.push_str("```\n");
            m.push_str(&self.log.join("\n"));
            m.push_str("\n```\n");
        }
        m
    }

    /// The links to search for this failure and to file it, and the title to use.
    pub(crate) fn links(&self) -> Value {
        if self.repo.is_empty() {
            return json!({"search": null, "new_issue": null, "title": self.title(), "body_in_link": false});
        }
        let base = format!("https://github.com/{}/issues", self.repo);
        let search = url::Url::parse_with_params(&base, [("q", format!("is:issue \"{}\"", self.id))])
            .map(String::from)
            .unwrap_or_default();
        let new = format!("{base}/new");
        let versions = format!(
            "{} → {}",
            Self::version(&self.old_version, &self.old_image),
            Self::version(&self.new_version, &self.new_image)
        );
        let mut params = vec![
            ("template", TEMPLATE.to_owned()),
            ("title", self.title()),
            ("fingerprint", self.id.clone()),
            ("app", format!("{} ({})", self.label, self.kind)),
            ("versions", versions),
        ];
        let short = url::Url::parse_with_params(&new, &params).map(String::from).unwrap_or_default();
        params.push(("report", self.markdown()));
        let full = url::Url::parse_with_params(&new, &params).map(String::from).unwrap_or_default();
        let body_in_link = full.len() <= URL_MAX;
        json!({
            "search": search,
            "new_issue": if body_in_link { full } else { short },
            "title": self.title(),
            "body_in_link": body_in_link,
        })
    }

    /// What a run's result and the Updates page show of it.
    pub(super) fn brief(&self) -> Value {
        let l = self.links();
        json!({"key": self.key, "id": self.id, "title": l["title"], "search": l["search"], "state": self.state})
    }

    /// The report for the API: itself, its Markdown and links, and how often
    /// the same failure is among the kept ones (`all`).
    pub(crate) fn view(&self, all: &[Report]) -> Value {
        let same: Vec<&Report> = all.iter().filter(|r| r.id == self.id).collect();
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        v["title"] = json!(self.title());
        v["outcome"] = json!(self.outcome());
        v["markdown"] = json!(self.markdown());
        v["links"] = self.links();
        v["seen"] = json!(same.len().max(1));
        v["first_seen"] = json!(same.iter().map(|r| r.at.as_str()).min().unwrap_or(&self.at));
        v
    }
}

/// Where GitHub's API is: `WRANGLARR_GITHUB_API` points the tests at a stand-in.
fn github_api() -> String {
    std::env::var("WRANGLARR_GITHUB_API")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://api.github.com".into())
}

/// The oldest issue in a GitHub search answer, pull requests left out, and
/// only one in `repo`: its link is opened in the browser.
fn first_issue(v: &Value, repo: &str) -> Option<Value> {
    let prefix = format!("https://github.com/{repo}/issues/");
    v["items"]
        .as_array()?
        .iter()
        .filter(|i| i.get("pull_request").is_none())
        .filter(|i| i["html_url"].as_str().is_some_and(|u| u.starts_with(&prefix)))
        .min_by_key(|i| i["number"].as_i64().unwrap_or(i64::MAX))
        .map(|i| json!({"number": i["number"], "title": i["title"], "state": i["state"], "url": i["html_url"]}))
}

/// `https://github.com/owner/name(.git)(/...)` -> `owner/name`
fn github_repo(url: &str) -> Option<String> {
    let rest =
        url.trim().strip_prefix("https://github.com/").or_else(|| url.trim().strip_prefix("http://github.com/"))?;
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let (owner, name) = (parts.next()?, parts.next()?.trim_end_matches(".git"));
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    (ok(owner) && ok(name)).then(|| format!("{owner}/{name}"))
}

/// `lscr.io/linuxserver/radarr:latest` -> `lscr.io/linuxserver/radarr`
fn repo_of(reference: &str) -> String {
    let r = reference.split('@').next().unwrap_or(reference);
    match r.rfind(':') {
        Some(i) if !r[i..].contains('/') => r[..i].to_owned(),
        _ => r.to_owned(),
    }
}

static ERROR_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(error|fatal|exception|panic|panicked|traceback|critical|failed|failure|unhandled)\b")
        .expect("static regex")
});
/// A stack frame or the continuation of one.
static FRAME_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s+(at |File "|\.\.\.|---|Caused by|[0-9]+: )"#).expect("static regex"));

/// The lines worth reading: errors and their stack frames, at most
/// [`LOG_LINES`]; the last lines when nothing looks like an error.
fn pick_lines(log: &str) -> Vec<String> {
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut keep: Vec<String> = Vec::new();
    let mut in_error = false;
    for l in &lines {
        if ERROR_LINE.is_match(l) {
            in_error = true;
        } else if !(in_error && FRAME_LINE.is_match(l)) {
            in_error = false;
            continue;
        }
        keep.push(head(l.trim_end(), 500));
    }
    if keep.is_empty() {
        return lines[lines.len().saturating_sub(20)..].iter().map(|l| head(l.trim_end(), 500)).collect();
    }
    // the last ones: the first errors are often noise from before the crash
    keep.split_off(keep.len().saturating_sub(LOG_LINES))
}

static NUMBERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|\b0x[0-9a-f]+|\b[0-9a-f]{8,}\b|[0-9]+",
    )
    .expect("static regex")
});
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("static regex"));

/// The parts of a line that stay the same from one machine to the next:
/// numbers, ids and timestamps go.
fn normalize(s: &str) -> String {
    let s = NUMBERS.replace_all(s, "#");
    SPACE.replace_all(s.trim(), " ").to_lowercase()
}

/// `wf-` and ten hex digits of what this failure looks like.
fn fingerprint(kind: &str, image: &str, new_version: &str, error: &str, first_error: &str) -> String {
    // the first error line without its timestamp and log level prefix
    let first = first_error.rsplit('|').next().unwrap_or(first_error);
    let src = format!("wf1|{kind}|{image}|{new_version}|{}|{}", normalize(error), normalize(first));
    let h = Sha256::digest(src.as_bytes());
    format!("wf-{}", head(&hex::encode(h), 10))
}

// ---- taking secrets and places out -----------------------------------------------

/// Takes out what shouldn't leave the machine: the known secrets and domains
/// by value, and anything shaped like a key, a token, an address or a domain.
/// It's best effort: a secret in a shape it doesn't know can get through, so
/// a report is read before it's sent.
pub(crate) struct Redactor {
    /// Known secrets of 6 characters or more, and their encoded forms: taken
    /// out wherever they appear.
    secrets: Vec<String>,
    /// Known secrets of 3 to 5 characters: only as a whole word, or they'd eat
    /// into other words. Shorter ones can't be told from ordinary text.
    short: Option<Regex>,
    domains: Option<Regex>,
}

/// Headers that carry a credential: everything after the name goes, to the end
/// of the line (`Cookie: a=1; b=2`, `Authorization: Basic …`).
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(set-cookie|cookie|proxy-authorization|authorization|x-api-key|x-plex-token|x-emby-token|x-emby-authorization|x-mediabrowser-token)(\s*[:=]\s*)[^\r\n]*",
    )
    .expect("static regex")
});
/// `key=value`, `"key": "value"`, `key: 'two words'`, `\"key\":\"value\"` for
/// the usual secret names. A quoted value goes whole.
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b((?:x-)?api[_-]?key|apikey|key|private[_-]?key|access[_-]?token|refresh[_-]?token|token|password|passwd|pass|pwd|pw|secret|client[_-]?secret|credentials?|auth|authorization|cookie|session[_-]?id|sid|passkey|torrent_pass|rsskey|signature)(\\?["']?\s*[=:]\s*)("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\\"(?:[^\\]|\\[^"])*\\"|[^\s&"',;}\]\\]+)"#,
    )
    .expect("static regex")
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(bearer|basic|token)\s+[A-Za-z0-9._~+/=-]{8,}").expect("static regex"));
static USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s:@]+(?::[^/\s@]*)?@").expect("static regex"));
static JWT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*").expect("static regex"));
/// Long runs of key characters: the arrs' 32 hex digit keys, base64 tokens.
/// Needs a digit and a letter, so long words and class names stay.
static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[A-Za-z0-9_+-]{24,}={0,2}").expect("static regex"));
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[a-z0-9._%+-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,}\b").expect("static regex")
});
static IPV4: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").expect("static regex"));
static IPV6: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:[0-9a-f]{0,4}:){2,7}[0-9a-f]{1,4}\b").expect("static regex"));
/// A dotted name; `(` after it marks a call in code.
static HOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+([a-z][a-z0-9-]{1,23})\b(\()?").expect("static regex")
});

/// Names in the apps' code that end like a domain. Matched exactly, capitals
/// and all: `system.io` or `radarr.host` written as a host name still goes.
const CODE_NAMES: &[&str] = &[
    "System.IO",
    "System.Net",
    "System.Web",
    "Npgsql.Internal",
    "Microsoft.AspNetCore.Server",
    "NzbDrone.Host",
    "NzbDrone.Core.Tv",
    "NzbDrone.Core.Music",
    "Radarr.Host",
    "Sonarr.Host",
    "Lidarr.Host",
    "Readarr.Host",
    "Prowlarr.Host",
    "Whisparr.Host",
];
static HOME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(/home/|/Users/|\\Users\\)[^/\\\s]+").expect("static regex"));

/// Top-level domains a host is taken out for. The ones shared with file
/// extensions (`.md`, `.rs`, `.py`, `.sh`, `.so`, `.pl`) aren't here, so file
/// names stay; your own domain goes by value whatever its ending.
const TLDS: &[&str] = &[
    "com", "net", "org", "edu", "gov", "int", "io", "dev", "app", "me", "tv", "xyz", "info", "biz", "co", "uk", "de",
    "nl", "fr", "eu", "ca", "us", "au", "nz", "ch", "se", "no", "fi", "dk", "be", "at", "it", "es", "pt", "ie", "cz",
    "sk", "hu", "ro", "gr", "lt", "lv", "ee", "is", "ru", "ua", "jp", "kr", "cn", "tw", "hk", "sg", "in", "br", "mx",
    "ar", "za", "cc", "ws", "to", "ly", "gg", "im", "fm", "nu", "su", "ovh", "cloud", "site", "online", "tech",
    "space", "host", "club", "link", "page", "one", "top", "pro", "network", "systems", "email", "live", "life",
    "world", "zone", "fun", "lol", "best", "media", "digital", "studio", "rocks", "ninja", "guru", "tools", "services",
    "server", "stream", "video", "audio", "movie", "music", "photos", "games", "party", "house", "family", "home",
    "lan", "local", "internal", "arpa", "box", "casa", "duckdns", "ts", "onion",
];

/// Public places that say nothing about whose machine it is.
const PUBLIC_HOSTS: &[&str] = &[
    "github.com",
    "githubusercontent.com",
    "ghcr.io",
    "docker.io",
    "docker.com",
    "lscr.io",
    "linuxserver.io",
    "servarr.com",
    "themoviedb.org",
    "thetvdb.com",
    "musicbrainz.org",
    "radarr.video",
    "sonarr.tv",
    "lidarr.audio",
    "prowlarr.com",
    "readarr.com",
    "bazarr.media",
    "opensubtitles.com",
    "opensubtitles.org",
    "plex.tv",
    "jellyfin.org",
    "sentry.io",
    "nuget.org",
    "microsoft.com",
    "wranglarr.com",
];

fn public_host(h: &str) -> bool {
    let h = h.to_lowercase();
    PUBLIC_HOSTS.iter().any(|p| h == *p || h.ends_with(&format!(".{p}")))
}

/// Addresses that only mean something inside your own network: they stay.
fn local_v4(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() || ip.is_broadcast()
}

impl Redactor {
    /// `secrets`: values to take out wherever they appear (keys, passwords).
    /// `domains`: yours, taken out with every name under them.
    pub(crate) fn new(
        secrets: impl IntoIterator<Item = String>,
        domains: impl IntoIterator<Item = String>,
    ) -> Redactor {
        let (mut long, mut short) = (HashSet::new(), HashSet::new());
        for s in secrets {
            match s.trim().chars().count() {
                0..=2 => {}
                3..=5 => {
                    short.insert(regex::escape(&s));
                }
                _ => {
                    // as written, in a URL, in JSON
                    let url: String = url::form_urlencoded::byte_serialize(s.as_bytes()).collect();
                    let json = serde_json::to_string(&s).unwrap_or_default();
                    long.insert(url.replace('+', "%20"));
                    long.insert(url);
                    long.insert(json.trim_matches('"').to_owned());
                    long.insert(s);
                }
            }
        }
        let mut secrets: Vec<String> = long.into_iter().filter(|s| s.len() >= 6).collect();
        // the longest first, so one that contains another goes whole
        secrets.sort_by_key(|s| (std::cmp::Reverse(s.len()), s.clone()));
        let mut short: Vec<String> = short.into_iter().collect();
        short.sort();
        let short = (!short.is_empty())
            .then(|| Regex::new(&format!(r"(^|[^A-Za-z0-9])(?:{})($|[^A-Za-z0-9])", short.join("|"))).ok())
            .flatten();
        let domains: Vec<String> = domains
            .into_iter()
            .map(|d| d.trim().trim_start_matches("*.").trim_matches('.').to_lowercase())
            .filter(|d| d.contains('.') && d.parse::<std::net::IpAddr>().is_err())
            .map(|d| regex::escape(&d))
            .collect();
        let domains = (!domains.is_empty())
            .then(|| Regex::new(&format!(r"(?i)(?:[a-z0-9-]+\.)*(?:{})\b", domains.join("|"))).ok())
            .flatten();
        Redactor { secrets, short, domains }
    }

    pub(crate) fn clean(&self, s: &str) -> String {
        let mut s = s.to_owned();
        for secret in &self.secrets {
            s = s.replace(secret.as_str(), "[redacted]");
        }
        if let Some(short) = &self.short {
            // twice: two short ones side by side share the character between them
            for _ in 0..2 {
                s = short.replace_all(&s, "${1}[redacted]${2}").into_owned();
            }
        }
        if let Some(d) = &self.domains {
            s = d.replace_all(&s, "[domain]").into_owned();
        }
        let s = USERINFO.replace_all(&s, "${1}[redacted]@");
        let s = JWT.replace_all(&s, "[token]");
        let s = HEADER.replace_all(&s, "${1}${2}[redacted]");
        let s = BEARER.replace_all(&s, "${1} [redacted]");
        let s = KEY_VALUE.replace_all(&s, |c: &Captures| {
            let v = &c[3];
            let quote = ["\\\"", "\"", "'"].into_iter().find(|q| v.starts_with(q) && v.len() >= 2 * q.len());
            match quote {
                Some(q) => format!("{}{}{q}[redacted]{q}", &c[1], &c[2]),
                // already a placeholder (`[redacted]`), or `token abc…` done above
                None if v.starts_with('[') || ["bearer", "basic"].contains(&v.to_lowercase().as_str()) => {
                    c[0].to_owned()
                }
                None => format!("{}{}[redacted]", &c[1], &c[2]),
            }
        });
        let s = EMAIL.replace_all(&s, "[email]");
        let s = TOKEN.replace_all(&s, |c: &Captures| {
            let t = &c[0];
            let digits = t.bytes().filter(u8::is_ascii_digit).count();
            let letters = t.bytes().filter(u8::is_ascii_alphabetic).count();
            if digits >= 2 && letters >= 2 && !t.contains("--") { "[key]".to_owned() } else { t.to_owned() }
        });
        let s = IPV4.replace_all(&s, |c: &Captures| match c[0].parse::<Ipv4Addr>() {
            Ok(ip) if !local_v4(ip) => "[ip]".to_owned(),
            _ => c[0].to_owned(),
        });
        let s = IPV6.replace_all(&s, |c: &Captures| {
            let t = c[0].trim_end_matches(':');
            match c[0].parse::<Ipv6Addr>().or_else(|_| t.parse::<Ipv6Addr>()) {
                Ok(ip) if !ip.is_loopback() && !ip.is_unspecified() && c[0].matches(':').count() >= 3 => {
                    "[ip]".to_owned()
                }
                _ => c[0].to_owned(),
            }
        });
        let s = HOST.replace_all(&s, |c: &Captures| {
            let name = c[0].trim_end_matches('(');
            let call = c.get(2).is_some();
            let tld = c[1].to_lowercase();
            if !call && !CODE_NAMES.contains(&name) && TLDS.contains(&tld.as_str()) && !public_host(name) {
                format!("[domain]{}", if call { "(" } else { "" })
            } else {
                c[0].to_owned()
            }
        });
        HOME.replace_all(&s, "${1}[user]").into_owned()
    }
}

/// Decode strictly inside a write transaction. A malformed value must not be
/// treated as an absent list and overwritten by the next save or deletion.
fn stored_reports(tx: &crate::state::Tx<'_>) -> crate::error::Result<Vec<Report>> {
    match tx.get_value(KEY)? {
        None => Ok(vec![]),
        Some(value) => serde_json::from_value(value)
            .map_err(|e| crate::error::Error::internal(format!("stored failure reports are malformed: {e}"))),
    }
}

// ---- keeping them --------------------------------------------------------------

impl Updater {
    /// What to take out of a report: every key and password wranglarr knows,
    /// and your domains.
    pub(super) async fn redactor(&self) -> Redactor {
        let cfg = self.app.cfg();
        let mut secrets: Vec<String> = Vec::new();
        let mut domains: Vec<String> = Vec::new();
        let host_of = |u: &str| url::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_owned));
        for a in cfg.all_apps() {
            secrets.extend([a.api_key.clone(), a.password.clone(), a.username.clone()]);
            if let Ok(k) = crate::service::api_key(a).await {
                secrets.push(k);
            }
            domains.extend(host_of(&a.url));
        }
        secrets.push(cfg.postgres.password.clone());
        domains.push(cfg.postgres.host.clone());
        if let Some(v) = &cfg.vpn {
            secrets.push(v.api_key.clone());
            domains.extend(host_of(&v.control_url));
        }
        domains.push(self.app.proxy().settings().await.domain);
        Redactor::new(secrets, domains)
    }

    pub(super) async fn reports(&self) -> Vec<Report> {
        self.app.state().get::<Vec<Report>>(KEY).await.ok().flatten().unwrap_or_default()
    }

    /// Keep `r` at the top: each container's failure in each run has its own,
    /// however many share its fingerprint.
    pub(super) async fn keep_report(&self, r: Report) -> crate::error::Result<Report> {
        let saved = r.clone();
        self.app
            .state()
            .write(move |tx| {
                let mut all = stored_reports(tx)?;
                all.retain(|x| x.key != saved.key);
                all.insert(0, saved);
                all.truncate(KEEP);
                tx.put(KEY, &all)
            })
            .await?;
        Ok(r)
    }

    /// Make and keep the report of `t`'s failed update, and say in the run's
    /// log what it left behind and where to report it.
    pub(super) async fn report_failure(
        &self,
        rl: &RunLog,
        t: &Target,
        facts: &SwapFacts,
        error: &str,
        captured: Option<&Captured>,
        outcome: &Outcome,
    ) {
        let kind = t.kind();
        let def = crate::service::get(&kind);
        let label = def.map_or_else(|| t.container.clone(), |d| d.label.to_owned());
        let report_to = match (def, t.role) {
            (Some(d), _) => d.report_to(),
            // somebody else's image: its own repo, if it names one
            (None, "extra") => self.source_of(&facts.new_image).await,
            _ => ReportTo::wranglarr(),
        };
        let f = Failure {
            container: &t.container,
            kind: &kind,
            label: &label,
            facts,
            error,
            captured,
            outcome,
            report_to,
            run_id: rl.run_id,
        };
        let r = Report::new(&f, &self.redactor().await, &crate::time::now_iso());
        let name = &t.container;
        let new = Report::version(&r.new_version, &r.new_image);
        rl.append(format!("{name}: the update of {label} to {new} failed. {}", r.outcome())).await;
        let r = match self.keep_report(r).await {
            Ok(r) => r,
            Err(e) => {
                rl.append(format!("{}: could not save the failure report: {e}", t.container)).await;
                return;
            }
        };
        let l = r.links();
        let page = format!("/updates/report/{}", r.key);
        match l["search"].as_str() {
            Some(search) => {
                rl.append(format!(
                    "{name}: a report ({}) is at {page}: check there whether it's been reported, and report it if not",
                    r.id
                ))
                .await;
                rl.append(format!("{name}: or search for it by hand: {search}")).await;
            }
            None => {
                rl.append(format!(
                    "{name}: a report ({}) is at {page}; its image doesn't say where to report it",
                    r.id
                ))
                .await;
            }
        }
    }

    /// Where an image that isn't one of wranglarr's says it comes from: its
    /// `org.opencontainers.image.source` label, when that's on GitHub.
    async fn source_of(&self, image_id: &str) -> ReportTo {
        let nowhere = ReportTo { repo: String::new(), curated: false };
        let Ok(d) = self.docker() else { return nowhere };
        let Ok(img) = d.image(image_id).await else { return nowhere };
        let src = img["Config"]["Labels"]["org.opencontainers.image.source"].as_str().unwrap_or("");
        github_repo(src).map_or(nowhere, |repo| ReportTo { repo, curated: false })
    }

    /// The kept reports, newest first, for the API.
    pub(crate) async fn report_views(&self) -> Vec<Value> {
        let all = self.reports().await;
        all.iter().map(|r| r.view(&all)).collect()
    }

    pub(crate) async fn report_view(&self, key: &str) -> Option<Value> {
        let all = self.reports().await;
        all.iter().find(|r| r.key == key).map(|r| r.view(&all))
    }

    /// Forget a report: it was filed, or it doesn't matter any more.
    pub(crate) async fn forget_report(&self, key: &str) -> crate::error::Result<bool> {
        let key = key.to_owned();
        self.app
            .state()
            .write(move |tx| {
                let mut all = stored_reports(tx)?;
                let before = all.len();
                all.retain(|r| r.key != key);
                if all.len() == before {
                    return Ok(false);
                }
                tx.put(KEY, &all)?;
                Ok(true)
            })
            .await
    }

    /// Whether report `key`'s failure has been filed: the oldest issue in its
    /// repo, open or closed, with its fingerprint. Asked from here rather
    /// than from the browser, which would tell GitHub your wranglarr's address.
    pub(crate) async fn reported(&self, key: &str) -> crate::error::Result<Value> {
        use crate::error::Error;
        let r = self.reports().await.into_iter().find(|r| r.key == key);
        let r = r.ok_or_else(|| Error::not_found(format!("no failure report {key}")))?;
        if r.repo.is_empty() {
            return Err(Error::conflict(format!("{}'s image doesn't say where it's reported", r.label)));
        }
        let c = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent(format!("wranglarr/{}", crate::VERSION))
            .build()
            .map_err(|e| Error::internal(e.to_string()))?;
        let q = format!("repo:{} is:issue \"{}\"", r.repo, r.id);
        let net = |e: reqwest::Error| Error::upstream(format!("couldn't ask GitHub: {}", e.without_url()));
        let resp = c
            .get(format!("{}/search/issues", github_api()))
            .query(&[("q", q.as_str()), ("sort", "created"), ("order", "asc"), ("per_page", "10")])
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(net)?;
        match resp.status().as_u16() {
            403 | 429 => return Err(Error::upstream("GitHub is limiting searches: try again in a minute")),
            s if !(200..300).contains(&s) => return Err(Error::upstream(format!("GitHub answered {s}"))),
            _ => {}
        }
        let v: Value = resp.json().await.map_err(net)?;
        Ok(json!({"issue": first_issue(&v, &r.repo)}))
    }

    /// The reports a run made: container -> {key, id, title, search}.
    pub(super) async fn run_reports(&self, run_id: i64) -> Map<String, Value> {
        self.reports().await.into_iter().filter(|r| r.run_id == run_id).map(|r| (r.target.clone(), r.brief())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_failed_report_save_keeps_recovery_instructions_in_the_run_log() {
        let (_dir, app) = crate::app::tests::app().await;
        app.state().put(KEY, &json!({"malformed": true})).await.expect("seed");
        let jobs = crate::jobs::JobRunner::new(app.state().clone());
        let (finished, mut finished_rx) = tokio::sync::mpsc::unbounded_channel();
        jobs.start(move |run| {
            let _ = finished.send(run);
        });
        for state in [Recovery::NotStarted, Recovery::Unhealthy] {
            let updater = app.updates();
            let run = jobs
                .submit("media", "test", "postgres", HashSet::new(), move |rl| async move {
                    let target = Target::new("postgres", "postgres");
                    let facts = SwapFacts {
                        name: "postgres".into(),
                        reference: "postgres:17".into(),
                        old_image: "old".into(),
                        new_image: "new".into(),
                        old_version: "17.1".into(),
                        new_version: "17.2".into(),
                    };
                    let outcome = Outcome {
                        state,
                        blocked: true,
                        data: "Data needs inspection.".into(),
                        data_recovery: DataRecovery::default(),
                        recovery_phase: None,
                    };
                    updater.report_failure(&rl, &target, &facts, "update failed", None, &outcome).await;
                    Ok(crate::jobs::JobOutcome::ok(json!({})))
                })
                .await
                .expect("submit");
            let finished = tokio::time::timeout(std::time::Duration::from_secs(5), finished_rx.recv())
                .await
                .expect("finished")
                .expect("worker alive");
            assert_eq!(finished.run_id, run);
            let log = app.state().get_run(run).await.expect("run").expect("exists").log;
            let instruction = match state {
                Recovery::NotStarted => "couldn't be started: start it by hand",
                _ => "didn't come up healthy: check it",
            };
            let outcome = log.find(instruction).expect("recovery instructions retained");
            let save_error = log.find("could not save the failure report").expect("save failure logged");
            assert!(outcome < save_error, "{log}");
            assert!(log.contains("Data needs inspection."), "{log}");
            assert!(!log.contains("is at /updates/report/"), "{log}");
        }
        jobs.begin_shutdown();
        jobs.join().await.expect("worker shutdown");
        app.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn concurrent_report_save_and_delete_preserve_both_changes() {
        use std::future::{Future, poll_fn};
        use std::task::Poll;

        let (_dir, app) = crate::app::tests::app().await;
        let updater = app.updates();
        for save_first in [true, false] {
            let old = Report { key: "old".into(), ..Report::default() };
            let new = Report { key: "new".into(), ..Report::default() };
            app.state().put(KEY, &vec![old]).await.expect("seed");
            let state = app.state().clone();
            let (entered, entered_rx) = tokio::sync::oneshot::channel();
            let (release, release_rx) = std::sync::mpsc::channel();
            let gate = tokio::spawn(async move {
                state
                    .call(move |_| {
                        let _ = entered.send(());
                        release_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .map_err(|e| crate::error::Error::internal(e.to_string()))?;
                        Ok(())
                    })
                    .await
            });
            entered_rx.await.expect("state worker gated");
            let mut save = Box::pin(updater.keep_report(new.clone()));
            let mut delete = Box::pin(updater.forget_report("old"));
            // Queue both operations before either gets its first state reply.
            // Separate get/put calls would read the same stale list here.
            if save_first {
                assert!(poll_fn(|cx| Poll::Ready(save.as_mut().poll(cx).is_pending())).await);
                assert!(poll_fn(|cx| Poll::Ready(delete.as_mut().poll(cx).is_pending())).await);
            } else {
                assert!(poll_fn(|cx| Poll::Ready(delete.as_mut().poll(cx).is_pending())).await);
                assert!(poll_fn(|cx| Poll::Ready(save.as_mut().poll(cx).is_pending())).await);
            }
            release.send(()).expect("release");
            let (saved, deleted) = tokio::join!(save, delete);
            saved.expect("save");
            assert!(deleted.expect("delete"));
            gate.await.expect("worker task").expect("worker gate");
            assert_eq!(updater.reports().await, [new]);
        }
        app.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn report_mutations_do_not_overwrite_malformed_state() {
        let (_dir, app) = crate::app::tests::app().await;
        app.state().put(KEY, &json!([])).await.expect("seed");
        let updater = app.updates();
        for raw in [r#"{"not":"a report list"}"#, "{not json"] {
            app.state()
                .write(move |tx| {
                    tx.conn().execute("UPDATE kv SET value=? WHERE key=?", rusqlite::params![raw, KEY])?;
                    Ok(())
                })
                .await
                .expect("corrupt stored value");
            assert!(updater.keep_report(Report { key: "new".into(), ..Report::default() }).await.is_err());
            assert!(updater.forget_report("old").await.is_err());
            let actual: String = app
                .state()
                .call(|db| {
                    // Read the raw value inside the worker, bypassing JSON decoding.
                    db.write(|tx| Ok(tx.conn().query_row("SELECT value FROM kv WHERE key=?", [KEY], |r| r.get(0))?))
                })
                .await
                .expect("raw value");
            assert_eq!(actual, raw);
        }
        app.shutdown().await.expect("shutdown");
    }

    fn r() -> Redactor {
        Redactor::new(
            ["0123abcd0123abcd0123abcd0123abcd".to_string(), "hunter22".to_string()],
            ["example.net".to_string()],
        )
    }

    #[test]
    fn known_secrets_and_your_domain_go() {
        let out = r().clean("key 0123abcd0123abcd0123abcd0123abcd pw hunter22 at https://radarr.example.net/x");
        assert_eq!(out, "key [redacted] pw [redacted] at https://[domain]/x");
    }

    #[test]
    fn things_shaped_like_secrets_go() {
        let r = Redactor::new([], []);
        let cases = [
            ("apikey=9f8e7d6c5b4a39281706f5e4d3c2b1a0&x=1", "apikey=[redacted]&x=1"),
            (r#"{"ApiKey": "abc12345"}"#, r#"{"ApiKey": "[redacted]"}"#),
            ("Authorization: Bearer abcdef0123456789", "Authorization: [redacted]"),
            ("token abcdef0123456789 sent", "token [redacted] sent"),
            (r#"ERROR password="two word secret""#, r#"ERROR password="[redacted]""#),
            ("ERROR pass: 'single quoted one' end", "ERROR pass: '[redacted]' end"),
            (r#"{\"password\":\"x y z\"}"#, r#"{\"password\":\"[redacted]\"}"#),
            ("ERROR Cookie: first=abc; second=other-cookie-secret", "ERROR Cookie: [redacted]"),
            ("ERROR remote HOST.PRIVATE.EXAMPLE.COM", "ERROR remote [domain]"),
            ("pulled Registry.Home.Lan/radarr", "pulled [domain]/radarr"),
            ("at system.io:8080", "at [domain]:8080"),
            ("X-Api-Key: 9f8e7d6c", "X-Api-Key: [redacted]"),
            ("postgres://bob:pw@db:5432/radarr", "postgres://[redacted]@db:5432/radarr"),
            ("sent with 9f8e7d6c5b4a39281706f5e4d3c2b1a0 attached", "sent with [key] attached"),
            ("mail me at someone@mail.example.org", "mail me at [email]"),
            ("from 8.8.4.4 and 192.168.1.20 and 127.0.0.1", "from [ip] and 192.168.1.20 and 127.0.0.1"),
            ("peer 2001:db8:85a3::8a2e:370:7334 down", "peer [ip] down"),
            ("connect to tracker.mysite.xyz:443 failed", "connect to [domain]:443 failed"),
            ("at /home/johnny/data", "at /home/[user]/data"),
            ("announce?passkey=a1b2c3d4&info", "announce?passkey=[redacted]&info"),
            ("GET /9f8e7d6c5b4a39281706f5e4d3c2b1a0/announce", "GET /[key]/announce"),
            ("Cookie: SID=Zx81yQ2Lp0", "Cookie: [redacted]"),
            ("X-Plex-Token=AbCd12EfGh34", "X-Plex-Token=[redacted]"),
        ];
        for (input, want) in cases {
            assert_eq!(r.clean(input), want, "{input}");
        }
    }

    #[test]
    fn short_and_encoded_secrets_go() {
        let r = Redactor::new(["abc12".to_string(), "p@ss w0rd!".to_string(), "x".to_string()], []);
        assert_eq!(
            r.clean("login abc12 ok, abc123 stays, abc12abc12 stays"),
            "login [redacted] ok, abc123 stays, abc12abc12 stays"
        );
        assert_eq!(r.clean("abc12,abc12"), "[redacted],[redacted]");
        assert_eq!(r.clean("GET /login?pw=p%40ss+w0rd%21&x=1"), "GET /login?pw=[redacted]&x=1");
        assert_eq!(r.clean("GET /login?u=p%40ss%20w0rd%21"), "GET /login?u=[redacted]");
        assert_eq!(r.clean("said p@ss w0rd! twice"), "said [redacted] twice");
        assert_eq!(r.clean("x marks the spot"), "x marks the spot", "too short to tell from text");
    }

    #[test]
    fn whats_needed_to_read_the_log_stays() {
        let r = Redactor::new([], []);
        let keep = [
            "at NzbDrone.Core.Datastore.Migration.Framework.MigrationController.Migrate(String connectionString)",
            "System.Net.Http.HttpRequestException: Connection refused",
            "[Fatal] ConsoleApp: EPIC FAIL! 12:01:02.123",
            "reading /config/config.xml and main.rs, README.md",
            "pulling from ghcr.io/hotio/radarr and https://github.com/Radarr/Radarr/issues",
            "Radarr version 5.14.0.9383",
            "MigrationControllerFactoryExtensionsAndMore",
            "at System.IO.FileStream..ctor(String path) and System.IO",
            "Radarr.Host.Bootstrap failed in Microsoft.AspNetCore.Server",
            "called foo.app(x)",
        ];
        for line in keep {
            assert_eq!(r.clean(line), line);
        }
    }

    #[test]
    fn the_log_keeps_errors_and_their_stacks() {
        let log = "info: starting\n[Error] boom\n   at A.B()\n   at C.D()\ninfo: more\nwarn: meh\n";
        assert_eq!(pick_lines(log), vec!["[Error] boom", "   at A.B()", "   at C.D()"]);
        let quiet = (0..30).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let got = pick_lines(&quiet);
        assert_eq!(got.len(), 20);
        assert_eq!(got[0], "line 10");
    }

    #[test]
    fn the_same_failure_gets_the_same_fingerprint() {
        let a = fingerprint(
            "radarr",
            "lscr.io/linuxserver/radarr",
            "5.15.0",
            "exited with code 139",
            "2026-10-29 12:00:01.5|Fatal|ConsoleApp|Migration 231 failed at 0x7ff3a2b1c0",
        );
        let b = fingerprint(
            "radarr",
            "lscr.io/linuxserver/radarr",
            "5.15.0",
            "exited with code 139",
            "2026-11-02 08:13:44.9|Fatal|ConsoleApp|Migration 231 failed at 0x7ff3a2b1ff",
        );
        assert_eq!(a, b);
        assert!(a.starts_with("wf-") && a.len() == 13);
        let other = fingerprint("radarr", "lscr.io/linuxserver/radarr", "5.16.0", "exited with code 139", "");
        assert_ne!(a, other);
    }

    #[test]
    fn nothing_known_leaves_in_any_field_of_a_report() {
        let facts = SwapFacts {
            name: "radarr".into(),
            reference: "Registry.Home.Lan:5000/me/radarr:latest".into(),
            old_image: format!("sha256:{}", "a1".repeat(32)),
            new_image: format!("sha256:{}", "b2".repeat(32)),
            old_version: "5.14.0".into(),
            new_version: "5.15.0".into(),
        };
        let captured = Captured {
            log: "[Fatal] boom: password=\"two words\" Cookie: a=b; c=d; apikey=SECRETKEY123\n   at X.Y()\n".into(),
            state: json!({"ExitCode": 139, "OOMKilled": false,
                          "Health": {"Log": [{"Output": "curl http://radarr.mine.example.org/ping SECRETKEY123"}]}}),
        };
        let outcome = Outcome {
            state: Recovery::Parked,
            blocked: false,
            data: "used SECRETKEY123".into(),
            data_recovery: DataRecovery::default(),
            recovery_phase: None,
        };
        let f = Failure {
            container: "radarr",
            kind: "radarr",
            label: "Radarr",
            facts: &facts,
            error: "exited with code 139 (apikey=SECRETKEY123)",
            captured: Some(&captured),
            outcome: &outcome,
            report_to: ReportTo::wranglarr(),
            run_id: 7,
        };
        let red = Redactor::new(["SECRETKEY123".to_string()], ["mine.example.org".to_string()]);
        let rep = Report::new(&f, &red, "2026-10-29T00:00:00Z");
        assert_eq!(rep.key, "7-radarr");
        assert_eq!(rep.image, "[domain]:5000/me/radarr");
        let link = rep.links()["new_issue"].as_str().expect("link").to_owned();
        let decoded: String =
            url::Url::parse(&link).expect("url").query_pairs().map(|(k, v)| format!("{k}={v}\n")).collect();
        let stored = serde_json::to_string(&rep).expect("json");
        for text in [&rep.markdown(), &decoded, &stored] {
            for leak in ["SECRETKEY123", "two words", "c=d", "Registry.Home.Lan", "mine.example.org"] {
                assert!(!text.to_lowercase().contains(&leak.to_lowercase()), "{leak} in {text}");
            }
        }
        let after = rep.outcome();
        assert!(after.contains("parked as radarr-wranglarr-old") && after.contains("may be tried again"), "{after}");
        assert!(!after.contains("is back on") && !after.contains("skipped from now on"), "{after}");
    }

    #[test]
    fn a_github_source_label_names_the_repo() {
        assert_eq!(github_repo("https://github.com/owner/app.git").as_deref(), Some("owner/app"));
        assert_eq!(github_repo("https://github.com/owner/app/tree/main").as_deref(), Some("owner/app"));
        assert_eq!(github_repo("https://gitlab.com/owner/app"), None);
        assert_eq!(github_repo("https://github.com/owner"), None);
    }

    #[test]
    fn the_oldest_issue_in_the_repo_is_the_one() {
        let v = json!({"items": [
            {"number": 9, "title": "later", "state": "open", "html_url": "https://github.com/o/r/issues/9"},
            {"number": 3, "title": "a pr", "state": "open", "html_url": "https://github.com/o/r/pull/3", "pull_request": {}},
            {"number": 5, "title": "first", "state": "closed", "html_url": "https://github.com/o/r/issues/5"},
            {"number": 1, "title": "elsewhere", "state": "open", "html_url": "https://evil.example/o/r/issues/1"},
        ]});
        let i = first_issue(&v, "o/r").expect("an issue");
        assert_eq!(i["number"], 5);
        assert_eq!(i["url"], "https://github.com/o/r/issues/5");
        assert!(first_issue(&json!({"items": []}), "o/r").is_none());
    }

    #[test]
    fn images_lose_their_tag() {
        assert_eq!(repo_of("lscr.io/linuxserver/radarr:latest"), "lscr.io/linuxserver/radarr");
        assert_eq!(repo_of("localhost:5000/radarr"), "localhost:5000/radarr");
        assert_eq!(repo_of("radarr@sha256:abc"), "radarr");
    }

    #[test]
    fn links_go_to_the_repo_with_the_fingerprint() {
        let rep = Report {
            id: "wf-0123456789".into(),
            label: "Radarr".into(),
            kind: "radarr".into(),
            old_version: "5.14.0".into(),
            new_version: "5.15.0".into(),
            repo: "Wranglarrr/Wranglarr".into(),
            curated: true,
            ..Default::default()
        };
        let l = rep.links();
        let search = l["search"].as_str().expect("a search link");
        assert!(search.starts_with("https://github.com/Wranglarrr/Wranglarr/issues?q="), "{search}");
        assert!(search.contains("wf-0123456789"));
        let new = l["new_issue"].as_str().expect("a new-issue link");
        assert!(new.starts_with("https://github.com/Wranglarrr/Wranglarr/issues/new?template=update-failure.yml"));
        assert!(new.contains("fingerprint=wf-0123456789"));
        assert_eq!(l["title"], "Update failed: Radarr 5.14.0 → 5.15.0 [wf-0123456789]");
        assert_eq!(l["body_in_link"], true);
        let nowhere = Report { repo: String::new(), ..rep };
        assert!(nowhere.links()["search"].is_null());
    }
}
