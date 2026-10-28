# Service: Bazarr

| | |
| --- | --- |
| Image | `${BAZARR_IMAGE}` = `lscr.io/linuxserver/bazarr:latest` |
| Profile | `bazarr` |
| Port | `${BAZARR_PORT}`:6767 |
| Config | `appdata/bazarr/config/config.yaml` (API key under `auth.apikey`) |
| Backups | `appdata/bazarr/backup/*.zip` |
| Database | `bazarr` |
| API | `/api`, header `X-API-KEY` |
| Volumes | `appdata/bazarr:/config`, `${DATA_ROOT}:/data` |

## Environment

| Variable | Value | Purpose |
| --- | --- | --- |
| `PUID`, `PGID`, `TZ`, `UMASK` | from `.env` | |
| `POSTGRES_ENABLED` | `true` | use Postgres instead of SQLite |
| `POSTGRES_HOST` | `postgres` | |
| `POSTGRES_PORT` | `5432` | |
| `POSTGRES_DATABASE` | `bazarr` | |
| `POSTGRES_USERNAME` | `${POSTGRES_USER}` | |
| `POSTGRES_PASSWORD` | `${POSTGRES_PASSWORD}` | |
| `PGSSLCERT` | `/tmp/postgresql.crt` | the linuxserver image runs as PUID; without this psycopg2 looks for a cert under `/root` and fails |

These environment variables override `config.yaml`
(https://wiki.bazarr.media/Additional-Configuration/PostgreSQL-Database/), so
no file patching is needed and nothing changes if you edit the file by hand.

## wranglarr integration

Two Bazarr quirks shape it:

- Bazarr's zip download route accepts only a browser login, not the API key, so wranglarr reads the zip from the mounted `/arrconfig/bazarr/backup/` instead (`config_dir` in `wranglarr.toml`). The mount is read-write for this reason.
- On Postgres, Bazarr's backup zip contains only `config.yaml`, and Bazarr's own restore refuses any zip without a `bazarr.db`. So a config restore writes `config.yaml` straight into `config/`, keeps the old one as `config.yaml.pre-wranglarr`, and asks Bazarr to restart via `POST /api/system?action=restart`. The database is restored with `pg_restore`.

Collection stats shown: series and movies under subtitle management, wanted
episodes and movies; scope: providers, language profiles, notifications.
