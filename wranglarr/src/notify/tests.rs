use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::arr::*;
use super::catalog::*;
use super::channel::*;
use super::delivery::*;
use super::runs::*;
use super::settings::*;
use super::watcher::*;
use super::*;

fn body(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn arrs() -> HashSet<String> {
    ["radarr", "sonarr"].iter().map(|s| s.to_string()).collect()
}

fn err(v: Value) -> String {
    clean(&body(v), None, &arrs()).expect_err("invalid").to_string()
}

fn chan(kind: &str, cfg: &[(&str, &str)]) -> Channel {
    Channel {
        id: "abcd1234".into(),
        name: "Mine".into(),
        kind: kind.into(),
        enabled: true,
        config: cfg.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Default::default()
    }
}

#[test]
fn validation_messages() {
    assert_eq!(err(json!({"kind": "irc"})), r#"unknown kind "irc""#);
    assert_eq!(err(json!({})), "no kind given");
    assert_eq!(err(json!({"kind": "discord", "config": "x"})), "config and arr must be objects");
    assert_eq!(err(json!({"kind": "discord", "arr": [1]})), "config and arr must be objects");
    assert_eq!(err(json!({"kind": "discord", "events": "all"})), "events must be a list");
    assert_eq!(err(json!({"kind": "discord"})), "Discord: Webhook URL is needed");
    assert_eq!(
        err(json!({"kind": "discord", "name": "x".repeat(61), "config": {"webhook_url": "https://d"}})),
        "the name is too long"
    );
    assert_eq!(
        err(json!({"kind": "discord", "config": {"webhook_url": "discord.com/x"}})),
        "webhook url: must start with http:// or https://"
    );
    let mail = json!({"host": "smtp", "sender": "a@b.c", "to": "d@e.f"});
    let mut m = mail.clone();
    m["port"] = json!("58x");
    assert_eq!(err(json!({"kind": "email", "config": m})), "Port: a number");
    let mut m = mail.clone();
    m["security"] = json!("tls13");
    assert_eq!(err(json!({"kind": "email", "config": m})), "Encryption: not one of the choices");
    assert_eq!(
        err(json!({"kind": "apprise", "config": {"server": "http://apprise:8000"}})),
        "Apprise: a config key or Apprise URLs is needed"
    );
    assert_eq!(
        err(json!({"kind": "ntfy", "config": {"topic": "t", "server": "ntfy.sh"}})),
        "server: must start with http:// or https://"
    );
}

#[test]
fn a_clean_channel() {
    let c = clean(
        &body(json!({"kind": "ntfy", "name": "  ", "config": {"topic": " arrs ", "token": 12345},
                     "events": ["backup_failed", "nope", 3], "arr": {"apps": ["radarr", "lidarr"]}})),
        None,
        &arrs(),
    )
    .expect("valid");
    assert_eq!(c.name, "ntfy");
    assert_eq!(c.cfg("server"), "https://ntfy.sh", "the field's default");
    assert_eq!(c.cfg("topic"), "arrs");
    assert_eq!(c.cfg("token"), "12345");
    assert_eq!(c.cfg("username"), "");
    assert_eq!(c.events, vec!["backup_failed"]);
    assert_eq!(c.arr.apps, vec!["radarr"]);
    assert_eq!(c.arr.events, vec!["import", "manual", "health"]);
    assert!(c.enabled);
    assert_eq!(c.id.len(), 8);
    assert!(c.created.ends_with("+00:00"));

    // the masked secret keeps the stored one; kind and id carry over
    let again = clean(
        &body(json!({"enabled": false, "config": {"topic": "arrs", "token": MASK}, "arr": {"events": ["grab", "x"]}})),
        Some(&c),
        &arrs(),
    )
    .expect("valid");
    assert_eq!((again.id.as_str(), again.kind.as_str()), (c.id.as_str(), "ntfy"));
    assert_eq!(again.cfg("token"), "12345");
    assert_eq!(again.created, c.created);
    assert!(!again.enabled);
    assert_eq!(again.arr.events, vec!["grab"]);
    let off =
        clean(&body(json!({"kind": "ntfy", "enabled": null, "config": {"topic": "t"}})), None, &arrs()).expect("valid");
    assert!(!off.enabled, "enabled: null is false, like bool(None)");
}

#[test]
fn masking_and_scrubbing() {
    let c = chan("ntfy", &[("server", "https://ntfy.sh"), ("topic", "t"), ("token", "tk_secret"), ("password", "")]);
    let p = public(&c);
    assert_eq!(p["config"]["token"], MASK);
    assert_eq!(p["config"]["password"], "", "an empty secret isn't masked");
    assert_eq!(p["config"]["topic"], "t");
    assert_eq!(p["id"], "abcd1234");
    assert_eq!(scrub(&c, "401 for tk_secret"), "401 for …");
    let short = chan("ntfy", &[("token", "abc")]);
    assert_eq!(scrub(&short, "abc"), "abc", "secrets under 4 characters stay");
    assert_eq!(scrub(&c, &"x".repeat(400)).chars().count(), 300);
}

#[test]
fn settings() {
    let cur = Settings::default();
    assert_eq!((cur.interval, cur.disk_free_gb, cur.stall_minutes, cur.link.as_str()), (5, 20.0, 60, ""));
    let s = cur
        .merged(&body(
            json!({"interval": "0", "disk_free_gb": "12.5", "stall_minutes": -3, "link": " https://w.example/ "}),
        ))
        .expect("ok");
    assert_eq!((s.interval, s.disk_free_gb, s.stall_minutes, s.link.as_str()), (1, 12.5, 0, "https://w.example"));
    let s = cur.merged(&body(json!({"interval": 99999, "disk_free_gb": -1}))).expect("ok");
    assert_eq!((s.interval, s.disk_free_gb), (1440, 0.0));
    assert_eq!(cur.merged(&body(json!({}))).expect("ok"), cur);
    let bad = cur.merged(&body(json!({"interval": "five"}))).expect_err("bad");
    assert_eq!(bad.to_string(), "interval, disk space and stall time must be numbers");
    let bad = cur.merged(&body(json!({"stall_minutes": null}))).expect_err("bad");
    assert_eq!(bad.to_string(), "interval, disk space and stall time must be numbers");
    let bad = cur.merged(&body(json!({"link": "w.example"}))).expect_err("bad");
    assert_eq!(bad.to_string(), "the link must start with http:// or https://");
}

#[test]
fn stored_settings() {
    // as stored: the shape the page and the API see
    let s = Settings::default().merged(&body(json!({"interval": 10, "link": "https://w.example"}))).expect("ok");
    let stored = serde_json::to_value(&s).expect("json");
    assert_eq!(stored, json!({"interval": 10, "disk_free_gb": 20.0, "stall_minutes": 60, "link": "https://w.example"}));
    assert_eq!(Settings::from_stored(Some(&stored)), s, "a written row reads back the same");
    // older rows: numbers as strings, values that don't read, keys from elsewhere
    let old =
        Settings::from_stored(Some(&json!({"interval": "15", "disk_free_gb": 0, "stall_minutes": "x", "note": 1})));
    assert_eq!((old.interval, old.disk_free_gb, old.stall_minutes, old.link.as_str()), (15, 0.0, 60, ""));
    assert_eq!(serde_json::to_value(&old).expect("json")["note"], 1, "kept as it is");
    assert_eq!(Settings::from_stored(None), Settings::default());
    assert_eq!(Settings::from_stored(Some(&json!({"interval": 0}))).interval, 1);
}

fn ep() -> Endpoints {
    Endpoints { telegram: "http://tg".into(), pushover: "http://po/".into() }
}

fn json_body(o: &Outgoing) -> &Value {
    match &o.body {
        OutBody::Json(v) => v,
        _ => &Value::Null,
    }
}

#[test]
fn payloads() {
    let at = "2026-10-26T09:00:00+00:00";
    let t = chan("telegram", &[("bot_token", "123:ABC"), ("chat_id", "-100"), ("topic_id", "7")]);
    let o = build_request(&t, "Backup <x>", "a & 'b'", "error", "backup_failed", at, &ep()).expect("ok");
    assert_eq!(o.url, "http://tg/bot123:ABC/sendMessage");
    assert_eq!(json_body(&o)["text"], "❌ <b>Backup &lt;x&gt;</b>\na &amp; &#39;b&#39;");
    assert_eq!(json_body(&o)["message_thread_id"], 7);
    assert_eq!(json_body(&o)["parse_mode"], "HTML");
    let bad = chan("telegram", &[("bot_token", "x"), ("chat_id", "1"), ("topic_id", "seven")]);
    assert_eq!(
        build_request(&bad, "t", "m", "info", "e", at, &ep()).expect_err("bad topic"),
        r#"topic ID "seven" isn't a whole number"#
    );

    let d = chan("discord", &[("webhook_url", "https://d/hook")]);
    let o = build_request(&d, &"T".repeat(300), "m", "warning", "disk_low", at, &ep()).expect("ok");
    let e = &json_body(&o)["embeds"][0];
    assert_eq!(e["title"].as_str().map(|s| s.len()), Some(256));
    assert_eq!(
        (e["color"].clone(), e["footer"]["text"].clone(), e["timestamp"].clone()),
        (json!(0xFFA500), json!("disk_low"), json!(at))
    );
    assert_eq!(json_body(&o)["username"], "wranglarr");

    let s = chan("slack", &[("webhook_url", "https://hooks")]);
    let o = build_request(&s, "T", "m", "ok", "e", at, &ep()).expect("ok");
    assert_eq!(json_body(&o)["text"], "✅ *T*\nm");

    let n = chan(
        "ntfy",
        &[("server", "https://ntfy.sh/"), ("topic", "arrs"), ("token", ""), ("username", "u"), ("password", "p")],
    );
    let o = build_request(&n, "Café", "body", "error", "e", at, &ep()).expect("ok");
    assert_eq!(o.url, "https://ntfy.sh/arrs");
    assert_eq!(o.headers[0], ("Title".to_string(), "Café".as_bytes().to_vec()));
    assert_eq!(o.headers[1].1, b"high");
    assert_eq!(o.headers[2].1, b"rotating_light");
    assert_eq!(o.basic, Some(("u".into(), "p".into())));
    assert_eq!(o.body, OutBody::Bytes(b"body".to_vec()));
    let n = chan("ntfy", &[("server", "https://ntfy.sh"), ("topic", "arrs"), ("token", "tk"), ("username", "u")]);
    let o = build_request(&n, "T", "m", "info", "e", at, &ep()).expect("ok");
    assert_eq!(o.basic, None, "a token wins over the user");
    assert!(o.headers.contains(&("Authorization".into(), b"Bearer tk".to_vec())));
    assert!(o.headers.contains(&("Priority".into(), b"low".to_vec())));

    let g = chan("gotify", &[("server", "https://g/"), ("token", "gt")]);
    let o = build_request(&g, "T", "m", "warning", "e", at, &ep()).expect("ok");
    assert_eq!(o.url, "https://g/message");
    assert_eq!(json_body(&o)["priority"], 5);
    assert_eq!(o.headers, vec![("X-Gotify-Key".to_string(), b"gt".to_vec())]);

    let p = chan("pushover", &[("user_key", "uk"), ("token", "pt")]);
    let o = build_request(&p, "T", &"m".repeat(2000), "error", "e", at, &ep()).expect("ok");
    assert_eq!(o.url, "http://po/1/messages.json");
    let OutBody::Form(f) = &o.body else { panic!("a form") };
    assert_eq!(f[0], ("token".into(), "pt".into()));
    assert_eq!(f[1], ("user".into(), "uk".into()));
    assert_eq!(f[3].1.len(), 1024);
    assert_eq!(f[4], ("priority".into(), "1".into()));

    let a = chan("apprise", &[("server", "http://apprise:8000/"), ("key", "arrs"), ("urls", "")]);
    let o = build_request(&a, "T", "m", "ok", "e", at, &ep()).expect("ok");
    assert_eq!(o.url, "http://apprise:8000/notify/arrs");
    assert_eq!(json_body(&o), &json!({"title": "T", "body": "m", "type": "success"}));
    let a = chan("apprise", &[("server", "http://apprise:8000"), ("key", "arrs"), ("urls", "tgram://x")]);
    let o = build_request(&a, "T", "m", "error", "e", at, &ep()).expect("ok");
    assert_eq!(o.url, "http://apprise:8000/notify/");
    assert_eq!(json_body(&o)["urls"], "tgram://x");
    assert_eq!(json_body(&o)["type"], "failure");

    let w = chan("webhook", &[("url", "https://hook"), ("username", "u"), ("password", "")]);
    let o = build_request(&w, "T", "m", "info", "update_available", at, &ep()).expect("ok");
    assert_eq!(
        json_body(&o),
        &json!({"source": "wranglarr", "event": "update_available", "level": "info", "title": "T", "message": "m", "at": at})
    );
    assert_eq!(o.basic, Some(("u".into(), String::new())));
    assert_eq!(o.what, "the webhook");
}

#[test]
fn error_answers() {
    assert_eq!(check_response(200, "", "ntfy"), Ok(()));
    assert_eq!(
        check_response(401, r#"{"ok":false,"description":"Unauthorized"}"#, "Telegram"),
        Err("Telegram answered 401: Unauthorized".into())
    );
    assert_eq!(
        check_response(400, r#"{"message":"","error":"bad topic"}"#, "ntfy"),
        Err("ntfy answered 400: bad topic".into())
    );
    assert_eq!(check_response(500, "boom", "Gotify"), Err("Gotify answered 500: boom".into()));
    assert_eq!(check_response(500, "[1,2]", "Slack"), Err("Slack answered 500: [1,2]".into()));
}

#[test]
fn an_email() {
    let c = chan("email", &[("sender", "wranglarr <w@example.com>"), ("to", "a@example.com, b@example.com,")]);
    let m = build_email(&c, "Backup of media done", "Version x.").expect("email");
    let raw = String::from_utf8(m.formatted()).expect("utf8");
    assert!(raw.contains("Subject: Backup of media done"), "{raw}");
    assert!(raw.contains("From: wranglarr <w@example.com>"), "{raw}");
    assert!(raw.contains("To: a@example.com, b@example.com"), "{raw}");
    assert!(raw.contains("Content-Type: text/plain"), "{raw}");
    assert!(raw.trim_end().ends_with("Version x."), "{raw}");
    let bad = chan("email", &[("sender", "not an address"), ("to", "a@example.com")]);
    assert!(build_email(&bad, "t", "m").expect_err("bad from").starts_with("From: "));
}

#[test]
fn job_messages() {
    let m =
        run_message("media", "backup", "", "failed", &json!({"error": "disk full", "version": "v1"}), 7).expect("msg");
    assert_eq!(
        m,
        (
            "backup_failed",
            "Backup of media failed".into(),
            "Version v1 is failed: disk full. See Activity, run 7.".into(),
            "error"
        )
    );
    let m = run_message("media", "backup", "", "partial", &json!({}), 8).expect("msg");
    assert_eq!(m.2, "Version ? is partial. See Activity, run 8.");
    let m = run_message("media", "backup", "", "ok", &json!({"version": "v2", "total_bytes": 1536}), 9).expect("msg");
    assert_eq!(m, ("backup_ok", "Backup of media done".into(), "Version v2, 1.5 KB.".into(), "ok"));
    let m = run_message("media", "restore", "radarr", "ok", &json!({}), 3).expect("msg");
    assert_eq!(m, ("restore", "Restore radarr ok".into(), "Suite media: ok. See Activity, run 3.".into(), "ok"));
    let m = run_message("media", "restore-plan", "plan", "failed", &json!({"error": "x"}), 3).expect("msg");
    assert_eq!(m.3, "error");
    let m = run_message(
        "_",
        "update",
        "radarr",
        "ok",
        &json!({"results": {"radarr": "updated", "sonarr": "unchanged"}}),
        4,
    )
    .expect("msg");
    assert_eq!(m, ("update_done", "Container update done".into(), "radarr updated, sonarr unchanged".into(), "ok"));
    let m = run_message("_", "rollback", "radarr", "failed", &json!({"error": "pull"}), 5).expect("msg");
    assert_eq!(
        m,
        ("update_failed", "Container rollback failed".into(), "failed: pull. See Activity, run 5.".into(), "error")
    );
    let failed = json!({
        "results": {"radarr": "failed"},
        "reports": {"radarr": {"id": "wf-0123456789", "search": "https://github.com/o/r/issues?q=wf-0123456789"}}
    });
    let m = run_message("_", "update", "radarr", "failed", &failed, 7).expect("msg");
    assert_eq!(
        m.2,
        "radarr failed. See Activity, run 7. radarr: check whether it's been reported: \
         https://github.com/o/r/issues?q=wf-0123456789 If it hasn't, its report is on the Updates page."
    );
    let m = run_message("media", "connect", "", "partial", &json!({}), 6).expect("msg");
    assert_eq!(
        m,
        ("sync_failed", "Connecting apps partial".into(), "media: partial. See Activity, run 6.".into(), "warning")
    );
    assert!(run_message("media", "sync", "x", "ok", &json!({}), 6).is_none());
    assert!(run_message("media", "prune", "x", "failed", &json!({}), 6).is_none());
}

#[test]
fn self_update_messages() {
    let ok = json!({"results": {"wranglarr": "updated", "wranglarr-init": "recreated"}, "version": "0.2.0"});
    let m = run_message("system", "self-update", "wranglarr", "ok", &ok, 9).expect("msg");
    assert_eq!(m, ("update_done", "wranglarr updated".into(), "wranglarr is now 0.2.0.".into(), "ok"));
    let m = run_message("system", "self-update", "wranglarr", "ok", &json!({"results": {"wranglarr": "updated"}}), 9)
        .expect("msg");
    assert_eq!(m.2, "wranglarr is on its new image.");
    let failed = json!({"results": {"wranglarr": "failed"}, "error": "the new wranglarr exited with code 1"});
    let m = run_message("system", "self-update", "wranglarr", "failed", &failed, 10).expect("msg");
    assert_eq!(
        m,
        (
            "update_failed",
            "wranglarr update failed".into(),
            "failed: the new wranglarr exited with code 1. The version before it is running. See Activity, run 10."
                .into(),
            "error"
        )
    );
    // nothing newer was pulled: nothing to say
    let current = json!({"results": {"wranglarr": "current"}});
    assert!(run_message("system", "self-update", "wranglarr", "ok", &current, 11).is_none());
}

#[test]
fn a_newer_wranglarr_points_at_update_now_when_it_can_update_itself() {
    let row = |can: bool| {
        json!({"status": "self", "newer": true, "remote_digest": "sha:9", "self_update": can,
               "update_hint": "update it from the shell: `docker compose pull wranglarr && docker compose up -d wranglarr`"})
    };
    let msg = |can: bool| {
        let inp = Inputs { updates: vec![("wranglarr".into(), row(can))], ..Default::default() };
        let c = build_conditions(&inp, &Settings::default());
        let (_, me) = c.0.into_iter().find(|(k, _)| k == "self:wranglarr:sha:9").expect("the alert");
        me.message
    };
    let notes = crate::updates::RELEASE_NOTES;
    assert_eq!(
        msg(true),
        format!(
            "Press Update now on the dashboard or the Updates page, or update it from the shell: \
             `docker compose pull wranglarr && docker compose up -d wranglarr`. Release notes: {notes}"
        )
    );
    assert!(msg(false).starts_with("Update it from the shell: `docker compose pull"), "{}", msg(false));
}

#[test]
fn arr_side() {
    let mut t = chan("telegram", &[("bot_token", "b"), ("chat_id", "c"), ("topic_id", "12")]);
    let f = arr_fields(&t).expect("fields");
    assert_eq!(Value::Object(f), json!({"botToken": "b", "chatId": "c", "sendSilently": false, "topicId": 12}));
    let e = chan(
        "email",
        &[("host", "smtp"), ("port", "465"), ("security", "none"), ("sender", "a@b"), ("to", "x@y, z@w")],
    );
    let f = arr_fields(&e).expect("fields");
    assert_eq!(f["port"], 465);
    assert_eq!(f["useEncryption"], 2);
    assert_eq!(f["requireEncryption"], false);
    assert_eq!(f["to"], json!(["x@y", "z@w"]));
    let s = arr_fields(&chan("slack", &[("webhook_url", "h")])).expect("fields");
    assert_eq!(s["username"], "wranglarr");

    let schema = vec![json!({"implementation": "Telegram", "onGrab": false, "onDownload": false, "onUpgrade": false,
        "onHealthIssue": false, "onHealthRestored": false, "supportsOnHealthRestored": false, "includeHealthWarnings": false,
        "fields": [{"name": "botToken"}, {"name": "chatId"}, {"name": "topicId"}]})];
    t.arr.events = vec!["grab".into(), "health".into()];
    let b = arr_body(&schema, &t).expect("body");
    assert_eq!(b["name"], "wranglarr · Mine");
    assert_eq!(
        (b["onGrab"].clone(), b["onDownload"].clone(), b["onHealthIssue"].clone()),
        (json!(true), json!(false), json!(true))
    );
    assert_eq!(b["onHealthRestored"], false, "the app doesn't support it");
    assert_eq!(b["includeHealthWarnings"], true);
    assert_eq!(b["fields"][2]["value"], 12);
    t.arr.events = vec!["import".into()];
    assert_eq!(arr_body(&schema, &t).expect("body")["includeHealthWarnings"], false);
}

#[test]
fn an_import_sets_every_arrs_import_flag() {
    let mut import: Vec<&str> = arr_flags("import").collect();
    import.sort_unstable();
    assert_eq!(import, ["onBookFileImport", "onDownload", "onImportComplete", "onReleaseImport"]);
    assert_eq!(arr_flags("grab").collect::<Vec<_>>(), ["onGrab"]);
    assert_eq!(arr_flags("health").collect::<Vec<_>>(), ["onHealthIssue", "onHealthRestored"]);
    assert_eq!(arr_flags("nope").count(), 0);
}

#[test]
fn sizes_for_people() {
    assert_eq!(human(&json!(0)), "0 B");
    assert_eq!(human(&Value::Null), "0 B");
    assert_eq!(human(&json!(1023)), "1023 B");
    assert_eq!(human(&json!(5 * 1024 * 1024)), "5.0 MB");
    assert_eq!(human(&json!(3.0 * 1024f64.powi(5))), "3.0 PB");
    assert_eq!(human(&json!("x")), "?");
    assert_eq!(human(&json!(true)), "?");
}

#[test]
fn catalogue_keeps_its_order() {
    let s = serde_json::to_string(&kinds_json()).expect("json");
    let order: Vec<usize> = KINDS.iter().filter_map(|k| s.find(&format!("\"{}\":", k.id))).collect();
    assert_eq!(order.len(), KINDS.len());
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{s}");
    let e = serde_json::to_value(events_json()).expect("json");
    assert_eq!(e["app_down"]["plural"], "apps offline");
    assert!(e["backup_ok"].get("plural").is_none());
    assert_eq!(e["backup_ok"]["default"], false);
    let k = serde_json::to_value(kinds_json()).expect("json");
    assert_eq!(k["email"]["fields"][2]["options"][0], json!(["starttls", "STARTTLS (587)"]));
    assert_eq!(k["ntfy"]["fields"][0]["default"], "https://ntfy.sh");
    assert_eq!(k["telegram"]["fields"][0]["type"], "secret");
}

fn probe(name: &str, online: bool) -> Probe {
    Probe { name: name.into(), url: format!("http://{name}:1"), online, ..Default::default() }
}

#[test]
fn conditions() {
    let mut radarr = probe("radarr", true);
    radarr.health = vec![json!({"type": "warning", "source": "IndexerCheck", "message": "No indexers"})];
    radarr.queue = vec![("dl1".into(), "Movie".into(), "import blocked: no match".into())];
    radarr.roots = vec![json!({"path": "/data/media", "freeSpace": 1024}), json!({"path": "/x", "freeSpace": null})];
    let inp = Inputs {
        probes: vec![probe("sonarr", false), radarr],
        backups_disk: Some(("/backups".into(), 100.0 * 1024f64.powi(3))),
        vpn: Some(json!({"ok": false, "status": "stopped", "public_ip": null})),
        downloads: vec![
            json!({"client": "qb", "id": "h1", "name": "A", "state": "error", "state_text": "", "arr": {"name": "radarr"}}),
            json!({"client": "qb", "id": "h2", "name": "B", "state": "stalled", "state_text": "stalled", "arr": null}),
        ],
        updates: vec![
            ("sonarr".into(), json!({"status": "available", "held": false, "remote_digest": "sha:2"})),
            ("radarr".into(), json!({"status": "available", "held": false, "remote_digest": "sha:1"})),
            ("prowlarr".into(), json!({"status": "available", "held": true})),
            (
                "wranglarr".into(),
                json!({"status": "self", "newer": true, "remote_digest": "sha:3",
                                        "update_hint": "update it from the shell: `x`"}),
            ),
            ("wranglarr2".into(), json!({"status": "self", "self_update": false})),
        ],
    };
    let c = build_conditions(&inp, &Settings::default());
    let keys: Vec<&str> = c.0.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "down:sonarr",
            "health:radarr:IndexerCheck",
            "queue:radarr:dl1",
            "disk:/data/media",
            "vpn",
            "dl:qb:h1",
            "dl:qb:h2",
            "upd:radarr:sha:1",
            "upd:sonarr:sha:2",
            "self:wranglarr:sha:3"
        ]
    );
    let get = |k: &str| c.0.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).expect("there");
    let down = get("down:sonarr");
    assert_eq!(
        (down.title.as_str(), down.message.as_str(), down.confirm),
        ("sonarr is offline", "wranglarr can't reach http://sonarr:1.", 2)
    );
    assert_eq!(down.clear, Some(("sonarr is back".into(), "sonarr answers again.".into())));
    assert_eq!(get("health:radarr:IndexerCheck").title, "radarr: health warning");
    assert_eq!(get("queue:radarr:dl1").title, "radarr needs you: Movie");
    assert_eq!(get("disk:/data/media").message, "1 KB free (warning below 20 GB).".replace("1 KB", "1.0 KB"));
    assert_eq!(get("vpn").message, "gluetun reports stopped; the download clients behind it are cut off.");
    assert_eq!(get("vpn").clear.map(|c| c.1), Some("gluetun is running again (no IP yet).".into()));
    assert_eq!(get("dl:qb:h1").title, "Download error in qb (radarr)");
    assert_eq!(get("dl:qb:h1").message, "A: error");
    let stuck = get("dl:qb:h2");
    assert_eq!((stuck.message.as_str(), stuck.after), ("B: stalled for over 60 min.", 3600.0));
    assert_eq!(get("upd:radarr:sha:1").group, Some("updates"));
    let me = get("self:wranglarr:sha:3");
    assert_eq!((me.event, me.title.as_str(), me.group), ("self_update_available", "A newer wranglarr is out", None));
    assert_eq!(me.message, format!("Update it from the shell: `x`. Release notes: {}", crate::updates::RELEASE_NOTES));
}

fn cond(event: &'static str, level: &'static str, title: &str) -> Cond {
    Cond::new(event, level, title.into(), format!("{title}!"))
}

#[test]
fn the_watchers_rounds() {
    let off = HashSet::new();
    let mut cur = Conds::default();
    let mut down = cond("app_down", "error", "radarr is offline");
    down.confirm = 2;
    down.clear = Some(("radarr is back".into(), "radarr answers again.".into()));
    cur.put("down:radarr".into(), down);
    cur.put("health:sonarr:a".into(), cond("app_health", "warning", "sonarr: a"));
    cur.put("health:sonarr:b".into(), cond("app_health", "warning", "sonarr: b"));
    let mut u1 = cond("update_available", "info", "Container updates available");
    u1.message = "sonarr".into();
    u1.group = Some("updates");
    let mut u2 = u1.clone();
    u2.message = "radarr".into();
    cur.put("upd:sonarr:".into(), u1);
    cur.put("upd:radarr:".into(), u2);

    // round 1: the health warnings (one message for two) and the updates; the app needs a second look
    let (seen, sends, n) = step(&Map::new(), &cur, 1000.0, true, &off);
    assert_eq!(n, 2);
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0].title, "2 new health warnings");
    assert_eq!(sends[0].message, "• sonarr: a: sonarr: a!\n• sonarr: b: sonarr: b!");
    assert_eq!(sends[0].level, "warning");
    assert_eq!((sends[1].title.as_str(), sends[1].message.as_str()), ("Container updates available", "radarr, sonarr"));
    assert_eq!(seen["down:radarr"]["count"], 1);
    assert_eq!(seen["down:radarr"]["sent"], false);
    assert_eq!(seen["health:sonarr:a"]["sent"], true);
    assert_eq!(seen["down:radarr"]["clear"], json!(["radarr is back", "radarr answers again."]));

    // round 2: the app is confirmed down; nothing else again
    let (seen, sends, n) = step(&seen, &cur, 1300.0, true, &off);
    assert_eq!(n, 1);
    assert_eq!(
        sends,
        vec![Send {
            event: "app_down".into(),
            title: "radarr is offline".into(),
            message: "radarr is offline!".into(),
            level: "error".into()
        }]
    );
    assert_eq!(seen["down:radarr"]["first"], 1000.0);

    // round 3: the app is back; the health warnings of an app that's offline are kept
    let mut cur3 = Conds::default();
    cur3.put("down:sonarr".into(), cond("app_down", "error", "sonarr is offline"));
    let offline: HashSet<String> = ["sonarr".to_string()].into();
    let (seen3, sends, _) = step(&seen, &cur3, 1600.0, true, &offline);
    assert!(seen3.contains_key("health:sonarr:a"), "kept while sonarr is down");
    assert!(!seen3.contains_key("upd:sonarr:"));
    assert!(sends.iter().any(|s| s.title == "radarr is back" && s.level == "ok" && s.event == "app_down"));

    // no channel: tracked but not used up
    let (seen, sends, n) = step(&Map::new(), &cur, 1000.0, false, &off);
    assert!(sends.is_empty() && n == 0);
    assert_eq!(seen["health:sonarr:a"]["sent"], false);
}

