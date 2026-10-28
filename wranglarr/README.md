# wranglarr

Backup, version and restore the configuration of your \*arr suite: Radarr,
Sonarr, Lidarr, Readarr, Prowlarr, Whisparr and Bazarr (including their
Postgres databases), Seerr, plus qBittorrent, SABnzbd and NZBGet (config directories). One click per suite, cron automation, retention, and a
"sync shared settings" button so a download client or notification is defined once.
It also wires the apps together (download clients into every arr, Sonarr and
Radarr into Bazarr, the arrs into Prowlarr) and pushes trackers and
aggregators from its Indexers page to the arrs you pick.

What a backup version contains, per app:

| | |
| --- | --- |
| `<app>/<db>.dump` | `pg_dump -Fc` of every database the app uses |
| `<app>/<app>_backup_….zip` | the app's **own** backup task output (config.xml / config.yaml) |
| `<app>/config/`, `<app>/config.sha256` | the app's `/config` as a plain folder (logs, caches, live SQLite files left out); for download clients that includes the torrent files / NZBs, never the downloaded data |
| `manifest.json` | app version, database type, migration version, collection stats (movies / series / artists, files, bytes on disk, missing), configuration scope (indexers, download clients, profiles, custom formats, …), live database sizes, sha256 of every file |
| `globals.sql` | Postgres roles |

Why both halves: on Postgres the arrs' built-in backup contains only
config.xml and skips the database entirely, while `pg_dump` knows nothing
about config.xml.

## Running

See the top-level `compose.yaml`: the `wranglarr` service runs the image
built from this directory (`ghcr.io/wranglarrr/wranglarr`, or a local
`docker build -t wranglarr:local .` with `WRANGLARR_IMAGE=wranglarr:local` in
`.env`; under Podman use `localhost/wranglarr:local`), mounts `/config` (holding `wranglarr.toml` and the state db),
`/backups`, each app's config dir (API keys are read from there) and,
optionally, the Docker socket. Configuration is `wranglarr.example.toml`.

