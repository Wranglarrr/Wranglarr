# Fuzz targets

Each target feeds generated input to one of wranglarr's parsers of outside
input and fails if it panics (or, for the manifest, if what it read doesn't
write back the same). What each one checks is in [`src/lib.rs`](src/lib.rs).

| Target | Input |
|---|---|
| `community_config` | a community config, pasted or fetched |
| `jackett_xml` | Jackett's list of trackers |
| `backup_manifest` | a backup's `manifest.json` |
| `wranglarr_toml` | `wranglarr.toml` |
| `small_parsers` | image references, app dates, cron schedules, `.env` lines, API keys in INI files |

## Running one

On stable Rust, from `wranglarr/fuzz` (the flags are the ones cargo-fuzz uses;
`--target` keeps them off build scripts):

```sh
export RUSTFLAGS="-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 \
  -Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table \
  -Cllvm-args=-sanitizer-coverage-trace-compares --cfg fuzzing -Cdebug-assertions -Coverflow-checks"
cargo build --release --target x86_64-unknown-linux-gnu
mkdir -p /tmp/fuzz-jackett
target/x86_64-unknown-linux-gnu/release/jackett_xml -max_total_time=300 /tmp/fuzz-jackett corpus/jackett_xml
```

libFuzzer adds what it finds to the first folder, so give it a scratch one and
keep `corpus/` to a few real examples. With nightly and
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz), `cargo fuzz run jackett_xml`
works too.

`cargo test` here (stable, no flags) checks that every target is wired up and
replays every file in `corpus/` and `regressions/`.

## When one fails

1. Keep the input it wrote (`crash-…`; CI uploads it as an artifact).
2. Fix the parser, and add a unit test next to it for that input.
3. Save the input as `regressions/<target>/<short-name>` so `cargo test` keeps
   replaying it.

## Adding a target

Add the function to `src/lib.rs` and to `TARGETS`, a `[[bin]]` in `Cargo.toml`,
`fuzz_targets/<name>.rs`, and at least one real example in `corpus/<name>/`.
`cargo test` fails until all four agree. If it needs something inside the app
crate, expose it in `../src/fuzz.rs` (built only with the `fuzzing` feature).
