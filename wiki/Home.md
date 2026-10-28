<p align="center"><img src="images/brand/wordmark.png" alt="wranglarr" width="420"></p>

# wranglarr wiki

wranglarr is a Docker Compose stack for a Postgres-backed \*arr suite plus a web
app that backs up, versions and restores every app in it.

| Page | What's there |
| --- | --- |
| [Architecture](Architecture.md) | the containers, networks, volumes and how traffic flows |
| [Quickstart](Quickstart.md) | **start here**: a first install step by step, with screenshots |
| [Login](Login.md) | wranglarr's sign-in: password, two-factor (TOTP), passkeys, API keys, recovery |
| [App Logins](App-Logins.md) | the apps' logins: one for every app, AuthTunnel (sign in once, to wranglarr), or your own |
| [Security](Security.md) | what the Docker socket and the web UI expose, how wranglarr runs without root, a service user for each OS |
| [TrueNAS SCALE](TrueNAS.md) | running the stack on TrueNAS SCALE 24.10+: datasets, permissions, `sudo`, the Apps screen |
| [Setup](Setup.md) | the first-run wizard, `init.sh`, opting services in |
| [Bringing in existing apps](Importing-Existing-Apps.md) | moving a Radarr, Sonarr, Prowlarr, Bazarr or Seerr you already run into the stack, keeping its library and history: config, SQLite → Postgres, paths, connections |
| [Environment Variables](Environment-Variables.md) | **every** `.env` key, with defaults and which service reads it |
| [Service: Postgres](Service-Postgres.md) | the database server and the databases it creates |
| [Service: Radarr, Sonarr, Lidarr](Service-Radarr-Sonarr-Lidarr.md) | the config.xml-based arrs on Postgres |
| [Service: Prowlarr](Service-Prowlarr.md) | indexer manager (opt-in), registered in every arr automatically |
| [Service: Bazarr](Service-Bazarr.md) | subtitles, Postgres via environment |
| [Service: qBittorrent](Service-qBittorrent.md) | torrent client |
| [Service: SABnzbd](Service-SABnzbd.md) | usenet client |
| [Service: NZBGet](Service-NZBGet.md) | usenet client |
| [Service: Seerr](Service-Seerr.md) | requests (opt-in): your Plex / Jellyfin / Emby users ask, Radarr and Sonarr get it |
| [Service: wranglarr](Service-wranglarr.md) | the backup/restore app: environment, `wranglarr.toml`, HTTP API |
| [VPN: Shared Tunnel](VPN-Shared-Tunnel.md) | one gluetun WireGuard/OpenVPN tunnel: every app, download clients and Prowlarr, or download clients only |
| [VPN: Per App](VPN-Per-App.md) | hotio images, one tunnel per app |
| [Remote Access](Remote-Access.md) | Tailscale, Cloudflare Tunnel, Cloudflare DNS: reaching the stack from outside, with or without the VPN |
| [Reverse Proxy](Reverse-Proxy.md) | config for Caddy, nginx, Traefik, caddy-docker-proxy, Cloudflare Tunnel, Nginx Proxy Manager; public, LAN only or not per app |
| [Dashboard](Dashboard.md) | posters for the media arrs with what's coming up and missing, cards for the rest with their queues and lists; where each app opens (IP and port, or a domain); each app's login |
| [Notifications](Notifications.md) | Telegram, Discord, Slack, ntfy, Gotify, Pushover, Apprise, email, webhook: failed backups, apps offline, stuck downloads, blocked imports… |
| [Connections](Connections.md) | Plex, Jellyfin, Emby and Kodi into the arrs (library rescans); Seerr |
| [Container Updates](Updates.md) | registry checks, backup-first image updates with health checks, rollback, holds, auto-update |
| [Release Notes](Release-Notes.md) | what each release adds, changes and fixes |
| [Backups and Restore](Backups-and-Restore.md) | what a version contains, retention, restore semantics, restore plans, running a version without wranglarr |
| [Live status and API keys](Live-Status.md) | the arrs tell wranglarr when something changes, so the pages update straight away; every app's API key in one place; a key changed in an app reconnects the apps using it |
| [App Connections](App-Connections.md) | download clients into every arr, Sonarr/Radarr into Bazarr, the arrs into Prowlarr; categories and folders |
| [Indexers](Indexers.md) | the Indexers page: Torznab/Newznab feeds, Jackett, NZBHydra2 or an outside Prowlarr, pushed to the arrs you pick |
| [Community configs](Community-Configs.md) | custom formats, quality profiles, sizes and release profiles for one job (x265 first at the smallest size, anime English dub…) as JSON: import with a preview and undo, test a release name, export your own |
| [Shared Settings](Shared-Settings.md) | define download clients / notifications once |
| [HTTP API](API.md) | every endpoint for scripts, Home Assistant and cron: API keys, jobs and runs, errors, bodies and answers |
| [Troubleshooting](Troubleshooting.md) | the gotchas, each with its fix |
| [Development](Development.md) | how the code is organized; building, testing, adding a service, releases |

Conventions used throughout: `appdata/` is `${APPDATA}` (default `./appdata`);
"the arrs" means Radarr, Sonarr and Lidarr (Prowlarr is an indexer manager:
it is backed up like them but downloads nothing); a *version* is one backup set.
