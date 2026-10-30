// SPDX-License-Identifier: GPL-3.0-or-later
//! Taking secrets and places out of a report before it leaves the machine.

use std::collections::HashSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use regex::{Captures, Regex};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
