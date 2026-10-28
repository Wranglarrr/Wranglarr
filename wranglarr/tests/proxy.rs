// SPDX-License-Identifier: GPL-3.0-or-later
//! The reverse proxy output for every proxy, style, upstream and logins setup,
//! against the expected output in `fixtures/proxy.json`: one entry for each case
//! `cases()` builds, in the same order. The rendered configs are what people paste
//! into their proxy, so a change to them shows up here line by line.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use wranglarr::links::Published;
use wranglarr::proxy::{self, Entry, ProxyEnv, ProxySettings};
use wranglarr_core::config::AppConfig;

fn apps() -> Vec<AppConfig> {
    [
        ("radarr", "radarr", "http://radarr:7878"),
        ("sonarr", "sonarr", "http://sonarr:8989/sonarr"),
        ("sabnzbd", "sabnzbd", "http://sabnzbd:8080"),
        ("qbittorrent", "qbittorrent", "http://qbittorrent:8090"),
        ("nzbget", "nzbget", "http://nzbget:6789"),
        ("bazarr", "bazarr", "http://bazarr:6767/"),
        ("prowlarr", "prowlarr", "http://prowlarr:9696"),
        ("seerr", "seerr", "http://seerr:5055"),
        ("my.lidarr", "lidarr", "https://lidarr"),
    ]
    .iter()
    .map(|(n, k, u)| AppConfig { name: n.to_string(), kind: k.to_string(), url: u.to_string(), ..Default::default() })
    .collect()
}

fn ports() -> BTreeMap<String, Published> {
    let p = |port: Option<u16>, loopback: bool, gate: Option<bool>, how: &str| Published {
        port,
        loopback,
        how: how.into(),
        gate,
    };
    BTreeMap::from([
        ("radarr".to_string(), p(Some(7878), false, Some(false), "published port")),
        ("sonarr".to_string(), p(Some(18989), true, Some(false), "published port")),
        ("sabnzbd".to_string(), p(Some(8080), false, Some(false), "published port")),
        ("qbittorrent".to_string(), p(None, false, None, "not published on the host")),
        ("nzbget".to_string(), p(Some(6789), false, Some(true), "through wranglarr-gate")),
        ("bazarr".to_string(), p(Some(6767), false, Some(false), "published port")),
        ("prowlarr".to_string(), p(Some(9696), false, Some(false), "published port")),
        ("seerr".to_string(), p(Some(5055), false, None, "guessed: the app's own port (no Docker socket to ask)")),
    ])
}

