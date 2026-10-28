// SPDX-License-Identifier: GPL-3.0-or-later
//! `wranglarr serve`: the web service.

use crate::app::App;
use crate::deployment::Deployment;
use crate::{config, owner, web};

fn auth_off_warning() {
    // straight to stderr: the log formatter would escape the colour codes
    eprintln!(
        "{}",
        owner::amber(&[
            "WARNING: wranglarr's login is OFF (WRANGLARR_AUTH=off).",
            "",
            "Anyone who can reach its port can stop containers, restore old",
            "configs, delete backups, and read every app's settings. With the",
            "Docker socket mounted, that's close to root on this machine.",
            "",
            "Only do this behind a reverse proxy that asks for a login itself.",
            "Turn it back on: remove WRANGLARR_AUTH from .env, then",
            "docker compose up -d wranglarr.  Details: wiki/Login.md",
        ])
    );
}

/// The process log at `level`, unless `filter` (WRANGLARR_LOG, a tracing
/// filter) is set and valid.
pub fn init_logging(level: &str, filter: Option<&str>) {
    let level = match level.to_lowercase().as_str() {
        "debug" | "trace" => "debug",
        "warning" | "warn" => "warn",
        "error" | "critical" => "error",
        _ => "info",
    };
    let filter = filter
        .and_then(|f| tracing_subscriber::EnvFilter::try_new(f).ok())
        .unwrap_or_else(|| tracing_subscriber::EnvFilter::new(format!("wranglarr={level}")));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(true).try_init();
}

pub fn main(_args: &[String]) -> i32 {
    let deployment = Deployment::read(config::process_env);
    let path = deployment.config_path.clone();
    let cfg = match crate::service::apps::load(&path, config::process_env) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wranglarr: {}: {e}", path.display());
            return 1;
        }
    };
    init_logging(&cfg.log_level, deployment.log_filter.as_deref());
    owner::apply_umask(&deployment.umask);
    // started as root with PUID set: become PUID:PGID before any thread starts
    let socket = if cfg.docker_socket.is_empty() { "/var/run/docker.sock" } else { cfg.docker_socket.as_str() };
    let own = [
        path.parent(),
        cfg.state_db.parent(),
        Some(cfg.storage_root.as_path()),
        Some(deployment.pre_restore.as_path()),
    ];
    let own: Vec<&std::path::Path> = own.into_iter().flatten().filter(|p| !p.as_os_str().is_empty()).collect();
    match owner::drop_root(
        deployment.stack_owner,
        &own,
        std::path::Path::new(socket),
        config::process_env("DOCKER_GID").as_deref(),
    ) {
        Ok(Some((uid, gid))) => tracing::info!("started as root: now running as {uid}:{gid} (PUID/PGID)"),
        Ok(None) => {}
        Err(e) => tracing::warn!("{e}; staying root"),
    }
    owner::describe();
    if let Some(p) = cfg.state_db.parent() {
        deployment.stack_owner.give(p, false); // only when someone runs it as root on the host anyway
    }
    crate::files::set_pre_restore_dir(deployment.pre_restore.clone());
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("wranglarr: {e}");
            return 1;
        }
    };
    rt.block_on(async move {
        let addr = format!("{}:{}", cfg.bind, cfg.port);
        let app = match App::new(cfg, deployment).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("could not start: {e}");
                return 1;
            }
        };
        if !app.auth_enabled() {
            auth_off_warning();
        }
        app.start().await;
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("could not listen on {addr}: {e}");
                return 1;
            }
        };
        tracing::info!("wranglarr {} listening on http://{addr}", crate::VERSION);
        let router = web::router(app.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
        let shutdown_app = app.clone();
        let shutdown = async move {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending().await } } => {}
            }
            shutdown_app.begin_shutdown().await;
        };
        let result = axum::serve(listener, router).with_graceful_shutdown(shutdown).await;
        if let Err(e) = app.shutdown().await {
            tracing::error!("worker shutdown: {e}");
        }
        match result {
            Ok(()) => 0,
            Err(e) => {
                tracing::error!("server: {e}");
                1
            }
        }
    })
}
