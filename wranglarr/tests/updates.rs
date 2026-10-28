// SPDX-License-Identifier: GPL-3.0-or-later
//! Container updates over HTTP against a mock Docker Engine API (axum on a
//! unix socket in a tempdir): check, update with a dependent in the
//! container's network namespace, manual rollback + hold, a crashing image
//! that is rolled back automatically and blocked, settings, the page; and
//! wranglarr updating itself through its helper, handed over to the next
//! wranglarr, and put back when the new one crashes.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wranglarr::app::App;

// ---- the mock engine ---------------------------------------------------------------

#[derive(Default)]
struct Engine {
    fail_init_inspect: bool,
    fail_init_start: bool,
    /// the parked one-shot can't get its name back
    fail_init_put_back: bool,
    images: HashMap<String, Value>,
    /// ref -> image id
    tags: HashMap<String, String>,
    /// ref -> {"digest", "version", "crash"}: what a pull of ref gets
    registry: HashMap<String, Value>,
    /// name -> container (with "_body", the create body)
    containers: BTreeMap<String, Value>,
    /// exec id -> (container name, Cmd)
    execs: HashMap<String, (String, Value)>,
    n: u64,
}

type Mock = Arc<Mutex<Engine>>;

fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

impl Engine {
    fn add_image(&mut self, reference: &str, digest: &str, version: &str, crash: bool) -> String {
        let id = format!("sha256:{}", hash(&format!("{reference}{digest}")));
        let repo = reference.rsplit_once(':').map_or(reference, |(a, _)| a);
        let mut labels = json!({"org.opencontainers.image.version": version, "maintainer": "x"});
        if crash {
            labels["mock.crash"] = json!("1");
        }
        self.images.insert(
            id.clone(),
            json!({"Id": id, "RepoTags": [reference], "RepoDigests": [format!("{repo}@{digest}")],
                   "Created": "2026-10-01T00:00:00Z",
                   "Config": {"Env": ["PATH=/usr/bin"], "Labels": labels, "Cmd": ["/init"],
                              "Healthcheck": {"Test": ["CMD", "true"]}}}),
        );
        self.tags.insert(reference.into(), id.clone());
        id
    }

    fn resolve_image(&self, r: &str) -> Option<String> {
        if self.images.contains_key(r) {
            return Some(r.into());
        }
        if let Some(id) = self.tags.get(r) {
            return Some(id.clone());
        }
        self.images.keys().find(|i| i.starts_with(r) || i.starts_with(&format!("sha256:{r}"))).cloned()
    }

    fn find(&self, x: &str) -> Option<String> {
        if self.containers.contains_key(x) {
            return Some(x.into());
        }
        self.containers.iter().find(|(_, c)| c["Id"].as_str().is_some_and(|i| i.starts_with(x))).map(|(k, _)| k.clone())
    }

    fn make_container(&mut self, name: &str, body: &Value, iid: &str) -> Value {
        self.n += 1;
        let id = hash(&format!("container{}", self.n));
        let img = &self.images[iid]["Config"];
        let hc = body["HostConfig"].clone();
        let mode = hc["NetworkMode"].as_str().unwrap_or("").to_string();
        let mut env: BTreeMap<String, Value> = BTreeMap::new();
        for e in img["Env"].as_array().into_iter().flatten().chain(body["Env"].as_array().into_iter().flatten()) {
            let s = e.as_str().unwrap_or("");
            env.insert(s.split('=').next().unwrap_or("").to_string(), e.clone());
        }
        let mut labels = img["Labels"].as_object().cloned().unwrap_or_default();
        labels.extend(body["Labels"].as_object().cloned().unwrap_or_default());
        let hostname = match body["Hostname"].as_str() {
            Some(h) if !h.is_empty() => h.to_string(),
            _ if mode.starts_with("container:") => String::new(),
            _ => id[..12].to_string(),
        };
        // like Podman, which lists the HOSTNAME it set among the env
        if !hostname.is_empty() {
            env.insert("HOSTNAME".into(), json!(format!("HOSTNAME={hostname}")));
        }
        let health =
            if body["Healthcheck"].is_null() { img["Healthcheck"].clone() } else { body["Healthcheck"].clone() };
        let mounts: Vec<Value> = hc["Binds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|b| {
                let mut p = b.split(':');
                json!({"Type": "bind", "Source": p.next(), "Destination": p.next(), "RW": true})
            })
            .collect();
        let mut nets = serde_json::Map::new();
        for (net, ep) in body["NetworkingConfig"]["EndpointsConfig"].as_object().into_iter().flatten() {
            let mut aliases = ep["Aliases"].as_array().cloned().unwrap_or_default();
            aliases.push(json!(id[..12]));
            nets.insert(net.clone(), json!({"Aliases": aliases}));
        }
        let mut c = json!({"Id": id, "Name": format!("/{name}"), "Image": iid,
               "Config": {"Image": body["Image"], "Env": env.into_values().collect::<Vec<_>>(), "Labels": labels,
                          "Cmd": if body["Cmd"].is_null() { img["Cmd"].clone() } else { body["Cmd"].clone() },
                          "Hostname": hostname, "Healthcheck": health},
               "HostConfig": hc, "State": {"Running": false, "Restarting": false, "ExitCode": 0},
               "Mounts": mounts, "NetworkSettings": {"Networks": nets}, "_body": body});
        if body["User"].is_string() {
            c["Config"]["User"] = body["User"].clone();
        }
        c
    }

    /// Start `name`. A container labelled `mock.exit` runs to that exit code (a
    /// one-shot); on an image labelled `mock.crash` the others exit with 1,
    /// except the self-update helper, which only uses the image's binary.
    fn start(&mut self, name: &str) {
        let (crash, exit) = {
            let c = &self.containers[name];
            let helper = c["Config"]["Cmd"].as_array().is_some_and(|a| a.contains(&json!("self-update")));
            let exit = c["Config"]["Labels"]["mock.exit"].as_str().and_then(|x| x.parse::<i64>().ok());
            let crash = !self.images[c["Image"].as_str().unwrap_or("")]["Config"]["Labels"]["mock.crash"].is_null();
            (crash && !helper && exit.is_none(), exit)
        };
        let Some(c) = self.containers.get_mut(name) else { return };
        if let Some(code) = exit {
            c["State"] = json!({"Running": false, "Restarting": false, "ExitCode": code});
        } else if crash {
            c["State"] = json!({"Running": false, "Restarting": false, "ExitCode": 1});
        } else {
            let mut st = json!({"Running": true, "Restarting": false, "ExitCode": 0});
            if !c["Config"]["Healthcheck"].is_null() {
                st["Health"] = json!({"Status": "healthy"});
            }
            c["State"] = st;
        }
    }

    fn seed(&mut self, name: &str, body: Value) -> String {
        let iid = self.resolve_image(body["Image"].as_str().unwrap_or("")).expect("seed image");
        let c = self.make_container(name, &body, &iid);
        let id = c["Id"].as_str().unwrap_or("").to_string();
        self.containers.insert(name.into(), c);
        self.start(name);
        id
    }
}

