# Service: wranglarr

| | |
| --- | --- |
| Image | built from `./wranglarr` (one static Rust binary on `alpine` + `postgresql17-client`) |
| Container | `wranglarr` |
| Profile | always on |
| Port | `${WRANGLARR_PORT}`:8787 |
| Volumes | `appdata/wranglarr:/config`, `appdata/backups:/backups`, `appdata/<app>:/arrconfig/<app>` for every app (arrs read-only; Bazarr and download clients read-write), `/var/run/docker.sock` |

## Environment

| Variable | Value | Purpose |
| --- | --- | --- |
| `TZ` | from `.env` | cron schedules are interpreted in this zone |
| `PUID`, `PGID` | from `.env` | the user wranglarr runs as (`user:` in compose.yaml); `wranglarr-init` hands `/config` and `/backups` to it. Started as root without `user:`, wranglarr hands them over itself and switches to this user. See [Security](Security.md) |
| `UMASK` | from `.env` | permissions of the files wranglarr creates |
| `WRANGLARR_AUTH` | from `.env`, default `on` | the [login](Login.md); `off` turns it off |
| `DOCKER_GID` | from `.env` (`init.sh`) | added as a group so the non-root process can use the Docker socket |
| `POSTGRES_USER`, `POSTGRES_PASSWORD` | from `.env` | expanded into `wranglarr.toml` via `${…}` |
| `GLUETUN_API_KEY` | from `.env` | same, for `[vpn]` |
| `QBITTORRENT_USER`, `QBITTORRENT_PASSWORD` | from `.env` | same, for the `qbittorrent` app |
| `NZBGET_USER`, `NZBGET_PASS` | from `.env` | same, for the `nzbget` app (the NZBGet image applies them at launch, `nzbget.conf` keeps its default) |
| `DOCKER_SOCKET` (host side) | `/var/run/docker.sock` | which host socket is mounted; Podman: `init.sh` sets it to Podman's own socket (rootless: `$XDG_RUNTIME_DIR/podman/podman.sock`) |
| `WRANGLARR_ENABLED_APPS` | `${COMPOSE_PROFILES}` (+`,vpn` under `compose.vpn.yaml`) | apps not listed are dropped from the suites; `vpn` enables the VPN status panel |
| `WRANGLARR_VPN_APPS` | set by `compose.vpn*.yaml` | the services in the shared tunnel: App Connections uses `localhost` only between those (without the Docker socket), and the dashboard warns about indexer traffic around the tunnel |
| `WRANGLARR_CONFIG` | `/config/wranglarr.toml` | config path |

**Secrets from files.** As in the LinuxServer.io images, a secret can come from
a file: `FILE__POSTGRES_PASSWORD=/run/secrets/postgres_password` sets
`POSTGRES_PASSWORD` to the file's contents (trailing newlines dropped), for
Docker and Compose secrets. The file wins over the variable itself. It works for
every `${…}` in `wranglarr.toml` (the passwords and keys above) and for
wranglarr's own settings such as `WRANGLARR_AUTH`.

## `wranglarr.toml`

Seeded from `wranglarr/wranglarr.example.toml` by `init.sh`/the wizard. `${VAR}`
in any string is expanded from the container environment.

App names must be unique across all suites, including disabled apps. They are
the identity used for connections, updates and global lookups. Malformed
sections, string lists, yes/no settings and fractional integer settings are
rejected at startup instead of silently falling back to defaults.

