# Service: Seerr

[Seerr](https://github.com/seerr-team/seerr) is a request app: you and the
people you share your media server with ask for a movie or a series, and
Seerr hands it to Radarr or Sonarr. It's the successor of Overseerr and
Jellyseerr, and works with Plex, Jellyfin and Emby.

| | |
| --- | --- |
| Image | `${SEERR_IMAGE}` = `ghcr.io/seerr-team/seerr:latest` |
| Profile | `seerr` |
| Port | `${SEERR_PORT}`:5055 |
| Config | `appdata/seerr/settings.json` (`main.apiKey`) |
| Volumes | `appdata/seerr:/app/config` |
| Database | `seerr` on the stack's Postgres |
| API | `/api/v1`, header `X-Api-Key` |

## Environment

| Variable | From `.env` | Purpose |
| --- | --- | --- |
| `TZ` | `TZ` | |
| `LOG_LEVEL` | | `info` |
| `DB_TYPE`, `DB_HOST`, `DB_PORT`, `DB_USER`, `DB_PASS`, `DB_NAME` | `POSTGRES_USER`, `POSTGRES_PASSWORD` | Postgres instead of SQLite; database `seerr` |

The image runs as `PUID:PGID` (`user:` in compose) rather than reading
`PUID`/`PGID` itself, and `init.sh` hands `appdata/seerr` to that user.
Seerr isn't put inside the VPN: it talks to your media server and to the
people using it, not to indexers.

## Turning it on

Add `seerr` to `COMPOSE_PROFILES` (or tick it in the setup wizard, under
*Requests*) and run `./scripts/init.sh`. It creates the `seerr` database,
the folder, and adds a `requests` suite with Seerr to an existing
`wranglarr.toml`.

## First start

Open Seerr (port 5055) and sign in with your Plex, Jellyfin or Emby
account. That's Seerr's own setup; it makes you the admin and picks the
media server. Seerr's users are your media server's users, so there's no
separate login to set ([App logins](App-Logins.md)).

After that wranglarr adds every Radarr and Sonarr to Seerr by itself, within
5 minutes. Until then the links show **waiting** on the System and
Connections pages ([Connections](Connections.md#seerr)).

## Behind a reverse proxy

Seerr needs its own host name (`requests.example.com`); it doesn't work
under a path. With AuthTunnel it isn't put behind wranglarr-gate: the
people requesting things don't have a wranglarr account, and Seerr has its
own sign-in ([Reverse Proxy](Reverse-Proxy.md)).

## wranglarr integration

Seerr is its own suite, `requests`, so it's backed up and restored on its
own. A version holds the `seerr` database dump and `/app/config` as a plain
folder, without `logs/` and `cache/`. Stats: requests, pending, available;
scope: the media server and the number of Radarr and Sonarr servers.
