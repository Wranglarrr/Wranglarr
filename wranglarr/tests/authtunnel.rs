// SPDX-License-Identifier: GPL-3.0-or-later
//! scripts/authtunnel.sh writes wranglarr-gate's nginx config. Its `location /` forwards the
//! browser's Cookie header to the app through a chain of `map`s that drop wranglarr's own
//! cookies. nginx isn't here, so this replays those maps (read from the script itself) the way
//! nginx evaluates them: the first matching regex wins, its named captures fill the value.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use regex::Regex;

const SCRIPT: &str = include_str!("../../scripts/authtunnel.sh");

struct Map {
    source: String,
    target: String,
    default: String,
    rules: Vec<(Regex, String)>,
}

/// The `map $a $b { default …; "~re" value; … }` lines that build `$wr_cookie`.
fn cookie_maps() -> Vec<Map> {
    let line = Regex::new(r"^map \$(\S+) \$(wr_cookie\S*) \{ default \$(\S+); (.*) \}$").unwrap();
    let rule = Regex::new(r#""~((?:[^"\\]|\\.)*)" ("[^"]*"|\$\w+);"#).unwrap();
    SCRIPT
        .lines()
        .filter_map(|l| line.captures(l.trim()))
        .map(|c| {
            assert_eq!(c[1], c[3], "a map's default is its own source");
            let rules = rule
                .captures_iter(&c[4])
                .map(|r| (Regex::new(&r[1]).expect("nginx regex is valid"), r[2].trim_matches('"').to_string()))
                .collect::<Vec<_>>();
            assert!(!rules.is_empty(), "{}", &c[0]);
            Map { source: c[1].to_string(), target: c[2].to_string(), default: c[3].to_string(), rules }
        })
        .collect()
}

/// `$name` references in a map value, with the regex's named captures.
fn expand(value: &str, caps: &regex::Captures, re: &Regex) -> String {
    let var = Regex::new(r"\$(\w+)").unwrap();
    var.replace_all(value, |v: &regex::Captures| {
        let name = &v[1];
        assert!(re.capture_names().flatten().any(|n| n == name), "${name} isn't a capture of {re}");
        caps.name(name).map_or("", |m| m.as_str()).to_string()
    })
    .into_owned()
}

/// What `$wr_cookie` is for a request whose Cookie header is `cookie`.
fn forwarded(cookie: &str) -> String {
    let maps = cookie_maps();
    let mut vars: HashMap<String, String> = HashMap::from([("http_cookie".to_string(), cookie.to_string())]);
    // each map reads a variable an earlier one set (nginx evaluates them lazily, in this order)
    for m in &maps {
        let input = vars.get(&m.source).cloned().unwrap_or_else(|| panic!("${} isn't set yet", m.source));
        let out = m
            .rules
            .iter()
            .find_map(|(re, v)| re.captures(&input).map(|c| expand(v, &c, re)))
            .unwrap_or_else(|| vars[&m.default].clone());
        vars.insert(m.target.clone(), out);
    }
    vars.get("wr_cookie").cloned().expect("the chain ends in $wr_cookie")
}

#[test]
fn the_gate_forwards_the_filtered_cookie() {
    assert_eq!(cookie_maps().len(), 4);
    assert!(
        SCRIPT.contains("proxy_set_header Cookie \\$wr_cookie;"),
        "location / must forward $wr_cookie, not the browser's Cookie header"
    );
}

#[test]
fn wranglarr_cookies_are_dropped_and_the_rest_kept() {
    for (cookie, want) in [
        ("", ""),
        ("a=1", "a=1"),
        ("a=1; b=2", "a=1; b=2"),
        ("wranglarr_session=tok", ""),
        ("wranglarr_session=tok; a=1", "a=1"),
        ("a=1; wranglarr_session=tok", "a=1"),
        ("a=1; wranglarr_session=tok; b=2", "a=1; b=2"),
        ("a=1;wranglarr_session=tok;b=2", "a=1;b=2"),
        ("wranglarr_mfa=x; radarr=y", "radarr=y"),
        // host-only and Domain= cookies both arrive, plus the code step's
        ("wranglarr_session=a; SID=q; wranglarr_session=b; wranglarr_mfa=c", "SID=q"),
        ("wranglarr_session =spaced; a=1", "a=1"),
        // other cookies that merely contain the name stay
        ("my_wranglarr_session=1; a=wranglarr_session=2", "my_wranglarr_session=1; a=wranglarr_session=2"),
        ("wranglarr_setup=t; a=1", "wranglarr_setup=t; a=1"),
    ] {
        assert_eq!(forwarded(cookie), want, "Cookie: {cookie}");
    }
}

#[test]
fn too_many_copies_drop_every_cookie() {
    // an app can plant more same-named cookies (one per path); none of wranglarr's may get through
    let cookie = "wranglarr_session=1; wranglarr_session=2; wranglarr_session=3; wranglarr_session=real; a=1";
    assert_eq!(forwarded(cookie), "");
    let cookie = "a=1; wranglarr_mfa=1; wranglarr_mfa=2; wranglarr_mfa=3; b=2; wranglarr_session=real";
    assert_eq!(forwarded(cookie), "");
}