fn cases() -> Vec<Value> {
    let exposes = json!({
        "wranglarr": {"expose": "public"}, "radarr": {"expose": "public"}, "sonarr": {"expose": "lan", "sub": "tv"},
        "sabnzbd": {"expose": "lan"}, "qbittorrent": {"expose": "public", "sub": "qb"}, "nzbget": {"expose": "lan"},
        "bazarr": {"expose": "public"}, "prowlarr": {"expose": "off"}, "seerr": {"expose": "public"},
        "my.lidarr": {"sub": "music"}, "unknown": {"expose": "public"}});
    // (env, domain, lan, upstream_host, browser host)
    let envs = [
        (json!({}), "example.com", "", "192.168.1.10", "wranglarr.local"),
        (
            json!({"APPS_AUTH": "tunnel", "AUTH_COOKIE_DOMAIN": "other.org"}),
            "home.example.com",
            "192.168.1.0/24, 10.0.0.0/8",
            "",
            "10.0.0.5",
        ),
        (json!({"APPS_AUTH": "apps", "APPS_AUTH_REQUIRED": "disabledForLocalAddresses"}), "", "", "127.0.0.1", ""),
        (
            json!({"APPS_AUTH": " Tunnel ", "AUTH_COOKIE_DOMAIN": "example.com", "AUTH_LOGIN_URL": "https://auth.example.com/x",
                "WRANGLARR_PORT": "9999"}),
            "Example.com.",
            "fc00::/7",
            "",
            "nas",
        ),
        (json!({"APPS_AUTH": "manual", "WRANGLARR_PORT": "nope"}), "lan.example", "", "nas.lan", "x"),
    ];
    let apps: Vec<Value> = apps().iter().map(|a| json!({"name": a.name, "kind": a.kind, "url": a.url})).collect();
    let ports = serde_json::to_value(ports()).expect("ports");
    let mut out = vec![];
    for (kind, _) in proxy::KINDS {
        for style in ["subdomain", "path"] {
            for upstream in ["host", "docker"] {
                for (env, domain, lan, uh, bh) in &envs {
                    out.push(json!({"apps": apps, "ports": ports, "env": env, "browser_host": bh,
                                    "body": {"kind": kind, "style": style, "upstream": upstream, "domain": domain,
                                             "lan": lan, "upstream_host": uh, "apps": exposes,
                                             "network": "web", "entrypoint": "https", "resolver": "le"}}));
                }
            }
        }
    }
    // bad values: the same message
    for body in [
        json!({"kind": "apache"}),
        json!({"style": "sideways"}),
        json!({"domain": "https://example.com/x"}),
        json!({"upstream": "moon"}),
        json!({"upstream_host": "a b"}),
        json!({"lan": "not-a-range"}),
        json!({"lan": "10.0.0.0/255.0.0.0 10.0.0.0/33"}),
        json!({"apps": {"radarr": {"expose": "everyone"}}}),
        json!({"apps": {"radarr": {"sub": "A.B"}}}),
        json!({"apps": []}),
        json!({"kind": 3}),
        json!({"network": "a b"}),
    ] {
        out.push(json!({"apps": apps, "ports": ports, "env": {}, "browser_host": "h", "body": body}));
    }
    out
}

fn env_of(e: &Value) -> ProxyEnv {
    ProxyEnv::read(&|k: &str| e[k].as_str().map(str::to_string))
}

fn render(case: &Value) -> Value {
    let apps = apps();
    let mut entries = vec![Entry::wranglarr()];
    entries.extend(apps.iter().map(Entry::for_app));
    let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
    let body = case["body"].as_object().cloned().unwrap_or_default();
    let s = match proxy::check(&ProxySettings::default(), &body, &names) {
        Ok(s) => s,
        Err(e) => return json!({"error": e.to_string()}),
    };
    let env = env_of(&case["env"]);
    let host = case["browser_host"].as_str().unwrap_or("");
    let r = proxy::render(proxy::plan(&entries, &s, host, &ports(), &env), &s, &env);
    let mut v = serde_json::to_value(&r).expect("json");
    v["settings"] = serde_json::to_value(&s).expect("json");
    v
}

#[test]
fn renders_the_expected_configs() {
    let want: Vec<Value> = serde_json::from_str(include_str!("fixtures/proxy.json")).expect("fixture");
    let cases = cases();
    assert_eq!(want.len(), cases.len());
    let mut bad = vec![];
    for (i, (case, want)) in cases.iter().zip(&want).enumerate() {
        let got = render(case);
        if &got != want {
            let field = ["error", "settings", "text", "filename", "how", "plan", "notes"]
                .into_iter()
                .find(|k| got.get(k) != want.get(k))
                .unwrap_or("?");
            bad.push(format!(
                "case {i} ({}) differs in {field}:\n--- expected\n{}\n--- rendered\n{}",
                case["body"],
                want.get(field).map_or("-".into(), |v| v.as_str().map_or_else(|| v.to_string(), str::to_string)),
                got.get(field).map_or("-".into(), |v| v.as_str().map_or_else(|| v.to_string(), str::to_string)),
            ));
        }
    }
    assert!(bad.is_empty(), "{} of {} cases differ; first:\n{}", bad.len(), cases.len(), bad[0]);
    let errors = want.iter().filter(|w| w.get("error").is_some()).count();
    assert_eq!(errors, 12, "every bad body is refused");
}
