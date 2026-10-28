# Shared settings

Define a download client or notification **once** in
`appdata/wranglarr/wranglarr.toml` and push it to every arr with **Sync
shared settings** (Suites page / a suite's page) or `POST /api/suites/{s}/sync`.

```toml
[[shared.download_client]]
name = "qBittorrent"
implementation = "QBittorrent"      # the arr's implementation name
protocol = "torrent"
priority = 1
apply_to = ["radarr", "sonarr", "lidarr"]   # app kinds or app names; omit for all arrs
test = true                                 # run the app's connection test before saving
  [shared.download_client.fields]           # the app's own field names
  host = "qbittorrent"
  port = 8090
  username = "admin"
  password = "${QBITTORRENT_PASSWORD}"
  useSsl = false
  [shared.download_client.overrides.radarr.fields]
  movieCategory = "radarr"
  [shared.download_client.overrides.sonarr.fields]
  tvCategory = "sonarr"
  [shared.download_client.overrides.lidarr.fields]
  musicCategory = "lidarr"
```

How a sync works, per app and per resource:

1. Build the body: top-level keys (snake_case → camelCase, so `enable_rss` becomes `enableRss`), `configContract` defaulting to `<implementation>Settings`, `fields` from the base `fields` merged with `overrides.<kind>.fields`.
2. If `test = true`, `POST /api/v3/<resource>/test`; a failure is logged and the item skipped.
3. `GET` the app's list; if an entry with the same `name` exists, `PUT` it (fields the app already has are kept, ours override); else `POST`.

Resources: `[[shared.download_client]]` → `downloadclient`,
`[[shared.notification]]` → `notification`. Indexers are added on the
[Indexers](Indexers.md) page instead.
Field names are whatever the app's own UI posts; the quickest way to find them
is the app's API docs (`/api/v3/downloadclient/schema`) or the browser's
network tab. Per-kind differences (category names) go in `overrides`.

Bazarr and the download clients have none of these resources and are skipped.
The stack's own download clients are added to the arrs without any of this
([App Connections](App-Connections.md)).