#[test]
fn waits_for_a_stall() {
    let mut c = cond("download_problem", "warning", "Download stuck");
    c.after = 600.0;
    let mut cur = Conds::default();
    cur.put("dl:qb:1".into(), c);
    let (seen, sends, _) = step(&Map::new(), &cur, 0.0, true, &HashSet::new());
    assert!(sends.is_empty());
    let (_, sends, _) = step(&seen, &cur, 300.0, true, &HashSet::new());
    assert!(sends.is_empty());
    let (_, sends, _) = step(&seen, &cur, 601.0, true, &HashSet::new());
    assert_eq!(sends.len(), 1);
}

#[test]
fn flagged_queue_items() {
    let q = json!({"records": [
        {"downloadId": "abc", "title": "Movie.2024", "trackedDownloadStatus": "warning", "trackedDownloadState": "importBlocked",
         "statusMessages": [{"title": "Movie.2024", "messages": ["No files found", "No files found"]}, {"title": "Other"}]},
        {"id": 5, "trackedDownloadStatus": "ok", "trackedDownloadState": "downloading"},
        {"id": 6, "trackedDownloadStatus": "ok", "trackedDownloadState": "failedPending", "errorMessage": "bad"},
        {"id": 7, "trackedDownloadStatus": "error"}
    ]});
    let p = queue_problems(&q);
    assert_eq!(p.len(), 3);
    assert_eq!(p[0], ("abc".into(), "Movie.2024".into(), "import blocked: No files found; Other".into()));
    assert_eq!(p[1], ("6".into(), "?".into(), "failed: bad".into()));
    assert_eq!(p[2], ("7".into(), "?".into(), "check the app's Activity → Queue".into()));
    assert_eq!(
        queue_problems(
            &json!([{"id": 1, "trackedDownloadStatus": "warning", "trackedDownloadState": "importPending"}])
        )[0]
        .2,
        "waiting to import: check the app's Activity → Queue"
    );
}
