# Indexers

The **Indexers** page (sidebar → Indexers, `/indexers`) keeps trackers and
aggregators in wranglarr and pushes them to the arrs you pick
(`src/indexers.rs`).

## Prowlarr in the stack first

With the `prowlarr` profile enabled ([Service: Prowlarr](Service-Prowlarr.md)),
the simplest path is to add your trackers once in Prowlarr. wranglarr
registers every arr in it as an application ([App
Connections](App-Connections.md)), and Prowlarr pushes its indexers to them
itself. The page's **Prowlarr in this stack** panel shows, per arr, whether
it is registered.

The sources below are for anything you'd rather not run through Prowlarr: a
tracker's own Torznab feed, a usenet indexer, Jackett, NZBHydra2, or a
Prowlarr elsewhere.

## Sources

**Add tracker or aggregator** opens the form. A source is one of:

| Type | What each picked arr gets |
| --- | --- |
| Tracker (Torznab) | one Torznab indexer, named after the source. The API key may be blank. |
| Usenet indexer (Newznab) | one Newznab indexer, e.g. `https://api.nzbgeek.info` |
| Jackett | one Torznab indexer per tracker configured in Jackett, named `<tracker> (<source name>)`. The list comes from Jackett's `t=indexers&configured=true`, and is read again on every sync. |
| NZBHydra2 | one Newznab indexer; with **Torrents too** ticked, also a Torznab indexer `<source name> (torrents)` for Hydra's `/torznab` feed |
| Prowlarr (outside this stack) | no indexers from wranglarr: the picked arrs are registered in that Prowlarr as applications (Full Sync) and it pushes its own indexers. Pointing it at the Prowlarr in this stack is refused, because that one is wired already. |

Fields:

| Field | |
| --- | --- |
| Name | 1–60 letters, digits, spaces and `. _ ( ) - + &`; unique |
| URL | as the arrs reach it. A container name such as `http://jackett:9117` works inside the stack. For Torznab/Newznab a URL ending in `/api` is split into base URL + API path `/api`. |
| API key | required except for Torznab. Blank when editing keeps the stored key. |
| Push to | the arrs that get it. All ticked means every arr, including ones enabled later. Prowlarr is never a target. |
| Priority | 1–50, 1 most preferred (default 25, the arrs' own default) |
| Min. seeders | Torznab indexers only (default 1) |
| Enabled | off removes the source's indexers from the arrs on the next sync, and keeps the source |

Each indexer is created with RSS, automatic and interactive search on, and
the arr's default Newznab/Torznab categories:

| Arr | Categories |
| --- | --- |
| Radarr | 2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060, 2070, 2080 |
| Sonarr | 5000, 5010, 5020, 5030, 5040, 5045, 5050, 5090; anime 5070 |
| Lidarr | 3000, 3010, 3030, 3040, 3050, 3060 |
| Readarr | 3030, 7000, 7010, 7020, 7030, 7040, 7050, 7060 |
| Whisparr | 6000, 6010, 6020, 6030, 6040, 6045, 6050, 6070, 6080, 6090 |

Sources, API keys included, are stored in wranglarr's state db
(`appdata/wranglarr/wranglarr.db`, kv `indexers:sources`), not in
`wranglarr.toml`. The API never returns the keys, only whether one is set.

## Syncing

**Save & sync** saves the source and starts a sync run; deleting a source
starts one too; **Sync to arrs** runs one by hand. A sync, per arr:

- creates each indexer that should be there and isn't, and updates one whose URL, API path, categories, seeders or priority changed. Indexers are matched by name. The arr tests the feed before it saves.
- removes the indexers wranglarr created that are no longer wanted: the source was deleted or disabled, the arr was un-picked, or the tracker is gone from Jackett.

wranglarr records what it created in each arr (kv `indexers:pushed`), so
indexers you added by hand are never touched. If one of yours already has
the name a source would use, the sync reports it as a failure and leaves it
alone; rename the source to manage it here. If a source can't be read
during a sync (Jackett down, say), its indexers are left as they are rather
than removed.

For an external Prowlarr, a sync creates or updates an application per
picked arr, using the arr's `url` from `wranglarr.toml` (for example
`http://radarr:7878`), so that Prowlarr must be able to reach the arrs at that
address. Prowlarr tests the application before saving. Un-picking an arr
removes its application. Deleting the source leaves the applications in that
Prowlarr; the run log says so, and you remove them there.

At startup, if any sources exist, a background thread waits (up to 10
minutes) for the arrs to answer and runs a sync if some arr lacks indexers it
should have, for example an arr you just enabled.

The table shows each source against each arr: ✓ (all its indexers present),
`some`, `missing`, `?` (the arr couldn't be read), `–` (not picked). Below it,
the number of indexers each arr has right now.

## API

```
GET    /api/indexers                                 sources (without keys), per-arr state, targets, types, Prowlarr in the stack
POST   /api/indexers/sources                         SourceIn; saves, then starts a sync -> {"source": …, "run": id}
DELETE /api/indexers/sources/{id}                    removes the source, then starts a sync -> {"run": id}
POST   /api/indexers/sync                            -> {"run": id}
```

`SourceIn`: `{"id": "", "type": "torznab"|"newznab"|"jackett"|"nzbhydra"|"prowlarr", "name": "…", "url": "…", "api_key": "", "apply_to": [], "enabled": true, "priority": 25, "min_seeders": 1, "torrents": false}`.
An empty `id` adds a source, an existing one edits it; `apply_to` is a list
of arr app names, `[]` for every arr. A validation error is a 400. If
another system run is in progress, save and delete still apply and return
`"run": null`; the sync starts by itself once that run ends. `POST
/api/indexers/sync` returns 409.
