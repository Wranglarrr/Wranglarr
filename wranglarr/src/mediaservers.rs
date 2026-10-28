// SPDX-License-Identifier: GPL-3.0-or-later
//! Media servers (Plex, Jellyfin, Emby, Kodi) for the arrs.
//!
//! Defined once here, each one is added to the picked arrs' Settings → Connect, so
//! the arr tells the server to scan the right folder after it imports, upgrades,
//! renames or deletes something. The server must see the files: when its path
//! differs from the arrs' (/data/media/...), a path mapping translates it.
//!
//! Helpers for the token: Plex's sign-in (a PIN the user approves at plex.tv), and
//! for Jellyfin / Emby an API key created with an admin login that isn't kept.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use url::Url;

use crate::app::App;
use crate::arr_notifications;
use crate::config::AppConfig;
use crate::http::{reason, response_bytes};
use crate::service::AppConfigExt;
use crate::service::{Roles, arr};
use crate::value::{head, text, truthy};

use crate::error::{Error, Result};
use crate::ids::token_hex;

pub const KEY: &str = "connections:servers";
pub const KEY_PLEX_ID: &str = "connections:plex_client_id";
pub const MASK: &str = "••••••••";

pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    /// The arr Connect implementation.
    pub arr: &'static str,
    pub port: u16,
    pub secret: &'static str,
    pub secret_label: &'static str,
}

pub static KINDS: &[Kind] = &[
    Kind {
        id: "plex",
        label: "Plex",
        icon: "/static/icons/plex.svg",
        arr: "PlexServer",
        port: 32400,
        secret: "token",
        secret_label: "Plex token",
    },
    Kind {
        id: "jellyfin",
        label: "Jellyfin",
        icon: "/static/icons/jellyfin.svg",
        arr: "MediaBrowser",
        port: 8096,
        secret: "token",
        secret_label: "API key",
    },
    Kind {
        id: "emby",
        label: "Emby",
        icon: "/static/icons/emby.svg",
        arr: "MediaBrowser",
        port: 8096,
        secret: "token",
        secret_label: "API key",
    },
    Kind {
        id: "kodi",
        label: "Kodi",
        icon: "/static/icons/kodi.svg",
        arr: "Xbmc",
        port: 8080,
        secret: "password",
        secret_label: "Password",
    },
];

pub fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

/// Arr events that don't change files a media server should rescan, besides
/// each arr's "added to the library" (see [`skipped`]).
pub const SKIP_FLAGS: &[&str] = &[
    "onGrab",
    "onHealthIssue",
    "onHealthRestored",
    "onApplicationUpdate",
    "onManualInteractionRequired",
    "onImportFailure",
    "onDownloadFailure",
];

/// An arr event a media server needn't hear about (any arr's: a flag the arr
/// at hand doesn't have isn't in its schema).
pub fn skipped(flag: &str) -> bool {
    SKIP_FLAGS.contains(&flag) || arr::all().any(|a| a.events.added == Some(flag))
}
const PLEX_HEADERS: &[(&str, &str)] =
    &[("X-Plex-Product", "wranglarr"), ("X-Plex-Version", "1"), ("Accept", "application/json")];

/// One media server, as stored under `connections:servers`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Server {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub update_library: bool,
    #[serde(default)]
    pub map_from: String,
    #[serde(default)]
    pub map_to: String,
    #[serde(default)]
    pub arrs: Vec<String>,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub password: String,
}

/// A server as the browser sees it: token and password masked.
pub fn public(m: &Server) -> Value {
    let mut v = serde_json::to_value(m).unwrap_or(Value::Null);
    for k in ["token", "password"] {
        if truthy(&v[k]) {
            v[k] = json!(MASK);
        }
    }
    v
}

// ---- addresses -----------------------------------------------------------------------

/// The port written in an address. `Url` leaves out a scheme's default port, so an
/// explicit `:80` on http is read back from the text.
fn written_port(raw: &str, u: &Url) -> Option<u16> {
    u.port().or_else(|| {
        let authority = raw.split_once("://")?.1.split(['/', '?', '#']).next()?;
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (_, port) = host_port.rsplit_once(':')?;
        (!port.contains(']') && !port.is_empty()).then(|| u.port_or_known_default()).flatten()
    })
}

