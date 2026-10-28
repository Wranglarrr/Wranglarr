# Service: Radarr, Sonarr, Lidarr

| | Radarr | Sonarr | Lidarr |
| --- | --- | --- | --- |
| Image | `${RADARR_IMAGE}` = `lscr.io/linuxserver/radarr:latest` | `…/sonarr:latest` | `…/lidarr:latest` |
| Profile | `radarr` | `sonarr` | `lidarr` |
| Port (host → container) | `${RADARR_PORT}`:7878 | `${SONARR_PORT}`:8989 | `${LIDARR_PORT}`:8686 |
| Config | `appdata/radarr/config.xml` | `appdata/sonarr/config.xml` | `appdata/lidarr/config.xml` |
| Databases | `radarr`, `radarr-log` | `sonarr`, `sonarr-log` | `lidarr`, `lidarr-log` |
| API | `/api/v3`, header `X-Api-Key` | `/api/v3` | `/api/v1` |
| Volumes | `appdata/<app>:/config`, `${DATA_ROOT}:/data` (+ optional `/data2`) |

## Environment

| Variable | From `.env` |
| --- | --- |
| `PUID`, `PGID`, `TZ`, `UMASK` | same names |

With `compose.vpn-per-app.yaml` the hotio images add `WEBUI_PORTS` and the
`VPN_*` block (see [VPN: Per App](VPN-Per-App.md)).

## Postgres via config.xml

The arrs read these keys but never write them, which is why `init.sh` has a
patch step. The keys `scripts/patch_config.py` inserts:

```xml
<PostgresHost>postgres</PostgresHost>
<PostgresPort>5432</PostgresPort>
<PostgresUser>…</PostgresUser>
<PostgresPassword>…</PostgresPassword>
<PostgresMainDb>radarr</PostgresMainDb>
<PostgresLogDb>radarr-log</PostgresLogDb>
```

The equivalent environment variables (`RADARR__POSTGRES__HOST`,
`__PORT`, `__USER`, `__PASSWORD`, `__MAINDB`, `__LOGDB`; same with `SONARR__`
/ `LIDARR__`) also work and take precedence over the file; this stack uses the
file so the password is not visible in `docker inspect`.

Source: https://wiki.servarr.com/radarr/postgres-setup (identical for Sonarr
and Lidarr; Lidarr needs v1.1.2.2890+, Sonarr v4.0.0.615+, Radarr v4.1.0.6133+).

## Root folders

Point them at `/data/media/movies`, `/data/media/tv`, `/data/media/music`;
wranglarr doesn't set root folders. The download clients save under the same
`/data` mount (`/data/torrents/<arr>`, `/data/usenet/complete/<arr>`), so
imports can hardlink. The clients themselves are added to each arr by
wranglarr ([App Connections](App-Connections.md)).

## wranglarr integration

wranglarr reads the API key from the mounted `config.xml`, triggers the app's
own **Backup** task (`POST /api/v3/command {"name":"Backup"}`), downloads the
resulting zip from `/backup/manual/<name>`, deletes it from the app, and
`pg_dump`s both databases. On Postgres the app's zip holds only `config.xml`,
which is why both halves are taken. Restore uploads the zip to
`system/backup/restore/upload` (the app applies it and restarts) and
`pg_restore`s the dumps.
