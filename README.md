<h1 align="center"><img src="wiki/images/brand/wordmark.png" alt="wranglarr" width="420"></h1>

<p align="center"><b>Your whole *arr stack on Postgres, with backups you can actually restore.</b><br>
Radarr, Sonarr, Lidarr, Prowlarr, Bazarr, Seerr and your download clients in one Docker Compose stack,<br>
plus the app that sets it up, wires it together, backs it up and keeps it updated.</p>

<p align="center">
  <a href="https://wranglarrr.github.io/Wranglarr/"><b>Docs site</b></a> ·
  <a href="wiki/Quickstart.md"><b>Quickstart</b></a> ·
  <a href="wiki/Home.md"><b>Wiki</b></a> ·
  <a href="#features"><b>Features</b></a> ·
  <a href="wiki/API.md"><b>API</b></a> ·
  <a href="wiki/Troubleshooting.md"><b>Troubleshooting</b></a> ·
  <a href="wiki/Release-Notes.md"><b>Release notes</b></a> ·
  <a href="ROADMAP.md"><b>Roadmap</b></a>
</p>

<p align="center">
  <img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue">
  <img alt="Docker Compose" src="https://img.shields.io/badge/Docker_Compose-2.24%2B-2496ED?logo=docker&logoColor=white">
  <img alt="Podman" src="https://img.shields.io/badge/Podman-rootless-892CA0?logo=podman&logoColor=white">
  <img alt="TrueNAS SCALE" src="https://img.shields.io/badge/TrueNAS_SCALE-24.10%2B-0095D5?logo=truenas&logoColor=white">
  <img alt="Postgres 17" src="https://img.shields.io/badge/Postgres-17-4169E1?logo=postgresql&logoColor=white">
  <img alt="Status: pre-release" src="https://img.shields.io/badge/status-pre--release_1.0.0-e5a00d">
</p>

<p align="center"><img src="wiki/images/readme/hero.png" alt="The wranglarr dashboard, half in the light theme and half in the dark theme" width="100%"></p>
<p align="center"><sub>The dashboard: every app with its stats, what's coming up, what's downloading and what's been requested. Light and dark follow your system, or flip them with one click.</sub></p>

## Why wranglarr

- **Backups that restore.** Every app's database and config, versioned on a schedule. Each version is a complete stack you can start with plain `docker compose up`, even without wranglarr.
- **Postgres done right, for you.** The arrs, Prowlarr, Bazarr and Seerr run on one Postgres 17, set up the way their wikis describe, with nothing to edit by hand.
- **Wired on first start.** Download clients go into every arr, the arrs into Prowlarr, Bazarr and Seerr. Each link is tested before it's saved.
- **One place to run it.** Updates, indexers, notifications, media servers, reverse-proxy config and app logins, behind a login with two-factor and passkeys.

## Features