/// The error for a status that isn't a success.
pub fn status_error(status: StatusCode, url: &str) -> Option<String> {
    (!status.is_success()).then(|| format!("{status} from {url}"))
}

// ---- validating and building ------------------------------------------------------------

/// The error for a request body's kind that isn't one wranglarr knows.
pub(crate) fn unknown_kind(kind: &Value) -> String {
    match kind {
        Value::Null => "no kind given".into(),
        k => format!("unknown kind {k}"),
    }
}

/// A server from a request body (and the stored one it replaces), checked.
pub fn clean(body: &Map<String, Value>, old: Option<&Server>, arr_names: &HashSet<String>) -> Result<Server> {
    let get = |k: &str| body.get(k).filter(|v| truthy(v));
    let s = |k: &str| get(k).map(text).unwrap_or_default().trim().to_string();
    let kind_v = get("kind").cloned().or_else(|| old.map(|o| json!(o.kind))).filter(truthy).unwrap_or(Value::Null);
    let k = kind_v.as_str().and_then(kind).ok_or_else(|| Error::invalid(unknown_kind(&kind_v)))?;
    let url = s("url").trim_end_matches('/').to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(Error::invalid("the address starts with http:// or https://, e.g. http://192.168.1.10:32400"));
    }
    match Url::parse(&url) {
        Ok(u) if crate::links::url_host(&u).is_some() => {}
        Ok(_) | Err(url::ParseError::EmptyHost) => return Err(Error::invalid("the address has no host")),
        Err(e) => return Err(Error::invalid(format!("the address isn't one: {e}"))),
    }
    let name = head(&s("name"), 60);
    let mut m = Server {
        id: old.map(|o| o.id.clone()).filter(|i| !i.is_empty()).unwrap_or_else(|| token_hex(4)),
        kind: k.id.into(),
        name: if name.is_empty() { k.label.into() } else { name },
        url,
        username: s("username"),
        update_library: body.get("update_library").is_none_or(truthy),
        map_from: s("map_from"),
        map_to: s("map_to"),
        arrs: get("arrs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|a| arr_names.contains(*a))
            .map(str::to_string)
            .collect(),
        token: String::new(),
        password: String::new(),
    };
    let keep = |v: String, old_v: Option<&String>| if v == MASK { old_v.cloned().unwrap_or_default() } else { v };
    m.token = keep(s("token"), old.map(|o| &o.token));
    m.password = keep(s("password"), old.map(|o| &o.password));
    if m.map_from.is_empty() != m.map_to.is_empty() {
        return Err(Error::invalid("a path mapping needs both sides, or neither"));
    }
    if k.id != "kodi" && m.token.is_empty() {
        return Err(Error::invalid(format!("{}: the {} is needed", k.label, k.secret_label)));
    }
    Ok(m)
}

/// The arr notification body for a server, from the arr's schema.
pub fn arr_body(schema: &[Value], m: &Server) -> std::result::Result<Value, String> {
    let k = kind(&m.kind).ok_or_else(|| format!("unknown kind {:?}", m.kind))?;
    let u = Url::parse(&m.url).map_err(|e| format!("the address {:?} isn't one: {e}", m.url))?;
    let ssl = u.scheme() == "https";
    let port = match written_port(&m.url, &u) {
        Some(p) if p != 0 => p,
        _ if ssl => 443,
        _ => k.port,
    };
    let base = u.path().trim_end_matches('/');
    let mut values = json!({"host": crate::links::url_host(&u), "port": port, "useSsl": ssl, "urlBase": base,
                            "updateLibrary": m.update_library, "mapFrom": m.map_from, "mapTo": m.map_to});
    match m.kind.as_str() {
        "plex" => values["authToken"] = json!(m.token),
        "jellyfin" | "emby" => {
            values["apiKey"] = json!(m.token);
            values["notify"] = json!(false);
        }
        _ => {
            values["username"] = json!(m.username);
            values["password"] = json!(m.password);
            values["notify"] = json!(false);
            values["cleanLibrary"] = json!(false);
            values["alwaysUpdate"] = json!(false);
            values["urlBase"] = json!(if base.is_empty() { "/jsonrpc" } else { base });
        }
    }
    let values = values.as_object().cloned().unwrap_or_default();
    arr_notifications::entry_body(schema, k.arr, &m.name, &values, |f| !skipped(f), None)
}

