# Service: qBittorrent

| | |
| --- | --- |
| Image | `${QBITTORRENT_IMAGE}` = `lscr.io/linuxserver/qbittorrent:latest` (hotio with the per-app VPN file) |
| Profile | `qbittorrent` |
| Ports | `${QBITTORRENT_PORT}`:**8090** (web UI), `6881` tcp+udp (torrenting) |
| Config | `appdata/qbittorrent/qBittorrent/qBittorrent.conf` (+ `BT_backup/` with torrent state) |
| Volumes | `appdata/qbittorrent:/config`, `${DATA_ROOT}:/data` |
| API | `/api/v2`, cookie session after `auth/login` |

## Environment

| Variable | Value | Purpose |
| --- | --- | --- |
| `PUID`, `PGID`, `TZ`, `UMASK` | from `.env` | |
| `WEBUI_PORT` | `8090` | the UI listens on 8090 **inside** the container so it never collides with SABnzbd's 8080 when both share the VPN container's network namespace. `QBITTORRENT_PORT` (default 8090) is the host port and should stay equal to it: qBittorrent's Host header validation rejects a request whose `Host` port differs from its own (log: `Invalid Host header, port mismatch`), so `8080:8090` makes the UI unusable from a browser. To publish another port, turn off Options → Web UI → *Enable Host header validation* first. |
| `TORRENTING_PORT` | `6881` | incoming peer port (behind the shared VPN use the provider's forwarded port instead) |

## First start

With `APPS_AUTH` `apps` or `tunnel` ([App logins](App-Logins.md)), init writes
`QBITTORRENT_USER` / `QBITTORRENT_PASSWORD` into `qBittorrent.conf` (as
qBittorrent's own PBKDF2 hash) while it's stopped, so there's nothing to do here.
It goes in the `[WebUI]` section, where qBittorrent 5 reads it. While the file
still has 4.x's `WebUI\…` keys in `[Preferences]` and no `[WebUI]`, those are
set too: 5 ignores them once it has converted the file.
With AuthTunnel it also turns on *Bypass authentication for clients in
whitelisted IP subnets* for the private ranges, so the gate gets in; the
port isn't published anywhere else.

With `manual`, the linuxserver image generates a temporary admin password and prints it:

```sh
docker compose logs qbittorrent | grep -i password
```

Set a permanent one in Options → Web UI, then put it in `.env` as
`QBITTORRENT_PASSWORD` (and `QBITTORRENT_USER` if not `admin`) and
`docker compose up -d wranglarr` so wranglarr can log in. Until then the
arrs' connection test fails and [App Connections](App-Connections.md) shows
qBittorrent as `failed` in the run log; press **Connect apps** on the System
page once the password is set.

## Save path and categories

`init.sh` writes these into `qBittorrent.conf` (section `[BitTorrent]`), only
while they are still at the image's defaults:

| Key | Value |
| --- | --- |
| `Session\DefaultSavePath` | `/data/torrents/` (`${TORRENTS_DIR}/`) |
| `Session\DisableAutoTMMByDefault` | `false`: automatic torrent management, so a torrent in category `radarr` is saved to `/data/torrents/radarr` |
| `Session\TempPath` | `/data/torrents/incomplete/`, with `Session\TempPathEnabled=true` |

It also creates `/data/torrents`, `incomplete/` and one folder per enabled
arr under `DATA_ROOT`. wranglarr adds qBittorrent to every arr with the arr's
name as category, and creates the category folder in the container if it is
missing ([App Connections](App-Connections.md)). Downloads land at the same
`/data` path the arrs see, so imports hardlink and no remote path mapping is
needed.

## Behind the shared VPN

Published on the `vpn` service as `${QBITTORRENT_PORT}:8090`. For an inbound
port with PIA/ProtonVPN set `VPN_PORT_FORWARDING=on` (and
`VPN_PORT_FORWARD_ONLY=on`). gluetun's `VPN_PORT_FORWARDING_UP_COMMAND` in
`compose.vpn.yaml` then pushes the forwarded port into qBittorrent over
`127.0.0.1:8090` and binds it to the tunnel's interface; tick "Bypass
authentication for clients on localhost" in its Web UI options so it can.
The image sets the port back to `TORRENTING_PORT` (6881) each time qBittorrent
restarts, and gluetun only pushes it when the forwarded port changes, so
wranglarr checks every minute and sets the forwarded port again (it logs
"vpn: qbittorrent listened on port 6881, not the VPN's forwarded …: set it").
Without port forwarding, set the network interface to `tun0` in Advanced
settings yourself, so torrenting is bound to the tunnel.

## wranglarr integration

No backup API, so each version holds a plain copy of `/config` as
`qbittorrent/config/`, without `logs/`, `GeoDB/` and `lockfile`. That
includes the `.torrent` file of every torrent and its resume data in
`BT_backup/` (progress, save path, category, tags) and the client's settings.
The downloaded data on `/data/torrents` is **not** in it; that belongs to
your normal file backups. After a restore qBittorrent re-checks the data
still on `/data` and carries on, or downloads again what is gone.

Restore copies the version's `qbittorrent/config/` back with the container
stopped (needs the Docker socket and `container = "qbittorrent"`) and keeps
the previous contents in `appdata/wranglarr/pre-restore/qbittorrent`. See [Backups and
Restore](Backups-and-Restore.md). Stats shown: torrents, downloading,
seeding, paused, completed bytes.