fn reply(code: u16, body: Value) -> Response {
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), axum::Json(body)).into_response()
}

fn empty(code: u16) -> Response {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR).into_response()
}

async fn engine(
    State(m): State<Mock>,
    method: Method,
    uri: Uri,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let mut e = m.lock().expect("mock lock");
    let p = uri.path().to_string();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let nope = |p: &str| reply(404, json!({"message": format!("No such thing: {p}")}));
    match method {
        Method::GET if p == "/_ping" => "OK".into_response(),
        Method::GET if p == "/containers/json" => {
            let list: Vec<Value> = e
                .containers
                .values()
                .map(|c| {
                    let state = if c["State"]["Running"] == true { "running" } else { "exited" };
                    json!({"Id": c["Id"], "Names": [c["Name"]], "Image": c["Config"]["Image"],
                           "Labels": c["Config"]["Labels"], "State": state,
                           "HostConfig": {"NetworkMode": c["HostConfig"]["NetworkMode"]}})
                })
                .collect();
            reply(200, json!(list))
        }
        Method::GET if p.starts_with("/containers/") && p.ends_with("/json") => {
            let Some(k) = e.find(&p["/containers/".len()..p.len() - 5]) else { return nope(&p) };
            if e.fail_init_inspect && k == "wranglarr-init" && e.containers.contains_key("wranglarr-init-wranglarr-old")
            {
                e.fail_init_inspect = false;
                return reply(500, json!({"message":"temporary inspect failure"}));
            }
            let mut c = e.containers[&k].clone();
            if let Some(o) = c.as_object_mut() {
                o.remove("_body");
            }
            reply(200, c)
        }
        Method::GET if p.starts_with("/images/") && p.ends_with("/json") => {
            match e.resolve_image(&p["/images/".len()..p.len() - 5]) {
                Some(i) => reply(200, e.images[&i].clone()),
                None => nope(&p),
            }
        }
        Method::GET if p.starts_with("/distribution/") && p.ends_with("/json") => {
            match e.registry.get(&p["/distribution/".len()..p.len() - 5]) {
                Some(r) => reply(200, json!({"Descriptor": {"digest": r["digest"]}})),
                None => reply(401, json!({"message": "unauthorized"})),
            }
        }
        Method::POST if p == "/images/create" => {
            let reference = format!("{}:{}", q["fromImage"], q["tag"]);
            let Some(r) = e.registry.get(&reference).cloned() else {
                return reply(200, json!({"error": "manifest unknown"}));
            };
            let digest = r["digest"].as_str().unwrap_or("");
            let current = e.tags.get(&reference).map(|i| e.images[i]["RepoDigests"][0].clone());
            if !current.is_some_and(|d| d.as_str().is_some_and(|d| d.ends_with(digest))) {
                e.add_image(&reference, digest, r["version"].as_str().unwrap_or(""), r["crash"] == true);
            }
            reply(200, json!({"status": "Digest: x"}))
        }
        Method::POST if p.starts_with("/images/") && p.ends_with("/tag") => {
            let Some(i) = e.resolve_image(&p["/images/".len()..p.len() - 4]) else { return nope(&p) };
            e.tags.insert(format!("{}:{}", q["repo"], q["tag"]), i);
            empty(201)
        }
        Method::POST if p == "/containers/create" => {
            let name = q["name"].clone();
            if e.containers.contains_key(&name) {
                return reply(
                    409,
                    json!({"message": format!("Conflict. The container name \"/{name}\" is already in use")}),
                );
            }
            let Some(iid) = e.resolve_image(body["Image"].as_str().unwrap_or("")) else { return nope(&p) };
            let c = e.make_container(&name, &body, &iid);
            let id = c["Id"].clone();
            e.containers.insert(name, c);
            reply(201, json!({"Id": id, "Warnings": []}))
        }
        // `wranglarr health` in a container answers when the container runs
        Method::POST if p.starts_with("/exec/") && p.ends_with("/start") => empty(200),
        Method::GET if p.starts_with("/exec/") && p.ends_with("/json") => {
            let Some((name, _)) = e.execs.get(p.split('/').nth(2).unwrap_or("")) else { return nope(&p) };
            let up = e.containers.get(name).is_some_and(|c| c["State"]["Running"] == true);
            reply(200, json!({"Running": false, "ExitCode": if up { 0 } else { 1 }}))
        }
        Method::POST if p.starts_with("/containers/") && p.ends_with("/exec") => {
            let Some(k) = e.find(p.split('/').nth(2).unwrap_or("")) else { return nope(&p) };
            e.n += 1;
            let id = format!("exec{}", e.n);
            e.execs.insert(id.clone(), (k, body["Cmd"].clone()));
            reply(201, json!({"Id": id}))
        }
        Method::POST if p.starts_with("/networks/") && p.ends_with("/connect") => {
            let net = p.split('/').nth(2).unwrap_or("").to_string();
            let Some(k) = e.find(body["Container"].as_str().unwrap_or("")) else { return nope(&p) };
            let aliases = body["EndpointConfig"]["Aliases"].clone();
            if let Some(c) = e.containers.get_mut(&k) {
                c["NetworkSettings"]["Networks"][net] = json!({"Aliases": aliases});
            }
            reply(200, json!({}))
        }
        Method::POST if p.starts_with("/containers/") => {
            let parts: Vec<&str> = p.split('/').collect();
            let Some(k) = e.find(parts.get(2).copied().unwrap_or("")) else { return nope(&p) };
            let running = e.containers[&k]["State"]["Running"] == true;
            match parts.get(3).copied() {
                Some("stop") if !running => empty(304),
                Some("stop") => {
                    if let Some(c) = e.containers.get_mut(&k) {
                        c["State"] = json!({"Running": false, "Restarting": false, "ExitCode": 0});
                    }
                    empty(204)
                }
                Some("start") if running => empty(304),
                Some("start") => {
                    if e.fail_init_start && k == "wranglarr-init" {
                        e.fail_init_start = false;
                        return reply(500, json!({"message":"temporary start failure"}));
                    }
                    e.start(&k);
                    empty(204)
                }
                Some("rename") => {
                    let new = q["name"].clone();
                    if e.fail_init_put_back && new == "wranglarr-init" {
                        return reply(500, json!({"message": "temporary rename failure"}));
                    }
                    if e.containers.contains_key(&new) {
                        return reply(409, json!({"message": "name in use"}));
                    }
                    if let Some(mut c) = e.containers.remove(&k) {
                        c["Name"] = json!(format!("/{new}"));
                        e.containers.insert(new, c);
                    }
                    empty(204)
                }
                _ => nope(&p),
            }
        }
        Method::DELETE if p.starts_with("/containers/") => {
            let Some(k) = e.find(p.split('/').nth(2).unwrap_or("")) else { return nope(&p) };
            e.containers.remove(&k);
            empty(204)
        }
        _ => nope(&p),
    }
}