| Section | Keys | Notes |
| --- | --- | --- |
| `[server]` | `bind` (`0.0.0.0`), `port` (`8787`), `log_level` | |
| `[storage]` | `root` (`/backups`), `state_db` (`/config/wranglarr.db`) | versions live under `root/<suite>/<timestamp>/`; settings overrides, run history, Indexers page sources (API keys included) and app-connection state in the SQLite state db; `restore-plan.json` beside it |
| `[postgres]` | `host`, `port`, `user`, `password`, `container` (`arrs-postgres`) | for `pg_dump` / `pg_restore` / sizes; `container` for updates |
| `[docker]` | `socket` | optional; enables stop/start around restores and [container updates](Updates.md) |
| `[vpn]` | `control_url` (`http://vpn:8000`), `api_key`, `name`, `container` (`arrs-vpn`), `apps` (when `WRANGLARR_VPN_APPS` isn't set) | gluetun control server; only active when `vpn` is in `WRANGLARR_ENABLED_APPS` |
| `[updates]` | `enabled`, `check_schedule`, `auto_update`, `auto_schedule`, `backup_first`, `self_update` (`false`), `self_schedule` (`30 5 * * *`), `health_timeout`, `include_postgres`, `include_vpn`, `extra`, `exclude` | [Container Updates](Updates.md); `self_update` updates wranglarr itself on `self_schedule` ([Keeping wranglarr itself up to date](Updates.md#keeping-wranglarr-itself-up-to-date)) |
| `[connect]` | `auto` (`true`) | at startup, wire the links never made before: download clients into the arrs, Sonarr/Radarr into Bazarr, the arrs into Prowlarr. [App Connections](App-Connections.md) |
| `[[suite]]` | `name`, `schedule` (cron), `enabled`, `keep_last`, `keep_days` | a group of apps backed up together; the UI can override schedule/retention (stored in the state db, "reset to config file" undoes it) |
| `[[suite.app]]` | `name`, `kind`, `url`, `api_key` or `api_key_file`, `username`/`password`, `databases`, `container`, `config_dir`, `enabled` | see below |
| `[[shared.download_client]]`, `[[shared.notification]]` | `name`, `implementation`, `fields`, `overrides.<kind>`, `apply_to`, `test`, any top-level resource key | [Shared Settings](Shared-Settings.md) |

### App kinds

| `kind` | API key source | `databases` | `config_dir` | `container` |
| --- | --- | --- | --- | --- |
| `radarr`, `sonarr`, `lidarr`, `readarr`, `prowlarr`, `whisparr` | `api_key_file` = config.xml (`<ApiKey>`) | `<app>`, `<app>-log` | optional (defaults to the folder holding `config.xml`); copied into each version as `<app>/config/` | optional: stop/start on restore, updates |
| `bazarr` | `api_key_file` = config/config.yaml (`apikey:`) | `bazarr` | **required** (zips, config restore, plain copy) | optional |
| `qbittorrent` | `username` / `password` | none | **required** | **required** for restore |
| `sabnzbd` | `api_key_file` = sabnzbd.ini (`api_key =`) | none | **required** | **required** for restore |
| `nzbget` | `username` / `password`, or `api_key_file` = nzbget.conf | none | **required** | **required** for restore |
| `seerr` | `api_key_file` = settings.json (`main.apiKey`) | `seerr` | **required** (plain copy) | optional |

## HTTP API

Everything the UI does is a JSON call. Every endpoint, with authentication, request bodies
and the job model, is in the [API reference](API.md).

## Commands in the image

| Command | |
| --- | --- |
| `wranglarr` | the service |
| `wranglarr-setup` | the first-run wizard ([Setup](Setup.md)); `--auth`: only the app-logins step, changing only those keys in `.env` |
| `wranglarr-apps-auth` | what the `wranglarr-apps-auth` one-shot runs for `init.sh`: sets the apps' logins the way `APPS_AUTH` says ([App logins](App-Logins.md)) |
| `wranglarr-prepare` | what `wranglarr-init` runs: hands `/config` and `/backups` to `PUID:PGID` ([Security](Security.md)) |
| `wranglarr-auth` | recover the login from a shell: `status`, `set-password`, `disable-2fa`, `remove-passkeys`, `sign-out-all`, `reset` ([Login](Login.md#locked-out)) |
| `proxy-setup` (or `wranglarr-proxy-setup`) | print the reverse proxy config for the saved settings, for `docker compose exec -T wranglarr proxy-setup > file`; `--kind nginx` for another proxy ([Reverse proxy](Reverse-Proxy.md#from-a-shell)) |
| `wranglarr --version` | the version |

## The setup wizard

Same image, different command:

```sh
docker run --rm -it -p 8788:8788 -v "$PWD":/stack -v /mnt/data:/data wranglarr wranglarr-setup
```

| Mount / flag | Purpose |
| --- | --- |
| `-v "$PWD":/stack` | the stack directory (read-write): `.env`, `appdata/wranglarr/`, legacy config copies land here |
| `-v <DATA_ROOT>:/data` | lets the wizard verify the data drive (owner, writability, free space) |
| `-v <dir>:/data2` | same for `DATA_ROOT_2` |
| `-v <dir>:/appdata` | only when `APPDATA` is an absolute path outside the stack dir |
| `-e TZ=…` | prefills the timezone |
| `-p 8788:8788` | the wizard's port (`127.0.0.1:8788:8788` keeps it local) |

It prints a one-time link, `http://localhost:8788/?t=<code>`. Only a browser
that opened it can use the wizard. Set `-e WRANGLARR_SETUP_TOKEN=…` to choose
the code yourself.

Flags: `--target` (default `/stack`), `--port` (8788), `--bind` (`0.0.0.0`
in the container). It writes only into `/stack` (and `/appdata` if mounted),
and the files it creates are owned by the PUID/PGID you chose.

## Running outside Docker


```sh
cd wranglarr && cargo build --release                          # Rust 1.89+, and OpenSSL's headers
WRANGLARR_CONFIG=./wranglarr.toml target/release/wranglarr     # the app
target/release/wranglarr setup --target ..                      # the setup wizard, natively
```

`pg_dump`, `pg_restore`, `psql`, `pg_isready` must be on `PATH`.
