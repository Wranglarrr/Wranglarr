// SPDX-License-Identifier: GPL-3.0-or-later
//! `wranglarr self-update`: the short-lived helper that replaces wranglarr's
//! own container. wranglarr starts it from the new image, with the Docker socket
//! and its state folder and nothing else (no network, no capabilities). The
//! helper then works on its own:
//!
//! 1. stops wranglarr and parks it under `<name>-wranglarr-old` (the shared
//!    [`swap`](super::super::recreate::swap)), which makes wranglarr hand its run over;
//! 2. runs the one-shots wranglarr waits for in Compose (`wranglarr-init`) on the
//!    new image;
//! 3. creates and starts the new wranglarr and waits until `wranglarr health`
//!    answers inside it;
//! 4. on a failure, removes the new container, puts the state database back
//!    from the snapshot taken before, starts the old wranglarr again, points the
//!    tag back at the old image and reports the image as rejected.
//!
//! Throughout, it writes the [report](super::report) the next wranglarr finishes
//! the run from. `wranglarr health` is here too: the probe it runs in the new one.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use clap::Parser;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use super::super::check::repo_digests;
use super::super::recreate::{Health, Log, SwapFailed, Swapped, Swapper, build_create_body, retag, swap, wait_running};
use super::super::{OLD_SUFFIX, UpdatesEnv, short};
use super::report::{Rejected, Report, UpdateRecord};
use crate::docker::{Docker, DockerError, image_reference};
use crate::error::{Error, Result};

const PROG: &str = "wranglarr self-update";

/// How long a one-shot (`wranglarr-init`) gets to finish.
const ONE_SHOT_TIMEOUT: Duration = Duration::from_secs(120);

/// Replace wranglarr's own container with one on a new image (started by
/// wranglarr itself; not meant to be run by hand).
#[derive(Parser, Debug, Clone, PartialEq)]
pub(crate) struct Args {
    /// wranglarr's container
    #[arg(long)]
    pub container: String,
    /// The image reference it runs, which now points at the new image
    #[arg(long)]
    pub image: String,
    /// The new image's id, as wranglarr pulled it
    #[arg(long)]
    pub image_id: String,
    /// The run to report on
    #[arg(long)]
    pub run: i64,
    /// wranglarr's state database (the same path inside both containers)
    #[arg(long)]
    pub state_db: PathBuf,
    /// The copy of the state database taken before the update
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
    /// Where wranglarr answers inside its container
    #[arg(long, default_value = "127.0.0.1:8787")]
    pub health: String,
    /// Seconds the new wranglarr gets to come up
    #[arg(long, default_value_t = 300)]
    pub timeout: u64,
    /// The Docker socket
    #[arg(long)]
    pub socket: String,
}

impl Args {
    /// The command line wranglarr starts the helper with.
    pub(crate) fn to_command(&self) -> Vec<String> {
        let mut c = vec![
            "wranglarr".to_string(),
            "self-update".into(),
            "--container".into(),
            self.container.clone(),
            "--image".into(),
            self.image.clone(),
            "--image-id".into(),
            self.image_id.clone(),
            "--run".into(),
            self.run.to_string(),
            "--state-db".into(),
            self.state_db.display().to_string(),
        ];
        if let Some(s) = &self.snapshot {
            c.extend(["--snapshot".into(), s.display().to_string()]);
        }
        c.extend([
            "--health".into(),
            self.health.clone(),
            "--timeout".into(),
            self.timeout.to_string(),
            "--socket".into(),
            self.socket.clone(),
        ]);
        c
    }

    /// The name the helper's container gets.
    pub(crate) fn helper_name(container: &str) -> String {
        format!("{container}-wranglarr-updater")
    }
}

// ---- the helper's container ------------------------------------------------------

/// The mount of `own` (`docker inspect`) that holds `path` inside it: the one
/// with the longest destination that is `path` or a folder above it, as a
/// /containers/create `Mounts` entry.
fn mount_for(own: &Value, path: &Path) -> Option<Value> {
    own["Mounts"]
        .as_array()?
        .iter()
        .filter(|m| matches!(m["Type"].as_str(), Some("bind" | "volume")))
        .filter_map(|m| Some((m, Path::new(m["Destination"].as_str()?))))
        .filter(|(_, dest)| path.starts_with(dest))
        .max_by_key(|(_, dest)| dest.as_os_str().len())
        .map(|(m, dest)| {
            let volume = m["Type"] == "volume";
            let source = if volume { &m["Name"] } else { &m["Source"] };
            json!({"Type": m["Type"], "Source": source, "Target": dest.display().to_string(),
                   "ReadOnly": !m["RW"].as_bool().unwrap_or(true)})
        })
}