fn mock_engine(dir: &Path) -> (Mock, String) {
    let mut e = Engine::default();
    e.add_image("acme/web:1", "sha256:d1", "1.0", false);
    e.add_image("acme/side:1", "sha256:s1", "2.0", false);
    e.registry.insert("acme/web:1".into(), json!({"digest": "sha256:d2", "version": "1.1"}));
    e.registry.insert("acme/side:1".into(), json!({"digest": "sha256:s1", "version": "2.0"}));
    let web = e.seed(
        "web",
        json!({"Image": "acme/web:1", "Env": ["MINE=1"],
               "Labels": {"com.docker.compose.service": "web", "com.docker.compose.image": "x"},
               "HostConfig": {"NetworkMode": "arrs", "Binds": ["/srv/web:/data"],
                              "RestartPolicy": {"Name": "unless-stopped"}},
               "NetworkingConfig": {"EndpointsConfig": {"arrs": {"Aliases": ["web"]}}}}),
    );
    e.seed("side", json!({"Image": "acme/side:1", "HostConfig": {"NetworkMode": format!("container:{web}")}}));
    let sock = dir.join("docker.sock");
    let m: Mock = Arc::new(Mutex::new(e));
    let listener = tokio::net::UnixListener::bind(&sock).expect("bind the mock socket");
    let router = Router::new().fallback(engine).with_state(m.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (m, sock.display().to_string())
}

// ---- the test ----------------------------------------------------------------------

fn snapshot(m: &Mock) -> (BTreeMap<String, Value>, HashMap<String, String>) {
    let e = m.lock().expect("mock lock");
    (e.containers.clone(), e.tags.clone())
}

async fn wait_run(app: &App, id: i64) -> wranglarr_core::state::Run {
    for _ in 0..200 {
        if let Some(r) = app.state().get_run(id).await.expect("run")
            && r.status != "queued"
            && r.status != "running"
        {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("run {id} did not finish");
}

fn target<'a>(o: &'a Value, name: &str) -> &'a Value {
    o["targets"].as_array().and_then(|t| t.iter().find(|t| t["container"] == name)).expect("target")
}

#[tokio::test(flavor = "multi_thread")]
async fn check_update_rollback_crash() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (m, sock) = mock_engine(tmp.path());
    let root = tmp.path().display();
    let cfg = wranglarr_services::apps::parse(
        &format!(
            r#"
[storage]
root = "{root}/backups"
state_db = "{root}/wranglarr.db"
[docker]
socket = "{sock}"
[updates]
backup_first = false
health_timeout = 20
extra = ["web", "side", "ghost"]
[[suite]]
name = "media"
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "http://127.0.0.1:1"
api_key = "x"
"#
        ),
        |_| None,
    )
    .expect("config");
    let login_off = wranglarr::auth::Options { enabled: false, ..Default::default() };
    let app =
        App::new(cfg, wranglarr::deployment::Deployment { auth: login_off, ..Default::default() }).await.expect("app");
    app.start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let router = wranglarr::web::router(app.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let c = reqwest::Client::new();
    let get = |p: &str| c.get(format!("{base}{p}")).send();
    let post = |p: &str, b: Value| c.post(format!("{base}{p}")).json(&b).send();

    // before a check
    let o: Value = get("/api/updates").await.expect("get").json().await.expect("json");
    assert_eq!(target(&o, "web")["status"], "unchecked");
    assert!(o["next"]["check"].is_string(), "{}", o["next"]);
    assert!(o["next"]["auto"].is_null());
    assert_eq!(o["docker"], true);

    // check
    let o: Value = post("/api/updates/check", json!(null)).await.expect("check").json().await.expect("json");
    let st: BTreeMap<String, String> = o["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| (t["container"].as_str().unwrap_or("").into(), t["status"].as_str().unwrap_or("").into()))
        .collect();
    assert_eq!(st["web"], "available");
    assert_eq!(st["side"], "current");
    assert_eq!(st["ghost"], "missing");
    assert_eq!(o["available"], 1);
    assert_eq!(target(&o, "web")["version"], "1.0");
    assert_eq!(target(&o, "web")["remote_digest"], "sha256:d2");
    assert_eq!(app.updates_available().await, 1);

    // update: web is recreated on the new image with its settings, side follows its namespace
    let (before, _) = snapshot(&m);
    let r = post("/api/updates/apply", json!({"containers": ["web"]})).await.expect("apply");
    assert_eq!(r.status(), 202);
    let id = r.json::<Value>().await.expect("json")["run"].as_i64().expect("run id");
    let run = wait_run(&app, id).await;
    assert_eq!(run.status, "ok", "{}", run.log);
    assert_eq!(run.result["results"], json!({"web": "updated", "side": "recreated"}), "{}", run.log);
    assert_eq!(run.suite, "system");
    let (after, _) = snapshot(&m);
    let (w0, w1) = (&before["web"], &after["web"]);
    assert_ne!(w1["Id"], w0["Id"]);
    assert_eq!(w1["State"]["Running"], true);
    assert_eq!(w1["Config"]["Labels"]["org.opencontainers.image.version"], "1.1");
    assert_eq!(w1["Config"]["Labels"]["com.docker.compose.image"], w1["Image"]);
    assert!(w1["Config"]["Env"].as_array().is_some_and(|e| e.contains(&json!("MINE=1"))));
    assert_eq!(w1["HostConfig"]["Binds"], w0["HostConfig"]["Binds"]);
    assert_eq!(w1["HostConfig"]["RestartPolicy"], json!({"Name": "unless-stopped"}));
    assert_eq!(w1["NetworkSettings"]["Networks"]["arrs"]["Aliases"][0], "web");
    assert!(w1["_body"].get("Hostname").is_none());
    let sent: Vec<&str> = w1["_body"]["Env"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    assert!(!sent.iter().any(|e| e.starts_with("HOSTNAME=")), "the old HOSTNAME isn't carried over: {sent:?}");
    let new_host = format!("HOSTNAME={}", &w1["Id"].as_str().unwrap_or("")[..12]);
    assert!(w1["Config"]["Env"].as_array().is_some_and(|e| e.contains(&json!(new_host))));
    let new_web = w1["Id"].as_str().unwrap_or("");
    assert_eq!(after["side"]["HostConfig"]["NetworkMode"], format!("container:{new_web}"));
    assert!(after["side"]["_body"].get("NetworkingConfig").is_none());
    assert!(!after.keys().any(|k| k.ends_with("-wranglarr-old")));
    let o: Value = get("/api/updates").await.expect("get").json().await.expect("json");
    let t = target(&o, "web");
    assert_eq!(t["status"], "current");
    assert_eq!(t["can_rollback"], true);
    assert_eq!(t["rollback_to"], "1.0");
    assert_eq!(t["last_update"]["action"], "update");
    // already on the image a rollback would go back to: nothing to offer
    let new_image = m.lock().expect("lock").containers["web"]["Image"].clone();
    m.lock().expect("lock").containers.get_mut("web").expect("web")["Image"] = w0["Image"].clone();
    let o: Value = get("/api/updates").await.expect("get").json().await.expect("json");
    assert_eq!(target(&o, "web")["can_rollback"], false);
    assert_eq!(post("/api/updates/rollback", json!({"container": "web"})).await.expect("rb").status(), 409);
    m.lock().expect("lock").containers.get_mut("web").expect("web")["Image"] = new_image;
    assert_eq!(app.updates_available().await, 0);

    // manual rollback: back on the old image, then held
    let r = post("/api/updates/rollback", json!({"container": "web"})).await.expect("rollback");
    assert_eq!(r.status(), 202);
    let run = wait_run(&app, r.json::<Value>().await.expect("json")["run"].as_i64().expect("id")).await;
    assert_eq!(run.status, "ok", "{}", run.log);
    let (now, tags) = snapshot(&m);
    assert_eq!(now["web"]["Image"], w0["Image"]);
    assert_eq!(now["web"]["Config"]["Labels"]["org.opencontainers.image.version"], "1.0");
    assert_eq!(now["web"]["Config"]["Image"], "acme/web:1");
    assert_eq!(tags["acme/web:1"], w0["Image"].as_str().unwrap_or(""));
    assert_eq!(
        now["side"]["HostConfig"]["NetworkMode"],
        format!("container:{}", now["web"]["Id"].as_str().unwrap_or(""))
    );
    let o: Value = get("/api/updates").await.expect("get").json().await.expect("json");
    let t = target(&o, "web");
    assert_eq!(t["held"], true);
    assert_eq!(t["hold_reason"], "rolled back to 1.0");
    assert_eq!(t["can_rollback"], false);
    assert_eq!(t["status"], "available");
    // held: "update all" has nothing to do
    let r = post("/api/updates/apply", json!({})).await.expect("apply");
    assert_eq!(r.status(), 409);
    assert_eq!(r.json::<Value>().await.expect("json")["error"], "nothing to update (run a check first?)");
    let r = post("/api/updates/rollback", json!({"container": "web"})).await.expect("rollback");
    assert_eq!(r.status(), 409);
    let o: Value = post("/api/updates/hold", json!({"container": "web", "hold": false}))
        .await
        .expect("hold")
        .json()
        .await
        .expect("j");
    assert_eq!(target(&o, "web")["held"], false);

    // a crashing image: the old container comes back, the tag goes back, the digest is blocked
    m.lock()
        .expect("lock")
        .registry
        .insert("acme/web:1".into(), json!({"digest": "sha256:d3", "version": "1.2", "crash": true}));
    post("/api/updates/check", json!(null)).await.expect("check");
    let (good, _) = snapshot(&m);
    let r = post("/api/updates/apply", json!({"containers": ["web"]})).await.expect("apply");
    let run = wait_run(&app, r.json::<Value>().await.expect("json")["run"].as_i64().expect("id")).await;
    assert_eq!(run.status, "failed", "{}", run.log);
    assert_eq!(run.result["results"], json!({"web": "failed"}));
    assert!(run.log.contains("exited with code 1"), "{}", run.log);
    assert!(run.log.contains("web: old container restored"), "{}", run.log);
    let (now, tags) = snapshot(&m);
    assert_eq!(now["web"]["Id"], good["web"]["Id"]);
    assert_eq!(now["web"]["State"]["Running"], true);
    assert!(!now.keys().any(|k| k.ends_with("-wranglarr-old")));
    assert_eq!(tags["acme/web:1"], good["web"]["Image"].as_str().unwrap_or(""));
    let o: Value = get("/api/updates").await.expect("get").json().await.expect("json");
    let t = target(&o, "web");
    assert_eq!(t["status"], "blocked");
    assert!(t["error"].as_str().is_some_and(|e| e.starts_with("1.2 failed to start: exited with code 1")), "{t}");
    assert_eq!(t["blocked"]["digests"], json!(["sha256:d3"]));
    let hist: Vec<(String, String, String)> = o["history"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| {
            let s = |k: &str| h[k].as_str().unwrap_or("").to_string();
            (s("container"), s("action"), s("status"))
        })
        .collect();
    assert_eq!(hist[0], ("web".into(), "update".into(), "failed".into()));
    assert_eq!(hist[1], ("side".into(), "recreate".into(), "ok".into()));
    assert_eq!(hist[2], ("web".into(), "rollback".into(), "ok".into()));

    // not managed
    let r = post("/api/updates/apply", json!({"containers": ["nope"]})).await.expect("apply");
    assert_eq!(r.status(), 404);
    assert_eq!(r.json::<Value>().await.expect("json")["error"], "not a managed container: 'nope'");
    assert_eq!(post("/api/updates/hold", json!({"container": "nope"})).await.expect("hold").status(), 404);
    assert_eq!(post("/api/updates/rollback", json!({"container": "nope"})).await.expect("rb").status(), 404);
    assert_eq!(post("/api/updates/hold", json!({})).await.expect("hold").status(), 422);

    // settings: partial PUTs keep the rest, a bad schedule is a 400
    let put = |b: Value| c.put(format!("{base}/api/updates/settings")).json(&b).send();
    let r = put(json!({"check_schedule": "nope"})).await.expect("put");
    assert_eq!(r.status(), 400);
    assert!(
        r.json::<Value>().await.expect("json")["error"].as_str().is_some_and(|e| e.starts_with("invalid schedule: "))
    );
    let s: Value =
        put(json!({"auto_update": true, "auto_schedule": "0 4 * * 1"})).await.expect("put").json().await.expect("j");
    assert_eq!(s["source"], "ui");
    assert_eq!(s["check_schedule"], "0 */6 * * *");
    assert_eq!(s["backup_first"], false);
    let o: Value = get("/api/updates").await.expect("get").json().await.expect("json");
    assert!(o["next"]["auto"].is_string() && o["next"]["check"].is_string(), "{}", o["next"]);
    let stored = app.state().get_value("updates:settings").await.expect("db").expect("saved");
    assert_eq!(stored["auto_schedule"], "0 4 * * 1");
    put(json!({"enabled": false})).await.expect("put");
    let o: Value = get("/api/updates").await.expect("get").json().await.expect("json");
    assert!(o["next"]["check"].is_null() && o["next"]["auto"].is_null());
    assert_eq!(o["settings"]["auto_update"], true);
    let s: Value = c.delete(format!("{base}/api/updates/settings")).send().await.expect("del").json().await.expect("j");
    assert_eq!(s["source"], "config");
    assert_eq!(s["auto_update"], false);

    // an update cut off mid-swap leaves the old container parked: the row says so
    m.lock().expect("lock").seed("web-wranglarr-old", json!({"Image": "acme/web:1"}));
    let o: Value = get("/api/updates").await.expect("get").json().await.expect("json");
    assert_eq!(target(&o, "web")["interrupted"], "web-wranglarr-old");
    assert!(target(&o, "side")["interrupted"].is_null());

    // the page
    let page = get("/updates").await.expect("page");
    assert_eq!(page.status(), 200);
    let html = page.text().await.expect("text");
    assert!(html.contains("An update was interrupted") && html.contains("<code>web-wranglarr-old</code>"), "{html}");
    assert!(html.contains("update blocked") && html.contains("Try again"), "{html}");
    assert!(html.contains(r#"data-hold="web" data-on="1""#));
    assert!(!html.contains("Docker control is off"));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_docker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().display();
    let cfg = wranglarr_services::apps::parse(
        &format!(
            r#"
[storage]
root = "{root}/backups"
state_db = "{root}/wranglarr.db"
[[suite]]
name = "media"
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "http://127.0.0.1:1"
api_key = "x"
container = "radarr"
"#
        ),
        |_| None,
    )
    .expect("config");
    let login_off = wranglarr::auth::Options { enabled: false, ..Default::default() };
    let app =
        App::new(cfg, wranglarr::deployment::Deployment { auth: login_off, ..Default::default() }).await.expect("app");
    app.start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let router = wranglarr::web::router(app.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let c = reqwest::Client::new();
    let page = c.get(format!("{base}/updates")).send().await.expect("page");
    assert_eq!(page.status(), 200);
    assert!(page.text().await.expect("text").contains("Docker control is off"));
    let r = c.post(format!("{base}/api/updates/check")).send().await.expect("check");
    assert_eq!(r.status(), 409);
    assert!(
        r.json::<Value>().await.expect("json")["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("docker control is off"))
    );
    let r =
        c.post(format!("{base}/api/updates/apply")).json(&json!({"containers": ["radarr"]})).send().await.expect("a");
    assert_eq!(r.status(), 409);
    let o: Value = c.get(format!("{base}/api/updates")).send().await.expect("get").json().await.expect("json");
    assert_eq!(o["docker"], false);
    assert!(o["next"]["check"].is_null(), "no docker: nothing scheduled");
    assert_eq!(target(&o, "radarr")["status"], "unchecked");
    assert_eq!(app.updates_available().await, 0);
}

// ---- wranglarr updating itself -----------------------------------------------------

const WR: &str = "ghcr.io/wranglarrr/wranglarr:latest";
const HELPER: &str = "wranglarr-wranglarr-updater";

/// wranglarr's own container on WR 0.1.0, with the `wranglarr-init` one-shot
/// Compose runs before it; 0.2.0 is in the registry. Its container id.
fn with_wranglarr(m: &Mock, root: &Path, sock: &str) -> String {
    let mut e = m.lock().expect("mock lock");
    e.add_image(WR, "sha256:w1", "0.1.0", false);
    e.registry.insert(WR.into(), json!({"digest": "sha256:w2", "version": "0.2.0"}));
    let compose = |service: &str| json!({"com.docker.compose.project": "arrs", "com.docker.compose.service": service});
    let mut init = compose("wranglarr-init");
    init["mock.exit"] = json!("0");
    e.seed(
        "wranglarr-init",
        json!({"Image": WR, "Cmd": ["prepare"], "Labels": init, "HostConfig": {"NetworkMode": "none"}}),
    );
    let mut labels = compose("wranglarr");
    labels["com.docker.compose.depends_on"] = json!("wranglarr-init:service_completed_successfully:false");
    let state = root.join("state");
    e.seed(
        "wranglarr",
        json!({"Image": WR, "User": "1000:1000", "Labels": labels,
               "HostConfig": {"NetworkMode": "arrs", "GroupAdd": ["969"], "SecurityOpt": ["label=disable"],
                              "Binds": [format!("{0}:{0}", state.display()), format!("{sock}:{sock}"), "/srv/media:/data"],
                              "RestartPolicy": {"Name": "unless-stopped"}},
               "NetworkingConfig": {"EndpointsConfig": {"arrs": {"Aliases": ["wranglarr"]}}}}),
    )
}

fn self_cfg(root: &Path, sock: &str) -> wranglarr_core::config::Config {
    let root = root.display();
    wranglarr_services::apps::parse(
        &format!(
            r#"
[storage]
root = "{root}/backups"
state_db = "{root}/state/wranglarr.db"
[docker]
socket = "{sock}"
[updates]
backup_first = false
health_timeout = 20
[[suite]]
name = "media"
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "http://127.0.0.1:1"
api_key = "x"
"#
        ),
        |_| None,
    )
    .expect("config")
}

fn quick(name: &str) -> Option<String> {
    match name {
        "WRANGLARR_UPDATE_POLL" => Some("0.05".into()),
        "WRANGLARR_UPDATE_STABLE" => Some("0.1".into()),
        _ => None,
    }
}

/// wranglarr in the container `id`, sign-in off.
fn in_container(id: &str) -> wranglarr::deployment::Deployment {
    let host = id[..12].to_string();
    let mut d =
        wranglarr::deployment::Deployment::read(move |k| if k == "HOSTNAME" { Some(host.clone()) } else { quick(k) });
    d.auth = wranglarr::auth::Options { enabled: false, ..Default::default() };
    d
}

async fn started(root: &Path, sock: &str, id: &str) -> (App, String) {
    let app = App::new(self_cfg(root, sock), in_container(id)).await.expect("app");
    app.start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let router = wranglarr::web::router(app.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (app, base)
}

/// The helper container, once wranglarr has started it.
async fn helper_started(m: &Mock) -> Value {
    for _ in 0..200 {
        if let Some(h) = m.lock().expect("mock lock").containers.get(HELPER) {
            return h.clone();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the helper was never started");
}

/// Run the helper as its container would: on the command line it was given,
/// and removed when it exits (AutoRemove).
async fn run_helper(m: &Mock, helper: &Value) -> i32 {
    let cmd: Vec<String> =
        helper["_body"]["Cmd"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    assert_eq!(cmd[..2], ["wranglarr", "self-update"], "{cmd:?}");
    let code = wranglarr::updates::self_update_helper(&cmd[2..], &wranglarr::updates::UpdatesEnv::read(&quick)).await;
    m.lock().expect("mock lock").containers.remove(HELPER);
    code
}

async fn update_now(c: &reqwest::Client, base: &str) -> i64 {
    let r = c.post(format!("{base}/api/updates/self")).send().await.expect("self");
    assert_eq!(r.status(), 202);
    r.json::<Value>().await.expect("json")["run"].as_i64().expect("run id")
}

async fn json_of(c: &reqwest::Client, url: String) -> Value {
    c.get(url).send().await.expect("get").json().await.expect("json")
}

async fn html_of(c: &reqwest::Client, url: String) -> String {
    c.get(url).send().await.expect("get").text().await.expect("text")
}

#[tokio::test(flavor = "multi_thread")]
async fn wranglarr_updates_itself_and_is_put_back_when_the_new_one_crashes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (m, sock) = mock_engine(tmp.path());
    let own = with_wranglarr(&m, tmp.path(), &sock);
    let report = tmp.path().join("state/self-update.json");
    let (app, base) = started(tmp.path(), &sock, &own).await;
    let c = reqwest::Client::new();

    // Update now shows only once a check found a newer wranglarr
    assert!(!html_of(&c, format!("{base}/")).await.contains("data-self-update"));
    assert!(!html_of(&c, format!("{base}/updates")).await.contains("data-self-update"));
    let o: Value = c.post(format!("{base}/api/updates/check")).send().await.expect("check").json().await.expect("j");
    let t = target(&o, "wranglarr");
    assert_eq!((&t["status"], &t["newer"], &t["self_update"]), (&json!("self"), &json!(true), &json!(true)), "{t}");
    assert!(t["self_update_blocker"].is_null(), "{t}");
    assert_eq!(t["error"], "a newer wranglarr is out: press Update now");
    assert_eq!(o["own"]["container"], "wranglarr");
    assert_eq!(o["available"], 0, "never part of Update all");
    let home = html_of(&c, format!("{base}/")).await;
    assert!(home.contains("A newer wranglarr is out") && home.contains("data-self-update"), "{home}");
    let page = html_of(&c, format!("{base}/updates")).await;
    assert!(page.contains("data-self-update") && page.contains("Update now"), "{page}");

    // Update now: a snapshot, then the helper on the new image with only what it needs
    let id = update_now(&c, &base).await;
    let helper = helper_started(&m).await;
    let (_, tags) = snapshot(&m);
    let new_image = tags[WR].clone();
    let b = &helper["_body"];
    assert_eq!(b["Image"], new_image.as_str());
    assert_eq!(b["User"], "1000:1000");
    assert_eq!(b["Labels"]["org.wranglarr.self-update"], id.to_string());
    let hc = &b["HostConfig"];
    assert_eq!(
        (&hc["AutoRemove"], &hc["NetworkMode"], &hc["ReadonlyRootfs"]),
        (&json!(true), &json!("none"), &json!(true))
    );
    assert_eq!((&hc["GroupAdd"], &hc["CapDrop"]), (&json!(["969"]), &json!(["ALL"])));
    assert_eq!(hc["SecurityOpt"], json!(["label=disable", "no-new-privileges:true"]));
    let targets: Vec<&str> =
        hc["Mounts"].as_array().into_iter().flatten().filter_map(|m| m["Target"].as_str()).collect();
    assert_eq!(targets, [sock.clone(), tmp.path().join("state").display().to_string()], "not /data");
    let snaps: Vec<_> = std::fs::read_dir(tmp.path().join("state/self-update")).expect("snapshots").collect();
    assert_eq!(snaps.len(), 1);
    let r = c.post(format!("{base}/api/updates/self")).send().await.expect("again");
    assert_eq!(r.status(), 409);
    assert_eq!(r.json::<Value>().await.expect("json")["error"], "an update of wranglarr is already under way");

    // the helper stops wranglarr, which hands the run over instead of finishing it
    app.shutdown().await.expect("shutdown");
    let run = app.state().get_run(id).await.expect("db").expect("run");
    assert_eq!((run.status.as_str(), run.finished.is_none()), ("running", true), "{}", run.log);
    let (before, _) = snapshot(&m);
    assert_eq!(run_helper(&m, &helper).await, 0);
    let (after, _) = snapshot(&m);
    let w = &after["wranglarr"];
    assert_ne!(w["Id"], before["wranglarr"]["Id"]);
    assert_eq!((&w["Image"], &w["State"]["Running"]), (&json!(new_image), &json!(true)));
    assert_eq!(w["Config"]["User"], "1000:1000");
    assert_eq!(
        w["Config"]["Labels"]["com.docker.compose.depends_on"],
        before["wranglarr"]["Config"]["Labels"]["com.docker.compose.depends_on"]
    );
    assert_eq!(w["HostConfig"]["Binds"], before["wranglarr"]["HostConfig"]["Binds"]);
    assert_eq!(after["wranglarr-init"]["Image"], new_image.as_str(), "the one-shot ran on the new image");
    assert_ne!(after["wranglarr-init"]["Id"], before["wranglarr-init"]["Id"]);
    assert!(!after.keys().any(|k| k.ends_with("-wranglarr-old")), "{:?}", after.keys());
    assert!(report.is_file(), "the report waits for the next wranglarr");

    // the new wranglarr finishes the run
    let new_id = w["Id"].as_str().expect("id").to_string();
    let (app2, base2) = started(tmp.path(), &sock, &new_id).await;
    let run = wait_run(&app2, id).await;
    assert_eq!(run.status, "ok", "{}", run.log);
    assert_eq!(run.result["results"], json!({"wranglarr": "updated", "wranglarr-init": "recreated"}), "{}", run.log);
    assert_eq!(run.result["version"], "0.2.0");
    assert!(
        run.log.contains("the new wranglarr is answering") && run.log.contains("wranglarr updated, now 0.2.0"),
        "{}",
        run.log
    );
    assert!(!report.exists(), "applied, then removed");
    let hist = app2.state().list_updates(Some("wranglarr"), 5).await.expect("history");
    let h = json!(hist[0]);
    assert_eq!(
        (&h["action"], &h["status"], &h["new_version"], &h["run_id"]),
        (&json!("update"), &json!("ok"), &json!("0.2.0"), &json!(id)),
        "{h}"
    );
    let o = json_of(&c, format!("{base2}/api/updates")).await;
    let t = target(&o, "wranglarr");
    assert!(t["newer"].is_null() && t["self_update"] == false, "checked again after the update: {t}");
    assert!(!html_of(&c, format!("{base2}/")).await.contains("data-self-update"));

    // a crashing image: the old wranglarr comes back, the tag goes back, the image is rejected
    m.lock()
        .expect("lock")
        .registry
        .insert(WR.into(), json!({"digest": "sha256:w3", "version": "0.3.0", "crash": true}));
    c.post(format!("{base2}/api/updates/check")).send().await.expect("check");
    let id = update_now(&c, &base2).await;
    let helper = helper_started(&m).await;
    app2.shutdown().await.expect("shutdown");
    let (good, _) = snapshot(&m);
    assert_eq!(run_helper(&m, &helper).await, 1);
    let (now, tags) = snapshot(&m);
    assert_eq!(
        (&now["wranglarr"]["Id"], &now["wranglarr"]["State"]["Running"]),
        (&good["wranglarr"]["Id"], &json!(true))
    );
    assert_eq!(tags[WR], new_image, "the tag points at the image that works");
    assert_eq!(now["wranglarr-init"]["Id"], good["wranglarr-init"]["Id"], "the one-shot from before is kept");
    assert!(!now.keys().any(|k| k.ends_with("-wranglarr-old")), "{:?}", now.keys());
    let (app3, base3) = started(tmp.path(), &sock, &new_id).await;
    let run = wait_run(&app3, id).await;
    assert_eq!(run.status, "failed", "{}", run.log);
    assert_eq!(run.result["results"], json!({"wranglarr": "failed", "wranglarr-init": "kept"}), "{}", run.log);
    assert!(run.result["error"].as_str().is_some_and(|e| e.contains("exited with code 1")), "{}", run.result);
    assert!(run.log.contains("put wranglarr's state database back from before the update"), "{}", run.log);
    let o = json_of(&c, format!("{base3}/api/updates")).await;
    let t = target(&o, "wranglarr");
    assert_eq!(t["blocked"]["digests"], json!(["sha256:w3"]), "{t}");
    assert!(t["blocked"]["reason"].as_str().is_some_and(|r| r.starts_with("0.3.0 failed to start")), "{t}");
    assert_eq!(o["history"][0]["status"], "failed");
    assert!(html_of(&c, format!("{base3}/updates")).await.contains("Try again"));
    app3.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_update_that_goes_wrong_wedges_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (m, sock) = mock_engine(tmp.path());
    let own = with_wranglarr(&m, tmp.path(), &sock);
    let report = tmp.path().join("state/self-update.json");
    let (app, base) = started(tmp.path(), &sock, &own).await;
    let c = reqwest::Client::new();
    c.post(format!("{base}/api/updates/check")).send().await.expect("check");

    // the helper dies before it reports: the run fails instead of waiting
    let id = update_now(&c, &base).await;
    helper_started(&m).await;
    m.lock().expect("lock").containers.remove(HELPER);
    let run = wait_run(&app, id).await;
    assert_eq!(run.status, "failed");
    assert!(run.log.contains(&format!("{HELPER} ended without reporting how the update went")), "{}", run.log);

    // the tag moved after wranglarr pulled it: the helper changes nothing and
    // the wranglarr still running finishes the run from its report
    let id = update_now(&c, &base).await;
    let helper = helper_started(&m).await;
    let (before, _) = snapshot(&m);
    let old_image = before["wranglarr"]["Image"].as_str().expect("image").to_string();
    m.lock().expect("lock").tags.insert(WR.into(), old_image);
    assert_eq!(run_helper(&m, &helper).await, 1);
    let run = wait_run(&app, id).await;
    assert_eq!(run.status, "failed", "{}", run.log);
    assert!(run.result["error"].as_str().is_some_and(|e| e.ends_with("nothing changed")), "{}", run.result);
    let (after, _) = snapshot(&m);
    assert_eq!(after["wranglarr"]["Id"], before["wranglarr"]["Id"]);
    assert!(!report.exists());
    let o = json_of(&c, format!("{base}/api/updates")).await;
    assert!(target(&o, "wranglarr")["blocked"].is_null(), "nothing to reject");

    // handed over, but no report ever came: the next wranglarr doesn't keep the run open
    m.lock().expect("lock").containers.remove(HELPER);
    c.post(format!("{base}/api/updates/check")).send().await.expect("check");
    let id = update_now(&c, &base).await;
    helper_started(&m).await;
    app.shutdown().await.expect("shutdown");
    assert!(!report.exists());
    let (app2, _) = started(tmp.path(), &sock, &own).await;
    let run = app2.state().get_run(id).await.expect("db").expect("run");
    assert!(run.finished.is_some() && run.status != "running", "{} {}", run.status, run.log);
    app2.shutdown().await.expect("shutdown");

    // an unreadable report is removed, and wranglarr starts as usual
    std::fs::write(&report, "{not json").expect("write");
    let (app3, base3) = started(tmp.path(), &sock, &own).await;
    assert!(!report.exists());
    assert_eq!(c.get(format!("{base3}/api/updates")).send().await.expect("get").status(), 200);
    app3.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread")]
async fn update_now_follows_wranglarrs_selected_track() {
    const EDGE: &str = "ghcr.io/wranglarrr/wranglarr:edge";
    let tmp = tempfile::tempdir().expect("tempdir");
    let (m, sock) = mock_engine(tmp.path());
    let own = with_wranglarr(&m, tmp.path(), &sock);
    {
        // :latest is current; only the development track has something newer
        let mut e = m.lock().expect("mock lock");
        e.registry.insert(WR.into(), json!({"digest": "sha256:w1", "version": "0.1.0"}));
        e.registry.insert(EDGE.into(), json!({"digest": "sha256:e1", "version": "0.3.0-dev"}));
    }
    let (_app, base) = started(tmp.path(), &sock, &own).await;
    let c = reqwest::Client::new();
    let o: Value = c.post(format!("{base}/api/updates/check")).send().await.expect("check").json().await.expect("j");
    assert_eq!(target(&o, "wranglarr")["newer"], Value::Null, "nothing newer on latest: {o}");

    let r = c.put(format!("{base}/api/updates/tracks/wranglarr")).json(&json!({"tag": "edge"})).send().await;
    assert_eq!(r.expect("track").status(), 200, "wranglarr's own container takes a track");
    let o: Value = c.post(format!("{base}/api/updates/check")).send().await.expect("check").json().await.expect("j");
    let t = target(&o, "wranglarr");
    assert_eq!((&t["newer"], &t["self_update"], &t["target_image"]), (&json!(true), &json!(true), &json!(EDGE)), "{t}");
    assert!(!snapshot(&m).1.contains_key(EDGE), "a check doesn't pull");

    update_now(&c, &base).await;
    let helper = helper_started(&m).await;
    let (_, tags) = snapshot(&m);
    let edge_image = tags.get(EDGE).expect("Update now pulled the edge image").clone();
    assert_eq!(helper["_body"]["Image"], edge_image.as_str(), "the helper runs the new image");
    let cmd = helper["_body"]["Cmd"].to_string();
    assert!(cmd.contains(EDGE), "the helper recreates wranglarr on the selected track: {cmd}");
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_state_recovery_keeps_the_old_wranglarr_stopped() {
    for failure in ["missing snapshot", "unwritable destination"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (m, sock) = mock_engine(tmp.path());
        let own = with_wranglarr(&m, tmp.path(), &sock);
        m.lock()
            .expect("engine")
            .registry
            .insert(WR.into(), json!({"digest":"sha256:bad-state-recovery", "version":"next", "crash":true}));
        let (app, base) = started(tmp.path(), &sock, &own).await;
        let c = reqwest::Client::new();
        update_now(&c, &base).await;
        let helper = helper_started(&m).await;
        app.shutdown().await.expect("shutdown");
        let command = helper["_body"]["Cmd"].as_array().expect("command");
        let pos = command.iter().position(|v| v == "--snapshot").expect("snapshot arg");
        let saved = Path::new(command[pos + 1].as_str().expect("snapshot path"));
        let recovery_copy = if failure == "missing snapshot" {
            let moved = saved.with_extension("saved");
            std::fs::rename(saved, &moved).expect("simulate missing snapshot");
            moved
        } else {
            std::fs::create_dir(tmp.path().join("state/wranglarr.db.restoring")).expect("block recovery destination");
            saved.to_path_buf()
        };
        assert_eq!(run_helper(&m, &helper).await, 1, "{failure}");
        let (after, _) = snapshot(&m);
        assert!(!after.contains_key("wranglarr"), "must not rename or restart on recovery failure: {failure}");
        let parked = &after["wranglarr-wranglarr-old"];
        assert_eq!(parked["Id"], own, "original container retained: {failure}");
        assert_eq!(parked["State"]["Running"], false, "old version stays stopped: {failure}");
        assert!(recovery_copy.is_file(), "recovery artifact retained: {failure}");
        let report: Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("state/self-update.json")).expect("report"))
                .expect("json");
        assert_eq!(report["phase"], "done");
        assert_eq!(report["ok"], false);
        let error = report["error"].as_str().expect("recovery error");
        assert!(error.contains("rollback failed") && error.contains("state database"), "{report}");
        assert!(!report["lines"].to_string().contains("wranglarr: old container restored"), "{report}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn preparation_failures_restore_the_original_one_shot() {
    for failure in ["inspect", "start"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (m, sock) = mock_engine(tmp.path());
        let own = with_wranglarr(&m, tmp.path(), &sock);
        let (app, base) = started(tmp.path(), &sock, &own).await;
        let c = reqwest::Client::new();
        update_now(&c, &base).await;
        let helper = helper_started(&m).await;
        app.shutdown().await.expect("shutdown");
        let (before, _) = snapshot(&m);
        {
            let mut engine = m.lock().expect("engine");
            engine.fail_init_inspect = failure == "inspect";
            engine.fail_init_start = failure == "start";
        }
        assert_eq!(run_helper(&m, &helper).await, 1);
        let (after, _) = snapshot(&m);
        assert_eq!(
            after["wranglarr-init"]["Id"], before["wranglarr-init"]["Id"],
            "original one-shot recovered after {failure} failure"
        );
        assert_eq!(after["wranglarr"]["Id"], before["wranglarr"]["Id"]);
        assert_eq!(after["wranglarr"]["State"]["Running"], true);
        assert!(!after.keys().any(|k| k.ends_with("-wranglarr-old")), "no half-replaced containers: {failure}");
        let report: Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("state/self-update.json")).expect("report"))
                .expect("json");
        assert_eq!(report["ok"], false);
        assert!(report["results"].as_array().expect("results").iter().any(|r| r == &json!(["wranglarr-init", "kept"])));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_one_shot_that_cant_be_put_back_doesnt_hold_up_wranglarrs_rollback() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (m, sock) = mock_engine(tmp.path());
    let own = with_wranglarr(&m, tmp.path(), &sock);
    let (app, base) = started(tmp.path(), &sock, &own).await;
    let c = reqwest::Client::new();
    update_now(&c, &base).await;
    let helper = helper_started(&m).await;
    app.shutdown().await.expect("shutdown");
    let (before, _) = snapshot(&m);
    {
        let mut engine = m.lock().expect("engine");
        engine.fail_init_start = true;
        engine.fail_init_put_back = true;
    }
    assert_eq!(run_helper(&m, &helper).await, 1);
    let (after, _) = snapshot(&m);
    assert_eq!(after["wranglarr"]["Id"], before["wranglarr"]["Id"], "the old wranglarr is back");
    assert_eq!(after["wranglarr"]["State"]["Running"], true);
    assert!(after.contains_key("wranglarr-init-wranglarr-old"), "the one-shot stays parked: {:?}", after.keys());
    let report: Value =
        serde_json::from_slice(&std::fs::read(tmp.path().join("state/self-update.json")).expect("report"))
            .expect("json");
    assert!(report["results"].as_array().expect("results").iter().any(|r| r == &json!(["wranglarr-init", "failed"])));
    assert!(report["lines"].to_string().contains("could not put the old one back"), "{report}");
}
