// SPDX-License-Identifier: GPL-3.0-or-later
//! Every fuzz target is wired up the same four ways, and every saved input
//! (seeds and inputs that once crashed) still runs clean.
use std::collections::BTreeSet;
use std::path::Path;

use wranglarr_fuzz::TARGETS;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

#[test]
fn every_target_is_listed_built_and_seeded() {
    let manifest = std::fs::read_to_string(Path::new(ROOT).join("Cargo.toml")).expect("Cargo.toml");
    let bins: BTreeSet<_> = manifest
        .lines()
        .filter_map(|l| l.strip_prefix("name = \"").and_then(|r| r.strip_suffix('"')))
        .filter(|n| *n != "wranglarr-fuzz")
        .collect();
    let files: BTreeSet<_> = std::fs::read_dir(Path::new(ROOT).join("fuzz_targets"))
        .expect("fuzz_targets/")
        .map(|e| e.expect("entry").path().file_stem().expect("name").to_string_lossy().into_owned())
        .collect();
    let listed: BTreeSet<_> = TARGETS.iter().map(|(n, _)| *n).collect();
    assert_eq!(bins, listed, "Cargo.toml's [[bin]]s and TARGETS");
    assert_eq!(files.iter().map(String::as_str).collect::<BTreeSet<_>>(), listed, "fuzz_targets/ and TARGETS");
    for name in listed {
        let seeds = std::fs::read_dir(Path::new(ROOT).join("corpus").join(name))
            .unwrap_or_else(|_| panic!("corpus/{name}/ is missing: add a real example of its input"));
        assert!(seeds.count() > 0, "corpus/{name}/ is empty: add a real example of its input");
    }
}

#[test]
fn saved_inputs_run_clean() {
    for (name, run) in TARGETS {
        for dir in ["corpus", "regressions"] {
            let Ok(entries) = std::fs::read_dir(Path::new(ROOT).join(dir).join(name)) else { continue };
            for entry in entries {
                let path = entry.expect("entry").path();
                let data = std::fs::read(&path).expect("input");
                if std::panic::catch_unwind(|| run(&data)).is_err() {
                    panic!("{name}: {} fails", path.display());
                }
            }
        }
    }
}
