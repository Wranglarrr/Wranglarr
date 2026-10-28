# App connections

wranglarr wires the stack's apps to each other, based on which ones are
enabled, so a fresh stack works without clicking through every arr's
settings. It runs once at startup and again whenever you press **Connect
apps** on the System page (`src/connect.rs`).

## What gets wired

| App configured | Gets | Where it ends up | Entry |
| --- | --- | --- | --- |
| every arr (Radarr, Sonarr, Lidarr, Readarr, Whisparr) | every download client (qBittorrent, SABnzbd, NZBGet) | Settings → Download Clients | named `qBittorrent` / `SABnzbd` / `NZBGet`, enabled, priority 1, category = the arr's name |
| Bazarr | Sonarr and Radarr (one of each) | Settings → Sonarr / Radarr | `use_sonarr` / `use_radarr` on, host, port, base URL, SSL, API key |
| Prowlarr | every arr | Settings → Apps | an *application* named `Radarr`, `Sonarr`, … with sync level **Full Sync**, so Prowlarr pushes its indexers to the arr |
| Prowlarr | every download client | Settings → Download Clients | the same entries, category `prowlarr`: for grabbing a release from Prowlarr's own search page. No arr imports those; they stay in `/data/torrents/prowlarr` or `/data/usenet/complete/prowlarr` |
| Seerr | every Radarr and Sonarr | Settings → Services | named after the arr; its first quality profile and root folder (your later picks in Seerr are kept); `4k` in the name makes it Seerr's 4K server. Only after Seerr's own first-run setup ([Connections](Connections.md#seerr)) |

Bazarr and Seerr only know Sonarr and Radarr.

Media servers (Plex, Jellyfin, Emby, Kodi) aren't part of the stack, so they
aren't wired automatically: add them on the Connections page, which puts
them in the arrs you pick ([Connections](Connections.md)). The same goes for
notifications ([Notifications](Notifications.md)).

An app whose `name` in `wranglarr.toml` differs from its kind (a second
Radarr called `radarr4k`, say) gets entries named after it instead.

The credentials come from wranglarr's own config: SABnzbd's API key from
`sabnzbd.ini`, qBittorrent's login from `QBITTORRENT_USER` /
`QBITTORRENT_PASSWORD`, NZBGet's from `NZBGET_USER` / `NZBGET_PASS`, and
each arr's API key from its `config.xml`.

## Categories and folders

Each arr's download client entry uses the arr's own name as category
(`radarr`, `sonarr`, `lidarr`), so finished downloads land in a folder per
arr:

| Client | Category folder |
| --- | --- |
| qBittorrent | `/data/torrents/<arr>` (automatic torrent management, set up by `init.sh`) |
| SABnzbd | `/data/usenet/complete/<arr>` |
| NZBGet | `/data/usenet/complete/<arr>` |

The paths are under the shared `/data` mount that the arrs see at the same
path, so imports can hardlink and no remote path mapping is needed. See
[Setup](Setup.md) for how `init.sh` sets the clients' save paths.

Before wiring anything, a run:

1. **Creates the category folders** inside each client's container: `mkdir -p <download folder>/<arr>` through the Docker socket, as the container's `PUID:PGID`. The download folder is the one the client reports (qBittorrent's save path, SABnzbd's `complete_dir`, NZBGet's `DestDir`). Without the socket, the run log says which folders to create by hand. The arrs warn about a client folder they can't see until the first download creates it, which is why this step exists.
2. **Creates the categories in SABnzbd and NZBGet**, because the arrs refuse a category the client doesn't have. SABnzbd gets a category per arr (folder = its name, under `complete_dir`); NZBGet gets `Category<N>.Name = <arr>` with an empty `DestDir` (= `<DestDir>/<arr>`) and is reloaded. qBittorrent needs nothing here: the arr creates the category in it.
3. **Wires each link** (below).

If `mkdir` fails, the run log says the download folder isn't writable by
`PUID:PGID`. Downloads would fail too, so fix the folder's ownership (see
[Troubleshooting](Troubleshooting.md)).

## Addressing

Entries point at the compose **service names** (`qbittorrent:8090`,
`sabnzbd:8080`, `radarr:7878`, …), taken from each app's `url` in
`wranglarr.toml`.

Apps that share one network namespace reach each other on `localhost`
instead. This is the case for the apps in the shared VPN tunnel
(`compose.vpn.yaml`), which run inside the `vpn` container's network; an app
outside the tunnel still uses the service name (`qbittorrent:8090`, which is an
alias of `vpn`). wranglarr detects it through the Docker socket: two containers
whose `NetworkMode` points at the same container share a namespace. Without the
socket it goes by `WRANGLARR_VPN_APPS`, the list of apps in the tunnel that the
compose files set. With per-app tunnels (`compose.vpn-per-app.yaml`)
each app has its own namespace, so service names are used.

For a Prowlarr application both directions are set: the arr's URL as
Prowlarr reaches it, and the Prowlarr URL as the arr reaches it.

## Modes

| Mode | When | What it does |
| --- | --- | --- |
| `new` | at startup, with `[connect] auto = true` | only links never handled before; an app that already has a matching entry is left alone ("kept"). Nothing is overwritten. |
| `all` | **Connect apps** button, or a row's **connect** button | creates missing entries and updates existing ones: host, port, SSL, URL base, credentials and category are set, the entry is enabled, the rest of its settings are kept |

An existing entry is one with the same implementation and either the same
name or the same host.

Links that were created, updated or kept are recorded in the state db
(kv `connect:done`). The startup run skips them from then on, so a link you
remove by hand in an arr is **not** put back. A link that failed is not
recorded and is tried again on the next start. Newly enabled apps get wired
on the next start too.

At startup wranglarr waits in the background (up to 15 minutes) until every
app involved answers, then runs `new` for the links whose two apps are up.
If none answer it logs a warning and leaves it to the button.

### Each entry is tested first

For download clients and Prowlarr applications, the app being configured
tests the entry (its `downloadclient/test`, Prowlarr's
`applications/test`) before wranglarr saves it. A client with wrong
credentials, or one the arr can't reach, is therefore not saved; it shows as
`failed` in the run log with the app's own message. Fix the cause (for
example `QBITTORRENT_PASSWORD` in `.env`, then `docker compose up -d
wranglarr`) and press **Connect apps**. Seerr tests each Radarr and Sonarr the same
way. Bazarr's settings are saved without a test.

## Configuration

```toml
[connect]
auto = true      # at startup, connect what was never connected before
```

`auto = false` turns the startup run off; the button and the API still work.

## System page

The **App connections** panel opens on a **map**: every app is a box, and every
link an arrow from the app that holds the setting to the app it reaches. The
columns follow the links: Prowlarr, Bazarr and Seerr on the left, the arrs in
the middle, the download clients on the right.

- **One arrow head**: one app calls the other (an arr sends what it grabs to a
  download client; Bazarr and Seerr read from Sonarr and Radarr).
- **Heads at both ends, in blue**: the two reach each other. Prowlarr puts its
  indexers into the arr, and the arr searches through Prowlarr.
- **Colour**: green is connected, orange `differs` or `waiting`, grey dashed
  not connected, red an error. An app with a link that isn't connected gets a
  dot in its corner.

Hover over an app to pick out its links. Click an app or a line to list those
links under the map, with a **connect** button on any that aren't connected.

**List** (top right) shows the same links as a table. The browser remembers
the view you picked.

| Column | |
| --- | --- |
| App | the app being configured |
| Gets | the app it is pointed at, and what it is (download client, sonarr/radarr, application) |
| At | the `host:port` it should use (`localhost:…` under the shared VPN) |
| State | `connected`; `not connected`; `differs` (points at another host/port, is disabled, or Bazarr has another API key); `waiting` (Seerr's first-run setup isn't done: wranglarr retries every 5 minutes and connects it by itself); or an error reading the app |

Rows that aren't connected have a **connect** button (mode `all`, that link
only). **Connect apps** below the table runs every link in mode `all`.

Runs are logged like backups (Activity, suite `system`), one line per folder,
category and link.

## API

```
GET    /api/connect                                  links with state, target, detail; "auto"; "missing" count
POST   /api/connect                                  {"mode":"all"|"new","links":["qbittorrent>radarr"]} -> {"run": id}
```

A link key is `<app pointed at>><app configured>`: `qbittorrent>radarr`,
`sonarr>bazarr`, `radarr>prowlarr`. An empty or missing `links` means every
link; `mode` defaults to `all`. Unknown link: 404; bad mode: 400; another
system run (an update, an indexer sync) in progress: 409.

## What it does not do

- **Root folders.** Add `/data/media/movies`, `/data/media/tv`, `/data/media/music` in each arr yourself.
- **Indexers.** Add them in Prowlarr (which the arrs are registered in), or on the [Indexers](Indexers.md) page.
- **Remove anything.** Entries are only created or updated; removing an app from `COMPOSE_PROFILES` leaves its entries in the others.
- **Quality profiles, notifications, other settings.** [Shared Settings](Shared-Settings.md) covers download clients, indexers and notifications defined in `wranglarr.toml`.
