# Service: Postgres

| | |
| --- | --- |
| Image | `${POSTGRES_IMAGE}` = `postgres:17` (pinned major; the servarr wiki warns against `latest`) |
| Container | `arrs-postgres` |
| Profile | always on |
| Port | not published (uncomment `ports:` + `POSTGRES_HOST_PORT` for pgAdmin etc.) |
| Volume | `${APPDATA}/postgres:/var/lib/postgresql/data`, `./postgres/initdb:/docker-entrypoint-initdb.d:ro` |
| Healthcheck | `pg_isready -U $POSTGRES_USER -d postgres` every 10 s |

## Environment

| Variable | From `.env` | Purpose |
| --- | --- | --- |
| `POSTGRES_USER` | `POSTGRES_USER` | superuser / owner of every app database |
| `POSTGRES_PASSWORD` | `POSTGRES_PASSWORD` | |
| `POSTGRES_DB` | `${POSTGRES_USER}` | the entrypoint's default database (unused by the apps) |
| `ARR_APPS` | `${COMPOSE_PROFILES}` | read by `01-create-arr-dbs.sh` |

## Databases created

`postgres/initdb/01-create-arr-dbs.sh` runs on first cluster init and can be
re-run any time:

```sh
docker compose exec postgres /docker-entrypoint-initdb.d/01-create-arr-dbs.sh
```

| Profile | Databases |
| --- | --- |
| radarr, sonarr, lidarr (also readarr, prowlarr, whisparr) | `<app>`, `<app>-log` |
| bazarr | `bazarr` |
| seerr | `seerr` |
| qbittorrent, sabnzbd, nzbget | none |

The apps create their own schema on first connect. wranglarr dumps each
database with `pg_dump -Fc --no-owner` and restores with
`pg_restore --clean --if-exists --single-transaction`.

## Useful commands

```sh
docker compose exec postgres psql -U "$POSTGRES_USER" -l
docker compose exec postgres psql -U "$POSTGRES_USER" -d radarr -c '\dt'
docker compose exec postgres psql -U "$POSTGRES_USER" -c "SELECT datname, pg_size_pretty(pg_database_size(datname)) FROM pg_database"
```

## Updates and major upgrades

Minor releases within the pinned major (17.2 → 17.3) are ordinary
[container updates](Updates.md): wranglarr backs up every suite with
databases, swaps the container on the same data directory, and waits until
Postgres accepts connections.

A **major** upgrade (17 → 18) changes the on-disk format, so a container swap
would leave Postgres unable to read its data directory. wranglarr detects it
(the image's `PG_MAJOR` changes), refuses, and blocks that image. To do one
deliberately, use wranglarr's dumps:

1. In wranglarr, **Backup now** on every suite that has databases.
2. `docker compose stop`, then move `appdata/postgres` aside (keep it until
   the upgrade has proven itself).
3. Set `POSTGRES_IMAGE=postgres:18` in `.env`. Check that image's notes for
   data directory changes before you start it.
4. `docker compose up -d postgres wranglarr`. The init script creates empty
   databases for every enabled profile.
5. In wranglarr, restore each app from the version you took in step 1, with
   **databases only**. Then `docker compose up -d`.
