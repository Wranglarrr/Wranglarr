# Service: SABnzbd

| | |
| --- | --- |
| Image | `${SABNZBD_IMAGE}` = `lscr.io/linuxserver/sabnzbd:latest` (hotio with the per-app VPN file) |
| Profile | `sabnzbd` |
| Port | `${SABNZBD_PORT}` (default 8085):8080 |
| Config | `appdata/sabnzbd/sabnzbd.ini` (API key: `api_key`) |
| Volumes | `appdata/sabnzbd:/config`, `${DATA_ROOT}:/data` |
| API | `/api?mode=…&apikey=…&output=json` |

## Environment

| Variable | Value |
| --- | --- |
| `PUID`, `PGID`, `TZ`, `UMASK` | from `.env` |

SABnzbd has no port/credential environment variables in this image; its port
stays 8080 inside the container and is set in `sabnzbd.ini`.

## Folders, host whitelist, categories

`init.sh` writes these into `sabnzbd.ini` (section `[misc]`). The folders are
only changed while still at the image's defaults; `sabnzbd` is added to the
whitelist if it isn't there:

| Key | Value |
| --- | --- |
| `download_dir` | `/data/usenet/incomplete` (`${USENET_DIR}/incomplete`) |
| `complete_dir` | `/data/usenet/complete` (`${USENET_DIR}/complete`) |
| `nzb_backup_dir` | `nzb-backup` (`${SABNZBD_NZB_BACKUP}`; relative to `/config`): a copy of every NZB added, so a backup can bring queued jobs back. Empty `SABNZBD_NZB_BACKUP` leaves it off. |
| `host_whitelist` | `sabnzbd` appended |

SABnzbd rejects requests whose `Host` header it doesn't know; the whitelist
entry is what lets the arrs reach it as `http://sabnzbd:8080`. Behind the
shared VPN the arrs use `localhost:8080`, which is always allowed. On an
install that predates this, add `sabnzbd` to Config → General →
`host_whitelist` yourself, or re-run `init.sh`.

`init.sh` also creates `incomplete/`, `complete/` and `complete/<arr>` for
each enabled arr under `DATA_ROOT`. wranglarr creates a category per arr
(`radarr`, `sonarr`, `lidarr`; folder = its name, under `complete_dir`) and
adds SABnzbd to each arr with that category ([App
Connections](App-Connections.md)). The arrs refuse a category SABnzbd
doesn't have, which is why the categories are created first.

## wranglarr integration

API key is read from `sabnzbd.ini`. Each version holds a plain copy of
`/config` as `sabnzbd/config/` (without `logs/`): the queue and history, the
NZB copies in `nzb_backup_dir`, NZBs waiting in a watched folder inside
`/config`, and the settings. If `download_dir` is inside `/config`, only each
job's `__ADMIN__` folder (its NZB and state) is kept; a `complete_dir` inside
`/config` is left out. Downloaded data on `/data/usenet` is never in the
backup. After a restore an interrupted job can be added again from its saved
NZB.

Restore copies the folder back with the container stopped. Stats: queue length, history count, paused;
SABnzbd's own warnings are shown as health.
