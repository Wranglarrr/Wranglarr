# Development

How the code is organized, how to build and test it, and how to add a service.
For running the stack, start at the [Quickstart](Quickstart.md).

## The repository

| Path | What it is |
| --- | --- |
| `compose*.yaml`, `.env.example`, `gate/`, `postgres/initdb/` | the stack ([Architecture](Architecture.md)) |
| `scripts/` | `init.sh` and the config patchers it runs (Python, in a `python:alpine` one-shot) |
| `wranglarr/` | the app: a Rust workspace, built into the image `ghcr.io/wranglarrr/wranglarr` |
| `tests/` | the black-box suite: Python, talks to a running wranglarr over HTTP only |
| `wiki/`, `README.md`, `site/` | the docs, and the site that renders them |
| `.github/workflows/` | `rust.yml` (checks and tests), `image.yml` (the image), `pages.yml` (the docs site), `wiki.yml` (copies `wiki/` to the GitHub wiki) |

## The app

One static binary with five names, picked by the name it runs as (the image
symlinks them) or by subcommand: `wranglarr [serve]`, `setup` (the first-run
wizard), `prepare` (the `wranglarr-init` one-shot: hands wranglarr's folders to
PUID:PGID), `auth` (recover the login from a shell) and `apps-auth` (sets the
apps' logins; run by `init.sh`). Two more subcommands are only for wranglarr's
own use: `self-update` (the helper container that replaces wranglarr's
container) and `health` (the probe it runs in the new one). Templates, static files and community configs
(`templates/`, `static/`, `configs/`) are embedded at build time.

The stack: axum 0.8 on tokio, minijinja for the pages, reqwest (rustls) for the
apps' APIs, a hyper client on a Unix socket for Docker, rusqlite for the state
database, the Postgres CLI tools (`pg_dump`, `pg_restore`, `psql`) through
`tokio::process`, croner for schedules, webauthn-rs for passkeys (TOTP is built in, on HMAC-SHA1).

### Three crates, in layers

`wranglarr/` is a Cargo workspace. Each crate reaches only the ones below it.

```
wranglarr            src/              jobs, web, setup, the binary
   │
wranglarr-services   crates/services/  what wranglarr knows about each kind of app
   │
wranglarr-core       crates/core/      plumbing that knows no kind of app
```

| Crate | Holds |
| --- | --- |
| `wranglarr-core` | `config` (wranglarr.toml as plain data, read in an environment the caller passes; kinds are checked through a `Kinds` trait the services implement), `state` (the SQLite state db), `store` (backup versions on disk), `files` (config snapshots and their copy rules), `docker`, `pg`, `owner` (PUID/PGID, `StackOwner`), `shared`, `registry` (image registries), `error`, `http` (timeouts, bounded bodies, redaction, polling), `value` (loose JSON, message helpers, and strict readers for what people send), `time` (the timestamp spellings) |
| `wranglarr-services` | the registry of services, the capability traits, one folder per kind, and the parts every service shares (loading wranglarr.toml against the registry, the home-page list types, the download clients' common types) |
| `wranglarr` | everything that works across apps: backups and restores (`jobs`), updates, Connect, indexers, notifications, the proxy configs, the setup wizard, auth, the web layer |

Inside the app crate, private aliases let its modules write `crate::config`,
`crate::service::get(…)` and so on. They aren't exported: the binary and the
integration tests import `wranglarr_core` and `wranglarr_services` by name.

**What the app crate exports.** Only what the binary and the integration tests
use is `pub`:
- the commands' entry points (`serve`, `setup`, `auth::cli`, `appsauth`, and
  the updates `self-update` and `health` mains);
- `App`, `Deployment` and `web::router`;
- the modules whose types the tests build or read: `auth`, `links`,
  `mediaservers`, `notify`, `proxy`, `setup` and `updates`.

The rest is `pub(crate)`, the web route modules are private, and community
exports only `Community` and `Request`. Keep it that way: an item becomes `pub`
when something outside the crate needs it, not by default. That way the
compiler reports dead code instead of hiding it.

### The app crate's modules

| Module | Does |
| --- | --- |
| `main`, `serve`, `app` | the entry point; the web service; `App`, the handle every module hangs its methods on |
| `deployment` | `Deployment`: the settings `.env` gives the container, read once where a command starts |
| `ids`, `format`, `background` | random ids and tokens; sizes, HTML escaping, percent-encoding; a subsystem's background tasks |
| `web/` | the router, pages, JSON API, templates, the CSRF/Origin guard |
| `auth/` | the admin account: password, TOTP, recovery codes, passkeys, API keys, sessions, the AuthTunnel gate |
| `jobs`, `sched` | the job worker (one for every suite: runs go one at a time; a run can be handed over to the wranglarr that starts next, which adopts it) and the cron schedules |
| `standalone` | the restore stack written into every version |
| `updates` | registry checks, recreate on a new image, rollback, holds; `updates/selfupdate/`: wranglarr replacing its own container through a helper, and the report the next wranglarr finishes the run from |
| `connect`, `arr_notifications`, `indexers` | wiring the apps together; the arrs' Connect entry; pushed indexers |
| `downloads`, `feeds`, `links` | the home page: the queues, the lists, where each app opens |
| `applogins`, `appsauth` | reading the apps' logins; `wranglarr-apps-auth` |
| `notify/`, `mediaservers`, `vpn`, `proxy`, `community/` | notifications, Plex/Jellyfin/Emby/Kodi, gluetun, reverse-proxy configs, community configs |
| `setup/` | `wranglarr-setup`, the first-run wizard |

## Services

wranglarr manages eleven kinds of app: six arrs (Radarr, Sonarr, Lidarr, Readarr,
Prowlarr, Whisparr), Bazarr, Seerr, and three download clients (qBittorrent,
SABnzbd, NZBGet). Everything it knows about a kind lives in that kind's folder in
`crates/services/src/`. The rest of the code asks what an app **can do**, never
which kind it is. `wranglarr/tests/kinds.rs` fails the build if a kind's id is
written as a string anywhere in `src/` or `crates/core/`.

```
crates/services/src/
  lib.rs          the registry: ALL, get(id), ServiceDef, Roles and the descriptor types
  caps.rs         Service, DownloadClient, Feed
  login.rs        Login, LoginApi: reading and setting an app's sign-in
  link.rs         LinkSource, LinkTarget, LinkHost: the apps that store Connect links
  client.rs       AnyClient, client(app)
  apps.rs         loading wranglarr.toml against the registry; AppConfigExt
  key_file.rs     KeyFile: where a config file keeps an API key, and the shared readers
  feed.rs         the home-page lists: settings, FeedQuery, FeedPage, Item, Lookups
  downloads.rs    what the download clients share: the Downloads list's entry
  file_app.rs     FileApp: an app whose config is a folder wranglarr copies (download clients, Seerr)
  arr/            the one arr implementation (client.rs, login.rs, shared.rs: the
                  [[shared]] resources' API bodies), driven by ArrDef
    radarr.rs  sonarr.rs  lidarr.rs  readarr.rs  prowlarr.rs  whisparr.rs   (data only)
  bazarr/  seerr/  qbittorrent/  sabnzbd/  nzbget/
    mod.rs        the descriptor, and its login, link and backup-payload code
    client.rs     its API client
crates/services/tests/
  clients.rs      each client against a small mock of its app
  descriptors.rs  each descriptor agrees with the client it builds
```

### The descriptor

Each kind is a `static ServiceDef`: plain data, also served to the front end as
the `SERVICES` JSON global (its `ServiceView`), so the pages and scripts keep no
tables of their own.

| Field | What it says |
| --- | --- |
| `id`, `label`, `description` | `"radarr"` (persisted, see below), `"Radarr"`, `"Movies"` |
| `roles` | `ARR`, `LIBRARY`, `INDEXERS`, `SUBTITLES`, `DOWNLOADS`, `REQUESTS`; modules filter on these |
| `home_group`, `color`, `primary_count`, `logo_paths`, `status_page` | how the home page shows it |
| `container` | image, ports, config mount, environment, whether it needs `/data` or can go in the VPN |
| `copy`, `backup` | what a config snapshot leaves out; `ApiZip` (the app's own backup) or `ConfigDir` (copy the folder with the container stopped) |
| `credentials` | an API key, with where its config file keeps it (`KeyFile`: an XML tag, a YAML or INI key, a JSON pointer), or a user and password (with where they're read from, and whether every request needs basic auth) |
| `database` | Postgres support: the environment the restore stack sets (`Database { env }`, with `{db}` and `{pg_host}` filled in), or `Database::IN_CONFIG` when the keys are in its config file |
| `proxy` | URL base handling, its own sign-in, Host checks, a warning for making it public |
| `wizard` | whether and where the setup wizard offers it |
| `takes` | the apps it links to (Bazarr takes Sonarr and Radarr) |
| `arr` | `ArrDef`, for the arrs: API version, library and scope endpoints, list shapes, events, community-config details |
| `downloads` | `DownloadsDef`, for the download clients: the arrs' implementation name, fields, categories, what a backup holds |
| `login` | `Login::Managed` (wranglarr reads and sets it, through the API or `.env`) or `Login::Own` (the app keeps its own accounts) |
| `feeds` | which home-page lists it has |
| `build` | makes its client from the app's config |

`service::ALL` is the registry and `service::get(id)` the lookup. Loading
`wranglarr.toml` rejects a kind that isn't in it.

### The capabilities

A client implements `Service` (async trait methods through `async-trait`, so they
work behind `dyn`):

- **Every service:** `ping`, `wait_ready`, `status`, `collection`, `scope`,
  `health`, `restart`, `capture_config` and `restore_config`.
- **Only when the kind has it:**
  - `arr()` gives the shared arr client: resources, Connect, indexers.
  - `download_client()` gives `DownloadClient`: `downloads`, `download_root`, `ensure_categories`.
  - `link_source()` gives `LinkSource`, the side of a Connect link that stores it (Bazarr, Seerr).
  - `feed()` gives `Feed`, the home page's lists: `list(kind, &FeedQuery, &Lookups)`
    returns a `FeedPage`, and refuses a list its descriptor's `feeds` doesn't name.

Each of these returns `None` by default. `AnyClient` wraps the client as an
`Arc<dyn Service>`, so adding a kind adds no enum variant and no match arm, and
nothing outside the services crate reaches for a concrete client: code that
needs what one kind has asks for the capability (`seerr_info` on the
Connections page uses `ping`, `status` and `LinkSource::accepts_links`).

The login is on the descriptor, not the client, because it is needed before a
client exists. `LoginApi` has `read` (the home page's App logins) and `set`
(`wranglarr-apps-auth`).

Logic that spans apps stays in the app crate and asks the registry. That covers
Connect's matrix, the merged download queue, indexer pushes, the VPN bypass rules,
backups and restores, the restore stack, the proxy configs and the updater's
ordering.

### Adding a service

**The Rust adapter** (all in `crates/services/`):

1. Copy the closest folder (a download client: `qbittorrent/`; an app with its own
   API: `bazarr/`) to `src/<id>/`.
2. Fill in its `DEF`. Use the roles it really has: those decide where it shows up.
   The data that used to need a central edit is the descriptor's now: where its
   config file keeps the API key (`Credentials::ApiKey { file: KeyFile::… }`) and
   the restore stack's Postgres environment (`Database { env: &[…] }`).
3. Write its client in `client.rs` and implement `Service`, plus the capabilities
   its roles and descriptor promise: `download_client()` for `DOWNLOADS`,
   `link_source()` when it `takes` apps, `feed()` for the lists in `feeds`.
4. Add `pub mod <id>;` and `&<id>::DEF` to `ALL` in `lib.rs`.
5. Test its client against a mock in `tests/clients.rs`. `tests/descriptors.rs`
   checks the descriptor against the client without changes: a role or list it
   declares and the client lacks fails there.
6. Add `static/icons/<id>.svg` (a test checks every service has one) and its wiki
   page.
7. If its image publisher documents release channels, add the exact repository
   and tags to `image_tracks.rs`, with a publisher documentation link. Do not
   infer channels from the application's branch names or another publisher.
   Unknown repositories still support custom update tags without presets.

An arr is a data file: copy `arr/radarr.rs`, fill in its `ArrDef`, and register it.

**The deployment**, outside the Rust adapter:

- **Compose:** its service in `compose.yaml` under a profile named for its id
  (`{ID}_PORT`, `{ID}_IMAGE`), the same in `compose.vpn-*.yaml` when it can run in
  the VPN (`container.vpn`), and its variables in `.env.example`.
- **Example config:** an `[[suite.app]]` entry in `wranglarr.example.toml`.
- **Setup:** the wizard offers it when `wizard` is set; a service with a user and
  password also needs its credential fields in `static/setup.js`.
- **Init:** `scripts/init.sh` creates the config before first start. A service
  whose config must be patched (Postgres keys, the shared `/data` paths) needs it in
  `scripts/patch_config.py` or `scripts/patch_downloads.py`, and a Postgres
  database in `postgres/initdb/`.
- **App logins:** a `Login::Managed` sign-in is set by `wranglarr-apps-auth`; an
  `Env` one also needs its `.env` variables.
- **A requests service:** the Connections page (`static/connections.js`) has a
  panel written for Seerr. Another requests app needs that panel made general, or
  its own.

### Spellings that must not change

These are persisted or external, so they stay exactly as they are:

- **Config:** `kind = "…"` in wranglarr.toml and in `[shared]` overrides.
- **Compose and `.env`:** service and profile names, `{ID}_PORT`, `{ID}_VPN` and the
  credential variables.
- **Backup manifests**, which are read years later: `apps.<name>.kind`, the collection
  and scope keys, and the `<app>/config/` layout.
- **Elsewhere in wranglarr:** `{kind}` in link patterns (it becomes a hostname),
  community history keys, icon file names, and the AuthTunnel gate's name-then-kind
  lookup.
- **Upstream APIs** that take these spellings: Bazarr's `use_sonarr`, Seerr's
  `settings/radarr`, the arrs' implementation names.

The state database's schema, the password hash format
(`scrypt$N$r$p$salt$hash`) and message wording the black-box suite checks are
kept too.

## Building and running

Rust 1.89 or newer, and OpenSSL's headers (webauthn-rs links it; the image links
it statically).

```sh
cd wranglarr
cargo build --release
WRANGLARR_CONFIG=./wranglarr.toml target/release/wranglarr
```

`pg_dump`, `pg_restore`, `psql` and `pg_isready` must be on `PATH`. The image:

```sh
docker build -t wranglarr:local wranglarr        # then WRANGLARR_IMAGE=wranglarr:local in .env
```

Under Podman, `podman build -t wranglarr:local` makes `localhost/wranglarr:local`,
but Compose (through Podman's Docker API) may look up `docker.io/library/wranglarr:local`.
Set `WRANGLARR_IMAGE=localhost/wranglarr:local` to be sure which one runs.

## Testing

```sh
cd wranglarr
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps   # links and markup in doc comments
cargo test --workspace
cargo deny check                         # licences, advisories, sources
```

- **Unit tests** sit in a `#[cfg(test)]` module at the end of each file.
- **Integration tests** in `wranglarr/tests/` drive the app crate in-process
  against small mock servers. Some compare with recorded expected answers in
  `tests/fixtures/` (setup files, proxy configs, apps-auth runs, Community steps);
  when an intended change alters one, edit the fixture and review each changed line.
- **`tests/kinds.rs`** is the layering guard above, plus the icon check.
- **The black-box suite** in the repo's `tests/` starts the built binary against
  mock Docker, mock arrs and download clients and fake Postgres tools, and talks to
  it only over HTTP. It is the behaviour contract: see [tests/README.md](../tests/README.md).

```sh
cargo build --manifest-path wranglarr/Cargo.toml        # from the repo root
python3 -m venv tests/.venv && tests/.venv/bin/pip install -r tests/requirements.txt   # once
tests/.venv/bin/python -m pytest tests
```

CI (`rust.yml`) runs all of the above on pushes to `main` and on pull requests that
touch `wranglarr/` or `tests/`, plus `cargo check` on the minimum Rust version. The
weekly and manual runs, and every release tag, add the slow black-box tests
(`WRANGLARR_TEST_SLOW=1`).

## Conventions

- `#![forbid(unsafe_code)]`, and clippy with `-D warnings`.
- The apps' APIs are large and change between versions, so wranglarr passes them
  through as `serde_json::Value`. What wranglarr owns should be typed: its config,
  settings, state rows and API request bodies are; backup manifests, restore plans
  and job results are still `Value`s.
- The environment is read where a command starts, and passed in from there.
  `Deployment::read` collects the deployment settings (`WRANGLARR_CONFIG`,
  `WRANGLARR_LOG`, the login and AuthTunnel variables, `FORWARDED_ALLOW_IPS`,
  `WRANGLARR_HOST_BACKUPS`, `WRANGLARR_IMAGE`, what a restore stack copies,
  `PUID`/`PGID`, `UMASK`, `WRANGLARR_PRE_RESTORE`, and the updater's
  `WRANGLARR_UPDATE_POLL`, `WRANGLARR_UPDATE_STABLE` and `HOSTNAME`, and
  `WRANGLARR_VPN_PORT_POLL`, how often the forwarded port is checked), and
  `service::apps::load(path, env)` reads wranglarr.toml with a variable lookup
  (`${VAR}`, `WRANGLARR_ENABLED_APPS`, `WRANGLARR_VPN_APPS`).
  `App::new(cfg, deployment)` takes both, so tests pass their own instead of
  setting process variables.
- Each shared helper has one home. Timestamps come from `time`, in one of three
  spellings named for where they are kept (`iso`, `iso_micros`, `iso_z`); keep a
  stored value's spelling when you touch it. Random ids and tokens come from
  `ids`. Request bodies, query strings and `.env` flags are read with
  `value::flag_word`, `value::as_flag` and `value::as_whole`, so every input takes
  the same spellings; the apps' answers are read leniently with `value::truthy`,
  `value::text`, `value::int` and the rest.
- Background work belongs to a subsystem: a `Background` (or the scheduler, or the
  job runner) owns each task, and `App::shutdown` stops them. No detached
  `tokio::spawn`.
- One error type, `error::Error`. The web layer answers it as `{"error": "…"}` with
  the matching status ([HTTP API](API.md#errors)).
- Anything that reaches an app is bounded: timeouts on every request, a 16 MiB cap
  on bodies, and credentials redacted from errors and logs.
- Files wranglarr writes are written atomically. Paths from a manifest are checked
  before use: they must stay inside the version and follow no symlinks.

## Releases

Images are built and published by CI (`.github/workflows/image.yml`) for amd64
and arm64: `:edge` from `main`, `:X.Y.Z`, `:X.Y`, `:X` and `:latest` from a
release tag. To smoke-test an image you built yourself:
`scripts/smoke-image.sh wranglarr:local 1.0.0` (`DOCKER=podman` for Podman).
