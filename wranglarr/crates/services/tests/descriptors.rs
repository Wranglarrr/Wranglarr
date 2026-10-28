// SPDX-License-Identifier: GPL-3.0-or-later
//! Every registered service's descriptor agrees with the client it builds: a
//! declared role or capability is one the client has, and one it doesn't declare
//! is one it lacks. The clients point at a closed port, so a capability that
//! asks the app fails with a connection error, while one the client lacks fails
//! at once with its own message.

use std::net::TcpListener;
use std::ptr;

use wranglarr_core::config::AppConfig;
use wranglarr_services::feed::{FeedQuery, Lookups, no_list};
use wranglarr_services::login::{Login, LoginSource};
use wranglarr_services::{ALL, AnyClient, BackupMode, Credentials, Database, FeedKind, Roles, ServiceDef, get};

/// A URL nothing listens on.
fn closed_url() -> String {
    let port = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).expect("free port");
    format!("http://127.0.0.1:{port}")
}

/// An app of this kind, with everything any kind needs to build its client.
fn app(d: &ServiceDef, url: &str) -> AppConfig {
    AppConfig {
        name: format!("my{}", d.id),
        kind: d.id.into(),
        url: url.into(),
        api_key: "k".into(),
        username: "u".into(),
        password: "p".into(),
        enabled: true,
        ..Default::default()
    }
}

fn build(d: &ServiceDef, url: &str) -> AnyClient {
    wranglarr_services::client_blocking(&app(d, url)).unwrap_or_else(|e| panic!("{}: {e}", d.id))
}

#[test]
fn each_client_is_its_kinds() {
    let url = closed_url();
    for d in ALL {
        let c = build(d, &url);
        assert!(ptr::eq(c.def(), *d), "{}: built by its own descriptor", d.id);
        assert_eq!((c.kind(), c.name()), (d.id, format!("my{}", d.id).as_str()));
    }
}

#[test]
fn roles_match_capabilities() {
    let url = closed_url();
    for d in ALL {
        let c = build(d, &url);
        let s = c.service();
        assert_eq!(c.arr().is_some(), d.has(Roles::ARR), "{}: the arr API", d.id);
        if let Some(arr) = c.arr() {
            assert!(ptr::eq(arr.def(), *d), "{}", d.id);
            assert!(d.arr.is_some_and(|a| ptr::eq(arr.arr_def(), a)), "{}: its ArrDef", d.id);
        }
        assert_eq!(s.download_client().is_some(), d.has(Roles::DOWNLOADS), "{}: a download client", d.id);
        assert_eq!(c.link_source().is_some(), d.takes.is_some(), "{}: stores links to what it takes", d.id);
        assert_eq!(s.feed().is_some(), !d.feeds.is_empty(), "{}: makes the lists it names", d.id);
        if d.has(Roles::REQUESTS) {
            assert!(d.takes.is_some(), "{}: a requests app links itself to the arrs", d.id);
        }
    }
}

#[test]
fn what_a_service_takes_is_registered_arrs() {
    for d in ALL {
        for kind in d.takes.iter().flat_map(|t| t.kinds) {
            assert!(get(kind).is_some_and(|t| t.has(Roles::ARR)), "{}: takes {kind:?}, not a registered arr", d.id);
        }
    }
}

#[tokio::test]
async fn each_lists_what_it_names_and_nothing_else() {
    let url = closed_url();
    let (query, lookups) = (FeedQuery::default(), Lookups::default());
    for d in ALL {
        let c = build(d, &url);
        let Some(feed) = c.service().feed() else { continue };
        for kind in FeedKind::ALL {
            let lacks = no_list(c.name(), kind).to_string();
            let got = feed.list(kind, &query, &lookups).await;
            let refused = got.as_ref().err().is_some_and(|e| e.message() == lacks);
            assert_eq!(!refused, d.feeds.contains(&kind), "{}: the {} list: {got:?}", d.id, kind.key());
        }
    }
}

#[tokio::test]
async fn backup_mode_matches_the_backup_api() {
    let url = closed_url();
    let dest = tempfile::tempdir().expect("tmp");
    for d in ALL {
        let c = build(d, &url);
        let got = c.capture_config(dest.path()).await;
        match d.backup {
            // its own backup, through its API: there's no app to ask
            BackupMode::ApiZip => assert!(got.is_err(), "{}: asks the app for its backup", d.id),
            // only its config folder, copied; nothing to ask for
            BackupMode::ConfigDir => assert!(matches!(got, Ok(None)), "{}: {got:?}", d.id),
        }
    }
}

#[test]
fn credentials_match_how_it_signs_in() {
    let url = closed_url();
    for d in ALL {
        let mut a = app(d, &url);
        a.api_key.clear();
        let built = wranglarr_services::client_blocking(&a);
        match d.credentials {
            Credentials::ApiKey { .. } => {
                let e = built.err().map(|e| e.to_string());
                assert_eq!(e, Some(format!("my{}: api_key or api_key_file is required", d.id)), "{}", d.id);
            }
            Credentials::UserPassword { .. } => {
                assert!(built.is_ok(), "{}: signs in with a username and password", d.id);
                assert!(
                    matches!(d.login, Some(Login::Managed(m)) if m.source == LoginSource::Env),
                    "{}: its login is .env's",
                    d.id
                );
            }
        }
    }
}

#[test]
fn logins_and_proxy_traits_agree() {
    for d in ALL {
        let own = matches!(d.login, Some(Login::Own { .. }));
        assert_eq!(own, d.proxy.own_login, "{}: its own accounts keep it out from behind the gate", d.id);
    }
}

#[test]
fn only_the_arrs_keep_postgres_in_their_config_file() {
    for d in ALL {
        if d.database == Some(Database::IN_CONFIG) {
            assert!(d.has(Roles::ARR), "{}: config.xml is the arrs'", d.id);
        }
        if let Some(db) = d.database.filter(|db| *db != Database::IN_CONFIG) {
            let env = db.compose_env("db", "pg").join("\n");
            assert!(env.contains("db") && env.contains("pg"), "{}: names the database and host:\n{env}", d.id);
        }
    }
}
