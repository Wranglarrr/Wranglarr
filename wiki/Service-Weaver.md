# Service: Weaver

[Weaver](https://github.com/scryer-media/weaver) is a Usenet client that
answers NZBGet's API, so the arrs add it as an **NZBGet** download client. Pick
it in the setup wizard beside SABnzbd and NZBGet, or add `weaver` to
`COMPOSE_PROFILES`.

| | |
| --- | --- |
| Image | `${WEAVER_IMAGE}` = `ghcr.io/scryer-media/weaver:latest` |
| Profile | `weaver` |
| Port | `${WEAVER_PORT}`:9090 |
| Config | `appdata/weaver/weaver.db` (SQLite: login, settings, queue, history), `encryption.key`, `wranglarr-api-key.json` |
| Volumes | `appdata/weaver:/config`, `${DATA_ROOT}:/data` |
| API | NZBGet JSON-RPC at `/jsonrpc` (HTTP basic auth: any username, an API key as the password); GraphQL at `/graphql` |

## Environment

| Variable | From `.env` | Purpose |
| --- | --- | --- |
| `PUID`, `PGID`, `TZ`, `UMASK` | same | |
| `WEAVER_BOOTSTRAP_LOGIN_USERNAME` | `WEAVER_USER` (default `admin`) | Weaver's login |
| `WEAVER_BOOTSTRAP_LOGIN_PASSWORD` | `WEAVER_PASS` | 8 characters or more; `init.sh` refuses a shorter one |
| `WEAVER_INTERMEDIATE_DIR` | `${USENET_DIR}/incomplete` | downloads in progress |
| `WEAVER_COMPLETE_DIR` | `${USENET_DIR}/complete` | finished downloads |

Weaver reads all four **on its first start only**; after that they live in its
database. To change its login later, change it in Weaver (*Settings → Security*)
and set `WEAVER_USER` / `WEAVER_PASS` in `.env` to match: wranglarr signs in
with them.

With `APPS_AUTH=apps` the setup wizard makes them the apps' shared login. With
`APPS_AUTH=tunnel` the wizard generates a password, and the gate signs you in
with wranglarr's key, so you never type it ([App logins](App-Logins.md)).
Weaver's login can't be turned off, so `APPS_AUTH=manual` still has it.

## The API key

Nothing outside Weaver can set one of its API keys, so wranglarr makes its own:
the first time it needs one, it signs in with `WEAVER_USER` / `WEAVER_PASS`,
creates an admin key named `wranglarr`, signs out, and keeps the key in
`appdata/weaver/wranglarr-api-key.json` (mode 600). It replaces an older key of
that name, so there is only ever one.

The arrs get that key as the NZBGet password, with `wranglarr` as the username
(Weaver ignores it). If the key is deleted in Weaver, wranglarr notices within
a minute, creates a new one, and updates the arrs, as for any other changed key
([Live status](Live-Status.md#a-key-changed-in-an-app)). If its login stops working,
wranglarr says so on the app's card and tries again after five minutes.

## Folders and categories

The download folders come from `WEAVER_INTERMEDIATE_DIR` / `WEAVER_COMPLETE_DIR`
above, under `/data/usenet` by default. `init.sh` creates `incomplete/`,
`complete/` and `complete/<arr>` for each enabled arr under `DATA_ROOT`.
wranglarr adds a category per arr in Weaver (named after the arr, downloads
going to `complete/<arr>`) and adds Weaver to each arr with that category
([App Connections](App-Connections.md)). Weaver refuses a download whose
category it doesn't have.

## wranglarr integration

The `wranglarr.toml` entry has `username` / `password` (`${WEAVER_USER}` /
`${WEAVER_PASS}`) for signing in, and `api_key_file` for the key it creates:

```toml
  [[suite.app]]
  name = "weaver"
  kind = "weaver"
  url = "http://weaver:9090"
  username = "${WEAVER_USER}"
  password = "${WEAVER_PASS}"
  api_key_file = "/arrconfig/weaver/wranglarr-api-key.json"
  config_dir = "/arrconfig/weaver"
  container = "weaver"
```

wranglarr seeds `wranglarr.toml` only on its first start. On an existing
install, add this entry to the `downloads` suite yourself, and the
`appdata/weaver:/arrconfig/weaver` mount comes with the updated `compose.yaml`.

Each version holds a plain copy of `/config` as `weaver/config/`: the database
(login, settings, the queue with each job's NZB, the history), copied by SQLite
so it's consistent while Weaver runs, `encryption.key`,
which the database's secrets need, and wranglarr's key file, so a restore
brings back a key that matches the database. Downloaded data on `/data/usenet`
is never in the backup. Restore copies the folder back with the container
stopped. Stats: queue, history, paused, size on disk.

## VPN

Weaver can use the [shared tunnel](VPN-Shared-Tunnel.md) like the other
download clients. There's no hotio image of it, so it has no
[per-app VPN](VPN-Per-App.md).
