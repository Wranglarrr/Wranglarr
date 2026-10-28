// SPDX-License-Identifier: GPL-3.0-or-later
//! What can be notified and where to: the channel kinds with their fields, and
//! the events with their levels, in the order the page lists them.

use serde_json::{Map, Value, json};

use crate::service::arr;

// ---- the catalogue ------------------------------------------------------------------

pub(super) struct Field {
    pub key: &'static str,
    pub label: &'static str,
    /// text | secret | number | select
    pub kind: &'static str,
    pub required: bool,
    pub placeholder: &'static str,
    pub hint: &'static str,
    pub default: &'static str,
    pub options: &'static [(&'static str, &'static str)],
}

const fn f(key: &'static str, label: &'static str, kind: &'static str) -> Field {
    Field { key, label, kind, required: false, placeholder: "", hint: "", default: "", options: &[] }
}

impl Field {
    const fn req(mut self) -> Self {
        self.required = true;
        self
    }
    const fn ph(mut self, p: &'static str) -> Self {
        self.placeholder = p;
        self
    }
    const fn hint(mut self, h: &'static str) -> Self {
        self.hint = h;
        self
    }
    const fn default(mut self, d: &'static str) -> Self {
        self.default = d;
        self
    }
    fn json(&self) -> Value {
        json!({"key": self.key, "label": self.label, "type": self.kind, "required": self.required,
               "placeholder": self.placeholder, "hint": self.hint, "default": self.default,
               "options": self.options.iter().map(|(v, l)| json!([v, l])).collect::<Vec<_>>()})
    }
}

pub(super) struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub logo: &'static str,
    /// The arr Connect implementation it maps to.
    pub arr: &'static str,
    pub fields: &'static [Field],
}

/// Per kind: its fields, and the arr Connect implementation it maps to.
pub(super) static KINDS: &[Kind] = &[
    Kind {
        id: "telegram",
        label: "Telegram",
        icon: "✈",
        logo: "/static/icons/telegram.svg",
        arr: "Telegram",
        fields: &[
            f("bot_token", "Bot token", "secret").req().ph("123456:ABC-DEF…").hint("From @BotFather: /newbot."),
            f("chat_id", "Chat ID", "text").req().ph("-1001234567890").hint(
                "Your user ID, or a group's (starts with -100). @userinfobot tells you yours; add the bot to the group first.",
            ),
            f("topic_id", "Topic ID", "text").hint("Only for a group with topics."),
        ],
    },
    Kind {
        id: "discord",
        label: "Discord",
        icon: "💬",
        logo: "/static/icons/discord.svg",
        arr: "Discord",
        fields: &[f("webhook_url", "Webhook URL", "secret")
            .req()
            .ph("https://discord.com/api/webhooks/…")
            .hint("Channel settings → Integrations → Webhooks → New Webhook → Copy Webhook URL.")],
    },
    Kind {
        id: "slack",
        label: "Slack",
        icon: "#",
        logo: "/static/icons/slack.svg",
        arr: "Slack",
        fields: &[f("webhook_url", "Webhook URL", "secret")
            .req()
            .ph("https://hooks.slack.com/services/…")
            .hint("An incoming webhook of a Slack app (api.slack.com/apps → Incoming Webhooks).")],
    },
    Kind {
        id: "ntfy",
        label: "ntfy",
        icon: "🔔",
        logo: "/static/icons/ntfy.svg",
        arr: "Ntfy",
        fields: &[
            f("server", "Server", "text").req().ph("https://ntfy.sh").default("https://ntfy.sh"),
            f("topic", "Topic", "text")
                .req()
                .ph("my-arrs-x7k2")
                .hint("On ntfy.sh anyone who knows the topic can read it: pick something hard to guess."),
            f("token", "Access token", "secret").ph("tk_…").hint("Only if the topic is protected. Or a user and password:"),
            f("username", "User", "text"),
            f("password", "Password", "secret"),
        ],
    },
    Kind {
        id: "gotify",
        label: "Gotify",
        icon: "📣",
        logo: "/static/icons/gotify.svg",
        arr: "Gotify",
        fields: &[
            f("server", "Server", "text").req().ph("https://gotify.example.com"),
            f("token", "App token", "secret").req().hint("Gotify → Apps → Create application."),
        ],
    },
    Kind {
        id: "pushover",
        label: "Pushover",
        icon: "📱",
        logo: "/static/icons/pushover.svg",
        arr: "Pushover",
        fields: &[
            f("user_key", "User key", "secret").req().hint("On your pushover.net dashboard."),
            f("token", "App token", "secret").req().hint("pushover.net → Create an Application/API Token."),
        ],
    },
    Kind {
        id: "apprise",
        label: "Apprise",
        icon: "🔀",
        logo: "/static/icons/apprise.png",
        arr: "Apprise",
        fields: &[
            f("server", "Apprise API server", "text")
                .req()
                .ph("http://apprise:8000")
                .hint("An Apprise API container (caronc/apprise). It reaches 100+ services."),
            f("key", "Config key", "text")
                .ph("arrs")
                .hint("A configuration saved in Apprise under this key. Or the URLs directly:"),
            f("urls", "Apprise URLs", "secret").ph("tgram://… mailto://…").hint("Space or comma separated."),
        ],
    },
    Kind {
        id: "email",
        label: "Email",
        icon: "✉",
        logo: "/static/icons/email.svg",
        arr: "Email",
        fields: &[
            f("host", "SMTP server", "text").req().ph("smtp.gmail.com"),
            f("port", "Port", "number").req().ph("587").default("587"),
            Field {
                options: &[("starttls", "STARTTLS (587)"), ("ssl", "TLS (465)"), ("none", "none (25, local relay)")],
                ..f("security", "Encryption", "select").req().default("starttls")
            },
            f("username", "User", "text"),
            f("password", "Password", "secret").hint("For Gmail and others: an app password."),
            f("sender", "From", "text").req().ph("wranglarr@example.com"),
            f("to", "To", "text").req().ph("you@example.com").hint("Several: comma separated."),
        ],
    },
    Kind {
        id: "webhook",
        label: "Webhook",
        icon: "🪝",
        logo: "/static/icons/webhook.svg",
        arr: "Webhook",
        fields: &[
            f("url", "URL", "secret")
                .req()
                .ph("https://example.com/hook")
                .hint(r#"wranglarr POSTs JSON: {"event", "level", "title", "message", "at"}."#),
            f("username", "User", "text").hint("Basic auth, if the endpoint needs it."),
            f("password", "Password", "secret"),
        ],
    },
];

pub(super) fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

pub(super) struct Event {
    pub id: &'static str,
    pub label: &'static str,
    pub level: &'static str,
    /// On for new channels.
    pub default: bool,
    /// For several in one round: "3 apps offline".
    pub plural: Option<&'static str>,
}

const fn ev(id: &'static str, label: &'static str, level: &'static str, default: bool) -> Event {
    Event { id, label, level, default, plural: None }
}
const fn evp(id: &'static str, label: &'static str, level: &'static str, plural: &'static str) -> Event {
    Event { id, label, level, default: true, plural: Some(plural) }
}

/// wranglarr's own events.
pub(super) static EVENTS: &[Event] = &[
    ev("backup_failed", "A backup failed or is incomplete", "error", true),
    ev("backup_ok", "A backup finished", "info", false),
    ev("restore", "A restore finished or failed", "warning", true),
    ev("update_available", "Container updates are available", "info", true),
    ev("update_done", "Containers were updated", "info", false),
    ev("update_failed", "A container update failed or was reverted", "error", true),
    ev("self_update_available", "A newer wranglarr is out", "info", true),
    ev("sync_failed", "Connecting apps, shared settings or indexers failed", "warning", true),
    ev("api_key_changed", "An app's API key changed and wranglarr reconnected the apps using it", "info", true),
    evp("app_down", "An app went offline (and came back)", "error", "apps offline"),
    evp("app_health", "An app has a new health warning", "warning", "new health warnings"),
    ev("vpn_down", "The VPN is down (and back up)", "error", true),
    evp("disk_low", "Disk space is low", "warning", "disks low on space"),
    evp("download_problem", "A download is stalled or in error", "warning", "downloads stuck or in error"),
    evp("import_blocked", "An arr can't import a download and needs you", "warning", "downloads an arr can't import"),
    ev("sign_in_lockout", "Someone was locked out of wranglarr after failed sign-ins", "warning", true),
];

pub(super) fn event(id: &str) -> Option<&'static Event> {
    EVENTS.iter().find(|e| e.id == id)
}