/// The servers plex.tv lists for an account: name, token and the addresses to try.
pub fn plex_servers(resources: &Value, tok: &str) -> Vec<Value> {
    let mut servers = vec![];
    for d in resources.as_array().into_iter().flatten() {
        if !text(&d["provides"]).contains("server") {
            continue;
        }
        let mut conns: Vec<&Value> = d["connections"].as_array().into_iter().flatten().collect();
        conns.sort_by_key(|c| (!truthy(&c["local"]), truthy(&c["relay"])));
        let mut addrs: Vec<String> = conns
            .iter()
            .filter(|c| truthy(&c["local"]) && truthy(&c["address"]))
            .map(|c| format!("http://{}:{}", text(&c["address"]), text(&c["port"])))
            .collect();
        addrs.extend(conns.iter().filter(|c| truthy(&c["uri"]) && !truthy(&c["relay"])).map(|c| text(&c["uri"])));
        let mut uniq: Vec<String> = vec![];
        for a in addrs {
            if !uniq.contains(&a) {
                uniq.push(a);
            }
        }
        let token = if truthy(&d["accessToken"]) { text(&d["accessToken"]) } else { tok.to_string() };
        servers.push(json!({"name": d.get("name"), "token": token, "addresses": uniq}));
    }
    servers
}

// ---- the service side ----------------------------------------------------------------------

pub struct MediaServers {
    http: reqwest::Client,
    plex_tv: Mutex<(String, String)>,
}

impl Default for MediaServers {
    fn default() -> Self {
        MediaServers::new()
    }
}

#[derive(Serialize)]
pub struct Overview {
    pub kinds: Map<String, Value>,
    pub servers: Vec<Value>,
    pub arrs: Vec<Value>,
    pub mask: &'static str,
}

/// A failed request to a server: the reason without the URL.
/// What a body that isn't JSON reads as (reqwest's own wording, kept).
const NOT_JSON: &str = "error decoding response body";

fn req_err(e: &reqwest::Error) -> String {
    let r = reason(e);
    if r.is_empty() { "the request failed".into() } else { r }
}

impl MediaServers {
    pub fn new() -> MediaServers {
        MediaServers {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            plex_tv: Mutex::new(("https://plex.tv".into(), "https://app.plex.tv".into())),
        }
    }

    /// Point plex.tv (the API, and the app's sign-in page) somewhere else (tests).
    pub fn set_plex_tv(&self, api: &str, app: &str) {
        if let Ok(mut p) = self.plex_tv.lock() {
            *p = (api.trim_end_matches('/').into(), app.trim_end_matches('/').into());
        }
    }

    fn plex_tv(&self) -> (String, String) {
        self.plex_tv.lock().map(|p| p.clone()).unwrap_or_default()
    }

    // ---- storage ---------------------------------------------------------------------------
    pub async fn servers(&self, app: &App) -> Vec<Server> {
        app.state().get::<Vec<Server>>(KEY).await.ok().flatten().unwrap_or_default()
    }

    /// The arrs a server can be added to (not Prowlarr).
    pub fn arr_apps(app: &App) -> Vec<AppConfig> {
        app.cfg().all_apps().filter(|a| a.has(Roles::LIBRARY) && a.enabled).cloned().collect()
    }

    fn arr_names(app: &App) -> HashSet<String> {
        Self::arr_apps(app).into_iter().map(|a| a.name).collect()
    }

    pub async fn overview(&self, app: &App) -> Overview {
        Overview {
            kinds: KINDS
                .iter()
                .map(|k| {
                    (
                        k.id.to_string(),
                        json!({"label": k.label, "icon": k.icon, "arr": k.arr, "port": k.port,
                                   "secret": k.secret, "secret_label": k.secret_label}),
                    )
                })
                .collect(),
            servers: self.servers(app).await.iter().map(public).collect(),
            arrs: Self::arr_apps(app).iter().map(|a| json!({"name": a.name, "kind": a.kind})).collect(),
            mask: MASK,
        }
    }