<table>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/suite-media-dark.png"><img src="wiki/images/readme/suite-media-light.png" alt="A backup suite with its schedule, retention and versions"></picture>
<h3>Scheduled, versioned backups</h3>
Databases (<code>pg_dump</code>), each app's own config backup and a plain copy of <code>/config</code>, grouped into suites. Cron schedule, keep-last / keep-days retention, size history, and what changed since the last version (an app upgraded, +12 movies).
<br><a href="wiki/Backups-and-Restore.md">Backups and restore →</a>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/version-dark.png"><img src="wiki/images/readme/version-light.png" alt="One backup version with per-app restore buttons"></picture>
<h3>Restore per app, or without wranglarr</h3>
Restore an app's databases, its config or both, with checksums verified first. Every version also carries a pinned <code>compose.yaml</code> that restores Postgres on first start, so it runs on its own.
<br><a href="wiki/Backups-and-Restore.md#without-wranglarr">Running a version on its own →</a>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/updates-dark.png"><img src="wiki/images/readme/updates-light.png" alt="The Updates page listing every container and its image status"></picture>
<h3>Container updates with a safety net</h3>
Checks the registries without pulling. Choose an update track per container, including wranglarr's development builds. An update backs up first, recreates the container with the same settings, waits for it to be healthy and puts the old one back if it isn't. Rollback, holds, scheduled auto-update, and no surprise Postgres major upgrades.
<br><a href="wiki/Updates.md">Container updates →</a>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/wizard-dark.png"><img src="wiki/images/readme/wizard-light.png" alt="The setup wizard's service picker"></picture>
<h3>A setup wizard, not a wall of YAML</h3>
One <code>docker run</code> opens a web wizard: pick services, users, storage (checked for ownership, free space and hardlinks), VPN and logins. It writes your <code>.env</code>, then <code>init.sh</code> builds the rest.
<br><a href="wiki/Quickstart.md">Quickstart →</a>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/configs-dark.png"><img src="wiki/images/readme/configs-light.png" alt="Importing a community config with a preview of the changes"></picture>
<h3>Community configs</h3>
Custom formats, quality profiles, sizes and release profiles for one job ("x265 first, smallest size", "anime, English dub only"), shared as JSON. See what changes in each arr before applying, remove it again later, test a release name, or export your own.
<br><a href="wiki/Community-Configs.md">Community configs →</a>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/notifications-dark.png"><img src="wiki/images/readme/notifications-light.png" alt="Notification channels and what wranglarr watches"></picture>
<h3>Notifications that matter</h3>
Telegram, Discord, Slack, ntfy, Gotify, Pushover, Apprise, email or a webhook. Failed backups, apps offline, VPN down, low disk, stuck downloads, imports that need you. Each problem once, and again when it clears.
<br><a href="wiki/Notifications.md">Notifications →</a>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/connections-dark.png"><img src="wiki/images/readme/connections-light.png" alt="Media server and Seerr connections"></picture>
<h3>Media servers and Seerr</h3>
Plex (sign in with Plex), Jellyfin, Emby and Kodi are added to the arrs so the library rescans after each import, with path mapping. Seerr gets every Radarr and Sonarr once its own setup is done.
<br><a href="wiki/Connections.md">Connections →</a>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/indexers-dark.png"><img src="wiki/images/readme/indexers-light.png" alt="The Indexers page with Prowlarr's registrations"></picture>
<h3>Indexers in one place</h3>
Prowlarr is registered in every arr for you. Trackers, usenet indexers, Jackett, NZBHydra2 or a Prowlarr outside the stack go to the arrs you pick, and come out again when you remove them.
<br><a href="wiki/Indexers.md">Indexers →</a>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/proxy-dark.png"><img src="wiki/images/readme/proxy-light.png" alt="The reverse-proxy config generator"></picture>
<h3>Reverse-proxy config, written for you</h3>
Caddy, nginx, Traefik, caddy-docker-proxy, Cloudflare Tunnel or Nginx Proxy Manager, generated from the stack as it is. Each app public, LAN only or not proxied, at a name or a path.
<br><a href="wiki/Reverse-Proxy.md">Reverse proxy →</a> · <a href="wiki/Remote-Access.md">Tailscale and Cloudflare →</a>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="wiki/images/readme/account-dark.png"><img src="wiki/images/readme/account-light.png" alt="Account page with password, two-factor, passkeys and API keys"></picture>
<h3>A real login, built in</h3>
Password, two-factor (any authenticator app) with recovery codes, and passkeys, with no identity provider to run. API keys for scripts. Optional <b>AuthTunnel</b> puts every app behind the same sign-in.
<br><a href="wiki/Login.md">Login →</a> · <a href="wiki/App-Logins.md">App logins →</a>
</td>
</tr>
</table>

### Light, dark, and your phone

<table>
<tr>
<td align="center" width="25%"><img src="wiki/images/readme/mobile-light.png" alt="The dashboard on a phone, light theme" width="200"></td>
<td align="center" width="25%"><img src="wiki/images/readme/mobile-dark.png" alt="The dashboard on a phone, dark theme" width="200"></td>
<td valign="middle">
The UI looks like the arrs you already know: dark header, sidebar, page toolbar. The theme follows your system, or switch it from the header. On a phone the sidebar becomes a menu drawer.
<br><br>
<a href="wiki/images/readme/dashboard-light.png">Full dashboard, light</a> · <a href="wiki/images/readme/dashboard-dark.png">Full dashboard, dark</a>
</td>
</tr>
</table>

### Also in the box

