// SPDX-License-Identifier: GPL-3.0-or-later
//! What wranglarr knows about each kind of app lives in the services crate
//! (crates/services): the app and the core ask a descriptor or a capability, never
//! which kind an app is. This fails on a kind's id written as a string literal in
//! either of them (test code aside), so a new service stays one folder plus a
//! registry line.

use std::path::{Path, PathBuf};

use wranglarr_services as service;

/// Spellings that are something other than wranglarr's knowledge of a kind:
/// (file, the code that holds it, why).
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "src/indexers.rs",
        "pub const PROWLARR_SOURCE: &str = \"prowlarr\";",
        "an indexer source's type (a Prowlarr outside the stack), persisted in the state db",
    ),
    (
        "src/setup/env/render.rs",
        "\"prowlarr\" => \":compose.vpn-prowlarr.yaml\"",
        "VPN_SCOPE=prowlarr, a persisted .env value",
    ),
    (
        "src/web/connections.rs",
        "json!({\"media\": media, \"seerr\": seerr})",
        "the connections page's key for its Seerr panel",
    ),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("read sources").flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The file without its test code: a `#[cfg(test)]` at the top level starts the
/// tests module, which runs to the end of the file by this codebase's layout.
fn without_tests(text: &str) -> &str {
    text.find("\n#[cfg(test)]").map_or(text, |i| &text[..i])
}

#[test]
fn kind_names_stay_in_the_services_crate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![];
    for dir in ["src", "crates/core/src"] {
        rust_files(&root.join(dir), &mut files);
    }
    files.sort();
    let mut found = vec![];
    for path in files {
        let rel = path.strip_prefix(root).expect("in the workspace").to_string_lossy().replace('\\', "/");
        // files that are test code throughout
        if rel.ends_with("/tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read");
        for (n, line) in without_tests(&text).lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let allowed = ALLOWED.iter().any(|(f, snippet, _)| *f == rel && code.contains(snippet));
            if !allowed && service::ALL.iter().any(|d| code.contains(&format!("\"{}\"", d.id))) {
                found.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "kind names outside crates/services (ask the descriptor or a capability instead):\n{}",
        found.join("\n")
    );
}

/// The front end shows `icon()` for every service: static/icons/<id>.svg.
#[test]
fn every_service_has_an_icon() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for d in service::ALL {
        let file = root.join(d.icon().trim_start_matches('/'));
        assert!(file.is_file(), "{}", file.display());
    }
}
