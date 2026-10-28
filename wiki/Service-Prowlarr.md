# Service: Prowlarr

Indexer manager: add trackers and usenet indexers once in Prowlarr, and it
pushes them to every arr registered in it. It doesn't download anything
itself, so it has no `/data` mount and gets no download clients.

| | |
| --- | --- |
| Image | `${PROWLARR_IMAGE}` = `lscr.io/linuxserver/prowlarr:latest` (`ghcr.io/hotio/prowlarr:latest` with the per-app VPN file) |
| Profile | `prowlarr` (opt-in) |
| Port (host → container) | `${PROWLARR_PORT}` (default 9696):9696 |
| Config | `appdata/prowlarr/config.xml` |
| Databases | `prowlarr`, `prowlarr-log` |
| API | `/api/v1`, header `X-Api-Key` |
| Volumes | `appdata/prowlarr:/config` |

## Environment

| Variable | From `.env` |
| --- | --- |
| `PUID`, `PGID`, `TZ`, `UMASK` | same names |

With `compose.vpn-per-app.yaml` the hotio image adds `WEBUI_PORTS=9696/tcp`
and the `VPN_*` block, with `VPN_ENABLED` from `PROWLARR_VPN` (see [VPN: Per
App](VPN-Per-App.md)).

## Postgres via config.xml

Same as the other arrs ([Service: Radarr, Sonarr,
Lidarr](Service-Radarr-Sonarr-Lidarr.md)): `init.sh` starts it once through
`compose.init.yaml` to generate `config.xml`, then `patch-config` inserts the
`Postgres*` keys with `PostgresMainDb` = `prowlarr` and `PostgresLogDb`
= `prowlarr-log`. Postgres creates both databases when `prowlarr` is in
`COMPOSE_PROFILES`; for a cluster initialised before that, see
[Setup](Setup.md#opting-services-in-and-out).

## Behind the shared VPN

Attached to the `vpn` container like the other apps; its port is published on
`vpn` as `${PROWLARR_PORT}:9696`, and `vpn` carries the network alias
`prowlarr`. Its searches then leave through the tunnel.

## wranglarr integration

- **Backup**: in the `media` suite of the example `wranglarr.toml`, backed up like the arrs: the app's own Backup task zip, `pg_dump` of both databases, and its `/config` as `prowlarr/config/`. Left out of that copy: the arrs' usual folders (`logs/`, `Backups/`, …) plus the files directly in `Definitions/`, the indexer definitions Prowlarr downloads on start (about 5 MB). `Definitions/Custom/`, your own definitions, is kept. See [Backups and Restore](Backups-and-Restore.md).
- **App connections**: every arr is registered in Prowlarr as an application (Settings → Apps, sync level Full Sync), so the indexers you add in Prowlarr reach the arrs without further setup. This happens at startup and with **Connect apps** on the System page; see [App Connections](App-Connections.md).
- **Indexers page**: shows whether each arr is registered. Its own sources are pushed to the arrs, never to Prowlarr; a source of type *Prowlarr* is for a Prowlarr outside this stack. See [Indexers](Indexers.md).
- **Updates**: managed like any other container with `container = "prowlarr"` ([Container Updates](Updates.md)).
