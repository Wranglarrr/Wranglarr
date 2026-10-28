// SPDX-License-Identifier: GPL-3.0-or-later
//! Sending a message: the HTTP request each channel kind takes, email, and
//! reading the answer.

use std::time::Duration;

use serde_json::{Value, json};

use crate::format::html_escape;
use crate::value::{head, text, truthy};

use super::catalog::{level_color, level_emoji};
use super::channel::{Channel, whole_number};

// ---- building the messages ---------------------------------------------------------------

/// Where Telegram and Pushover are (fixed; tests point them at a mock).
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub telegram: String,
    pub pushover: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints { telegram: "https://api.telegram.org".into(), pushover: "https://api.pushover.net".into() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum OutBody {
    Json(Value),
    Form(Vec<(String, String)>),
    Bytes(Vec<u8>),
}

/// One HTTP POST that sends a message.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Outgoing {
    pub url: String,
    pub headers: Vec<(String, Vec<u8>)>,
    pub basic: Option<(String, String)>,
    pub body: OutBody,
    /// For error messages: "Telegram answered 401: …"
    pub what: &'static str,
}

/// The request for a message on an HTTP channel (every kind but email).
pub(super) fn build_request(
    c: &Channel,
    title: &str,
    message: &str,
    level: &str,
    event: &str,
    at: &str,
    ep: &Endpoints,
) -> std::result::Result<Outgoing, String> {
    let g = |k: &str| c.cfg(k).to_string();
    let base = |k: &str| g(k).trim_end_matches('/').to_string();
    let mut out =
        Outgoing { url: String::new(), headers: vec![], basic: None, body: OutBody::Json(json!({})), what: "" };
    match c.kind.as_str() {
        "telegram" => {
            let mut body = json!({"chat_id": g("chat_id"), "parse_mode": "HTML", "disable_web_page_preview": true,
                "text": format!("{} <b>{}</b>\n{}", level_emoji(level), html_escape(title), html_escape(message))});
            let topic = g("topic_id");
            if !topic.is_empty() {
                body["message_thread_id"] = json!(whole_number("topic ID", &topic)?);
            }
            out.url = format!("{}/bot{}/sendMessage", ep.telegram.trim_end_matches('/'), g("bot_token"));
            out.body = OutBody::Json(body);
            out.what = "Telegram";
        }
        "discord" => {
            out.url = g("webhook_url");
            out.body = OutBody::Json(json!({"username": "wranglarr", "embeds": [{
                "title": head(title, 256), "description": head(message, 4000), "color": level_color(level),
                "footer": {"text": event}, "timestamp": at}]}));
            out.what = "Discord";
        }
        "slack" => {
            out.url = g("webhook_url");
            out.body = OutBody::Json(json!({"text": format!("{} *{title}*\n{message}", level_emoji(level))}));
            out.what = "Slack";
        }
        "ntfy" => {
            let prio = match level {
                "error" => "high",
                "info" | "ok" => "low",
                _ => "default",
            };
            let tags = match level {
                "error" => "rotating_light",
                "warning" => "warning",
                "ok" => "white_check_mark",
                _ => "information_source",
            };
            // the title's UTF-8 bytes as they are (ntfy reads headers as UTF-8)
            out.headers = vec![
                ("Title".into(), title.as_bytes().to_vec()),
                ("Priority".into(), prio.into()),
                ("Tags".into(), tags.into()),
            ];
            if !g("token").is_empty() {
                out.headers.push(("Authorization".into(), format!("Bearer {}", g("token")).into_bytes()));
            } else if !g("username").is_empty() {
                out.basic = Some((g("username"), g("password")));
            }
            out.url = format!("{}/{}", base("server"), g("topic"));
            out.body = OutBody::Bytes(message.as_bytes().to_vec());
            out.what = "ntfy";
        }
        "gotify" => {
            let prio = match level {
                "error" => 8,
                "warning" => 5,
                _ => 3,
            };
            out.url = format!("{}/message", base("server"));
            out.headers = vec![("X-Gotify-Key".into(), g("token").into_bytes())];
            out.body = OutBody::Json(json!({"title": title, "message": message, "priority": prio}));
            out.what = "Gotify";
        }
        "pushover" => {
            out.url = format!("{}/1/messages.json", ep.pushover.trim_end_matches('/'));
            out.body = OutBody::Form(vec![
                ("token".into(), g("token")),
                ("user".into(), g("user_key")),
                ("title".into(), title.into()),
                ("message".into(), head(message, 1024)),
                ("priority".into(), if level == "error" { "1" } else { "0" }.into()),
            ]);
            out.what = "Pushover";
        }
        "apprise" => {
            let typ = match level {
                "error" => "failure",
                "warning" => "warning",
                "ok" => "success",
                _ => "info",
            };
            let mut body = json!({"title": title, "body": message, "type": typ});
            if !g("urls").is_empty() {
                body["urls"] = json!(g("urls"));
                out.url = format!("{}/notify/", base("server"));
            } else {
                out.url = format!("{}/notify/{}", base("server"), g("key"));
            }
            out.body = OutBody::Json(body);
            out.what = "Apprise";
        }
        "webhook" => {
            if !g("username").is_empty() {
                out.basic = Some((g("username"), g("password")));
            }
            out.url = g("url");
            out.body = OutBody::Json(json!({"source": "wranglarr", "event": event, "level": level,
                "title": title, "message": message, "at": at}));
            out.what = "the webhook";
        }
        other => return Err(format!("unknown kind {other:?}")),
    }
    Ok(out)
}