    pub async fn save(&self, app: &App, body: &Map<String, Value>, sid: Option<&str>) -> Result<(Server, Vec<Value>)> {
        let (body, sid, names) = (body.clone(), sid.map(str::to_owned), Self::arr_names(app));
        let (m, old) = app
            .state()
            .write(move |tx| {
                let all: Vec<Server> = tx.get(KEY)?.unwrap_or_default();
                let old = sid.as_deref().and_then(|id| all.iter().find(|m| m.id == id).cloned());
                if sid.is_some() && old.is_none() {
                    return Err(Error::not_found("no such media server"));
                }
                let m = clean(&body, old.as_ref(), &names)?;
                if all.iter().any(|x| x.name.to_lowercase() == m.name.to_lowercase() && x.id != m.id) {
                    return Err(Error::invalid(format!("there's already one called {:?}", m.name)));
                }
                let mut rest: Vec<Server> = all.into_iter().filter(|x| x.id != m.id).collect();
                rest.push(m.clone());
                tx.put(KEY, &rest)?;
                Ok((m, old))
            })
            .await?;
        let pushed = self.push(app, Some(&m), old.as_ref()).await;
        Ok((m, pushed))
    }

    pub async fn delete(&self, app: &App, sid: &str) -> Result<Vec<Value>> {
        let sid = sid.to_owned();
        let old = app
            .state()
            .write(move |tx| {
                let all: Vec<Server> = tx.get(KEY)?.unwrap_or_default();
                let old = all
                    .iter()
                    .find(|m| m.id == sid)
                    .cloned()
                    .ok_or_else(|| Error::not_found("no such media server"))?;
                tx.put(KEY, &all.into_iter().filter(|x| x.id != sid).collect::<Vec<_>>())?;
                Ok(old)
            })
            .await?;
        Ok(self.push(app, None, Some(&old)).await)
    }

    // ---- into the arrs -----------------------------------------------------------------------
    pub async fn push(&self, app: &App, new: Option<&Server>, old: Option<&Server>) -> Vec<Value> {
        let secrets: Vec<String> = [new.map(|n| &n.token), new.map(|n| &n.password), old.map(|o| &o.token)]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();
        let scrub = |t: &str| {
            let mut t = t.to_string();
            for v in &secrets {
                t = t.replace(v.as_str(), "…");
            }
            head(&t, 300)
        };
        let want: HashSet<String> = new.map(|n| n.arrs.iter().cloned().collect()).unwrap_or_default();
        let had: HashSet<String> = old.map(|o| o.arrs.iter().cloned().collect()).unwrap_or_default();
        arr_notifications::sync(
            &Self::arr_apps(app),
            new.map(|n| n.name.as_str()),
            old.map(|o| o.name.as_str()),
            &want,
            &had,
            |schema| match new {
                Some(n) => arr_body(schema, n),
                None => Err("nothing to add".into()),
            },
            scrub,
        )
        .await
    }

    // ---- talking to the servers ----------------------------------------------------------------
    /// Try a server's address and token: its version and libraries.
    pub async fn test(&self, app: &App, body: &Map<String, Value>, sid: Option<&str>) -> Value {
        let old = match sid {
            Some(id) => self.servers(app).await.into_iter().find(|m| m.id == id),
            None => None,
        };
        let mut b = body.clone();
        b.insert("arrs".into(), json!([]));
        let m = match clean(&b, old.as_ref(), &HashSet::new()) {
            Ok(m) => m,
            Err(e) => return json!({"ok": false, "error": e.message()}),
        };
        let r = match m.kind.as_str() {
            "plex" => self.test_plex(&m).await,
            "kodi" => self.test_kodi(&m).await,
            _ => self.test_jellyfin(&m).await,
        };
        match r {
            Ok(Value::Object(info)) => {
                let mut out = Map::new();
                out.insert("ok".into(), json!(true));
                out.extend(info);
                Value::Object(out)
            }
            Ok(_) => json!({"ok": true}),
            Err(e) => {
                let mut t = e;
                for v in [&m.token, &m.password] {
                    if !v.is_empty() {
                        t = t.replace(v.as_str(), "…");
                    }
                }
                let t = head(&t, 300);
                json!({"ok": false, "error": if t.is_empty() { "the request failed".to_string() } else { t }})
            }
        }
    }

