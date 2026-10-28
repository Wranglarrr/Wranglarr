// SPDX-License-Identifier: GPL-3.0-or-later
//! The notifications API in-process, with mock receivers and a mock arr.

mod common;

use std::collections::HashSet;

use axum::http::StatusCode;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wranglarr::notify::{Endpoints, MASK};

use common::*;

fn webhook(name: &str, url: &str, events: &[&str]) -> Value {
    json!({"kind": "webhook", "name": name, "config": {"url": url, "username": "u", "password": "hunter22"},
           "events": events})
}

#[tokio::test]
async fn channels_crud_and_test() {
    let (rx, base) = Receiver::start().await;
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let app = &t.app;

    let (st, r) = call(app, "POST", "/api/notify/channels", Some(webhook("hook", &format!("{base}/hook"), &[]))).await;
    assert_eq!(st, StatusCode::CREATED, "{r}");
    let cid = r["channel"]["id"].as_str().expect("id").to_string();
    assert_eq!(r["channel"]["config"]["password"], MASK);
    assert_eq!(r["channel"]["config"]["url"], MASK, "the webhook URL is a secret");
    assert_eq!(r["channel"]["config"]["username"], "u");
    assert_eq!(r["arrs"], json!([]));

    // the stored channel: secrets in full, an RFC 3339 creation time
    let stored = app.state().get_value("notify:channels").await.expect("db").expect("there");
    assert_eq!(stored[0]["config"]["password"], "hunter22");
    assert_eq!(stored[0]["arr"], json!({"apps": [], "events": ["import", "manual", "health"]}));
    assert!(stored[0]["created"].as_str().is_some_and(|c| c.ends_with("+00:00")));

    // the overview: catalogue in order, channels masked
    let (st, o) = call(app, "GET", "/api/notify", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(o["mask"], MASK);
    assert_eq!(o["channels"][0]["id"], cid.as_str());
    assert_eq!(o["arrs"], json!([{"name": "radarr", "kind": "radarr"}]));
    assert_eq!(o["arr_events_default"], json!(["import", "manual", "health"]));
    assert_eq!(o["settings"], json!({"interval": 5, "disk_free_gb": 20.0, "stall_minutes": 60, "link": ""}));
    // the order, on the raw JSON
    let text = serde_json::to_string(&app.notifier().overview(app).await).expect("json");
    let pos = |k: &str| text.find(&format!("\"{k}\":{{\"")).expect(k);
    assert!(pos("telegram") < pos("discord") && pos("discord") < pos("webhook"));

    // test by id: the saved channel, with its secrets
    let (st, r) = call(app, "POST", "/api/notify/test", Some(json!({"id": cid}))).await;
    assert_eq!((st, r.clone()), (StatusCode::OK, json!({"ok": true})));
    let g = rx.wait_for(|g| g.path == "/hook").await;
    let b = g.json();
    assert_eq!(b["event"], "test");
    assert_eq!(b["level"], "ok");
    assert_eq!(b["title"], "wranglarr: test");
    assert_eq!(b["message"], "If you can read this, Webhook works. wranglarr will send what you picked here.");
    assert_eq!(b["source"], "wranglarr");
    assert_eq!(g.header("authorization"), "Basic dTpodW50ZXIyMg==");

    // test what's on the page (masked secret = the saved one), and an error answer
    let ch = json!({"kind": "webhook", "config": {"url": format!("{base}/fail"), "password": MASK, "username": "u"}});
    let (st, r) = call(app, "POST", "/api/notify/test", Some(json!({"id": cid, "channel": ch}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r, json!({"ok": false, "error": "the webhook answered 401: nope"}));
    let g = rx.wait_for(|g| g.path == "/fail").await;
    assert_eq!(g.header("authorization"), "Basic dTpodW50ZXIyMg==");
    // an unreachable one
    let ch = json!({"kind": "webhook", "config": {"url": format!("{}/x", closed_url())}});
    let (_, r) = call(app, "POST", "/api/notify/test", Some(json!({"channel": ch}))).await;
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().is_some_and(|e| !e.is_empty()), "{r}");

    // errors
    let (st, r) = call(app, "POST", "/api/notify/test", Some(json!({"id": "nope"}))).await;
    assert_eq!((st, r), (StatusCode::NOT_FOUND, json!({"error": "no such channel"})));
    let (st, r) = call(app, "POST", "/api/notify/test", Some(json!({"channel": {"kind": "webhook"}}))).await;
    assert_eq!((st, r), (StatusCode::BAD_REQUEST, json!({"error": "Webhook: URL is needed"})));
    let (st, _) = call(app, "POST", "/api/notify/channels", Some(json!({"kind": "webhook", "config": ["x"]}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, r) = call(
        app,
        "POST",
        "/api/notify/channels",
        Some(json!({"kind": "webhook", "config": {"url": "http://a/x"}, "arr": "radarr"})),
    )
    .await;
    assert_eq!((st, r), (StatusCode::BAD_REQUEST, json!({"error": "config and arr must be objects"})));
    let (st, r) = call(app, "POST", "/api/notify/channels", Some(webhook("HOOK", "http://a/x", &[]))).await;
    assert_eq!((st, r), (StatusCode::BAD_REQUEST, json!({"error": r#"there's already a channel called "HOOK""#})));
    let (st, r) = call(app, "PUT", "/api/notify/channels/nope", Some(webhook("x", "http://a/x", &[]))).await;
    assert_eq!((st, r), (StatusCode::NOT_FOUND, json!({"error": "no such channel"})));
    let (st, r) = call(app, "DELETE", "/api/notify/channels/nope", None).await;
    assert_eq!((st, r), (StatusCode::NOT_FOUND, json!({"error": "no such channel"})));
    let (st, _) = call(app, "POST", "/api/notify/channels", Some(json!([1]))).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // edit: masked secrets stay, the rest changes
    let (st, r) = call(
        app,
        "PUT",
        &format!("/api/notify/channels/{cid}"),
        Some(json!({"name": "renamed", "enabled": false, "events": ["backup_ok"],
                    "config": {"url": MASK, "password": MASK, "username": "v"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["channel"]["name"], "renamed");
    assert_eq!(r["channel"]["kind"], "webhook");
    let c = app.notifier().channel(app, &cid).await.expect("there");
    assert_eq!(
        (c.cfg_value("password"), c.cfg_value("url"), c.cfg_value("username")),
        ("hunter22", format!("{base}/hook").as_str(), "v")
    );
    assert!(!c.enabled);
    assert_eq!(c.events, vec!["backup_ok"]);

    let (st, r) = call(app, "DELETE", &format!("/api/notify/channels/{cid}"), None).await;
    assert_eq!((st, r), (StatusCode::OK, json!({"arrs": []})));
    assert!(app.notifier().channels(app).await.is_empty());
}

trait CfgValue {
    fn cfg_value(&self, k: &str) -> &str;
}
impl CfgValue for wranglarr::notify::Channel {
    fn cfg_value(&self, k: &str) -> &str {
        self.config.get(k).map_or("", String::as_str)
    }
}

#[tokio::test]
async fn every_http_kind_reaches_its_service() {
    let (rx, base) = Receiver::start().await;
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let app = &t.app;
    app.notifier().set_endpoints(Endpoints { telegram: format!("{base}/tg"), pushover: format!("{base}/po") });
    let test = |ch: Value| async move {
        let (st, r) = call(app, "POST", "/api/notify/test", Some(json!({"channel": ch}))).await;
        assert_eq!(st, StatusCode::OK);
        r
    };

    let r =
        test(json!({"kind": "ntfy", "config": {"server": format!("{base}/"), "topic": "arrs", "token": "tk_1234"}}))
            .await;
    assert_eq!(r, json!({"ok": true}));
    let g = rx.wait_for(|g| g.path == "/arrs").await;
    assert_eq!(g.text(), "If you can read this, ntfy works. wranglarr will send what you picked here.");
    assert_eq!(
        (g.header("title"), g.header("priority"), g.header("tags")),
        ("wranglarr: test", "low", "white_check_mark")
    );
    assert_eq!(g.header("authorization"), "Bearer tk_1234");

    test(json!({"kind": "discord", "config": {"webhook_url": format!("{base}/discord")}})).await;
    let g = rx.wait_for(|g| g.path == "/discord").await.json();
    assert_eq!(g["username"], "wranglarr");
    assert_eq!(g["embeds"][0]["title"], "wranglarr: test");
    assert_eq!(g["embeds"][0]["color"], 0x27C24C);
    assert_eq!(g["embeds"][0]["footer"]["text"], "test");

    test(json!({"kind": "slack", "config": {"webhook_url": format!("{base}/slack")}})).await;
    let g = rx.wait_for(|g| g.path == "/slack").await.json();
    assert!(
        g["text"].as_str().is_some_and(|t| t.starts_with("✅ *wranglarr: test*\nIf you can read this, Slack works."))
    );

    test(json!({"kind": "gotify", "config": {"server": format!("{base}/gotify"), "token": "AbCdEf"}})).await;
    let g = rx.wait_for(|g| g.path == "/gotify/message").await;
    assert_eq!(g.header("x-gotify-key"), "AbCdEf");
    assert_eq!(g.json()["priority"], 3);

    test(json!({"kind": "apprise", "config": {"server": format!("{base}/apprise"), "key": "arrs"}})).await;
    let g = rx.wait_for(|g| g.path == "/apprise/notify/arrs").await.json();
    assert_eq!(g["type"], "success");

    test(json!({"kind": "telegram", "config": {"bot_token": "123:XYZ", "chat_id": "-100", "topic_id": "4"}})).await;
    let g = rx.wait_for(|g| g.path == "/tg/bot123:XYZ/sendMessage").await.json();
    assert_eq!(g["chat_id"], "-100");
    assert_eq!(g["message_thread_id"], 4);
    assert_eq!(g["parse_mode"], "HTML");

    test(json!({"kind": "pushover", "config": {"user_key": "uk", "token": "ptok"}})).await;
    let g = rx.wait_for(|g| g.path == "/po/1/messages.json").await;
    assert_eq!(g.header("content-type"), "application/x-www-form-urlencoded");
    assert!(g.text().contains("token=ptok") && g.text().contains("user=uk") && g.text().contains("priority=0"));

    // a failed answer, with the token scrubbed out of the reason
    let r = test(json!({"kind": "telegram", "config": {"bot_token": "fail-SECRET", "chat_id": "1"}})).await;
    assert_eq!(r, json!({"ok": false, "error": "Telegram answered 401: nope"}));
}

/// A minimal SMTP server: answers everything, keeps the DATA.
async fn smtp_server() -> (u16, tokio::sync::mpsc::UnboundedReceiver<(Vec<String>, String)>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = l.local_addr().expect("addr").port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let (r, mut w) = s.into_split();
                let mut r = BufReader::new(r);
                let _ = w.write_all(b"220 mock ESMTP\r\n").await;
                let mut cmds = vec![];
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    let cmd = line.trim_end().to_string();
                    cmds.push(cmd.clone());
                    let up = cmd.to_uppercase();
                    if up.starts_with("EHLO") {
                        let _ = w.write_all(b"250-mock\r\n250 AUTH PLAIN LOGIN\r\n").await;
                    } else if up.starts_with("AUTH") {
                        let _ = w.write_all(b"235 ok\r\n").await;
                    } else if up == "DATA" {
                        let _ = w.write_all(b"354 go\r\n").await;
                        let mut data = String::new();
                        loop {
                            line.clear();
                            if r.read_line(&mut line).await.unwrap_or(0) == 0 || line == ".\r\n" {
                                break;
                            }
                            data.push_str(&line);
                        }
                        let _ = tx.send((cmds.clone(), data));
                        let _ = w.write_all(b"250 queued\r\n").await;
                    } else if up == "QUIT" {
                        let _ = w.write_all(b"221 bye\r\n").await;
                        break;
                    } else {
                        let _ = w.write_all(b"250 ok\r\n").await;
                    }
                }
            });
        }
    });
    (port, rx)
}

#[tokio::test]
async fn email_over_plain_smtp() {
    let (port, mut mails) = smtp_server().await;
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let ch = json!({"kind": "email", "config": {"host": "127.0.0.1", "port": port.to_string(), "security": "none",
                                               "username": "me", "password": "pw", "sender": "w@example.com",
                                               "to": "a@example.com, b@example.com"}});
    let (st, r) = call(&t.app, "POST", "/api/notify/test", Some(json!({"channel": ch}))).await;
    assert_eq!((st, r), (StatusCode::OK, json!({"ok": true})));
    let (cmds, data) = mails.recv().await.expect("a mail");
    assert!(cmds.iter().any(|c| c.starts_with("AUTH")), "{cmds:?}");
    assert!(cmds.iter().any(|c| c.contains("<a@example.com>")) && cmds.iter().any(|c| c.contains("<b@example.com>")));
    assert!(data.contains("Subject: wranglarr: test"), "{data}");
    assert!(data.contains("If you can read this, Email works."), "{data}");

    // STARTTLS against a server without it: an error, not a plain-text send
    let ch = json!({"kind": "email", "config": {"host": "127.0.0.1", "port": port.to_string(), "security": "starttls",
                                               "sender": "w@example.com", "to": "a@example.com"}});
    let (_, r) = call(&t.app, "POST", "/api/notify/test", Some(json!({"channel": ch}))).await;
    assert_eq!(r["ok"], false, "{r}");
}

#[tokio::test]
async fn channels_go_into_the_arrs() {
    let (arr, url) = MockArr::start().await;
    let t = app_with(&(arr_toml("radarr", "radarr", &url) + &arr_toml("sonarr", "sonarr", &closed_url()))).await;
    let app = &t.app;
    let mut body = webhook("alerts", "https://hooks.example/x", &[]);
    body["arr"] = json!({"apps": ["radarr", "sonarr", "unknown"], "events": ["grab", "health"]});
    let (st, r) = call(app, "POST", "/api/notify/channels", Some(body.clone())).await;
    assert_eq!(st, StatusCode::CREATED, "{r}");
    assert_eq!(r["channel"]["arr"]["apps"], json!(["radarr", "sonarr"]));
    let arrs = r["arrs"].as_array().expect("results");
    assert_eq!(arrs[0], json!({"app": "radarr", "action": "created"}));
    assert_eq!(arrs[1]["app"], "sonarr");
    assert!(arrs[1]["error"].as_str().is_some(), "{r}");
    let n = arr.get("wranglarr · alerts").expect("added to radarr");
    assert_eq!(n["implementation"], "Webhook");
    assert_eq!(field(&n, "url"), "https://hooks.example/x");
    assert_eq!(field(&n, "method"), 1);
    assert_eq!(field(&n, "password"), "hunter22");
    assert_eq!(
        (n["onGrab"].clone(), n["onDownload"].clone(), n["onHealthIssue"].clone()),
        (json!(true), json!(false), json!(true))
    );
    assert_eq!(n["includeHealthWarnings"], true);
    assert_eq!(n["tags"], json!([]));
    assert!(n.get("presets").is_none());
    let cid = r["channel"]["id"].as_str().expect("id").to_string();

    // the same again: updated in place
    body["config"]["password"] = json!(MASK);
    body["arr"] = json!({"apps": ["radarr"]});
    let (_, r) = call(app, "PUT", &format!("/api/notify/channels/{cid}"), Some(body.clone())).await;
    assert_eq!(r["arrs"][0], json!({"app": "radarr", "action": "updated"}));
    assert_eq!(r["arrs"][1]["app"], "sonarr", "unpicked: removed (here: tried to)");
    assert_eq!(arr.names(), vec!["wranglarr · alerts"]);
    assert_eq!(field(&arr.get("wranglarr · alerts").expect("there"), "password"), "hunter22");

    // renamed: the old entry goes, a new one comes
    body["name"] = json!("alarms");
    let (_, r) = call(app, "PUT", &format!("/api/notify/channels/{cid}"), Some(body.clone())).await;
    assert_eq!(r["arrs"], json!([{"app": "radarr", "action": "created"}]));
    assert_eq!(arr.names(), vec!["wranglarr · alarms"]);

    // switched off: removed from the arr
    body["enabled"] = json!(false);
    let (_, r) = call(app, "PUT", &format!("/api/notify/channels/{cid}"), Some(body.clone())).await;
    assert_eq!(r["arrs"], json!([{"app": "radarr", "action": "removed"}]));
    assert!(arr.names().is_empty());

    // back on, then deleted
    body["enabled"] = json!(true);
    call(app, "PUT", &format!("/api/notify/channels/{cid}"), Some(body)).await;
    assert_eq!(arr.names().len(), 1);
    let (_, r) = call(app, "DELETE", &format!("/api/notify/channels/{cid}"), None).await;
    assert_eq!(r, json!({"arrs": [{"app": "radarr", "action": "removed"}]}));
    assert!(arr.names().is_empty());
}

#[tokio::test]
async fn watcher_settings() {
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let app = &t.app;
    let (st, r) = call(app, "PUT", "/api/notify/settings", Some(json!({"interval": "abc"}))).await;
    assert_eq!(
        (st, r),
        (StatusCode::BAD_REQUEST, json!({"error": "interval, disk space and stall time must be numbers"}))
    );
    let (st, r) = call(app, "PUT", "/api/notify/settings", Some(json!({"link": "ftp://x"}))).await;
    assert_eq!((st, r), (StatusCode::BAD_REQUEST, json!({"error": "the link must start with http:// or https://"})));
    let (st, r) = call(
        app,
        "PUT",
        "/api/notify/settings",
        Some(json!({"interval": "10", "disk_free_gb": "5", "stall_minutes": 30, "link": "https://w.example/"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r, json!({"interval": 10, "disk_free_gb": 5.0, "stall_minutes": 30, "link": "https://w.example"}));
    let (_, o) = call(app, "GET", "/api/notify", None).await;
    assert_eq!(o["settings"]["interval"], 10);
}

#[tokio::test]
async fn the_watcher_reports_an_app_once_it_is_confirmed_down() {
    let (rx, base) = Receiver::start().await;
    let (_arr, url) = MockArr::start().await;
    let down = closed_url();
    let t = app_with(&(arr_toml("radarr", "radarr", &url) + &arr_toml("sonarr", "sonarr", &down))).await;
    let app = &t.app;
    // no channel yet: tracked, not sent
    let (st, r) = call(app, "POST", "/api/notify/check", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(r["problems"].as_u64().is_some_and(|p| p >= 1), "{r}");
    assert_eq!(r["sent"], 0);
    let (_, o) = call(app, "GET", "/api/notify", None).await;
    let w = o["watching"].as_array().expect("watching");
    let s = w.iter().find(|x| x["key"] == "down:sonarr").expect("tracked");
    assert_eq!(s["title"], "sonarr is offline");
    assert_eq!(s["sent"], false);
    assert_eq!(s["message"], format!("wranglarr can't reach {down}."));

    let (st, _) =
        call(app, "POST", "/api/notify/channels", Some(webhook("hook", &format!("{base}/hook"), &["app_down"]))).await;
    assert_eq!(st, StatusCode::CREATED);
    // the save started the watcher; checks now send it (the second sighting confirms it)
    for _ in 0..20 {
        if rx.all().iter().any(|g| g.json()["event"] == "app_down") {
            break;
        }
        let (_, r) = call(app, "POST", "/api/notify/check", None).await;
        assert!(r.get("problems").is_some() || r.get("skipped").is_some(), "{r}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let g = rx.wait_for(|g| g.json()["event"] == "app_down").await.json();
    assert_eq!(g["title"], "sonarr is offline");
    assert_eq!(g["level"], "error");
    // logged, and sent only once
    for _ in 0..3 {
        call(app, "POST", "/api/notify/check", None).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(rx.all().iter().filter(|g| g.json()["event"] == "app_down").count(), 1);
    let (_, o) = call(app, "GET", "/api/notify", None).await;
    let entry = o["log"].as_array().and_then(|l| l.iter().find(|e| e["event"] == "app_down")).expect("logged").clone();
    assert_eq!(entry["sent"], json!([{"channel": "hook", "ok": true}]));
    assert!(app.notifier().watching());
}

#[tokio::test]
async fn finished_jobs_and_lockouts_are_sent() {
    let (rx, base) = Receiver::start().await;
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let app = &t.app;
    app.start().await;
    let mut ch = webhook("hook", &format!("{base}/hook"), &["backup_failed", "sign_in_lockout"]);
    ch["config"]["username"] = json!("");
    call(app, "PUT", "/api/notify/settings", Some(json!({"link": "https://wranglarr.example"}))).await;
    call(app, "POST", "/api/notify/channels", Some(ch)).await;

    let run_id = app
        .submit("media", "backup", "", HashSet::new(), |_rl| async {
            Err(wranglarr_core::error::Error::internal("disk full"))
        })
        .await
        .expect("queued");
    let g = rx.wait_for(|g| g.json()["event"] == "backup_failed").await;
    assert_eq!(g.header("authorization"), "", "no user: no basic auth");
    let g = g.json();
    assert_eq!(g["title"], "Backup of media failed");
    assert_eq!(
        g["message"],
        format!("Version ? is failed: disk full. See Activity, run {run_id}.\nhttps://wranglarr.example")
    );

    app.notifier().lockout(app, "10.0.0.9", 15);
    let g = rx.wait_for(|g| g.json()["event"] == "sign_in_lockout").await.json();
    assert_eq!(g["title"], "Failed sign-ins to wranglarr");
    assert!(g["message"].as_str().is_some_and(|m| m.starts_with("10.0.0.9 was locked out for 15 min")));
    assert_eq!(g["level"], "warning");

    // the log keeps the message without the link
    let sent = app.notifier().emit_wait(app, "backup_failed", "T", "M", None).await;
    assert_eq!(sent, vec![json!({"channel": "hook", "ok": true})]);
    let h = app.notifier().history(app).await;
    assert_eq!(h[0]["message"], "M");
    assert_eq!(h[0]["level"], "error");
}

#[tokio::test]
async fn shutdown_stops_the_watcher_and_sends_what_is_pending() {
    let (rx, base) = Receiver::start().await;
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let app = &t.app;
    app.start().await;
    call(app, "POST", "/api/notify/channels", Some(webhook("hook", &format!("{base}/hook"), &["sign_in_lockout"])))
        .await;
    assert!(app.notifier().watching());

    // auth reports a lockout from a blocking thread, then the service stops
    let locked = app.clone();
    tokio::task::spawn_blocking(move || locked.auth_lockout("10.0.0.9", 15)).await.expect("lockout");
    app.shutdown().await.expect("shutdown");

    assert!(!app.notifier().watching());
    let h = app.notifier().history(app).await;
    assert_eq!(h[0]["event"], "sign_in_lockout");
    assert_eq!(h[0]["sent"], json!([{"channel": "hook", "ok": true}]), "sent before shutdown returned");
    assert_eq!(rx.wait_for(|g| g.json()["event"] == "sign_in_lockout").await.path, "/hook");
}

#[tokio::test]
async fn the_page_renders() {
    let t = app_with(&arr_toml("radarr", "radarr", &closed_url())).await;
    let (st, body) = call(&t.app, "GET", "/notifications", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.as_str().is_some_and(|h| h.contains("notifications.js")), "{body}");
}
