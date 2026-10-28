// SPDX-License-Identifier: GPL-3.0-or-later
//! First-run setup wizard: a small web UI that writes .env (and seeds
//! wranglarr.toml / a restore plan) for the compose stack. Runs from the
//! wranglarr image before anything is deployed:
//!
//! ```text
//! docker run --rm -it -p 8788:8788 -v "$PWD":/stack -v /mnt/data:/data \
//!     ghcr.io/wranglarrr/wranglarr:latest wranglarr-setup
//! ```
//!
//! `wranglarr-setup --auth` shows only the app-logins step and changes only those
//! keys in an existing .env; ./scripts/init.sh --auth then applies them.
//!
//! Mounts: /stack = the directory with compose.yaml (read-write; .env lands
//! here); /data = the drive DATA_ROOT will point at, so it can be checked;
//! optional /data2 and /appdata (only if APPDATA is an absolute path outside
//! the stack dir). Paths typed in the UI are host paths and are written to
//! .env verbatim; the checks run against the mounts.

pub mod env;
pub mod host;
pub mod input;
mod legacy;
pub mod scan;
pub mod web;
pub mod write;

use std::io::Write;
use std::path::Path;

use clap::Parser;

pub use web::{Wizard, router};

const PROG: &str = "wranglarr-setup";

/// wranglarr first-run setup wizard
#[derive(Parser, Debug)]
struct Args {
    /// Directory holding compose.yaml (where .env is written); /stack in the container
    #[arg(long)]
    target: Option<String>,
    /// Address to listen on [default: 0.0.0.0 in a container, else 127.0.0.1]
    #[arg(long)]
    bind: Option<String>,
    /// Port to listen on
    #[arg(long, default_value_t = 8788)]
    port: u16,
    /// Only the app-logins step: change those keys in the existing .env, then run
    /// ./scripts/init.sh --auth
    #[arg(long)]
    auth: bool,
}

/// `wranglarr setup` / `wranglarr-setup`.
pub fn main(args: &[String]) -> i32 {
    let envs = host::Env::detect();
    let a = match crate::cli::parse::<Args>(PROG, args) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let target = a.target.unwrap_or_else(|| {
        std::env::var("WRANGLARR_SETUP_TARGET")
            .ok()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| if envs.in_docker { "/stack".into() } else { ".".into() })
    });
    let bind = a.bind.unwrap_or_else(|| if envs.in_docker { "0.0.0.0".into() } else { "127.0.0.1".into() });
    let (port, auth_only) = (a.port, a.auth);
    let target = host::resolve(Path::new(&target));
    if !target.join("compose.yaml").exists() {
        return crate::cli::usage_error::<Args>(
            PROG,
            format!("{} has no compose.yaml; mount the stack directory:  -v \"$PWD\":/stack", target.display()),
        );
    }
    if auth_only && !target.join(".env").exists() {
        return crate::cli::usage_error::<Args>(
            PROG,
            format!("{} has no .env yet: run the full setup first (without --auth)", target.display()),
        );
    }
    let token = std::env::var("WRANGLARR_SETUP_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| crate::ids::random_token(8));
    let host = if envs.in_docker || bind == "0.0.0.0" || bind == "::" { "localhost" } else { bind.as_str() };
    println!(
        "wranglarr setup{}, writing to {}\n\n  open  http://{host}:{port}/?t={token}\n\n  From another computer, use this machine's address instead of {host}{}. Code: {token}\n",
        if auth_only { " (app logins only)" } else { "" },
        target.display(),
        if envs.in_docker { ", and the port you published" } else { "" },
    );
    let _ = std::io::stdout().flush();
    crate::serve::init_logging("warning", crate::config::process_env("WRANGLARR_LOG").as_deref());
    let wizard = Wizard { target, auth_only, token: Some(token), env: envs };
    serve(wizard, &bind, port)
}

fn serve(wizard: Wizard, bind: &str, port: u16) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("wranglarr-setup: {e}");
            return 1;
        }
    };
    rt.block_on(async move {
        let addr = if bind.contains(':') { format!("[{bind}]:{port}") } else { format!("{bind}:{port}") };
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("wranglarr-setup: can't listen on {addr}: {e}");
                return 1;
            }
        };
        let shutdown = async {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending().await } } => {}
            }
        };
        match axum::serve(listener, router(wizard)).with_graceful_shutdown(shutdown).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("wranglarr-setup: {e}");
                1
            }
        }
    })
}