    async fn get_json(&self, rb: reqwest::RequestBuilder) -> std::result::Result<(StatusCode, String, Value), String> {
        let r = rb.timeout(Duration::from_secs(10)).send().await.map_err(|e| req_err(&e))?;
        let status = r.status();
        let url = r.url().to_string();
        let body = response_bytes(r).await.map_err(|e| e.describe(req_err))?;
        let j = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
        Ok((status, url, j))
    }

    async fn test_plex(&self, m: &Server) -> std::result::Result<Value, String> {
        let mut rb = self.http.get(format!("{}/library/sections", m.url)).header("X-Plex-Token", &m.token);
        for (k, v) in PLEX_HEADERS {
            rb = rb.header(*k, *v);
        }
        let (status, url, j) = self.get_json(rb).await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err("Plex refused the token".into());
        }
        if let Some(e) = status_error(status, &url) {
            return Err(e);
        }
        if !j.is_object() {
            return Err("Plex's answer isn't JSON".into());
        }
        let libs: Vec<Value> = j["MediaContainer"]["Directory"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| d.get("title").cloned().unwrap_or(Value::Null))
            .collect();
        let mut rb = self.http.get(format!("{}/identity", m.url));
        for (k, v) in PLEX_HEADERS {
            rb = rb.header(*k, *v);
        }
        let (_, _, ident) = self.get_json(rb).await?;
        Ok(json!({"version": ident["MediaContainer"].get("version"), "libraries": libs}))
    }

    async fn test_jellyfin(&self, m: &Server) -> std::result::Result<Value, String> {
        let rb = self.http.get(format!("{}/System/Info", m.url)).header("X-Emby-Token", &m.token);
        let (status, url, j) = self.get_json(rb).await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err("the server refused the API key".into());
        }
        if let Some(e) = status_error(status, &url) {
            return Err(e);
        }
        if !j.is_object() {
            return Err("the server's answer isn't JSON".into());
        }
        let rb = self.http.get(format!("{}/Library/VirtualFolders", m.url)).header("X-Emby-Token", &m.token);
        let (st, _, libs) = self.get_json(rb).await?;
        let names: Vec<Value> = if st == StatusCode::OK {
            libs.as_array().into_iter().flatten().map(|x| x.get("Name").cloned().unwrap_or(Value::Null)).collect()
        } else {
            vec![]
        };
        Ok(json!({"version": j.get("Version"), "name": j.get("ServerName"), "product": j.get("ProductName"),
                  "libraries": names}))
    }

    async fn test_kodi(&self, m: &Server) -> std::result::Result<Value, String> {
        let has_path = Url::parse(&m.url).is_ok_and(|u| u.path() != "/");
        let url = format!("{}{}", m.url, if has_path { "" } else { "/jsonrpc" });
        let mut rb = self.http.post(&url).json(&json!({"jsonrpc": "2.0", "method": "JSONRPC.Version", "id": 1}));
        if !m.username.is_empty() {
            rb = rb.basic_auth(&m.username, Some(&m.password));
        }
        let (status, url, j) = self.get_json(rb).await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err("Kodi refused the user / password".into());
        }
        if let Some(e) = status_error(status, &url) {
            return Err(e);
        }
        let v = &j["result"]["version"];
        Ok(json!({"version": format!("JSON-RPC {}.{}", text(&v["major"]), text(&v["minor"]))}))
    }

    // ---- getting a token ----------------------------------------------------------------------
    /// wranglarr's Plex client id, made the first time it's needed.
    async fn plex_client_id(&self, app: &App) -> Result<String> {
        let made = format!("wranglarr-{}", token_hex(8));
        app.state()
            .write(move |tx| {
                if let Some(cid) = tx.get::<String>(KEY_PLEX_ID)?.filter(|c| !c.is_empty()) {
                    return Ok(cid);
                }
                tx.put(KEY_PLEX_ID, &made)?;
                Ok(made)
            })
            .await
    }

    fn plex_req(&self, rb: reqwest::RequestBuilder, cid: &str) -> reqwest::RequestBuilder {
        let mut rb = rb.header("X-Plex-Client-Identifier", cid).timeout(Duration::from_secs(15));
        for (k, v) in PLEX_HEADERS {
            rb = rb.header(*k, *v);
        }
        rb
    }

    /// One plex.tv request: (status, url, JSON); a failure is a 502 "plex.tv: …".
    async fn plex_call(&self, rb: reqwest::RequestBuilder) -> Result<(StatusCode, String, Value)> {
        let up = |e: String| Error::upstream(format!("plex.tv: {e}"));
        let r = rb.send().await.map_err(|e| up(req_err(&e)))?;
        let status = r.status();
        let url = r.url().to_string();
        let body = response_bytes(r).await.map_err(|e| up(e.describe(req_err)))?;
        Ok((status, url, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }

    /// Start Plex's sign-in: the user opens auth_url and approves; poll plex_check.
    pub async fn plex_pin(&self, app: &App) -> Result<Value> {
        let cid = self.plex_client_id(app).await?;
        let (api, web) = self.plex_tv();
        let rb = self.plex_req(self.http.post(format!("{api}/api/v2/pins")).query(&[("strong", "true")]), &cid);
        let (status, url, j) = self.plex_call(rb).await?;
        if let Some(e) = status_error(status, &url) {
            return Err(Error::upstream(format!("plex.tv: {e}")));
        }
        let (Some(id), Some(code)) = (j.get("id").filter(|v| !v.is_null()), j.get("code").filter(|v| !v.is_null()))
        else {
            return Err(Error::internal("plex.tv: no PIN in the answer"));
        };
        let auth_url =
            format!("{web}/auth#?clientID={cid}&code={}&context%5Bdevice%5D%5Bproduct%5D=wranglarr", text(code));
        Ok(json!({"id": id, "auth_url": auth_url}))
    }

    /// Whether the user approved the PIN: the token, and the servers it reaches.
    pub async fn plex_check(&self, app: &App, pin_id: i64) -> Result<Value> {
        let cid = self.plex_client_id(app).await?;
        let (api, _) = self.plex_tv();
        let rb = self.plex_req(self.http.get(format!("{api}/api/v2/pins/{pin_id}")), &cid);
        let (status, url, j) = self.plex_call(rb).await?;
        if let Some(e) = status_error(status, &url) {
            return Err(Error::upstream(format!("plex.tv: {e}")));
        }
        let tok = j.get("authToken").filter(|t| truthy(t)).map(text);
        let Some(tok) = tok else { return Ok(json!({"done": false})) };
        // the servers this account can reach, to offer an address
        let rb = self
            .plex_req(self.http.get(format!("{api}/api/v2/resources")).query(&[("includeHttps", "1")]), &cid)
            .header("X-Plex-Token", &tok);
        let (status, _, res) = self.plex_call(rb).await?;
        let servers = if status == StatusCode::OK { plex_servers(&res, &tok) } else { vec![] };
        Ok(json!({"done": true, "token": tok, "servers": servers}))
    }

    /// Sign in as an admin, create an API key named wranglarr, sign out. The login isn't kept.
    /// Errors: 400 for what the user can fix, 502 "the server: …" for the rest.
    pub async fn jellyfin_key(&self, url: &str, username: &str, password: &str) -> Result<Value> {
        let url = url.trim().trim_end_matches('/').to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(Error::invalid("the address starts with http:// or https://"));
        }
        let up = |e: String| Error::upstream(format!("the server: {e}"));
        let dev = r#"MediaBrowser Client="wranglarr", Device="wranglarr", DeviceId="wranglarr-setup", Version="1""#;
        let r = self
            .http
            .post(format!("{url}/Users/AuthenticateByName"))
            .header("Authorization", dev)
            .json(&json!({"Username": username, "Pw": password}))
            .send()
            .await
            .map_err(|e| Error::invalid(format!("can't reach {url}: {}", req_err(&e))))?;
        if r.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::invalid("wrong user or password"));
        }
        if let Some(e) = status_error(r.status(), r.url().as_str()) {
            return Err(up(e));
        }
        let body = response_bytes(r).await.map_err(|e| up(e.to_string()))?;
        let j: Value = serde_json::from_slice(&body).map_err(|_| up(NOT_JSON.into()))?;
        let tok = match j.get("AccessToken") {
            Some(t) => text(t),
            None => return Err(up("no access token in the answer".into())),
        };
        if !truthy(&j["User"]["Policy"]["IsAdministrator"]) {
            return Err(Error::invalid("that user isn't an administrator: API keys need one"));
        }
        let auth = format!(r#"{dev}, Token="{tok}""#);
        let with = |rb: reqwest::RequestBuilder| rb.header("Authorization", &auth).header("X-Emby-Token", &tok);
        let made: std::result::Result<Option<String>, String> = async {
            with(
                self.http
                    .post(format!("{url}/Auth/Keys"))
                    .query(&[("app", "wranglarr-arrs"), ("App", "wranglarr-arrs")]),
            )
            .send()
            .await
            .map_err(|e| req_err(&e))?;
            let r = with(self.http.get(format!("{url}/Auth/Keys"))).send().await.map_err(|e| req_err(&e))?;
            let body = response_bytes(r).await.map_err(|e| e.to_string())?;
            let keys: Value = serde_json::from_slice(&body).map_err(|_| NOT_JSON.to_string())?;
            Ok(keys["Items"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|k| k["AppName"] == "wranglarr-arrs")
                .and_then(|k| k.get("AccessToken"))
                .filter(|t| !t.is_null())
                .map(text))
        }
        .await;
        let out = with(self.http.post(format!("{url}/Sessions/Logout"))).send().await;
        let key = made.map_err(up)?;
        out.map_err(|e| up(req_err(&e)))?;
        match key.filter(|k| !k.is_empty()) {
            Some(k) => Ok(json!({"token": k})),
            None => {
                Err(Error::invalid("the server didn't return the new API key: create one under Dashboard → API Keys"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_what_needs_no_rescan() {
        for f in ["onGrab", "onHealthIssue", "onDownloadFailure", "onMovieAdded", "onSeriesAdd", "onArtistAdd"] {
            assert!(skipped(f), "{f}");
        }
        assert!(skipped("onAuthorAdded"));
        for f in ["onDownload", "onUpgrade", "onRename", "onReleaseImport", "onMovieDelete"] {
            assert!(!skipped(f), "{f}");
        }
    }

    fn names(n: &[&str]) -> HashSet<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    fn body(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn reads_ports() {
        let port = |raw: &str| written_port(raw, &Url::parse(raw).expect("url"));
        assert_eq!(port("https://User@Plex.Local:32400/base/x?q=1#f"), Some(32400));
        assert_eq!(port("http://[::1]:8096"), Some(8096));
        assert_eq!(port("http://h:80"), Some(80), "written, though it's http's default");
        assert_eq!(port("http://[::1]"), None);
        assert_eq!(port("http://h"), None);
        assert_eq!(port("http://u:p@h"), None);
    }

    #[test]
    fn cleaning() {
        let arrs = names(&["radarr", "sonarr"]);
        let e = |v: Value| clean(&body(v), None, &arrs).expect_err("bad").to_string();
        assert_eq!(e(json!({"kind": "vlc"})), r#"unknown kind "vlc""#);
        assert_eq!(e(json!({})), "no kind given");
        assert_eq!(
            e(json!({"kind": "plex", "url": "plex:32400"})),
            "the address starts with http:// or https://, e.g. http://192.168.1.10:32400"
        );
        assert_eq!(e(json!({"kind": "plex", "url": "http://:80"})), "the address has no host");
        assert_eq!(
            e(json!({"kind": "plex", "url": "http://p", "token": "t", "map_from": "/a"})),
            "a path mapping needs both sides, or neither"
        );
        assert_eq!(e(json!({"kind": "jellyfin", "url": "http://j"})), "Jellyfin: the API key is needed");
        let m = clean(
            &body(json!({"kind": "kodi", "url": "http://kodi:8080/ ", "arrs": ["radarr", "nope"], "password": "pw"})),
            None,
            &arrs,
        )
        .expect("kodi needs no token");
        assert_eq!(m.url, "http://kodi:8080");
        assert_eq!(m.name, "Kodi");
        assert_eq!(m.arrs, vec!["radarr"]);
        assert!(m.update_library);
        assert_eq!(m.id.len(), 8);
        let again = clean(
            &body(json!({"kind": "kodi", "url": "http://k", "password": MASK, "name": "  K  "})),
            Some(&m),
            &arrs,
        )
        .expect("kept");
        assert_eq!((again.password.as_str(), again.id.as_str(), again.name.as_str()), ("pw", m.id.as_str(), "K"));
        let p = public(&again);
        assert_eq!(p["password"], MASK);
        assert_eq!(p["token"], "");
    }

    fn schema(implementation: &str) -> Vec<Value> {
        vec![json!({"implementation": implementation, "onGrab": false, "onDownload": false, "onUpgrade": false,
                    "onRename": false, "supportsOnRename": true, "onHealthIssue": false,
                    "fields": [{"name": "host"}, {"name": "port"}, {"name": "useSsl"}, {"name": "urlBase"},
                               {"name": "authToken"}, {"name": "apiKey"}, {"name": "updateLibrary"},
                               {"name": "mapFrom"}, {"name": "mapTo"}, {"name": "username"}, {"name": "password"}]})]
    }

    fn field<'a>(b: &'a Value, n: &str) -> &'a Value {
        b["fields"].as_array().and_then(|f| f.iter().find(|x| x["name"] == n)).map_or(&Value::Null, |f| &f["value"])
    }

    #[test]
    fn arr_bodies() {
        let m = Server {
            kind: "plex".into(),
            name: "Plex".into(),
            url: "https://plex.example.com".into(),
            token: "tok".into(),
            update_library: true,
            ..Default::default()
        };
        let b = arr_body(&schema("PlexServer"), &m).expect("plex");
        assert_eq!(b["name"], "wranglarr · Plex");
        assert_eq!(field(&b, "host"), "plex.example.com");
        assert_eq!(field(&b, "port"), 443);
        assert_eq!(field(&b, "useSsl"), true);
        assert_eq!(field(&b, "authToken"), "tok");
        assert_eq!(field(&b, "urlBase"), "");
        assert_eq!(
            (b["onGrab"].clone(), b["onDownload"].clone(), b["onRename"].clone()),
            (json!(false), json!(true), json!(true))
        );
        assert_eq!(b["onHealthIssue"], false);
        let m = Server {
            kind: "jellyfin".into(),
            name: "J".into(),
            url: "http://j".into(),
            token: "k".into(),
            ..Default::default()
        };
        let b = arr_body(&schema("MediaBrowser"), &m).expect("jellyfin");
        assert_eq!(field(&b, "port"), 8096);
        assert_eq!(field(&b, "apiKey"), "k");
        let m = Server {
            kind: "kodi".into(),
            name: "K".into(),
            url: "http://k:9090".into(),
            username: "u".into(),
            ..Default::default()
        };
        let b = arr_body(&schema("Xbmc"), &m).expect("kodi");
        assert_eq!(field(&b, "urlBase"), "/jsonrpc");
        assert_eq!(field(&b, "port"), 9090);
        assert_eq!(field(&b, "username"), "u");
        assert_eq!(
            arr_body(&schema("Xbmc"), &Server { kind: "plex".into(), url: "http://p".into(), ..Default::default() })
                .expect_err("no plex"),
            "this app has no PlexServer connection"
        );
    }

    #[test]
    fn plex_resources() {
        let res = json!([
            {"name": "Home", "provides": "server", "accessToken": "srv", "connections": [
                {"uri": "https://relay.plex.direct:8443", "relay": true, "local": false},
                {"uri": "https://1-2-3-4.plex.direct:32400", "relay": false, "local": false},
                {"address": "192.168.1.10", "port": 32400, "uri": "https://192-168-1-10.plex.direct:32400", "local": true, "relay": false}]},
            {"name": "Phone", "provides": "player"}
        ]);
        let s = plex_servers(&res, "acct");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0]["token"], "srv");
        assert_eq!(
            s[0]["addresses"],
            json!([
                "http://192.168.1.10:32400",
                "https://192-168-1-10.plex.direct:32400",
                "https://1-2-3-4.plex.direct:32400"
            ])
        );
    }

    #[test]
    fn status_messages() {
        assert_eq!(status_error(StatusCode::OK, "u"), None);
        assert_eq!(status_error(StatusCode::NOT_FOUND, "http://x/a").as_deref(), Some("404 Not Found from http://x/a"));
    }
}