/// An error status as "<what> answered 401: <the service's reason>".
pub(super) fn check_response(status: u16, body: &str, what: &str) -> std::result::Result<(), String> {
    if status < 400 {
        return Ok(());
    }
    let mut detail = head(body, 200);
    if let Ok(Value::Object(j)) = serde_json::from_str::<Value>(body)
        && let Some(v) =
            ["description", "message", "error", "errors"].iter().filter_map(|k| j.get(*k)).find(|v| truthy(v))
    {
        detail = text(v);
    }
    Err(format!("{what} answered {status}: {detail}"))
}

/// The email for a message.
pub(super) fn build_email(c: &Channel, title: &str, message: &str) -> std::result::Result<lettre::Message, String> {
    use lettre::message::{Mailbox, header::ContentType};
    let from: Mailbox = c.cfg("sender").trim().parse().map_err(|e| format!("From: {e}"))?;
    let mut b = lettre::Message::builder().subject(title).from(from);
    for to in c.cfg("to").split(',').map(str::trim).filter(|x| !x.is_empty()) {
        let mb: Mailbox = to.parse().map_err(|e| format!("To {to}: {e}"))?;
        b = b.to(mb);
    }
    b.header(ContentType::TEXT_PLAIN).body(message.to_string()).map_err(|e| e.to_string())
}

/// A send error: the reason, or what reqwest says.
pub(super) fn send_error(e: &reqwest::Error) -> String {
    let r = crate::http::reason(e);
    if r.is_empty() { "HTTPError".into() } else { r }
}

pub(super) async fn send_email(c: &Channel, title: &str, message: &str) -> std::result::Result<(), String> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
    let msg = build_email(c, title, message)?;
    let port: u16 = match c.cfg("port") {
        "" => 587,
        p => p.parse().map_err(|_| format!("port must be 0-65535: {p}"))?,
    };
    let host = c.cfg("host");
    let builder = match c.cfg("security") {
        "ssl" => AsyncSmtpTransport::<Tokio1Executor>::relay(host).map_err(|e| e.to_string())?,
        "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host).map_err(|e| e.to_string())?,
        _ => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
    };
    let mut builder = builder.port(port).timeout(Some(Duration::from_secs(20)));
    if !c.cfg("username").is_empty() {
        builder = builder.credentials(Credentials::new(c.cfg("username").into(), c.cfg("password").into()));
    }
    builder.build().send(msg).await.map(|_| ()).map_err(|e| {
        let mut s = e.to_string();
        let mut src = std::error::Error::source(&e);
        while let Some(x) = src {
            s = format!("{s}: {x}");
            src = x.source();
        }
        s
    })
}
