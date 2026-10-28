# Setup

First time? The [Quickstart](Quickstart.md) walks through all of this with screenshots. Already running the apps somewhere? Read [Bringing in existing apps](Importing-Existing-Apps.md) before `init.sh`. On TrueNAS SCALE, read [TrueNAS SCALE](TrueNAS.md) first.

## 1. Choose how to create `.env`

### The wizard (recommended)

```sh
docker run --rm -it -p 8788:8788 \
  -v "$PWD":/stack \
  -v /mnt/data:/data \
  ghcr.io/wranglarrr/wranglarr:latest wranglarr-setup   # open the link it prints
```

That runs the published image. To build your own instead, see
[The wranglarr image](#the-wranglarr-image).

PowerShell: `-v ${PWD}:/stack -v D:\media:/data`. Mount the drive your
`DATA_ROOT` will point at as `/data` so the wizard can check it; add
`-v <dir>:/data2` for a second drive, and `-v <dir>:/appdata` only if
`APPDATA` is an absolute path outside the stack directory. Pass `-e TZ=…` to
prefill the timezone.

The wizard runs from the wranglarr image, so it needs nothing but Docker. It
writes into `/stack` (your directory) and never anywhere else. Paths you type
are **host** paths and go into `.env` verbatim; the checks run against the
mounts, which is why `/data` should be mounted. Files it writes are owned by
the PUID/PGID you choose.

It walks through:

1. **Services** – which profiles to enable.
2. **System** – timezone, the PUID/PGID to run as (on Linux: `id` on the host; the Storage step shows who owns the mounted drive), UMASK.
3. **Storage** – `APPDATA`, `DATA_ROOT`, `DATA_ROOT_2`; each is checked (through its mount) for existence, owner, mode, whether PUID/PGID can write it and free space; a relative `APPDATA` can be created from the page, anything else tells you the `mkdir` or the `-v` to add.
4. **Credentials** – Postgres user/password, gluetun API key, default backup schedule and retention.
5. **App logins** – one login for every app (generated password), AuthTunnel (and whether you reach the apps through a reverse proxy, with its domain), or leave them to you; for the last, the NZBGet and qBittorrent logins. See [App logins](App-Logins.md).
6. **Network & VPN** – published ports; optional wranglarr URL base (`WRANGLARR_URL_BASE`, e.g. `/wranglarr`, blank at the root; see [path hosting](Reverse-Proxy.md#wranglarr-at-a-path-on-the-domain)); this machine's IP (`HOST_IP`: filled in when you opened the wizard by IP, else `./scripts/init.sh` finds it; change it only if this `.env` goes to another machine); VPN mode none / shared / per app; provider, protocol and keys. WireGuard keys must be valid 32-byte base64, the endpoint an IP, networks valid CIDRs.
7. **Existing backups** – scans `APPDATA` for wranglarr versions (tick to restore after install) and app-native backup zips (choose one to reuse its config file). Also flags SQLite databases that will need pgloader.
8. **Review & write** – shows the `.env` (secrets masked), writes it with mode 600 (an existing one is kept as `.env.bak-<timestamp>`), seeds `appdata/wranglarr/wranglarr.toml`, writes `restore-plan.json`, copies chosen legacy config files, then lists the manual steps left.

### By hand

```sh
cp .env.example .env
$EDITOR .env        # at least: DATA_ROOT, POSTGRES_PASSWORD, TZ, PUID/PGID, COMPOSE_PROFILES
```

## 2. `./scripts/init.sh`

What it does, in order (re-running is safe):

1. Reads `COMPOSE_PROFILES` and decides which config.xml apps (radarr/sonarr/lidarr/prowlarr) and download clients (qbittorrent/sabnzbd/nzbget) are enabled.
2. For each config.xml app **without** an `appdata/<app>/config.xml`: starts it alone via `compose.init.yaml` (no Postgres), waits until `/ping` answers and `<ApiKey>` is in the file, stops it. Each download client without its config file (`qBittorrent.conf`, `sabnzbd.ini`, `nzbget.conf`) is started once the same way so it writes its defaults.
3. Creates the download folders under `DATA_ROOT`: `torrents/`, `torrents/incomplete/` and `torrents/<arr>` for qBittorrent; `usenet/incomplete/`, `usenet/complete/` and `usenet/complete/<arr>` for SABnzbd/NZBGet, one `<arr>` folder per enabled arr and Prowlarr (paths from `TORRENTS_DIR` / `USENET_DIR`).
4. Stops the enabled apps (they rewrite their config on shutdown and would undo the patch), then runs `patch-config` (a python:alpine one-shot):
   - inserts `PostgresHost`, `PostgresPort`, `PostgresUser`, `PostgresPassword`, `PostgresMainDb`, `PostgresLogDb` into every config.xml. The arrs never write these keys themselves, so this step is what moves them onto Postgres.
   - points the download clients' save paths at the shared `/data` mount, only where they are still at the image's defaults: qBittorrent `/data/torrents/` with automatic torrent management, SABnzbd and NZBGet `/data/usenet/incomplete` and `/data/usenet/complete`. SABnzbd also gets `nzb_backup_dir` and `sabnzbd` in its `host_whitelist`. Details on [qBittorrent](Service-qBittorrent.md), [SABnzbd](Service-SABnzbd.md), [NZBGet](Service-NZBGet.md).

   - with `APPS_AUTH` `apps` or `tunnel`: writes qBittorrent's web login from `QBITTORRENT_USER` / `QBITTORRENT_PASSWORD` into `qBittorrent.conf` (tunnel: plus the whitelist that lets wranglarr-gate in).

   Idempotent; a path you set yourself is never overwritten.
5. With `APPS_AUTH=tunnel`: writes `compose.authtunnel.yaml` and `appdata/gate/gate.conf.template`, and adds the first to `COMPOSE_FILE` in `.env` (other modes remove them).
6. Seeds `appdata/wranglarr/wranglarr.toml` from `wranglarr/wranglarr.example.toml` if missing. When the file exists and `seerr` is enabled but not in it, appends a `requests` suite with Seerr. With `seerr` on it also hands `appdata/seerr` to `PUID:PGID` (Seerr's image has no `PUID` of its own).
7. Starts Postgres and runs `postgres/initdb/01-create-arr-dbs.sh` in it, so an app added to `COMPOSE_PROFILES` since the last run gets its databases (`<app>` and `<app>-log` per arr, `bazarr`, `seerr`; on a new cluster the entrypoint does this on first start). Then `docker compose up -d --build`. Each app creates its own schema on first connect.
8. Unless `APPS_AUTH=manual`: runs the `wranglarr-apps-auth` one-shot, which sets each app's login through its API and signs in to check ([App logins](App-Logins.md)).

`./scripts/init.sh --auth` runs only the app-login parts (after
`wranglarr-setup --auth` changed them), in the order that never leaves an app
open.

Bazarr needs no patching: it reads its Postgres settings from the
environment. The download clients don't use Postgres.

Downloads land at the same `/data` path the arrs see, so imports hardlink and
no remote path mapping is needed.

**Rootless Podman** maps your host user to root inside the containers, so
folders you create on the host are root-owned in there and the apps (running
as `PUID`) can't write them. When `init.sh` runs under rootless Podman it
hands the folders it created to `PUID:PGID` with `podman unshare chown`. For
folders made some other way: `podman unshare chown -R "$PUID:$PGID" "$DATA_ROOT"`.

**Run as root** (`sudo ./scripts/init.sh`, as on [TrueNAS SCALE](TrueNAS.md)),
the download folders it creates would be root-owned, so `init.sh` hands the
new ones to `PUID:PGID`. Folders that already existed are left as they are.

The wizard's storage check also warns when `torrents`, `usenet` or `media`
inside `DATA_ROOT` are separate filesystems (ZFS child datasets, other
mounts). Hardlinks can't cross filesystems, so imports would become copies.

## 3. Afterwards

```sh
docker compose up -d        # start / apply .env changes
docker compose down         # stop
docker compose logs -f radarr
```

Open wranglarr on `WRANGLARR_PORT` (8787). If the wizard wrote a restore plan,
a banner offers to run it once the apps are up.

On its first start wranglarr wires the apps together once they answer: every
download client into every arr and Prowlarr (category = the app's name), Sonarr and
Radarr into Bazarr, every arr into Prowlarr. See [App
Connections](App-Connections.md). With `APPS_AUTH=manual`, set the qBittorrent
password first (see [Service: qBittorrent](Service-qBittorrent.md)), or press
**Connect apps** on the System page afterwards. Root folders and indexers are still yours to add
(indexers in Prowlarr, or on the [Indexers](Indexers.md) page).

## Users and permissions

Every app runs as `PUID:PGID`, and everything it creates on `/data` gets that
owner. So the choice decides who can manage your media later.

- **Not root.** `PUID=0` or `PGID=0` runs every app as root: your media fills up with root-owned files, and a compromised app can change anything it can see. The wizard and `init.sh` warn about it.
- **One user for every app**, so Radarr can rename what qBittorrent downloaded and Bazarr can write subtitles next to it. That's the default: one `PUID`/`PGID` in `.env` for the whole stack.
- **A shared group** if other people or services (an SMB share, Plex, your own login) manage the same files: make `PGID` that group, and keep `UMASK=002` so new files are group-writable. Existing files: `sudo chown -R "$PUID:$PGID" "$DATA_ROOT" && sudo chmod -R g+w "$DATA_ROOT"`.
- **TrueNAS SCALE:** the built-in `apps` user, `568:568`, or your own user. See [TrueNAS SCALE](TrueNAS.md#1-datasets).
- **Make a dedicated user** rather than using your own login: [Security → A service user for the stack](Security.md#a-service-user-for-the-stack) has the commands for each OS.

wranglarr runs as `PUID:PGID` too. Its folders are prepared by the one-shot
`wranglarr-init` container, and it reaches the Docker socket through the
socket's group (`DOCKER_GID`, which `init.sh` writes). How to create a service
user on each OS, and what the Docker socket and wranglarr's open web UI
expose: [Security](Security.md).

## Opting services in and out

`COMPOSE_PROFILES` is the single list that decides what exists:

```
COMPOSE_PROFILES=radarr,sonarr,lidarr,prowlarr,bazarr,qbittorrent,sabnzbd,nzbget
```

It is also passed to Postgres (which databases to create) and to wranglarr
(`WRANGLARR_ENABLED_APPS`: apps not in the list are dropped from its suites).
To add an app later: add it to the list, run `./scripts/init.sh` again (it
only initialises apps that lack a config), done; wranglarr wires the new app
into the others on its next start. `init.sh` also creates the new app's
databases in the existing cluster; by hand:

```sh
docker compose exec postgres /docker-entrypoint-initdb.d/01-create-arr-dbs.sh
```

## Changing the Postgres password

Postgres keeps its own copy of the password, so change it there first, then in
`.env`, then let `init.sh` hand it to the apps:

```sh
# 1. in Postgres: type \password, then the new password twice, then \q
docker compose exec postgres sh -c 'psql -U "$POSTGRES_USER" -d postgres'
# 2. put the same password in .env
$EDITOR .env          # POSTGRES_PASSWORD=…
# 3. stops the arrs, rewrites their config.xml, and recreates everything else
./scripts/init.sh
```

Doing step 2 without step 1 locks every app out of its database. Put the old
password back in `.env` and run `./scripts/init.sh` again to recover.

## Apps you already run

Moving a Radarr, Sonarr, Prowlarr, Bazarr or Seerr you already have into the
stack, with its library, history and settings: [Bringing in existing
apps](Importing-Existing-Apps.md). In short: its config folder goes into
`appdata/<app>/` before `init.sh`, then one command per app copies its SQLite
database into Postgres, and you point its paths and connections at the stack.

## The wranglarr image

wranglarr, its setup wizard and its two one-shot helpers (`wranglarr-init`,
`wranglarr-apps-auth`) all run from one image. `compose.yaml` takes its name
from `WRANGLARR_IMAGE` in `.env`, and the wizard writes there the image it was
started from.

**The published image** (the default) is built from this repository for
amd64 and arm64:

| Tag | What it is |
| --- | --- |
| `ghcr.io/wranglarrr/wranglarr:latest` | the latest release |
| `ghcr.io/wranglarrr/wranglarr:1`, `:1.0`, `:1.0.0` | a release line, or one exact release |
| `ghcr.io/wranglarrr/wranglarr:edge` | the main branch, untested between releases |

Updating it: when a newer image is out, the dashboard and the Updates page
offer **Update now**, and you can let it update itself on a schedule
([Keeping wranglarr itself up to date](Updates.md#keeping-wranglarr-itself-up-to-date)).
From the shell: `docker compose pull wranglarr && docker compose up -d wranglarr`.

**Your own build**, from the checkout:

```sh
docker build -t wranglarr:local ./wranglarr
docker run --rm -it -p 8788:8788 -v "$PWD":/stack -v /mnt/data:/data \
  wranglarr:local wranglarr-setup
```

Run that way, the wizard writes `WRANGLARR_IMAGE=wranglarr:local`. To switch an
existing stack, set it in `.env` yourself. `init.sh` builds `wranglarr:local`
when it's missing. After pulling new code, build again and
`docker compose up -d wranglarr`.