| | |
| --- | --- |
| **VPN** | One gluetun tunnel (WireGuard or OpenVPN, any provider) for the download clients, Prowlarr or every app, with a kill switch. Tunnel state, public IP and forwarded port in the header. Or a tunnel per app with hotio images. [Shared tunnel](wiki/VPN-Shared-Tunnel.md) · [Per app](wiki/VPN-Per-App.md) |
| **App connections** | Every download client in every arr and in Prowlarr (category = the app's name, folders created first), Sonarr and Radarr in Bazarr, every arr in Prowlarr. [App connections](wiki/App-Connections.md) |
| **Live status** | The arrs and Prowlarr tell wranglarr when something changes, and open pages update on their own. Every app's API key is on one page, and a key changed by hand is picked up: the apps using it are reconnected. [Live status](wiki/Live-Status.md) |
| **App logins** | The same login set in every app, or AuthTunnel: sign in once to wranglarr and the apps let you through. API keys still pass, so the arrs' mobile apps keep working. [App logins](wiki/App-Logins.md) |
| **Shared settings** | Define a download client, indexer or notification once and push it to every arr by name. [Shared settings](wiki/Shared-Settings.md) |
| **Bring your apps** | Move the Radarr, Sonarr, Prowlarr, Bazarr or Seerr you already run into the stack and keep the library and history. [Bringing in existing apps](wiki/Importing-Existing-Apps.md) |
| **HTTP API** | Everything the UI does is a JSON call, with API keys for scripts and cron jobs. [API reference](wiki/API.md) |
| **Locked down** | wranglarr runs as your user with no capabilities on a read-only image. [Security](wiki/Security.md) |

## Quick start

You need Docker with Compose 2.24+ (or rootless Podman) and Bash 4+ for `init.sh` (WSL on Windows; `brew install bash` on macOS).

**1. Get the stack**

```sh
git clone https://github.com/Wranglarrr/Wranglarr arrs && cd arrs
```

**2. Run the wizard** and open the link it prints (`http://localhost:8788/?t=…`)

```sh
docker run --rm -it -p 8788:8788 \
  -v "$PWD":/stack \
  -v /mnt/data:/data \
  ghcr.io/wranglarrr/wranglarr:latest wranglarr-setup
```

Mount the drive your media lives on as `/data` so the wizard can check it. PowerShell: `-v ${PWD}:/stack -v D:\media:/data`.

**3. Start everything**

```sh
./scripts/init.sh
```

Then open wranglarr at http://your-host:8787. After that it's plain `docker compose up -d` / `down`.

> **New here?** The [Quickstart](wiki/Quickstart.md) walks through every step with screenshots. On TrueNAS SCALE, read [TrueNAS SCALE](wiki/TrueNAS.md) first. Prefer no wizard? Copy `.env.example` to `.env`, fill in `DATA_ROOT`, `POSTGRES_PASSWORD`, `TZ`, `PUID`/`PGID` and `COMPOSE_PROFILES`, then run `./scripts/init.sh`.

## What's in the stack

| Service | Port | What it does |
| --- | --- | --- |
| **wranglarr** | 8787 | backups, restore, updates, dashboard, everything on this page |
| Radarr · Sonarr · Lidarr | 7878 · 8989 · 8686 | movies · TV · music, on Postgres |
| Prowlarr | 9696 | indexers for every arr, on Postgres |
| Bazarr | 6767 | subtitles, on Postgres |
| qBittorrent · SABnzbd · NZBGet | 8090 · 8085 · 6789 | download clients |
| Seerr | 5055 | requests (Plex, Jellyfin, Emby), on Postgres |
| Postgres 17 | – | one database server for all of them |
| gluetun | – | optional VPN tunnel |

Every service except Postgres and wranglarr is opt-in (`COMPOSE_PROFILES`). One `/data` mount everywhere, so imports are hardlinks, not copies.

```
 LAN ──▶ 7878 8989 8686 9696 6767 │ 8090 8085 6789 │ 5055 │ 8787
          ┌───────────────────────┴────────────────┴──────┴─────────────┐
          │  radarr  sonarr  lidarr  prowlarr  bazarr   qbittorrent sabnzbd nzbget   seerr
          │     └───────┴───────┴───────┴────────┴────────────────────────────────┴──▶ postgres:17  (one db cluster)
          │  wranglarr ──▶ app APIs · pg_dump/pg_restore · docker.sock
          │        └──▶ appdata/backups/<suite>/<version>/…
          └── optional: one gluetun WireGuard/OpenVPN tunnel for every app, or just the download clients (+ Prowlarr)
```

More in [Architecture](wiki/Architecture.md).

## Documentation

**Read it as a website:** [wranglarrr.github.io/Wranglarr](https://wranglarrr.github.io/Wranglarr/), with navigation, search and dark mode. It renders this README and the [wiki](wiki/Home.md).

| Getting started | Using it | Reference |
| --- | --- | --- |
| [Quickstart](wiki/Quickstart.md) | [Dashboard](wiki/Dashboard.md) | [Architecture](wiki/Architecture.md) |
| [Setup](wiki/Setup.md) | [Backups and restore](wiki/Backups-and-Restore.md) | [Environment variables](wiki/Environment-Variables.md) |
| [TrueNAS SCALE](wiki/TrueNAS.md) | [Container updates](wiki/Updates.md) | [HTTP API](wiki/API.md) |
| [Bringing in existing apps](wiki/Importing-Existing-Apps.md) | [App connections](wiki/App-Connections.md) | [Security](wiki/Security.md) |
| [Login](wiki/Login.md) | [Indexers](wiki/Indexers.md) | [Troubleshooting](wiki/Troubleshooting.md) |
| [App logins](wiki/App-Logins.md) | [Community configs](wiki/Community-Configs.md) | [Postgres](wiki/Service-Postgres.md) |
| [VPN: shared tunnel](wiki/VPN-Shared-Tunnel.md) | [Notifications](wiki/Notifications.md) | [Radarr, Sonarr, Lidarr](wiki/Service-Radarr-Sonarr-Lidarr.md) |
| [VPN: per app](wiki/VPN-Per-App.md) | [Connections](wiki/Connections.md) | [Prowlarr](wiki/Service-Prowlarr.md) · [Bazarr](wiki/Service-Bazarr.md) |
| [Remote access](wiki/Remote-Access.md) | [Reverse proxy](wiki/Reverse-Proxy.md) | [qBittorrent](wiki/Service-qBittorrent.md) · [SABnzbd](wiki/Service-SABnzbd.md) · [NZBGet](wiki/Service-NZBGet.md) |
| | [Shared settings](wiki/Shared-Settings.md) | [Seerr](wiki/Service-Seerr.md) · [wranglarr](wiki/Service-wranglarr.md) |
| | | [Development](wiki/Development.md) |

<details>
<summary><b>Repository layout</b></summary>

```
compose.yaml                 the stack
compose.init.yaml            first run only: generate + patch config.xml (via scripts/init.sh)
compose.vpn.yaml             optional: shared gluetun tunnel for the download clients
compose.vpn-prowlarr.yaml    add-on to it: Prowlarr in the tunnel too
compose.vpn-all.yaml         add-on to it: every app in the tunnel
compose.vpn-per-app.yaml     optional: hotio images, one tunnel per app
compose.authtunnel.yaml      AuthTunnel only, generated by init.sh: the apps' ports go to wranglarr-gate
gate/                        wranglarr-gate's nginx.conf (its per-app servers are generated by init.sh)
.env.example                 every option, commented
postgres/initdb/             creates the databases
scripts/init.sh              bootstrap
scripts/patch_config.py      inserts the Postgres keys into config.xml
scripts/patch_downloads.py   points the download clients' save paths at /data; writes qBittorrent's login
scripts/authtunnel.sh        writes compose.authtunnel.yaml and the gate's config (sourced by init.sh)
wranglarr/                   the app, a Rust workspace; the image ghcr.io/wranglarrr/wranglarr is built from it
tests/                       the black-box suite, run against the built binary
.github/workflows/rust.yml   checks and tests
.github/workflows/image.yml  builds, smoke-tests (amd64 + arm64) and publishes the image
scripts/smoke-image.sh       the image smoke test CI runs on each platform
wiki/                        documentation
site/                        docs website (renders README + wiki); .github/workflows/pages.yml publishes it
assets/brand/                logo and wordmark originals; build.py makes the sized copies
```

How the code is organized, and how to build, test and add a service: [Development](wiki/Development.md).

The docs site is published by the `pages` workflow whenever the docs change (on GitHub: Settings → Pages → Source: *GitHub Actions*). To preview it locally, run `python3 site/serve.py` and open http://127.0.0.1:8000.
</details>

<details>
<summary><b>Project status and testing</b></summary>

1.0.0 is the first release. What it brings is in the [release notes](wiki/Release-Notes.md).

**Tested against mocks.** Everything has been run end to end against mock Radarr, Bazarr, qBittorrent, SABnzbd, NZBGet and gluetun servers with fake Postgres tools. That covers backups, restores, retention, shared sync, the restore plan, every page and the wizard. Container updates were run against a mock Docker Engine API that enforces the daemon's create-time conflict rules.

**Tested for real.** The full stack has run under rootless Podman 6 with docker-compose 5. That covers init, the arrs and Bazarr on Postgres, backups, restores, a real image update, and a version started without wranglarr. The screenshots on this page come from a fresh install.

**Not tested yet.** A real Docker Engine run, and a real TrueNAS SCALE install.

What's planned next is in the [roadmap](ROADMAP.md).

Upstream guides this follows:
- [servarr Postgres guide](https://wiki.servarr.com/radarr/postgres-setup)
- [Bazarr's Postgres guide](https://wiki.bazarr.media/Additional-Configuration/PostgreSQL-Database/)
- [gluetun wiki](https://github.com/qdm12/gluetun-wiki)
- [hotio's container docs](https://hotio.dev/containers)
</details>

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). Radarr, Sonarr, Lidarr and Bazarr, whose backup conventions and APIs this builds on, are GPL-3.0 as well. Service icons are from [dashboard-icons](https://github.com/homarr-labs/dashboard-icons) (Apache-2.0).