/// Why wranglarr's container `own` can't hand itself to a helper: the Docker
/// socket at `socket` and the state folder `state_dir` must both be mounts the
/// helper can be given too. None: it can.
pub(crate) fn mounts_missing(own: &Value, socket: &str, state_dir: &Path) -> Option<String> {
    if mount_for(own, Path::new(socket)).is_none() {
        return Some(format!("the Docker socket {socket:?} isn't mounted into wranglarr's container"));
    }
    if mount_for(own, state_dir).is_none() {
        return Some(format!("wranglarr's state folder {:?} isn't a mount of its container", state_dir.display()));
    }
    None
}

/// The /containers/create body for the helper: the new image, the command
/// `args`, wranglarr's user, groups and security options, the socket and the
/// state folder, no network and no capabilities; removed when it exits.
pub(crate) fn container_body(own: &Value, args: &Args, env: &[String]) -> Result<Value> {
    let state_dir = args.state_db.parent().unwrap_or(Path::new("/"));
    if let Some(why) = mounts_missing(own, &args.socket, state_dir) {
        return Err(Error::conflict(why));
    }
    let mut mounts = vec![];
    for p in [Path::new(&args.socket), state_dir] {
        if let Some(m) = mount_for(own, p)
            && !mounts.contains(&m)
        {
            mounts.push(m);
        }
    }
    let hc = &own["HostConfig"];
    let mut security: Vec<Value> = hc["SecurityOpt"].as_array().cloned().unwrap_or_default();
    if !security.iter().any(|s| s.as_str().is_some_and(|s| s.starts_with("no-new-privileges"))) {
        security.push(json!("no-new-privileges:true"));
    }
    let mut labels = Map::new();
    labels.insert("org.wranglarr.self-update".into(), json!(args.run.to_string()));
    Ok(json!({
        "Image": args.image_id,
        "Cmd": args.to_command(),
        "User": own["Config"]["User"],
        "Env": env,
        "Labels": labels,
        "HostConfig": {
            "AutoRemove": true,
            "NetworkMode": "none",
            "Mounts": mounts,
            "GroupAdd": hc["GroupAdd"],
            "CapDrop": ["ALL"],
            "SecurityOpt": security,
            "ReadonlyRootfs": true,
            "Tmpfs": {"/tmp": ""},
        },
    }))
}

// ---- the helper's run ----------------------------------------------------------------

/// A one-shot that ran on the new image; the old one waits, parked, until the
/// update is settled.
#[derive(Debug, Clone)]
struct OneShot {
    name: String,
    parked: String,
    new_id: Option<String>,
}

/// The swap as the helper runs it: lines go into the report, the new wranglarr
/// is up once `wranglarr health` answers in it, the one-shots run before it is
/// created, and a failure restores the state database before the old one starts.
struct Helper<'a> {
    args: &'a Args,
    report_path: PathBuf,
    report: Mutex<Report>,
    health: Health,
    one_shots: Vec<String>,
    prepared: Mutex<Vec<OneShot>>,
    /// The new wranglarr was started, so it may have written to the state database.
    started: AtomicBool,
}

