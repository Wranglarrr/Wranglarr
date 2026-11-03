# Contributing

Bug reports, ideas and pull requests are all welcome.

## Reporting a bug or asking for a feature

Open an [issue](https://github.com/Wranglarrr/Wranglarr/issues). For a bug, include
the version (`wranglarr --version` or the image tag), what you did, what you expected
and what happened instead. Logs help; check them for API keys and passwords first.

A security problem goes in a private advisory, not an issue. See [SECURITY.md](SECURITY.md).

## Making a change

1. For anything bigger than a small fix, open an issue first so we can agree on the
   approach before you spend time on it.
2. Fork the repo and branch from `main`: `feature/<topic>` or `bugfix/<topic>`.
3. Make the change, with tests (see below) and docs. User-facing behaviour is
   documented in `wiki/`, and the HTTP API in `wiki/API.md`.
4. Run the checks locally:

   ```sh
   cd wranglarr
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cd ..
   tests/.venv/bin/python -m pytest tests
   ```

5. Open a pull request against `main` and say what it changes and why. CI runs the
   same checks, plus the minimum Rust version, the image build for amd64 and arm64,
   and the fuzz targets. Every required check must pass and the branch must be up to
   date with `main` before it's merged. Pull requests are squash- or rebase-merged,
   so `main` has no merge commits.

How the code is laid out, how to add a service, and the conventions to follow are in
the wiki's [Development](https://github.com/Wranglarrr/Wranglarr/wiki/Development) page.

## Tests

Every change that alters behaviour comes with tests:

- A new feature gets a black-box test in `tests/` of what a user does with it, and
  unit or integration tests for its pieces.
- A bug fix gets a test that fails without the fix.
- Code that writes to an app (an arr, a download client, Postgres) gets a test for a
  failed or partial write, not only the happy path.
- A new parser of outside input (an app's API, a registry, a provider, pasted text,
  files from elsewhere, archives) gets a fuzz target in `wranglarr/fuzz`. Its
  [README](wranglarr/fuzz/README.md) says how.

## Code style

- `cargo fmt` formatting, and no clippy warnings (`-D warnings`).
- No `unsafe` (`#![forbid(unsafe_code)]`).
- Every source file starts with `// SPDX-License-Identifier: GPL-3.0-or-later`.

## Licence

wranglarr is GPL-3.0-or-later. By sending a pull request you agree that your
contribution is released under the same licence.
