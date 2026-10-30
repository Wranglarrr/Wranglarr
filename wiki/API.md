# HTTP API

Everything the wranglarr UI does is a JSON call to its own HTTP API. Scripts,
Home Assistant, cron jobs and monitoring can make the same calls. This page
lists every address, what it takes and what it answers.

- **Base URL:** `http://<host>:8787` (the port is `WRANGLARR_PORT` in `.env`),
  or wranglarr's address behind your reverse proxy.
- **JSON in, JSON out.** Send a body as JSON with
  `Content-Type: application/json`. Two addresses answer with something
  else: an app's logo (an image) and the AuthTunnel gate (an empty answer).
- **Times** are ISO 8601 in UTC. A backup **version** id is its start time,
  `YYYYMMDDTHHMMSSZ` (with `-1`, `-2` … added when two start in the same second).
- **Query flags** such as `refresh` take `true`/`false` or `1`/`0`.

This page is the API's reference; wranglarr serves no generated schema.

## Authentication

Every address needs a sign-in, except the few listed under
[Without a sign-in](#without-a-sign-in). There are two kinds of sign-in.

### API keys (scripts)

Create one under **Account → API keys** (needs your password). The key starts
with `wr_`, is shown once, and only its hash is stored. Send it in the
`X-Api-Key` header. It is not accepted as a query parameter or as a bearer
token.

```sh
export WR=http://nas:8787 KEY=wr_...
curl -fsS -H "X-Api-Key: $KEY" "$WR/api/suites"
curl -fsS -X POST -H "X-Api-Key: $KEY" "$WR/api/suites/media/backup"     # -> {"run": 42}
```

A request with an API key needs no `Origin` header. A key can do everything on
this page except the account itself: the password, two-factor, passkeys, API
keys and sessions answer `403` "this needs a signed-in browser, not an API
key". **Revoke** on the Account page stops a key at once. See
[Login](Login.md#api-keys).

### Browser sessions

Signing in sets the `wranglarr_session` cookie (HttpOnly, SameSite=Strict, or
Lax with AuthTunnel; `Secure` over HTTPS). It lasts 24 hours, or 30 days with
**Remember me**. A script can sign in the same way, but an API key is simpler:

```sh
curl -fsS -c jar -H "Origin: $WR" -H "Content-Type: application/json" \
     -d '{"username":"admin","password":"..."}' "$WR/api/auth/login"
curl -fsS -b jar -H "Origin: $WR" -X POST "$WR/api/suites/media/backup"
```

With two-factor on, the login answers `{"mfa": true}` and sets a short-lived
`wranglarr_mfa` cookie. Send the code to `POST /api/auth/login/totp` with the
same cookie jar to get the session.

### The Origin check

A request signed in with a **session cookie** that changes something (any
method but GET, HEAD and OPTIONS) must come from wranglarr's own pages. The
`Origin` header (or, without one, `Referer`) must name the host the request
was sent to: the `Host` header, or `X-Forwarded-Host` from a trusted proxy. Otherwise the
answer is `403` with
`{"error": "refused: this request didn't come from wranglarr's own pages"}`.

Browsers send `Origin` on their own. A non-browser client using a session
cookie must send `Origin: http://<the host:port it calls>`. The same check
applies to the sign-in POSTs below, which work without a session. Requests
with an API key are not checked.

Behind a reverse proxy, preserve `Host` or set `X-Forwarded-Host`, and set
`X-Forwarded-Proto`. Configure the proxy's exact source IP in
`FORWARDED_ALLOW_IPS` (default `127.0.0.1`); untrusted peers' forwarded host
and scheme are ignored. See [Login](Login.md#passkeys) for the trust and port
access requirements.

### Without a sign-in

| Address | Notes |
| --- | --- |
| `GET /api/health` | answers `{"ok": true}` only; the details need a sign-in |
| `GET /api/auth/gate` | AuthTunnel's check, see [Sign-in and account](#sign-in-and-account) |
| `POST /api/auth/setup` | creating the account (needs the setup code from the log) |
| `POST /api/auth/login`, `POST /api/auth/login/totp` | password sign-in |
| `GET /api/auth/passkey/login-options`, `POST /api/auth/passkey/login` | passkey sign-in |
| `GET /login`, `GET /setup` | the sign-in pages |
| `/static/…` | stylesheets, scripts, icons |

Any other `/api/` address without a sign-in answers `401`
`{"error": "sign in first (or send an X-Api-Key header)"}`. A page redirects to
`/login` (or to `/setup` while there is no account yet).

### WRANGLARR_AUTH=off

With `WRANGLARR_AUTH=off` in `.env` there is no login: every address answers
without a key or cookie. A change that carries an `Origin` or `Referer` header
naming another site is still refused, so other web pages can't drive wranglarr;
scripts that send neither header are let through. The account
endpoints answer `403`, and `GET /api/auth/me` answers `{"enabled": false}`.
Only do this behind a reverse proxy that asks for a login itself
([Login](Login.md)).

## Jobs and runs

Backups, restores, updates, rollbacks, app connections, indexer syncs and
shared-settings syncs run in the background. Their POST answers `202` with
the run's id:

```json
{"run": 42}
```

Ask `GET /api/runs/{id}` until `status` is no longer `queued` or `running`:

```json
{
  "id": 42, "suite": "media", "kind": "backup", "target": "",
  "status": "ok",
  "started": "2026-10-26T03:30:00Z", "finished": "2026-10-26T03:31:12Z",
  "log": "2026-10-26T03:30:00Z backup of suite media -> version 20261026T033000Z (manual)\n...",
  "result": {"version": "20261026T033000Z", "pruned": [], "total_bytes": 812345678}
}
```

| Field | Values |
| --- | --- |
| `status` | `queued` → `running` → `ok`, `partial` (some parts failed) or `failed`. A run still queued or running when wranglarr restarts is marked `failed` ("abandoned: wranglarr restarted while this ran"); a container it had stopped may still be stopped |
| `kind` | `backup`, `restore`, `restore-plan`, `sync`, `connect`, `update`, `rollback`, `indexers` |
| `suite` | the suite's name; `system` for connect, update, rollback and indexers runs |
| `target` | what the run is about: `radarr@20261026T033000Z`, `all`, the containers, … |
| `log` | the whole log as text, one timestamped line per step |
| `result` | per kind, below; `{"error": "…"}` when the run failed outright |

| Kind | `result` |
| --- | --- |
| `backup` | `version`, `pruned` (versions removed by retention), `total_bytes` |
| `restore` | `version`, `app`, `databases` (names restored), `config` (true/false) |
| `restore-plan` | `done`, `failed`, `unavailable` (lists of `app@version`) |
| `sync` | `results`: `[{"app", "resource", "name", "action", "error"?}]` |
| `connect` | `results`: `[{"link", "src", "dst", "action", "detail"}]`; `action` is `created`, `updated`, `kept`, `connected`, `waiting` or `failed`. `kept`: already set up (in "new" mode left alone; in "all" mode saved again with its key, nothing else changed) |
| `update` | `results`: `{"<container>": "updated" \| "recreated" \| "current" \| "held" \| "skipped" \| "refused" \| "failed"}`, and `reports`: `{"<container>": {"key", "id", "title", "search", "state"}}` for the failures that left a [report](Updates.md#failure-reports) |
| `rollback` | `container`, `image`, and `data_from` (the backup version) when its data went back too |
| `indexers` | `results`: `[{"app", "indexer", "action", "error"?}]` |

**One run per suite at a time.** Starting another while one is queued or
running answers `409` ("suite media already has a run in progress"). All
`system` runs share one slot: an update is refused while an app-connections
or indexer run is going, and the other way round. Nothing queues: try again
when the other run has ended.

Poll from the shell (needs `jq`):

```sh
#!/bin/sh
# back up a suite and wait for it; exit 1 unless it ended ok
WR=http://nas:8787 KEY=wr_... SUITE=media
run=$(curl -fsS -X POST -H "X-Api-Key: $KEY" "$WR/api/suites/$SUITE/backup" | jq -r .run) || exit 1
while :; do
  status=$(curl -fsS -H "X-Api-Key: $KEY" "$WR/api/runs/$run" | jq -r .status)
  case "$status" in queued|running) sleep 5 ;; *) break ;; esac
done
echo "run $run: $status"
[ "$status" = ok ] || { curl -fsS -H "X-Api-Key: $KEY" "$WR/api/runs/$run" | jq -r .log; exit 1; }
```

Or from Python, with the standard library only:

```python
import json, time, urllib.request

WR, KEY = "http://nas:8787", "wr_..."

def call(method, path, body=None):
    req = urllib.request.Request(WR + path, method=method, headers={"X-Api-Key": KEY},
                                 data=json.dumps(body).encode() if body is not None else None)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req) as r:
        return json.load(r)

run = call("POST", "/api/suites/media/backup")["run"]
while (r := call("GET", f"/api/runs/{run}"))["status"] in ("queued", "running"):
    time.sleep(5)
print(r["status"], r["result"])
```

`urlopen` raises `urllib.error.HTTPError` on a `4xx`/`5xx`; its body holds the
`{"error": …}` described next.

## Errors

Every error comes back as JSON with one field:

```json
{"error": "suite media already has a run in progress"}
```

| Status | When |
| --- | --- |
| `200` | done; the body is the answer |
| `201` | created (a notification channel, a media server) |
| `202` | a job was started: `{"run": id}`. Saving or deleting an indexer source also answers `202`, with `"run": null` when another system run is busy: the sync follows it |
| `204` | the AuthTunnel gate lets a request through |
| `400` | the input is wrong; the text says what (an invalid cron schedule, a missing field, a bad address) |
| `401` | not signed in, or a wrong password or code at sign-in |
| `403` | the Origin check refused it; an account endpoint called with an API key; a wrong current password or setup code |
| `404` | no such suite, version, app, run, channel, server, source, container… |
| `409` | busy (a run in progress), or nothing to do (no restore plan, no update to roll back, Docker control off, the account already exists) |
| `422` | the body doesn't have the expected shape: a missing required field or a wrong type, e.g. `{"error": "app: required"}` |
| `429` | too many failed sign-ins from this address: 5 in 15 minutes locks it for 15 minutes (a restart clears it) |
| `502` | something wranglarr asked failed: plex.tv, a Jellyfin or Emby server, an arr during a Community configs call |
| `500` | a bug; the log (`docker compose logs wranglarr`) has the traceback |

An address that doesn't exist answers `404 {"error": "Not Found"}`, and a
wrong method `405 {"error": "Method Not Allowed"}`.

A job that fails after it started doesn't change the POST's answer: the
POST still answered `202`, and the run ends `failed` or `partial`.

## Endpoints

Below, `{suite}`, `{app}`, `{version}` and so on are path parts. "Body" is
the JSON to send. **Changes the apps**, **stops containers** and **deletes**
mark calls that do more than change wranglarr's own settings.

### Suites and backups

| Method | Path | Body / query | Answer |
| --- | --- | --- | --- |
| GET | `/api/suites` | | every suite's summary (below) |
| GET | `/api/suites/{suite}` | | one suite's summary |
| GET | `/api/suites/{suite}/overview` | `?refresh=true` skips the 5-minute cache | each app asked live (below) |
| PUT | `/api/suites/{suite}/settings` | schedule and retention (below) | the suite's settings; `400` for a bad cron schedule |
| DELETE | `/api/suites/{suite}/settings` | | back to `wranglarr.toml`'s values; the settings |
| POST | `/api/suites/{suite}/backup` | none | `202 {"run": id}`; `409` busy |
| POST | `/api/suites/{suite}/prune` | none | **deletes** versions retention no longer keeps: `{"pruned": ["20260901T033000Z", …]}` |

A suite summary:

```json
{
  "name": "media",
  "apps": [{"name": "radarr", "kind": "radarr", "databases": ["radarr", "radarr-log"]}],
  "settings": {"schedule": "30 3 * * *", "enabled": true, "keep_last": 14, "keep_days": 0, "source": "config"},
  "usage": {"versions": 12, "bytes": 9123456789, "newest": {"...": "the newest version's manifest"}},
  "next_run": "2026-10-27T03:30:00+00:00",
  "active_run": null,
  "shared": [{"resource": "downloadclient", "label": "download client", "name": "qBittorrent",
              "apps": ["radarr", "sonarr"]}]
}
```

`settings.source` is `config` (from `wranglarr.toml`) or `ui` (changed here;
then `updated` says when). `active_run` is the queued or running run, if any.

The settings body. A field left out keeps its current value (an empty
`schedule` means no scheduled backups):

```json
{"schedule": "30 3 * * *", "enabled": true, "keep_last": 14, "keep_days": 0}
```

`keep_last` and `keep_days` of `0` mean no limit of that kind.

The overview asks every app in the suite now:
`{"suite", "fetched", "apps": [...], "postgres": {"version"} | null}`. Each app
has `name`, `kind`, `url`, `online`, `databases` (`[{"name", "size"}]`) and,
when it answered, `version`, `database_type`, `instance_name`, `branch`,
`collection` (counts: movies, episodes, …), `scope` (counts of indexers,
download clients, profiles, …) and `health` (its warnings and errors). An app
that didn't answer has `error` instead.

Prune and version delete answer `409` while a run is queued or running in the
suite, or while another run uses its versions (a restore plan, an update's
pre-update backup).

### Versions and restore

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/api/suites/{suite}/versions` | | every version's manifest, newest first, with what changed since the one before |
| GET | `/api/suites/{suite}/versions/{version}` | | that version's `manifest.json`; `404` |
| DELETE | `/api/suites/{suite}/versions/{version}` | | **deletes** the version's folder: `{"deleted": "<version>"}`; `404` when there's no such version; `409` while a run uses the suite |
| POST | `/api/suites/{suite}/versions/{version}/restore` | `{"app": "radarr", "databases": true, "config": true}` | `202 {"run": id}`; **stops containers**; `404` for an app not in the suite or no such version; `409` busy |

A manifest has `suite`, `version`, `created`, `trigger` (`manual`,
`scheduled`, `pre-update`), `wranglarr` (its version), `status` (`ok`,
`partial`, `failed`; `running` while the backup runs; `incomplete` in the list
for a folder without a readable manifest), `total_bytes`, `postgres_version`,
`vpn`, and `apps`: per app its `kind`, `version`, `databases`
(`[{"name", "file", "size", "sha256"}]`), `config_zip`, `config_snapshot`,
`collection` and `errors`. The list adds to each:

- `deltas`: per app, `version_changed`, `prev_version`, `count_key`, `count_delta`;
- `changes`: `[{"app", "kind", "what": "added" | "removed" | "upgraded" | "count" | "error", …}]`;
- `size_delta`: bytes more or less than the version before (`null` for the oldest).

A restore of `databases` stops the app's container (when wranglarr has the
Docker socket and knows the container), restores each dump, and starts it
again. Without Docker control it restores while the app runs and asks the
app to restart. `config` restores an arr's or Bazarr's own config backup
through its API (the app restarts), or, for the download clients and Seerr,
the copy of its `/config` with its container stopped. Those can't have their
config restored without Docker control. Both flags default to `true`. Details:
[Backups and Restore](Backups-and-Restore.md).

### Runs

| Method | Path | Query | Answer |
| --- | --- | --- | --- |
| GET | `/api/runs` | `suite` (`system` for the stack-wide jobs), `limit` (default 20) | runs, newest first, each as in [Jobs and runs](#jobs-and-runs) |
| GET | `/api/runs/{id}` | | the run with its whole log; `404` |

### Restore plan

The setup wizard writes a restore plan when you pick versions to bring back
on a new install ([Setup](Setup.md)).

| Method | Path | Answer |
| --- | --- | --- |
| GET | `/api/restore-plan` | the plan, or `{"items": []}` |
| POST | `/api/restore-plan/run` | `202 {"run": id}`; **stops containers**; `409` when there's no plan, none of its versions exist, an item was interrupted, or the suite is busy |
| DELETE | `/api/restore-plan` | **deletes** the plan file: `{"discarded": true}`; `409` while a plan run is queued/running |

```json
{"created": "2026-10-20T12:00:00+00:00", "source": "setup wizard",
 "items": [{"suite": "media", "version": "20261019T033000Z", "app": "radarr",
            "databases": true, "config": true, "available": true}]}
```

`available` says whether the app is configured and its version is in the store.
Successful items are removed immediately; unavailable items remain pending.
The plan is removed only when no items remain. `started: true` marks an item
whose restore failed or was interrupted and requires review before any retry.
A failure stops the plan. Discarding a plan does not undo completed work.

### Shared settings

Download clients and notifications defined once in `wranglarr.toml`
(`[[shared]]`, see [Shared Settings](Shared-Settings.md)), listed in each
suite summary's `shared`.

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| POST | `/api/suites/{suite}/sync` | `{"app": "radarr"}`, or none / `{}` for every app in the suite | `202 {"run": id}`; **changes the apps**; `404` no such app; `409` busy |

### Container updates

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/api/updates` | | the overview (below), from the last check |
| POST | `/api/updates/check` | none | asks the registries now and answers the overview. It waits for every registry, so it can take a while; `409` when Docker control is off |
| POST | `/api/updates/apply` | `{"containers": ["radarr"], "force": false}`; none or `"containers": null` = everything with an update that isn't held | `202 {"run": id}`; **stops containers** (recreates them); `404` not a managed container; `409` Docker control off, nothing to update, or busy |
| POST | `/api/updates/rollback` | `{"container": "radarr", "restore_data": true}` | `202 {"run": id}`; **stops containers**; then holds it. `restore_data` (default `false`) also puts the app's data back from the backup taken just before that update; `404` not a managed container; `409` no successful update to roll back, that backup isn't kept any more, its data can't be put back with the app stopped (an SQLite arr whose config folder isn't mounted into wranglarr read-write), a container left parked as `<name>-wranglarr-old` from an earlier update or rollback, or busy |
| POST | `/api/updates/hold` | `{"container": "radarr", "hold": true}` | the overview; `"hold": false` releases it; `404` |
| POST | `/api/updates/self` | none | `202 {"run": id}`; **replaces wranglarr's own container**, so its API goes away for a while; needs a signed-in browser session (`403` with an API key); `409` Docker control off, its container not found, it can't update itself here (the reason), or an update of wranglarr already under way |
| PUT | `/api/updates/settings` | below | the settings; `400` for a bad cron schedule |
| DELETE | `/api/updates/settings` | | back to `wranglarr.toml`'s values; the settings |
| PUT | `/api/updates/tracks/{container}` | `{"tag":"develop"}`; `null` or blank follows the deployed tag/digest | overview; stores a preference without restarting; invalid tag `400`, unknown container `404`, queued/running update `409` |
| DELETE | `/api/updates/tracks/{container}` | | overview; removes the UI track override and uses `[updates.tracks]` or the deployed image |
| GET | `/api/updates/reports` | | `{"reports": [...]}`: the kept failure reports, newest first, each as below |
| GET | `/api/updates/reports/{key}` | | one failure report, by its `key` (`<run>-<container>`; `id` is the fingerprint, shared by the same failure): its fields, `state` (how the container was left, as Docker tells it afterwards: `running`, `unhealthy`, `stopped` as it was, `not_started` (back under its name, but it wouldn't start), `parked`, `rejected` (the new container couldn't be removed; it's stopped and the old one parked), `rejected_running` (it couldn't be stopped either) or `unknown`), `blocked` (whether the new image is skipped from now on), `data`, `outcome` (those in a sentence), `seen` and `first_seen` (how often and since when its fingerprint has failed here), `title`, `markdown` and `links` (`search`, `new_issue`, `body_in_link`; `null` links when there's no repo to report to); `404` |
| GET | `/api/updates/reports/{key}/reported` | | `{"issue": {"number", "title", "state", "url"}}`: the oldest issue in the report's repo, open or closed, with its fingerprint; `{"issue": null}` when there's none. wranglarr asks GitHub's search; `404` no such report, `409` no repo to look in, `502` GitHub didn't answer or is limiting searches |
| DELETE | `/api/updates/reports/{key}` | | `{"forgotten": key}`; `404` |

An update pulls the new images first, backs up the suites of the apps it is
about to replace (`backup_first`), then recreates each container with the
settings it had and waits for it to be healthy, putting the old one back if
it isn't. With `force`, it updates an app even when that pre-update backup
was incomplete. Holds only stop automatic updates: a container you name in
`containers` is updated even when held. `apply` never touches wranglarr's
own container: `/api/updates/self` does, through a helper container, and the
run (kind `self-update`) is finished by the wranglarr that comes up after it.
See [Keeping wranglarr itself up to date](Updates.md#keeping-wranglarr-itself-up-to-date).

Checks and updates use each container's selected track within its existing
image repository. A target's `image` is the deployed tracking reference;
`target_image` is the reference checked. `track` includes `tag` (null follows
the deployed image), `source` (`ui`, `config`, or `container`), the selected
`image`, publisher-specific `choices` (`tag`, `label`), `docs`, `changed`, and
an optional `error`. Changing a track invalidates that container's cached
check. Track preferences survive restarts and are separate from Automation
settings. Namespace-only recreations preserve dependents' exact running image.

The settings body. A field left out keeps its current value:

```json
{"enabled": true, "check_schedule": "0 */6 * * *", "auto_update": false,
 "auto_schedule": "", "backup_first": true, "self_update": false, "self_schedule": "30 5 * * *"}
```

The overview:

```json
{
  "settings": {"enabled": true, "check_schedule": "0 */6 * * *", "auto_update": false,
               "auto_schedule": "", "backup_first": true, "self_update": false,
               "self_schedule": "30 5 * * *", "source": "config"},
  "next": {"check": "2026-10-26T18:00:00+00:00", "auto": null, "self": null},
  "docker": true,
  "available": 2,
  "own": {"container": "wranglarr", "role": "self", "status": "self", "newer": true,
          "remote_digest": "sha256:…", "self_update": true, "self_update_blocker": null,
          "update_hint": "update it from the shell: `docker compose pull wranglarr && docker compose up -d wranglarr`",
          "error": "a newer wranglarr is out: press Update now", "blocked": null, "last_update": null},
  "targets": [{"container": "radarr", "role": "app", "suite": "media", "app": "radarr", "kind": "radarr",
               "status": "available", "image": "lscr.io/linuxserver/radarr:latest", "version": "5.14.0",
               "running": true, "health": "healthy", "checked": "2026-10-26T12:00:03+00:00", "error": null,
               "held": false, "hold_reason": null, "blocked": null, "last_update": null,
               "can_rollback": false, "rollback_to": null, "rollback_backup": null, "report": null,
               "interrupted": null, "recovery": null}],
  "history": [{"id": 7, "container": "sonarr", "action": "update", "status": "ok", "old_version": "4.0.9",
               "new_version": "4.0.10", "run_id": 40, "at": "2026-10-20T04:00:12Z", "note": ""}]
}
```

`report` is `{"key", "id", "title", "search", "state"}` while the container's last
update is a failed one that left a failure report, else `null`. `interrupted` names
the parked `<name>-wranglarr-old` container while there is one, and `recovery` says
why it was left stopped and what to do, when an update or rollback stopped because
the data couldn't go back.

The full `/api/updates/reports/{key}` response records recovery facts separately:
`state` describes the container's final availability; `recovery_phase` identifies
an incomplete container recovery (`remove_new`, `restore_data`, `rename_old` or
`start_old`), or is `null` when those steps completed. `data_recovery` contains
`state`, `completed` and `api`. Its state is `untouched` (no recovery writes
attempted), `restoring`, `offline_restored`, `verified` or `needs_recovery`.
`completed` lists the database/file steps that finished. The API outcome is
`not_attempted`, `unavailable_before_upload`, `request_failed`,
`upload_outcome_unknown`, `restart_not_ready` or `verified`. An app answering
requests does not prove that an uncertain config upload succeeded; inspect both
the container and data outcomes. `data` retains the human-readable explanation.
`request_failed` means the app returned an error and still answers, not that the
restore made no changes.

`role` is `app`, `postgres`, `vpn`, `extra` or `wranglarr`. `status` is `unchecked`,
`current`, `available`, `pinned` (a digest in the image reference), `local`
(built here, not pulled), `self` (wranglarr itself), `blocked` (a new image
wranglarr refuses, such as a Postgres major upgrade), `missing` or `error`.

`own` is wranglarr's own row (also in `targets`), or `null` when it
isn't running in a container it can find. `newer` is set when the registry
has a newer image. `self_update` says whether **Update now** can be used for
it. `self_update_blocker` says why it can't (built here, pinned to a digest,
no Docker socket or state folder mount), and then `update_hint` is the shell
command.

### App connections

Download clients into the arrs and Prowlarr, the arrs into Prowlarr, Sonarr
and Radarr into Bazarr and Seerr ([App Connections](App-Connections.md)).

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/api/connect` | | each link and its state, asked from the apps now (below) |
| POST | `/api/connect` | `{"mode": "all", "links": []}` | `202 {"run": id}`; **changes the apps**; `400` bad mode; `404` none of the links exist; `409` busy |

A link's `key` is `<the app pointed at>><the app being configured>`:
`qbittorrent>radarr` is qBittorrent as a download client in Radarr,
`radarr>prowlarr` is Radarr as an application in Prowlarr, `sonarr>bazarr`,
`radarr>seerr`. `mode` is `all` (create or update every link) or `new` (only
links never handled before). An empty `links` means every link. Both fields
are optional.

```json
{"links": [{"key": "qbittorrent>radarr", "src": "radarr", "src_kind": "radarr", "dst": "qbittorrent",
            "dst_kind": "qbittorrent", "what": "download client", "handled": true,
            "target": "qbittorrent:8090", "state": "connected", "detail": "'qBittorrent', category radarr"}],
 "auto": true, "missing": 0}
```

`state` is `connected`, `differs` (`detail` says how), `missing`, `waiting`
(Seerr isn't set up yet) or `error`.

### Live status and API keys

`POST /api/hooks/{app}` (the arrs' webhook, signed per app, no sign-in),
`GET /api/events`, `POST /api/events/wire`, `GET /api/events/stream`
(Server-Sent Events) and `GET /api/keys` (signed-in pages only):
[Live status and API keys](Live-Status.md#api).

### Indexers

Trackers, usenet indexers, Jackett, NZBHydra2 or an outside Prowlarr, pushed
to the arrs you pick ([Indexers](Indexers.md)).

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/api/indexers` | | sources and their state in each arr, asked now (below) |
| POST | `/api/indexers/sources` | a source (below) | `202 {"source": …, "run": id \| null}`; saves it and starts a sync; **changes the apps**; `400` |
| DELETE | `/api/indexers/sources/{id}` | | `202 {"run": id \| null}`; the sync **removes** the indexers wranglarr added for it; `404` |
| POST | `/api/indexers/sync` | none | `202 {"run": id}`; **changes the apps**; `409` busy |

`run` is `null` when another `system` run was busy: the source is saved, and
the next sync applies it.

A source. Leave `id` out to add one; send it to change one. A blank
`api_key` when changing keeps the stored key.

```json
{"id": "", "type": "jackett", "name": "Jackett", "url": "http://jackett:9117",
 "api_key": "...", "apply_to": [], "enabled": true, "priority": 25, "min_seeders": 1, "torrents": false}
```

- `type`: `torznab`, `newznab`, `jackett`, `nzbhydra` or `prowlarr` (a Prowlarr outside this stack).
- `name`: 1-60 letters, digits, spaces and `. _ ( ) - + &`; unique.
- `url`: `http://` or `https://`.
- `api_key`: required, except for `torznab`.
- `apply_to`: arr names; `[]` means every arr.
- `priority`: 1-50. `min_seeders`: for torrent indexers, `0` or more (default `1`).
- `torrents`: NZBHydra2 only: also add its Torznab feed.

`type`, `name` and `url` are required (`422` without them).

The answer to GET. `api_key` is never returned; `has_key` says whether one is
stored.

```json
{"targets": [{"name": "radarr", "kind": "radarr"}],
 "sources": [{"id": "a1b2c3d4", "type": "jackett", "type_label": "Jackett", "name": "Jackett",
              "url": "http://jackett:9117", "has_key": true, "apply_to": [], "enabled": true,
              "priority": 25, "min_seeders": 1, "torrents": false, "count": 6,
              "detail": "6 tracker(s) configured in Jackett", "cells": {"radarr": "ok"}}],
 "errors": {}, "types": {"torznab": "Tracker (Torznab)", "...": "..."},
 "stack_prowlarr": [{"name": "prowlarr", "url": "http://prowlarr:9696"}], "indexer_counts": {"radarr": 9}}
```

`cells` per arr: `ok`, `partial`, `missing`, `off` (not picked or the source
is off) or `error`.

### Notifications

Channels, events and the watcher ([Notifications](Notifications.md)).

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/api/notify` | | everything the page shows (below) |
| POST | `/api/notify/channels` | a channel (below) | `201 {"channel": …, "arrs": […]}`; **changes the apps** when `arr.apps` is set; `400` |
| PUT | `/api/notify/channels/{id}` | a channel | `{"channel": …, "arrs": […]}`; `400`; `404` |
| DELETE | `/api/notify/channels/{id}` | | `{"arrs": […]}`; removes it from the arrs too; `404` |
| POST | `/api/notify/test` | `{"channel": {…}, "id": "a1b2c3d4"}` | sends a test: `{"ok": true}` or `{"ok": false, "error": "…"}`. With only `id`, the saved channel is tested; `404` for an unknown `id`, `400` when the channel itself is invalid |
| PUT | `/api/notify/settings` | `{"interval": 5, "disk_free_gb": 20, "stall_minutes": 60, "link": "https://wranglarr.example.com"}` | the settings; fields left out keep their value; `400` |
| POST | `/api/notify/check` | none | one watcher round now, which **sends** what's new: `{"at", "problems", "sent"}`, or `{"skipped": "a check is already running"}` |

A channel:

```json
{"kind": "ntfy", "name": "Phone", "enabled": true,
 "config": {"server": "https://ntfy.sh", "topic": "my-arrs-x7k2", "token": ""},
 "events": ["backup_failed", "restore", "update_failed", "app_down", "vpn_down", "disk_low"],
 "arr": {"apps": ["radarr", "sonarr"], "events": ["import", "manual", "health"]}}
```

| `kind` | `config` keys (secret ones in bold) |
| --- | --- |
| `telegram` | **`bot_token`**, `chat_id`, `topic_id` |
| `discord` | **`webhook_url`** |
| `slack` | **`webhook_url`** |
| `ntfy` | `server`, `topic`, **`token`**, `username`, **`password`** |
| `gotify` | `server`, **`token`** |
| `pushover` | **`user_key`**, **`token`** |
| `apprise` | `server`, `key`, **`urls`** (a `key` or `urls` is needed) |
| `email` | `host`, `port`, `security` (`starttls`, `ssl`, `none`), `username`, **`password`**, `sender`, `to` |
| `webhook` | **`url`**, `username`, **`password`** |

`events` are wranglarr's own: `backup_failed`, `backup_ok`, `restore`,
`update_available`, `update_done`, `update_failed`, `self_update_available`, `sync_failed`, `api_key_changed`,
`app_down`, `app_health`, `vpn_down`, `disk_low`, `download_problem`,
`import_blocked`, `sign_in_lockout`. `arr.apps` adds the channel to those
arrs' Settings → Connect, as "wranglarr · <name>", sending `arr.events`:
`grab`, `import`, `upgrade`, `manual`, `health`, `app_update` (default
`import`, `manual`, `health`). `name` defaults to the service's name.

**Secrets are never sent back.** Answers show secret fields as `••••••••`
(also given as `mask` in `GET /api/notify`). Sending `••••••••` back keeps the
stored value, so a client can GET a channel, change a field and PUT it whole.
A PUT replaces the channel, so leave nothing out. For `test`, `id` names the
saved channel whose secrets fill in the masked fields; leave it out for a new
channel.

`arrs` in the answers says what happened in each arr:
`[{"app": "radarr", "action": "created" | "updated" | "removed" | "none"}]`, or
`{"app", "error"}` (with the secrets taken out of the text).

`GET /api/notify` answers `kinds` (each service's fields, as the form builds
them), `events`, `arr_events`, `arr_events_default`, `arrs`, `channels`,
`settings`, `watching` (the problems the watcher tracks now: `key`, `event`,
`title`, `message`, `since`, `sent`), `log` (the last 40 messages, with
which channels they reached) and `mask`.

### Media servers and Seerr

Plex, Jellyfin, Emby and Kodi in the arrs' Connect settings, and Seerr's
state ([Connections](Connections.md)).

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/api/connections` | | `{"media": …, "seerr": … \| null}` (below) |
| POST | `/api/connections/media` | a server (below) | `201 {"server": …, "arrs": […]}`; **changes the apps**; `400` |
| PUT | `/api/connections/media/{id}` | a server | `{"server": …, "arrs": […]}`; `400`; `404` |
| DELETE | `/api/connections/media/{id}` | | `{"arrs": […]}`; removes it from the arrs too; `404` |
| POST | `/api/connections/media/test` | `{"server": {…}, "id": "a1b2c3d4"}` | `{"ok": true, "version", "libraries", …}` or `{"ok": false, "error"}`, also for invalid input |
| POST | `/api/connections/plex/pin` | none | starts Plex's sign-in: `{"id": 123456, "auth_url": "https://app.plex.tv/auth#?…"}`; `502` |
| GET | `/api/connections/plex/pin/{id}` | | `{"done": false}` until the user approves at `auth_url`, then `{"done": true, "token", "servers": [{"name", "token", "addresses"}]}`; `502` |
| POST | `/api/connections/jellyfin/key` | `{"url": "http://192.168.1.10:8096", "username": "admin", "password": "..."}` | `{"token": "..."}`: a new API key named `wranglarr-arrs` on that Jellyfin or Emby. The login isn't kept; `400` wrong login or not an administrator; `502` |

A server:

```json
{"kind": "plex", "name": "Plex", "url": "http://192.168.1.10:32400", "token": "...",
 "update_library": true, "map_from": "", "map_to": "", "arrs": ["radarr", "sonarr"]}
```

`kind` is `plex`, `jellyfin`, `emby` or `kodi`. `token` is the Plex token or
the Jellyfin/Emby API key, and is required for those three. Kodi takes
`username` and `password` instead. `map_from` and `map_to` translate the
arrs' paths to the server's, both or neither. As with channels, `token` and
`password` come back as `••••••••`, and sending that keeps the stored value.
The Plex PIN check and the Jellyfin key call answer the new token in clear:
that's what they are for.

`media` is `{"kinds", "servers", "arrs", "mask"}`. `seerr` is `null` without
a Seerr in the stack, else `{"name", "online", "initialized", "url",
"version", "links"}` (`error` when it couldn't be asked). `links` are the
`radarr>seerr` and `sonarr>seerr` links as in
[App connections](#app-connections); connect them with `POST /api/connect`
and those keys. Seerr has no other endpoint here.

### Dashboard: links, logos, feeds, downloads

What the Dashboard shows ([Dashboard](Dashboard.md)).

| Method | Path | Body / query | Answer |
| --- | --- | --- | --- |
| GET | `/api/links` | | `{"settings": …, "links": {"<app>": {"url", "how", "note"}}}` |
| PUT | `/api/links` | link settings (below) | as GET; `400` |
| PUT | `/api/links/configuration` | `{"proxy": {…}, "links": {…}}` | as GET; atomic save of both; `400` |
| DELETE | `/api/links` | | back to the defaults; as GET |
| GET | `/api/apps/{app}/logo` | | the app's own logo (`image/svg+xml` or `image/png`), else the one wranglarr ships; `404` |
| GET | `/api/home/settings` | | what the Dashboard lists show |
| PUT | `/api/home/settings` | any of the settings' fields | the settings; fields left out keep their value; `400` |
| DELETE | `/api/home/settings` | | back to the defaults |
| GET | `/api/home/feeds` | `?refresh=true` skips the 2-minute cache | each app's lists (below) |
| GET | `/api/downloads` | `?refresh=true` skips the 4-second cache | every download client's queue (below) |

Link settings. A PUT replaces them all:

```json
{"mode": "port", "scheme": "http", "host": "192.168.1.10",
 "pattern": "", "apps": {"seerr": "https://requests.example.com"}}
```

`host` is this machine's address for the direct links (empty = the one the
browser used); `apps` sets one app's address outright. `mode` is `port`, or
`domain` for a `pattern` with `{name}` or `{kind}` saved before the Links panel
had the proxy settings (the panel saves `port`). Where each app is on the proxy
is `/api/proxy`'s `apps`. The GET answer also has `panel`: the proxy domain, the
local network, this machine's address, and wranglarr and each app with its
place (`at`, `local`, `sub`), its `port`, its `direct` address and where its card
opens `now`. In the
answer, `how` is `set`, `domain`, `proxy` (its name on the reverse proxy: the
request came in at the Reverse proxy page's domain, AuthTunnel's cookie
domain, or a name under it), `port` or `none` (not published: set an address
for it). Through the proxy, an app configured as Direct IP gets `port` at `host`
(else `HOST_IP`, else the Reverse proxy page's address), with a note saying
`local only`. Configured proxy URLs are used without network probing. Links are built from the host the request was sent to, so
a script gets the addresses for the name it used.

The Links panel uses `PUT /api/links/configuration`: `proxy` has the same patch
semantics as `PUT /api/proxy`, while `links` replaces the link settings. Both
objects are required and validated before either is written. They are committed
in one SQLite transaction, so validation or database failure changes neither.
Use the individual endpoints when changing only one set of settings.

Dashboard settings and their defaults:

```json
{"details": "hover", "queue": true, "upcoming": true, "days": 7, "unmonitored": false,
 "release_next": 10, "release_days": 90, "missing": true, "missing_days": 30,
 "requests": true, "indexers": true, "limit": 25}
```

`details` is `hover` or `always`. `days` 1-90, `missing_days` 0-3650 (0 = all),
`release_next` 1-200, `release_days` 1-365, `limit` (items per list) 3-200.

Feeds: `{"settings", "apps": {"radarr": {"lists": [{"key": "upcoming", "title": "Upcoming", "note", "items":
[{"title", "sub", "when", "tag", "state", "path", "day"}], "total"}]}}}`. The
arrs have `upcoming` and `missing`, Bazarr its wanted subtitles, Prowlarr its
indexers, Seerr its requests. A list or app that couldn't be read has
`error`.

Downloads:

```json
{"at": "2026-10-26T12:00:00+00:00", "speed": 5242880,
 "clients": [{"name": "qbittorrent", "kind": "qbittorrent", "ok": true, "error": "", "count": 3, "speed": 5242880}],
 "items": [{"id": "…", "name": "…", "category": "radarr", "state": "downloading", "state_text": "…",
            "progress": 0.42, "size": 4294967296, "done": 1803886264, "speed": 5242880, "eta": 600,
            "added": "…", "client": "qbittorrent", "client_kind": "qbittorrent",
            "arr": {"name": "radarr", "kind": "radarr"}}]}
```

`state` is `downloading`, `processing`, `checking`, `metadata`, `stalled`,
`error`, `queued` or `paused`. `progress` is 0 to 1; sizes are bytes, speeds
bytes per second, `eta` seconds. `arr` is the arr the category belongs to, or
`null`.

### App logins

| Method | Path | Answer |
| --- | --- | --- |
| GET | `/api/app-logins` | how the apps' logins are set up, read from each app now. Read only: logins are set at init ([App Logins](App-Logins.md)) |

```json
{"mode": "tunnel", "mode_text": "AuthTunnel: wranglarr signs you in to every app", "username": "",
 "required": "", "required_text": "", "cookie_domain": "example.com", "login_url": "https://wranglarr.example.com",
 "wranglarr_auth": true, "last_run": {"mode": "tunnel", "at": "…", "username": "…"}, "rerun": ["…"],
 "apps": [{"app": "radarr", "kind": "radarr", "method": "external", "username": "", "login": false,
           "gate": true, "warn": ""}]}
```

No passwords are returned.

With `WRANGLARR_URL_BASE=/wranglarr`, every API route also accepts the
`/wranglarr` prefix, while bare endpoints stay available for internal callers.
Browser URLs and redirects include the base. Authentication and origin checks
apply identically to both forms.

### Reverse proxy

Config text for a reverse proxy, built from the stack as it is now
([Reverse Proxy](Reverse-Proxy.md)). wranglarr only writes the text; nothing
is changed in your proxy.

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/api/proxy` | | `{"settings", "valid", "text", "filename", "how", "plan", "notes"}` |
| PUT | `/api/proxy` | the settings (below) | as GET; `400` |

```json
{"kind": "caddy", "domain": "example.com", "upstream": "host", "upstream_host": "",
 "lan": "192.168.1.0/24", "network": "proxy", "entrypoint": "websecure", "resolver": "letsencrypt",
 "apps": {"wranglarr": {"at": "subdomain", "local": false, "sub": ""},
          "radarr": {"at": "path", "local": true, "sub": ""},
          "seerr": {"at": "subdomain", "local": false, "sub": "requests"},
          "qbittorrent": {"at": "direct", "local": true, "sub": ""}}}
```

- `kind`: `caddy`, `nginx`, `traefik-file`, `traefik-labels`, `caddy-labels`, `cloudflared`, `npm`.
- `upstream`: `host` (the published ports) or `docker` (the proxy is a container on the stack's network).
- `lan`: the networks a local-only app answers; empty = the private ranges.
- `domain`: empty with AuthTunnel = `AUTH_COOKIE_DOMAIN`.
- `network`, `entrypoint`, `resolver`: Traefik's and caddy-docker-proxy's names.
- `apps`: per app (and `wranglarr`), `at` is `subdomain` (its own name), `path` (under wranglarr's name) or `direct` (not proxied); wranglarr's is `subdomain`, `domain` (the domain itself), `path` (at `/{sub}` on the domain, requiring the same `WRANGLARR_URL_BASE`) or `direct`. `local`: only the `lan` networks get through. `sub`: its name (default: the app's name). Unset, an app is where `style` puts it (`subdomain`, or `path` for a path each), local only. The older `{"expose": "public" | "lan" | "off"}` still reads.

A PUT merges with the saved settings: a field left out keeps its value. The answer's
`plan` says, per app, its address and where the proxy sends it; `notes`
lists what to do by hand (URL bases, DNS, …). Duplicate routes, reserved wranglarr
paths and unsupported placements are `400` errors and are not saved. NPM requires
one access policy per hostname. Cloudflare Tunnel rejects local-only routes and
paths that require stripping. An existing invalid configuration returns
`valid: false`, an empty `text`, and an explanation in `notes`, keeping the editor
accessible without producing an unsafe scaffold.

### Community configs

Custom formats, quality profiles, sizes and more, shared as JSON
([Community configs](Community-Configs.md)).

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/api/configs` | | `{"format": "wranglarr-config/1", "apps", "examples", "history", "prefix"}` |
| GET | `/api/configs/examples/{id}` | | `{"text": "<the example config's JSON>"}`; `404` |
| POST | `/api/configs/check` | a config and choices (below) | a preview of what applying would do; changes nothing; `400` |
| POST | `/api/configs/apply` | the same, plus `"ack": true` when the check's `ack` is true | **changes the apps**: `{"results": [{"app", "kind", "ok", "error", "changed", "history", "installed"}], "history"}`; `400` |
| GET | `/api/configs/installed` | | each app's timeline, after a look for changes made in the app: `{"installed": [{"app", "kind", "error", "marks": [{"id", "at", "kind", "label", "source", "history", "partial", "kept", "count", "items"}]}]}` |
| GET | `/api/configs/installed/{app}/{id}` | | what removing that install would do: `{"mode": "none"\|"confirm"\|"choose", "label", "keep", "throughout"}`; changes nothing; `404` |
| POST | `/api/configs/installed/{app}/{id}/remove` | `{"throughout": false}` | **changes the apps**: removes the install (below): `{"notes", "installed"}`; `400` for a mark that isn't an install; `404` |
| POST | `/api/configs/history/{id}/undo` | none | **changes the apps** back to the copies saved at import: `{"notes", "history"}`; `400` already undone; `404` |
| DELETE | `/api/configs/history/{id}` | | forgets the entry (the apps stay as they are): `{"history"}`; `404` for an unknown id |
| GET | `/api/configs/inventory/{app}` | | what the app has, part by part, for export; `400` for an app that takes no configs |
| POST | `/api/configs/export` | what to export (below) | `{"config": {…}, "notes": […]}`: a config made from your apps; changes nothing |
| POST | `/api/configs/try` | `{"app": "radarr", "title": "Some.Movie.2024.1080p.WEB-DL.x265-GRP"}` | how the app parses that release name: `{"quality", "languages", "formats", "profiles": [{"name", "score", "min", "allowed"}]}` |

Check and apply take:

```json
{"text": "{\"wranglarr\": \"wranglarr-config/1\", \"name\": \"…\", \"apps\": {…}}",
 "targets": {"radarr": ["radarr", "radarr4k"]},
 "extras": {"radarr/customFormats/x265 (HD)": false},
 "resolve": {"radarr|qualityProfiles|HD-1080p": "prefix"},
 "prefix": "v2 · "}
```

- `text`: the config as a string (up to 512 KB), as pasted.
- `targets`: per app kind in the config, the apps it goes into. A kind left out goes into the first app of that kind.
- `extras`: the config's optional parts, `<kind>/<part>/<name>` → on or off; left out = the config's default.
- `resolve`: for something the app already has by that name, `<app>|<part>|<name>` → `change`, `keep` or `prefix` (add the config's as a copy named with `prefix`). Default `change`.
- `prefix`: default `v2 · `.
- `ack` (apply only): OK to change what the plans list under `protected`.

The check answers `{"config", "extras", "plans", "fits", "chosen", "prefix",
"tag_prefix", "ack"}`. Each plan has:

- `kind`, `app`, `app_kind`;
- `rows`, one per item, with its `action` and details;
- counts of `changes`, `errors` and `warnings`;
- `protected`: `[{"part", "label", "name", "items", "why", "overrides"}]`, the
  changes to built-in items, app-wide parts and the default delay profile, or
  to what another installed config set (`overrides` names those configs).

`ack` is true when any plan has some. Apply then refuses that app (`ok: false`)
unless the body has `"ack": true`.

Apply writes a partial history entry before sending mutations and stores each
previous value before its request. An uncertain create with no returned ID remains
in history; Undo reports it for manual verification instead of deleting an item by
name. Unfinished entries are not evicted by the history limit. Removal also keeps
a partial recovery entry until its changes and timeline update are verified;
undoing an interrupted deletion requires manual recovery because an app may assign
a new ID when recreating it. Removal refuses to proceed or finish verification
when app resources are unreadable.

Each apply goes on the app's timeline as a mark, and its id is in `installed`.
An install's mark keeps the config's `source`. A mark's `kind` is one of:

- `before`: what the settings were before wranglarr first changed them;
- `config`: an install;
- `external`: changes wranglarr noticed made in the app, labelled
  `External manual change`.

`items` lists the changes per item: `{"part", "label", "name", "changes": ["min 35 → 40"]}`.

Removal replays the timeline without the install. Its `mode`:

- `none`: nothing in the app changes;
- `confirm`: only the install's own changes go back;
- `choose`: later changes touch the same settings, or use what it made, and the two ways differ.

`keep` and `throughout` each give `effects` (the same shape as `items`), `kept` (what
stays, and why) and `overrides` (later marks whose changes it undoes). With
`throughout: false`, later changes stay, and what they use stays in the install's
mark. With `true`, its settings go back to how they were before it, over later
changes, manual ones included. `throughout` only matters when the mode is `choose`.
Neither removal nor apply checks yet which films, series or artists use what they
change. An app that refuses to delete a profile in use keeps it, and the refusal
comes back in `notes`.

Export takes, per app, the names to export in each part (as the inventory
lists them):

```json
{"name": "My config", "description": "", "author": "",
 "apps": {"radarr": {"customFormats": ["x265 (HD)"], "qualityProfiles": ["HD-1080p"], "withFormats": true,
                     "extras": {"customFormats": {"x265 (HD)": {"default": false, "note": "…"}}}}}}
```

`withFormats` adds the custom formats the picked profiles score. An optional
`source` (a name, a URL, or `{"name", "url"}`) goes into the config as it is
given.

### VPN and health

| Method | Path | Query | Answer |
| --- | --- | --- | --- |
| GET | `/api/vpn` | `?refresh=true` skips the 30-second cache | `{"configured": false}`, or `{"configured": true, "name", "reachable", "status", "public_ip", "forwarded_port", "error", "ok"}` |
| GET | `/api/health` | | `{"ok": true}` without a sign-in; signed in: `{"ok": true, "version", "postgres", "docker", "vpn", "updates_available"}` |

`ok` in the health answer only says wranglarr answers. `postgres` and
`docker` say whether it reaches them; `vpn` is `true` when gluetun's tunnel is
running, `false` when not, `null` without a VPN. A monitoring check of the
stack should look at those fields.

### Sign-in and account

The sign-in calls are public but still need the Origin check. The calls
from `password` to `sessions/end-others` need a **browser session**: with an
API key they answer `403`. Calls that change the account and say "password" in the body
need the current password; a wrong one is `403`.

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| POST | `/api/auth/setup` | `{"code": "AB12-CD34-EF56", "username": "admin", "password": "..."}` | creates the account and signs in: `{"ok": true}` + cookie; `403` wrong setup code; `409` the account exists; `400` username or password rules |
| POST | `/api/auth/login` | `{"username": "admin", "password": "...", "remember": false}` | `{"ok": true}` + cookie, or `{"mfa": true}` + the `wranglarr_mfa` cookie; `401`; `429` |
| POST | `/api/auth/login/totp` | `{"code": "123456"}` (or a recovery code) | `{"ok": true}` + cookie; `401` |
| GET | `/api/auth/passkey/login-options` | | `{"flow", "options"}` for `navigator.credentials.get` |
| POST | `/api/auth/passkey/login` | `{"flow": "…", "credential": {…}, "remember": false}` | `{"ok": true}` + cookie; `401` |
| POST | `/api/auth/logout` | none | ends this session: `{"ok": true}` |
| GET | `/api/auth/me` | | `{"enabled", "username", "created", "totp", "recovery_left", "passkeys", "sessions", "api_keys", "rp_id", "https"}`; works with an API key too |
| POST | `/api/auth/password` | `{"current": "...", "new": "..."}` | `{"ok": true}` |
| POST | `/api/auth/totp/begin` | `{"password": "..."}` | `{"secret", "uri", "qr_svg"}` for the authenticator app |
| POST | `/api/auth/totp/enable` | `{"code": "123456"}` | `{"recovery": ["…", …]}`: the recovery codes, shown once |
| POST | `/api/auth/totp/disable` | `{"password": "..."}` | `{"ok": true}` |
| POST | `/api/auth/recovery` | `{"password": "..."}` | new recovery codes: `{"recovery": [...]}` (needs two-factor on) |
| POST | `/api/auth/passkey/register-options` | `{"password": "..."}` | `{"flow", "options"}` for `navigator.credentials.create` |
| POST | `/api/auth/passkey/register` | `{"flow": "…", "credential": {…}, "name": "Laptop"}` | `{"id", "name", "created", "rp_id"}` |
| PATCH | `/api/auth/passkeys/{id}` | `{"name": "Phone"}` | `{"ok": true}`; `404` |
| DELETE | `/api/auth/passkeys/{id}` | | `{"ok": true}`; `404` |
| POST | `/api/auth/api-keys` | `{"name": "home-assistant", "password": "..."}` | `{"id", "name", "prefix", "created", "last_used", "key"}`: `key` is shown this once |
| DELETE | `/api/auth/api-keys/{id}` | | revokes it: `{"ok": true}`; `404` |
| DELETE | `/api/auth/sessions/{id}` | | signs that session out: `{"ok": true}`; `404` for an unknown id |
| POST | `/api/auth/sessions/end-others` | none | signs out every other session: `{"ended": 3}` |
| GET | `/api/auth/gate` | | AuthTunnel only (below) |

Passwords need 12 characters or more, can't be a common one, and can't
contain the username.
Usernames are 1-64 letters, digits and `. _ - @`.

`GET /api/auth/me` never returns a password hash, a TOTP secret, or an API
key: `api_keys` lists each key's `id`, `name`, `prefix` (its first 7
characters), `created` and `last_used`, and `sessions` each session's `id`,
`created`, `last_seen`, `expires`, `method`, `ip`, `agent` and `current`.

`GET /api/auth/gate` is what wranglarr-gate asks before every request to an
app with `APPS_AUTH=tunnel` ([App Logins](App-Logins.md)). It answers `204`
for a signed-in browser session (or when wranglarr's login is off), and `401`
otherwise, with an
`X-Wranglarr-Login` header that says where to sign in. An API key is not
enough here. Without AuthTunnel it answers `404`.

## Setup wizard API (port 8788)

The first-run wizard (`wranglarr-setup`, [Setup](Setup.md)) is a separate
server on port 8788 with its own small API. It is internal to the wizard
page: it runs only while you set up and writes `.env` in the stack folder.
Opening the printed `/?t=<code>` link sets a cookie; without it every
`/api/setup/*` call answers `401`. Don't leave it running, and don't script against it. For
completeness:

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/` | the wizard page; `?t=<code>` sets the cookie and redirects to `/` |
| GET | `/api/setup/defaults` | what the wizard starts from: the existing `.env` (if any), the services, host user, time zone and mounts |
| POST | `/api/setup/check-path` | `{"path", "key", "uid", "gid", "create"}`: does a folder exist, can the apps' user write it, free space, filesystem notes |
| POST | `/api/setup/generate` | `{"kind": "password" \| "apikey"}` → `{"value"}` |
| POST | `/api/setup/check-vpn` | the VPN answers → `{"errors", "warnings"}` |
| POST | `/api/setup/scan-backups` | `{"appdata": "./appdata"}`: backups found there, to restore |
| POST | `/api/setup/preview` | the answers → `{"env"}`: the `.env` it would write, secrets masked |
| POST | `/api/setup/preview-auth` | `--auth` mode: the app-login keys it would change |
| POST | `/api/setup/write-auth` | `--auth` mode: changes only the app-login keys in `.env` (after a backup copy) |
| POST | `/api/setup/write` | writes `.env`, `wranglarr.toml` and the restore plan; `409` when `.env` exists and `overwrite` isn't set |

Errors come back as `{"error": "…"}`, as in the main API.

## Pages

The HTML pages, for links from elsewhere: `/` (Dashboard), `/suites`,
`/suites/{suite}`, `/suites/{suite}/versions/{version}`, `/activity`,
`/runs/{id}`, `/updates`, `/indexers`, `/notifications`, `/connections`,
`/configs`, `/keys`, `/proxy`, `/system`, `/account`, `/login`, `/setup`. The suite
page's settings form posts to `POST /suites/{suite}/settings` (form fields,
then a redirect); scripts use `PUT /api/suites/{suite}/settings` instead.