impl Helper<'_> {
    fn note(&self, msg: String) {
        eprintln!("{PROG}: {msg}");
        let Ok(mut r) = self.report.lock() else { return };
        r.line(Utc::now(), msg);
        if let Err(e) = r.save(&self.report_path) {
            eprintln!("{PROG}: could not write the report {}: {e}", self.report_path.display());
        }
    }

    fn with_report(&self, f: impl FnOnce(&mut Report)) {
        if let Ok(mut r) = self.report.lock() {
            f(&mut r);
        }
    }

    /// Run the one-shot `name` on the new image, keeping the old one parked.
    async fn prepare(&self, d: &Docker, name: &str) -> Result<()> {
        let old = d.inspect(name).await?;
        let old_img = d.image(old["Image"].as_str().unwrap_or("")).await.ok();
        let (body, _) = build_create_body(&old, old_img.as_ref(), &self.args.image, &self.args.image_id, None);
        let parked = format!("{name}{OLD_SUFFIX}");
        match d.inspect(&parked).await {
            Err(DockerError::NotFound(_)) => {}
            Ok(_) => return Err(Error::conflict(format!("{parked} exists from an earlier interrupted update"))),
            Err(e) => return Err(e.into()),
        }
        if old["State"]["Running"] == true {
            d.stop(name, 30).await?;
        }
        d.rename(name, &parked).await?;
        // Register recovery immediately after parking. Every later failure,
        // including create/start/inspect errors, is handled by before_put_back.
        self.prepared.lock().unwrap_or_else(|e| e.into_inner()).push(OneShot {
            name: name.to_owned(),
            parked,
            new_id: None,
        });
        let id = d.create(name, &body).await?;
        {
            let mut prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
            let entry = prepared.last_mut().ok_or_else(|| Error::internal("missing one-shot recovery entry"))?;
            entry.new_id = Some(id.clone());
        }
        d.start(&id).await?;
        let deadline = Instant::now() + ONE_SHOT_TIMEOUT;
        loop {
            let st = d.inspect(&id).await?["State"].clone();
            if st["Running"] != true && st["Restarting"] != true {
                let code = st["ExitCode"].as_i64().unwrap_or(-1);
                return if code == 0 {
                    Ok(())
                } else {
                    Err(Error::conflict(format!("{name} on the new image exited with code {code}")))
                };
            }
            if Instant::now() > deadline {
                return Err(Error::conflict(format!("{name} on the new image didn't finish in time")));
            }
            tokio::time::sleep(self.health.poll).await;
        }
    }
}

impl Log for Helper<'_> {
    fn line(&self, msg: String) -> impl Future<Output = ()> + Send {
        self.note(msg);
        async {}
    }
}

impl Swapper for Helper<'_> {
    fn ready(&self, d: &Docker, id: &str) -> impl Future<Output = Result<()>> + Send {
        self.started.store(true, Ordering::Release);
        async move {
            let deadline = Instant::now() + self.health.timeout;
            wait_running(d, id, &self.health).await?;
            let probe = ["wranglarr", "health", "--addr", self.args.health.as_str()];
            loop {
                match d.exec(id, &probe, "", Duration::from_secs(15)).await {
                    Ok(0) => {
                        self.note("the new wranglarr is answering".into());
                        return Ok(());
                    }
                    Ok(_) | Err(DockerError::Other(_)) => {}
                    Err(e) => return Err(e.into()),
                }
                if Instant::now() > deadline {
                    return Err(Error::conflict(format!(
                        "the new wranglarr didn't answer within {}s",
                        self.health.timeout.as_secs()
                    )));
                }
                // it may have exited meanwhile
                let st = d.inspect(id).await?["State"].clone();
                if st["Running"] != true && st["Restarting"] != true {
                    let code = st["ExitCode"].as_i64().map_or_else(|| "?".to_string(), |c| c.to_string());
                    return Err(Error::conflict(format!("the new wranglarr exited with code {code}")));
                }
                tokio::time::sleep(self.health.poll).await;
            }
        }
    }

    async fn before_create(&self, d: &Docker) -> Result<()> {
        for name in &self.one_shots {
            self.note(format!("{name}: running it on the new image first"));
            self.prepare(d, name).await?;
            self.with_report(|r| r.result(name, "recreated"));
        }
        Ok(())
    }

    async fn before_put_back(&self, d: &Docker) -> Result<()> {
        let prepared: Vec<OneShot> = self.prepared.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default();
        // every one-shot gets its try, and one that can't be put back doesn't
        // hold wranglarr's own rollback up: it's in the report to sort out
        for o in prepared {
            match put_back_one_shot(d, &o.name, &o.parked, o.new_id.as_deref()).await {
                Ok(()) => {
                    self.with_report(|r| r.result(&o.name, "kept"));
                    self.note(format!("{}: the old one is back", o.name));
                }
                Err(e) => {
                    self.with_report(|r| r.result(&o.name, "failed"));
                    self.note(format!("{}: could not put the old one back ({e}); it's parked as {}", o.name, o.parked));
                }
            }
        }
        if self.started.load(Ordering::Acquire)
            && let Some(snapshot) = &self.args.snapshot
        {
            let (snapshot, db) = (snapshot.clone(), self.args.state_db.clone());
            tokio::task::spawn_blocking(move || restore_db(&snapshot, &db))
                .await
                .map_err(|e| {
                    Error::internal(format!("state recovery task failed: {e}; keep the old container stopped"))
                })?
                .map_err(|e| {
                    Error::caused("could not restore the state database; keep the old container stopped", e)
                })?;
            self.note("put wranglarr's state database back from before the update".into());
        }
        Ok(())
    }
}

