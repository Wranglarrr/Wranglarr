# Service: NZBGet

| | |
| --- | --- |
| Image | `${NZBGET_IMAGE}` = `lscr.io/linuxserver/nzbget:latest` (hotio with the per-app VPN file) |
| Profile | `nzbget` |
| Port | `${NZBGET_PORT}`:6789 |
| Config | `appdata/nzbget/nzbget.conf` (`ControlUsername` / `ControlPassword`) |
| Volumes | `appdata/nzbget:/config`, `${DATA_ROOT}:/data` |
| API | JSON-RPC at `/jsonrpc`, HTTP basic auth |

## Environment

| Variable | From `.env` | Purpose |
| --- | --- | --- |
| `PUID`, `PGID`, `TZ`, `UMASK` | same | |
| `NZBGET_USER` | `NZBGET_USER` (default `nzbget`) | web/API login |
| `NZBGET_PASS` | `NZBGET_PASS` (default `tegbzn6789`) | change it |

With `APPS_AUTH=apps` the setup wizard makes these the apps' shared login;
with AuthTunnel the gate sends them for you ([App logins](App-Logins.md)).
The linuxserver image applies `NZBGET_USER` / `NZBGET_PASS` at launch;
`nzbget.conf` keeps its default `ControlUsername` / `ControlPassword`. The
same two variables are passed to wranglarr, which uses them to log in.

## Folders and categories

`init.sh` writes these into `nzbget.conf`, each only while it is still at the
image's default:

| Key | Value |
| --- | --- |
| `DestDir` | `/data/usenet/complete` (`${USENET_DIR}/complete`) |
| `InterDir` | `/data/usenet/incomplete` (`${USENET_DIR}/incomplete`) |

It also creates `incomplete/`, `complete/` and `complete/<arr>` for each
enabled arr under `DATA_ROOT`. wranglarr adds a category per arr to
`nzbget.conf` (`Category<N>.Name` = the arr's name, empty `DestDir`, so
downloads go to `/data/usenet/complete/<arr>`), reloads NZBGet, and adds
NZBGet to each arr with that category ([App Connections](App-Connections.md)).
The arrs refuse a category NZBGet doesn't have.

## wranglarr integration

Credentials are `username` / `password` in `wranglarr.toml` (the example
uses `${NZBGET_USER}` / `${NZBGET_PASS}`), falling back to `ControlUsername` /
`ControlPassword` in `nzbget.conf`. Each version holds a plain copy of
`/config` as `nzbget/config/` (without `nzbget.log*`): the settings, and the
queue (`QueueDir`: each job's NZB and download state) and incoming NZBs
(`NzbDir`) when those are inside `/config`. When they point elsewhere, the
downloads suite page lists them under *Not in the backup*. Downloaded data on
`/data/usenet` is never in the backup.

Restore copies the folder back with the container stopped. Stats: queue, history, paused, downloaded bytes.
