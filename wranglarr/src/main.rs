// SPDX-License-Identifier: GPL-3.0-or-later
//! One binary, several names: picked by argv[0] (symlinks in the image) or by
//! the first argument.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().and_then(|a| a.rsplit('/').next()).unwrap_or("wranglarr").to_string();
    let (cmd, rest) = match prog.as_str() {
        "wranglarr" => match args.get(1).map(String::as_str) {
            Some("--version" | "-V" | "version") => {
                println!("wranglarr {}", wranglarr::VERSION);
                return;
            }
            Some(c @ ("serve" | "setup" | "prepare" | "auth" | "apps-auth" | "self-update" | "health")) => {
                (c.to_string(), args[2..].to_vec())
            }
            _ => ("serve".to_string(), args[1..].to_vec()),
        },
        p => (p.trim_start_matches("wranglarr-").to_string(), args[1..].to_vec()),
    };
    let code = match cmd.as_str() {
        "prepare" => wranglarr_core::owner::prepare_main(&rest),
        "serve" => wranglarr::serve::main(&rest),
        "auth" => wranglarr::auth::cli::main(&rest),
        "setup" => wranglarr::setup::main(&rest),
        "apps-auth" => wranglarr::appsauth::main(&rest),
        "self-update" => wranglarr::updates::self_update_main(&rest),
        "health" => wranglarr::updates::health_main(&rest),
        other => {
            eprintln!("wranglarr {}: unknown command `{other}`", wranglarr::VERSION);
            2
        }
    };
    std::process::exit(code);
}