/// Undo a one-shot's replacement: remove the new one (if made), give the
/// parked one its name back. A one-shot isn't started again: it already ran.
async fn put_back_one_shot(d: &Docker, name: &str, parked: &str, new_id: Option<&str>) -> Result<()> {
    if let Some(id) = new_id {
        d.remove(id, true).await?;
    }
    d.rename(parked, name).await?;
    Ok(())
}

/// Put the state database at `db` back to `snapshot`: a copy beside it,
/// renamed over it, and the old journal files removed.
fn restore_db(snapshot: &Path, db: &Path) -> std::io::Result<()> {
    let mut tmp = db.as_os_str().to_owned();
    tmp.push(".restoring");
    let tmp = PathBuf::from(tmp);
    std::fs::copy(snapshot, &tmp)?;
    std::fs::rename(&tmp, db)?;
    for suffix in ["-wal", "-shm"] {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        match std::fs::remove_file(PathBuf::from(p)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// The one-shots Compose runs to completion before `own` (its
/// `service_completed_successfully` dependencies), when they use the same image.
async fn one_shots(d: &Docker, own: &Value, reference: &str) -> Vec<String> {
    let labels = &own["Config"]["Labels"];
    let Some(project) = labels["com.docker.compose.project"].as_str() else { return vec![] };
    let services: Vec<&str> = labels["com.docker.compose.depends_on"]
        .as_str()
        .unwrap_or("")
        .split(',')
        .filter_map(|dep| {
            let mut p = dep.split(':');
            let service = p.next()?.trim();
            (p.next() == Some("service_completed_successfully")).then_some(service)
        })
        .collect();
    if services.is_empty() {
        return vec![];
    }
    let Ok(all) = d.containers(true).await else { return vec![] };
    all.iter()
        .filter(|c| {
            let l = &c["Labels"];
            l["com.docker.compose.project"] == project
                && l["com.docker.compose.service"].as_str().is_some_and(|s| services.contains(&s))
                && c["Image"] == reference
        })
        .filter_map(|c| c["Names"][0].as_str().map(|n| n.trim_start_matches('/').to_string()))
        .collect()
}

/// Do the update, writing the report as it goes; the report as it ended.
pub(crate) async fn run(args: &Args, env: &UpdatesEnv) -> Report {
    let report_path = super::report::path(&args.state_db);
    let helper_name = Args::helper_name(&args.container);
    let now = Utc::now();
    // stop, the one-shots, the health wait, and room to put things back
    let budget = args.timeout + 30 + ONE_SHOT_TIMEOUT.as_secs() + 300;
    let deadline = now + chrono::Duration::seconds(i64::try_from(budget).unwrap_or(i64::MAX / 2));
    let report = Report::begin(args.run, &args.container, &helper_name, now, deadline);
    let d = Docker::new(&args.socket);
    let mut h = Helper {
        args,
        report_path,
        report: Mutex::new(report),
        health: Health { timeout: Duration::from_secs(args.timeout), poll: env.poll, stable: env.stable },
        one_shots: vec![],
        prepared: Mutex::new(vec![]),
        started: AtomicBool::new(false),
    };
    h.note(format!("{helper_name}: replacing {} with {} ({})", args.container, args.image, short(&args.image_id)));

    let outcome = async {
        // the tag may have moved since wranglarr pulled it: only the image it checked
        let now_at = d.image(&args.image).await?["Id"].as_str().unwrap_or("").to_string();
        if now_at != args.image_id {
            return Err(SwapFailed {
                error: Error::conflict(format!(
                    "{} now points at {}, not the {} wranglarr pulled; nothing changed",
                    args.image,
                    short(&now_at),
                    short(&args.image_id)
                )),
                attempted: None,
            });
        }
        let own = d.inspect(&args.container).await?;
        // the one-shots deployed with wranglarr's current image, whichever track it moves to
        h.one_shots = one_shots(&d, &own, image_reference(&own)).await;
        Ok(own)
    }
    .await;
    let outcome: std::result::Result<Swapped, SwapFailed> = match outcome {
        Ok(_) => swap(&d, &h, &args.container, Some(&args.image), None).await,
        Err(e) => Err(e),
    };

    let mut error = None;
    match outcome {
        Ok(swapped) => {
            let prepared: Vec<OneShot> = h.prepared.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default();
            for o in prepared {
                if let Err(e) = d.remove(&o.parked, true).await {
                    h.note(format!("{}: could not remove the old one {}: {e}", o.name, o.parked));
                }
            }
            let f = &swapped.facts;
            h.note(format!("{}: update done, now {}", args.container, f.new_shown()));
            for dep in &swapped.dependents {
                let n = dep["Names"][0].as_str().unwrap_or("?").trim_start_matches('/');
                h.note(format!("{n} shared wranglarr's network: run `docker compose up -d {n}` to reattach it"));
            }
            h.with_report(|r| {
                r.result(&args.container, "updated");
                r.version = f.new_version.clone();
                r.updates.push(UpdateRecord::from_row(f.row("update", "ok", String::new())));
            });
        }
        Err(SwapFailed { error: e, attempted, .. }) => {
            h.note(format!("{}: update FAILED: {e}", args.container));
            if let Some(attempt) = attempted {
                let f = attempt.facts;
                retag(&d, &h, &f.reference, &f.old_image).await;
                let digests = match d.image(&f.new_image).await {
                    Ok(img) => repo_digests(&img).into_iter().collect(),
                    Err(_) => vec![],
                };
                let reason = crate::value::head(&format!("{} failed to start: {e}", f.new_shown()), 200);
                h.with_report(|r| {
                    r.version = f.new_version.clone();
                    r.updates.push(UpdateRecord::from_row(f.row(
                        "update",
                        "failed",
                        crate::value::head(&e.to_string(), 300),
                    )));
                    r.rejected = Some(Rejected { image: f.new_image.clone(), digests, reason });
                });
            }
            h.with_report(|r| r.result(&args.container, "failed"));
            error = Some(e.to_string());
        }
    }
    let mut report = h.report.into_inner().unwrap_or_else(|e| e.into_inner());
    report.finish(error);
    if let Err(e) = report.save(&h.report_path) {
        eprintln!("{PROG}: could not write the report {}: {e}", h.report_path.display());
    }
    report
}

impl From<DockerError> for SwapFailed {
    fn from(e: DockerError) -> SwapFailed {
        SwapFailed { error: e.into(), attempted: None }
    }
}

/// `wranglarr self-update …`: 0 when the new wranglarr is up, 1 otherwise.
pub fn main(args: &[String]) -> i32 {
    let env = UpdatesEnv::read(&crate::config::process_env);
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(helper_main(args, &env)),
        Err(e) => {
            eprintln!("{PROG}: {e}");
            1
        }
    }
}

/// The helper on the command line `args` (what follows `self-update`), with
/// the timings in `env`: 0 when the new wranglarr is up, 1 otherwise.
pub async fn helper_main(args: &[String], env: &UpdatesEnv) -> i32 {
    match crate::cli::parse::<Args>(PROG, args) {
        Ok(args) => i32::from(!run(&args, env).await.ok),
        Err(code) => code,
    }
}

// ---- wranglarr health ------------------------------------------------------------------

/// Whether the wranglarr in this container answers.
#[derive(Parser, Debug)]
struct HealthArgs {
    /// Where it listens
    #[arg(long, default_value = "127.0.0.1:8787")]
    addr: String,
}

/// `wranglarr health`: 0 when `GET /api/health` answers 200 within 5 s.
pub fn health_main(args: &[String]) -> i32 {
    let a = match crate::cli::parse::<HealthArgs>("wranglarr health", args) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return 1 };
    let url = format!("http://{}/api/health", a.addr);
    let ok = rt.block_on(async {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().ok()?;
        Some(client.get(&url).send().await.ok()?.status().is_success())
    });
    if ok == Some(true) { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own() -> Value {
        json!({
            "Id": "0123456789abcdef", "Image": "sha256:old",
            "Config": {"Image": "ghcr.io/wranglarrr/wranglarr:latest", "User": "1000:1000",
                       "Labels": {"com.docker.compose.project": "arrs",
                                  "com.docker.compose.depends_on":
                                      "postgres:service_healthy:false,wranglarr-init:service_completed_successfully:false"}},
            "HostConfig": {"GroupAdd": ["969"], "SecurityOpt": ["no-new-privileges:true"], "CapDrop": ["ALL"]},
            "Mounts": [
                {"Type": "bind", "Source": "/srv/arrs/appdata/wranglarr", "Destination": "/config", "RW": true},
                {"Type": "bind", "Source": "/srv/arrs/appdata/radarr", "Destination": "/arrconfig/radarr", "RW": false},
                {"Type": "bind", "Source": "/run/user/1000/podman/podman.sock", "Destination": "/var/run/docker.sock", "RW": true},
                {"Type": "volume", "Name": "backups", "Source": "/var/lib/x", "Destination": "/backups", "RW": true}
            ]
        })
    }

    fn args() -> Args {
        Args {
            container: "wranglarr".into(),
            image: "ghcr.io/wranglarrr/wranglarr:latest".into(),
            image_id: "sha256:new".into(),
            run: 42,
            state_db: "/config/wranglarr.db".into(),
            snapshot: Some("/config/self-update/20261027T100000Z/wranglarr.db".into()),
            health: "127.0.0.1:8787".into(),
            timeout: 300,
            socket: "/var/run/docker.sock".into(),
        }
    }

    #[test]
    fn the_command_line_parses_back() {
        let a = args();
        let cmd = a.to_command();
        assert_eq!(&cmd[..2], ["wranglarr", "self-update"]);
        let back = crate::cli::parse::<Args>(PROG, &cmd[2..]).expect("parses");
        assert_eq!(back, a);
        assert_eq!(Args::helper_name("wranglarr"), "wranglarr-wranglarr-updater");
    }

    #[test]
    fn the_helper_gets_the_socket_and_state_folder_and_nothing_else() {
        let body = container_body(&own(), &args(), &["HOME=/tmp".into()]).expect("body");
        assert_eq!(body["Image"], "sha256:new");
        assert_eq!(body["Cmd"][1], "self-update");
        assert_eq!(body["User"], "1000:1000");
        assert_eq!(body["Env"], json!(["HOME=/tmp"]));
        assert_eq!(body["Labels"], json!({"org.wranglarr.self-update": "42"}));
        let hc = &body["HostConfig"];
        assert_eq!(hc["AutoRemove"], true);
        assert_eq!(hc["NetworkMode"], "none");
        assert_eq!(hc["CapDrop"], json!(["ALL"]));
        assert_eq!(hc["ReadonlyRootfs"], true);
        assert_eq!(hc["GroupAdd"], json!(["969"]));
        assert_eq!(hc["SecurityOpt"], json!(["no-new-privileges:true"]));
        assert_eq!(
            hc["Mounts"],
            json!([
                {"Type": "bind", "Source": "/run/user/1000/podman/podman.sock", "Target": "/var/run/docker.sock",
                 "ReadOnly": false},
                {"Type": "bind", "Source": "/srv/arrs/appdata/wranglarr", "Target": "/config", "ReadOnly": false}
            ]),
            "not the arrs' config folders or the backups"
        );
        assert!(body.get("NetworkingConfig").is_none());
    }

    #[test]
    fn without_the_mounts_it_can_hand_over_nothing() {
        let mut o = own();
        o["Mounts"] = json!([{"Type": "bind", "Source": "/x", "Destination": "/config", "RW": true}]);
        let why = mounts_missing(&o, "/var/run/docker.sock", Path::new("/config")).expect("refused");
        assert!(why.contains("Docker socket"), "{why}");
        o["Mounts"] = json!([{"Type": "bind", "Source": "/s", "Destination": "/var/run/docker.sock", "RW": true}]);
        let why = mounts_missing(&o, "/var/run/docker.sock", Path::new("/config")).expect("refused");
        assert!(why.contains("state folder"), "{why}");
        assert!(container_body(&o, &args(), &[]).is_err());
        // a socket inside a mounted folder, and no-new-privileges added when missing
        let mut o = own();
        o["Mounts"][2] = json!({"Type": "bind", "Source": "/run/podman", "Destination": "/run/podman", "RW": true});
        o["HostConfig"]["SecurityOpt"] = Value::Null;
        let mut a = args();
        a.socket = "/run/podman/podman.sock".into();
        let body = container_body(&o, &a, &[]).expect("body");
        assert_eq!(body["HostConfig"]["Mounts"][0]["Target"], "/run/podman");
        assert_eq!(body["HostConfig"]["SecurityOpt"], json!(["no-new-privileges:true"]));
    }

    #[test]
    fn a_failed_update_puts_the_state_database_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("wranglarr.db");
        let snap = dir.path().join("snap.db");
        std::fs::write(&db, "changed by the new version").expect("write");
        std::fs::write(dir.path().join("wranglarr.db-wal"), "wal").expect("write");
        std::fs::write(&snap, "before").expect("write");
        restore_db(&snap, &db).expect("restore");
        assert_eq!(std::fs::read_to_string(&db).expect("read"), "before");
        assert!(!dir.path().join("wranglarr.db-wal").exists());
        assert!(!dir.path().join("wranglarr.db.restoring").exists());
        assert!(snap.exists(), "the snapshot stays for later");
    }
}