Without Docker (Rust 1.89+ and OpenSSL's headers):

```sh
cargo build --release
WRANGLARR_CONFIG=./wranglarr.toml target/release/wranglarr
```

`pg_dump`, `pg_restore`, `psql` and `pg_isready` must be on `PATH`.

## Development

A Cargo workspace in three layers: `crates/core` (plumbing that knows no kind
of app), `crates/services` (one folder per kind of app, and the traits the rest
of the code uses) and the app itself in `src/`. One binary, five commands:
`wranglarr [serve]`, `setup`, `prepare`, `auth`, `apps-auth` (the image also
links them as `wranglarr-setup` etc.). The templates, static files and
community configs in `templates/`, `static/` and `configs/` are embedded at
build time.

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check                                   # licences, advisories, sources
```

The black-box suite in the repo's `tests/` runs against the built binary
([tests/README.md](../tests/README.md)). The layout, adding a service and the
conventions are in the wiki's [Development](../wiki/Development.md) page.

## Releases

`.github/workflows/image.yml` builds the image for amd64 and arm64 (each on a
native runner) and pushes it to GHCR: every push to `main` as `:edge`, every `vX.Y.Z` tag as `:X.Y.Z`,
`:X.Y`, `:X` and `:latest`. Pull requests are built and smoke-tested but not
pushed. To cut a release, set `version` under `[workspace.package]` in `Cargo.toml` (and run
`cargo check` so `Cargo.lock` follows), then push the tag `vX.Y.Z`; the
workflow stops if the two disagree.

## HTTP API

Everything the UI does is a JSON call, so scripts, Home Assistant and cron
jobs can do it too: send an API key (Account → API keys) in the `X-Api-Key`
header. Every endpoint, its body and its answer, the long-running jobs and
the error codes are in the wiki's [HTTP API](../wiki/API.md) page.

## Setup wizard

```sh
docker run --rm -it -p 8788:8788 -v "$PWD":/stack -v /mnt/data:/data wranglarr wranglarr-setup
```

serves the first-run wizard from the same image (see the top-level README
and the wiki). It only writes into `/stack` (and `/appdata` if mounted).

## Restore semantics

- **Databases**: with the Docker socket mounted and `container` set, the app's
  container is stopped, every dump is `pg_restore --clean --if-exists
  --single-transaction`'d, then the container is started. Without Docker the
  restore runs while the app is up and the app is restarted through its API.
- **Config** (arrs): the zip is uploaded to `system/backup/restore/upload`; the
  app applies config.xml and restarts itself.
- **Config** (Bazarr): Bazarr's own restore refuses a zip without a SQLite
  file, which is what a Postgres-mode backup is, so config.yaml is written
  into the mounted `config_dir` directly (the previous file is kept as
  `config.yaml.pre-wranglarr`) and Bazarr is restarted via its API.
- **Config** (qBittorrent / SABnzbd / NZBGet): the version's plain
  `<app>/config/` copy (checksums verified) goes back over the mounted
  `config_dir` with the container stopped (Docker socket required); the
  previous contents are kept in `/config/pre-restore/<app>` (`appdata/wranglarr/pre-restore/`).
- **Without wranglarr**: every version is a standalone stack (`files.rs` makes
  the natural-format `/config` copies, `standalone.rs` writes `compose.yaml`,
  `.env`, `RESTORE.md` and a `postgres/initdb` restore script). See the wiki,
  Backups and Restore → Without wranglarr.
- A **restore plan** (`restore-plan.json` next to the state db, written by the
  setup wizard) restores several app@version items in one run.
- Every file is checksum-verified against the manifest before it is used.

## Container updates

`updates.rs`, over the Docker socket:

- **Check.** `GET /distribution/{ref}/json` gives the registry digest for the
  container's tag without pulling. It is compared with the running image's
  `RepoDigests`.
- **Update.** Pull, refuse a Postgres `PG_MAJOR` change, back up the affected
  suite(s), then recreate the container:
  - Its `Config`, `HostConfig` and networks are copied.
  - Values that only came from the old image are dropped (env, labels, cmd,
    exposed ports) so the new image's defaults apply.
  - Anonymous volumes are reattached and `com.docker.compose.image` is updated.
  - The old container is parked as `<name>-wranglarr-old`. After a Docker
    health check, uptime and app API wait, it is removed, or renamed back if
    the new one failed.
- **Failure.** The tag goes back to the running image and the rejected digest is
  blocked until the registry moves again.
- **Shared namespace.** Containers with `NetworkMode: container:<id>` of a
  replaced container are recreated onto the new id.
- **Rollback.** Retag the previous image, recreate on it, and hold the container.

## App connections

`connect.rs` wires every download client into every arr (category = the
arr's name; SABnzbd/NZBGet categories and the per-arr folders are created
first), Sonarr and Radarr into Bazarr, and every arr into Prowlarr as a
Full Sync application. Hosts are the compose service names, or `localhost`
for apps sharing one network namespace (the shared VPN tunnel). Each entry is
tested by the target app before it is saved.

```toml
[connect]
auto = true      # at startup, connect only links never handled before (kv "connect:done")
```

The **Connect apps** button (`POST /api/connect`, mode `all`) creates or
updates every link.

## Indexers

`indexers.rs`: sources of type `torznab`, `newznab`, `jackett` (every
configured tracker becomes a Torznab indexer), `nzbhydra` (Newznab, plus
Torznab if ticked) or `prowlarr` (a Prowlarr outside the stack: the picked
arrs are registered in it). Sources and their API keys live in the state db.
A sync creates or updates the indexers in the picked arrs and removes the
ones wranglarr created when a source is deleted or an arr is un-picked
(kv `indexers:pushed`); hand-added indexers are never touched.

## Shared settings

`[[shared.download_client]]` and `[[shared.notification]]` in `wranglarr.toml`
are pushed to each app by name through the normal `downloadclient` /
`notification` endpoints, after the app's own connection test. Per-kind field
differences (movieCategory vs tvCategory) live under `overrides.<kind>`. Bazarr has none of these
resources and is skipped.

## License

GPL-3.0-or-later (see LICENSE).

The service icons in `static/icons/` (Discord, Telegram, Slack, ntfy, Gotify, Pushover,
Apprise, webhook, PostgreSQL, Gluetun, WireGuard, Docker) come from
[dashboard-icons](https://github.com/homarr-labs/dashboard-icons) under the Apache License 2.0. The logos
are trademarks of their owners and only identify the service; see that folder's README.

The line icons in the sidebar and toolbars (`templates/_icons.html`) are from
[Lucide](https://lucide.dev) (ISC license).
