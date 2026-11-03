# Release checklist

Go through this before tagging a release. Tick the boxes in the release PR's
description (copy the list there), not in this file. If an item doesn't apply,
say why in the PR. Add to this list when a release teaches us something.

## Open items for the next release

Things already known to need doing before the next tag. Clear them out as they
land, and move anything lasting into the sections below.

- [ ] Fuzz targets for the lariat parsers on the 1.2.0 branch: provider indexes,
      package manifests, archives and release signatures.
- [ ] Library doctor and config blast radius checked against real Radarr, Sonarr,
      Lidarr and Whisparr on a throwaway stack. So far they've only run against mocks.
- [ ] Scryer package tried against a live Scryer, or kept marked as a draft in
      the release notes.
- [ ] Questarr's Prowlarr and download-client wiring re-checked once Questarr
      1.5.1 (admin-scope API keys) is out.
- [ ] Decide whether CSP (`feature/csp`) ships in this release.

## Version and notes

- [ ] `version` under `[workspace.package]` in `wranglarr/Cargo.toml` is bumped,
      and `cargo check` has updated `Cargo.lock` to match.
- [ ] `wiki/Release-Notes.md` has a dated `## X.Y.Z — YYYY-MM-DD` heading. The
      image workflow refuses the tag without it.
- [ ] **Before you update** lists every manual step: renamed or removed
      settings, new permissions, anything in `wranglarr.toml` or `.env` that
      changes meaning.
- [ ] Every new feature has a wiki page or section, linked from `wiki/Home.md`,
      and the release notes link to it.
- [ ] `wiki/API.md` matches the endpoints: new ones, changed fields, removed ones.
- [ ] `SECURITY.md`'s supported versions include this release.

## Tests

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps` and
      `cargo test --workspace` are clean.
- [ ] The black-box suite passes with the slow tests:
      `WRANGLARR_TEST_SLOW=1 tests/.venv/bin/python -m pytest tests`.
- [ ] `cargo check --workspace --all-targets` passes on the minimum Rust version
      (`rust-version` in `Cargo.toml`).
- [ ] Every new feature has a black-box test of what a user does with it, and
      every bug fixed since the last release has a test that failed before the fix.
- [ ] New code that writes to an app (an arr, a download client, Postgres) has a
      test for a failed or partial write, not only the happy path.

## Fuzzing

- [ ] Every parser added since the last release that reads outside input has a
      fuzz target. Outside input means: anything from an app's API, a registry, a
      provider or the web, pasted text, files a user may bring from elsewhere
      (backups, configs, `.env`), and archives.
- [ ] Every fuzz target has its seed inputs and passes `cargo test` in
      `wranglarr/fuzz`. The test fails when a target isn't wired up, so a green
      run covers this.
- [ ] The latest weekly fuzz run is green, or a manual run (`fuzz` workflow,
      fifteen minutes per target) passed on the release commit.
- [ ] Every crash fuzzing found has a fix, a unit test next to the parser and its
      input in `wranglarr/fuzz/regressions/<target>/`.

## Dependencies and supply chain

- [ ] Open Dependabot PRs are merged, or each one left open has a reason in the
      release PR.
- [ ] `cargo deny check` is clean (advisories, licences, sources). Each
      exception in `deny.toml` is still needed.
- [ ] Workflow actions are pinned to a commit SHA, with the version in a comment.
- [ ] CodeQL alerts on `main` are fixed or dismissed with a reason.
- [ ] The Scorecard score hasn't dropped since the last release. Check
      [the results](https://scorecard.dev/viewer/?uri=github.com/Wranglarrr/Wranglarr).
- [ ] No secrets, keys or real hostnames in the diff since the last tag:
      `git diff vPREVIOUS..HEAD` checked for API keys, passwords, `.env` contents
      and private addresses.

## Security

- [ ] New HTTP endpoints require login (and an API key where scripts use them),
      and change state only on POST, PUT or DELETE.
- [ ] New outbound requests have a timeout and a response-size limit, and say
      where they go in the docs.
- [ ] Secrets stay out of logs, reports, history entries and error messages
      (`redact` where an app's answer can echo one back).
- [ ] New files and folders wranglarr creates have the right owner and mode,
      including under rootless Podman.

## Upgrade and compatibility

- [ ] Upgrading from the previous release works on the demo stack (rootless
      Podman), with its existing appdata and state: settings, history, schedules
      and backups made by the old version still read.
- [ ] Restoring a backup made by the previous release works.
- [ ] Rolling back to the previous image after the upgrade still starts. If it
      can't (a one-way state change), **Before you update** says so.
- [ ] Docker and Podman both run the stack; a VPN compose variant still starts.

## The image

- [ ] Both platform builds (amd64, arm64) and their smoke tests pass on the
      release commit.
- [ ] The image size hasn't grown much since the last release, or the release
      notes say why.

## Release

- [ ] The release branch is merged to `main` through a PR with every check green.
- [ ] Tag `vX.Y.Z` on that `main` commit and push the tag. The image workflow
      checks the version, builds, publishes `:X.Y.Z`, `:X.Y`, `:X` and `:latest`,
      and makes the GitHub release from the notes.
- [ ] `docker pull ghcr.io/wranglarrr/wranglarr:X.Y.Z` works, and
      `wranglarr --version` in it says `X.Y.Z`.
- [ ] Merged and stale branches and worktrees are cleaned up.