/// Events the arrs send themselves when a channel is added to them -> their flag
/// names (see [`arr_flags`]: "import" also sets each arr's own).
pub(super) static ARR_EVENTS: &[(&str, &str, &[&str])] = &[
    ("grab", "Grabbed a release", &["onGrab"]),
    ("import", "Imported", &["onDownload", "onImportComplete"]),
    ("upgrade", "Upgraded", &["onUpgrade"]),
    (
        "manual",
        "Needs manual import / import failed",
        &["onManualInteractionRequired", "onImportFailure", "onDownloadFailure"],
    ),
    ("health", "Health issue (and restored)", &["onHealthIssue", "onHealthRestored"]),
    ("app_update", "App updated itself", &["onApplicationUpdate"]),
];
pub(super) const ARR_EVENTS_DEFAULT: &[&str] = &["import", "manual", "health"];

/// The flags an arr event sets: its shared ones, and for an import every arr's
/// own import flags (a flag the arr at hand doesn't have isn't in its schema).
pub(super) fn arr_flags(event: &str) -> impl Iterator<Item = &'static str> {
    let shared = ARR_EVENTS.iter().filter(move |(id, _, _)| *id == event).flat_map(|(_, _, fl)| fl.iter().copied());
    let own = arr::all().filter(move |_| event == "import").flat_map(|a| a.events.import.iter().copied());
    shared.chain(own)
}

pub(super) fn level_color(level: &str) -> u32 {
    match level {
        "warning" => 0xFFA500,
        "error" => 0xF05050,
        "ok" => 0x27C24C,
        _ => 0x5D9CEC,
    }
}

pub(super) fn level_emoji(level: &str) -> &'static str {
    match level {
        "info" => "ℹ️",
        "warning" => "⚠️",
        "error" => "❌",
        "ok" => "✅",
        _ => "",
    }
}

/// The channel kinds for the page, in catalogue order.
pub(super) fn kinds_json() -> Map<String, Value> {
    KINDS
        .iter()
        .map(|k| {
            let fields: Vec<Value> = k.fields.iter().map(Field::json).collect();
            (
                k.id.to_string(),
                json!({"label": k.label, "icon": k.icon, "logo": k.logo, "arr": k.arr, "fields": fields}),
            )
        })
        .collect()
}

/// The events for the page, in catalogue order.
pub(super) fn events_json() -> Map<String, Value> {
    EVENTS
        .iter()
        .map(|e| {
            let mut v = json!({"label": e.label, "level": e.level, "default": e.default});
            if let Some(p) = e.plural {
                v["plural"] = json!(p);
            }
            (e.id.to_string(), v)
        })
        .collect()
}
