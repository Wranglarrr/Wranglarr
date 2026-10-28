// SPDX-License-Identifier: GPL-3.0-or-later
//! `wranglarr-proxy-setup` (also `proxy-setup` in the image): the reverse proxy
//! config for the settings saved on the Reverse proxy page and Dashboard → Links,
//! the same text the page shows, on stdout so it can go straight into a file:
//!
//! ```sh
//! docker compose exec -T wranglarr proxy-setup > Caddyfile.wranglarr
//! docker compose exec -T wranglarr proxy-setup --kind nginx | sudo tee /etc/nginx/sites-available/wranglarr
//! ```
//!
//! The notes (what to set in the apps, certificates, networks) go to stderr.
//! Exit codes: 0 with a config, 1 when the settings need attention, 2 for bad usage.

use clap::Parser;

use crate::app::App;
use crate::config;
use crate::deployment::Deployment;
use crate::proxy::KINDS;
use wranglarr_core::owner;

const PROG: &str = "wranglarr-proxy-setup";

#[derive(Parser, Debug)]
#[command(
    name = PROG,
    about = "Print the reverse proxy config for the saved settings (notes on stderr)",
    after_help = "Example: docker compose exec -T wranglarr proxy-setup > Caddyfile.wranglarr\n\
                  (-T: no terminal, so the file gets plain line endings)"
)]
struct Args {
    /// Write it for this proxy instead of the saved one (nothing is saved):
    /// caddy, nginx, traefik-file, traefik-labels, caddy-labels, cloudflared, npm
    #[arg(long)]
    kind: Option<String>,
}

pub fn main(args: &[String]) -> i32 {
    let a = match crate::cli::parse::<Args>(PROG, args) {
        Ok(a) => a,
        Err(code) => return code,
    };
    if let Some(k) = &a.kind
        && !KINDS.iter().any(|(id, _)| id == k)
    {
        let ids: Vec<&str> = KINDS.iter().map(|(id, _)| *id).collect();
        return crate::cli::usage_error::<Args>(PROG, format!("--kind {k:?}: one of {}", ids.join(", ")));
    }
    let deployment = Deployment::read(config::process_env);
    let path = deployment.config_path.clone();
    let cfg = match crate::service::apps::load(&path, config::process_env) {
        Ok(c) => c,
        Err(e) => return fail(&format!("{}: {e}", path.display())),
    };
    owner::apply_umask(&deployment.umask);
    // `docker compose exec` runs as root: become PUID:PGID like the service, so
    // nothing it touches in the state database's folder ends up owned by root
    let socket = if cfg.docker_socket.is_empty() { "/var/run/docker.sock" } else { cfg.docker_socket.as_str() };
    if let Err(e) = owner::drop_root(
        deployment.stack_owner,
        &[],
        std::path::Path::new(socket),
        config::process_env("DOCKER_GID").as_deref(),
    ) {
        eprintln!("{PROG}: {e}; staying root");
    }
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return fail(&e.to_string()),
    };
    rt.block_on(async move {
        let app = match App::open(cfg, deployment).await {
            Ok(a) => a,
            Err(e) => return fail(&e.to_string()),
        };
        let mut s = app.proxy().settings().await;
        if let Some(k) = a.kind {
            s.kind = k;
        }
        // no browser here: the proxy reaches the apps at HOST_IP (or the saved address)
        let v = match app.proxy().answer(&s, "").await {
            Ok(v) => v,
            Err(e) => return fail(&e.to_string()),
        };
        let text = v["text"].as_str().unwrap_or("");
        let str_of = |k: &str| v[k].as_str().unwrap_or("").to_string();
        eprintln!("# {}: {}", str_of("filename"), str_of("how"));
        for n in v["notes"].as_array().into_iter().flatten() {
            let warn = if n["cls"] == "warn" { "warning: " } else { "" };
            eprintln!("# {warn}{}", n["msg"].as_str().unwrap_or(""));
        }
        if v["valid"] == false || text.trim().is_empty() {
            return fail("no config for these settings: fix them under Dashboard → Links or on the Reverse proxy page");
        }
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        0
    })
}

fn fail(msg: &str) -> i32 {
    eprintln!("{PROG}: {msg}");
    1
}
