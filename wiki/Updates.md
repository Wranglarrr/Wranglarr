# Container Updates

wranglarr keeps the stack's images current: it checks the registries, backs up
what an update touches, swaps the container, and puts the old one back if the
new one doesn't come up. **Updates** in the sidebar lists every managed
container. The sidebar badge counts the containers with a newer image.

Requires the Docker socket (`/var/run/docker.sock` mounted, `[docker] socket`
set). Both are in the default `compose.yaml`.

## What's managed

| Container | Role | Included when |
| --- | --- | --- |
| every app with `container = "…"` in `wranglarr.toml` | `app` | always |
| `arrs-postgres` (`[postgres] container`) | `postgres` | any app has `databases`, and `include_postgres = true` |
| `arrs-vpn` (`[vpn] container`) | `vpn` | the VPN is enabled, and `include_vpn = true` |
| names in `[updates] extra` | `extra` | listed |

`[updates] exclude` removes names from all of these. wranglarr's own
container has a row too, but *Update all* and auto-update leave it alone: it
is updated only when you ask, or on its own opt-in schedule
([Keeping wranglarr itself up to date](#keeping-wranglarr-itself-up-to-date)).

## Update tracks

Each container's **Update track** selects the image tag to check and update.
Leave it blank to follow the deployed image's tag or digest. Suggestions are
specific to the **image publisher**, so LinuxServer and Hotio containers get
their own choices. Any valid custom Docker tag also works, including for extra
containers and private repositories. Track selection keeps the existing image
repository; it does not switch publishers or change the app's internal branch setting.

**Save track** stores a preference without pulling, stopping or restarting
anything. Press **Check now**, then **Update** to switch. Scheduled updates also
use the selected track, subject to holds, blocked images and backup checks.
Changing track invalidates the previous check. Selecting a tag on a digest-pinned
image explicitly switches from the pin to that tag; leaving it blank preserves the pin.

| Image publisher / app | Suggested tracks |
| --- | --- |
| wranglarr | Stable (`latest`), Development from main (`edge`) |
| LinuxServer Radarr, Lidarr, Prowlarr | Stable (`latest`), Development (`develop`), Nightly (`nightly`) |
| LinuxServer Sonarr | Stable (`latest`), Development (`develop`) |
| LinuxServer Bazarr | Stable (`latest`), Prerelease (`development`) |
| LinuxServer SABnzbd | Stable (`latest`), Prerelease (`unstable`), Nightly (`nightly`) |
| LinuxServer NZBGet | Stable (`latest`), Testing (`testing`) |
| LinuxServer qBittorrent | Stable with libtorrent v2 (`latest`), stable with libtorrent v1 (`libtorrentv1`) |
| Official Seerr | Stable (`latest`), Development (`develop`) |
| Hotio Radarr, Lidarr, Prowlarr, SABnzbd | Stable (`release`), Testing (`testing`), Nightly (`nightly`) |
| Hotio Sonarr, Bazarr | Stable (`release`), Nightly (`nightly`) |
| Hotio qBittorrent, NZBGet | Stable (`release`) |
| Gluetun | Development (`latest`); use a custom `vX.Y.Z` tag for a specific release |
| Postgres | Keep the deployed major tag, or enter a custom tag; major upgrades remain refused |

The presets were checked on 2026-10-27 against the publishers' documentation:
[LinuxServer](https://docs.linuxserver.io/images/),
[Hotio](https://hotio.dev/containers/),
[Seerr](https://docs.seerr.dev/getting-started/docker/), and
[Gluetun](https://github.com/qdm12/gluetun-wiki/blob/main/setup/docker-image-tags.md).
The page links each recognized image to its publisher's track descriptions.
Unknown images receive no guessed development channels. In particular,
Gluetun's `latest` is development, and the listed qBittorrent images do not
advertise a separate development channel.

Development tracks can migrate databases. Returning to a stable track may
require restoring the pre-update backup as well as changing the image.

### Keep Compose on the same track

UI preferences live in wranglarr's state database. They do **not** edit your
Compose files or `.env`. Keep the Compose image on the selected tag so that a
later `docker compose up` does not change the track back. For example, for
LinuxServer Radarr, set `RADARR_IMAGE=lscr.io/linuxserver/radarr:develop` in `.env`.
With the per-app VPN override, keep the Hotio repository instead.

For wranglarr's development track, select `edge` on its own row. The next check
compares against `edge`, and **Update now** (or its own schedule, see
[Keeping wranglarr itself up to date](#keeping-wranglarr-itself-up-to-date))
switches to it. Also set the track in `.env`, so a later `docker compose up`
stays on it:

```dotenv
WRANGLARR_IMAGE=ghcr.io/wranglarrr/wranglarr:edge
```

Main builds publish `edge`; release tags publish `latest`. The setup wizard
launched from an `edge` image keeps that image track.

### File defaults and UI overrides

```toml
[updates.tracks]
wranglarr = "edge"
radarr = "develop"
arrs-postgres = "17"
```

Keys are **container names**. A UI selection wins over the file. Clearing the
input explicitly follows the deployed image; **Reset** removes that UI override
and returns to the file default. Reset does not undo an already applied image
change. Track preferences are separate from the Automation settings and their reset.

## Statuses

| Status | Meaning |
| --- | --- |
| up to date | the registry's digest for the tag matches the running image |
| update available | the tag has moved on the registry |
| update blocked | this exact new image was tried and rejected (failed to start, or a refused Postgres major upgrade). It's skipped automatically until the registry publishes a newer one; **Try again** forces it |
| held | you (or a rollback) pinned it: skipped by *Update all* and auto-update, but a per-container **Update** still works |
| pinned | the compose file references a digest (`image@sha256:…`), so there's nothing to follow |
| local build | not from a registry |
| not running | no container by that name (profile off, or a different `container_name`) |
| check failed | the registry lookup failed (rate limit, private registry without credentials, network) |

## How a check works

For each container, wranglarr asks the Docker daemon for the registry's current
digest of the image *tag* the container was created from
(`GET /distribution/{ref}/json`). That is a manifest lookup, so nothing is
pulled. If the digest isn't among the running image's `RepoDigests`, an update
is available. The default schedule is every 6 hours. **Check now** runs a
check on demand.

Registries throttle bursts (GHCR, behind `lscr.io`, answers `429
toomanyrequests` for a moment), and a busy or filtering DNS resolver (AdGuard,
Pi-hole) can drop a lookup. So:

- a "too many requests" or a network blip is asked again three times (after
  2, 10 and 30 seconds) before the container shows **check failed**;
- a registry's answer is kept for 10 minutes, so the check, automatic update
  and self-update jobs, and the check after each update, don't ask about the
  same tag again (**Check now** always asks afresh);
- a schedule on a set minute (`0 */6 * * *`, `0 5 * * *`) starts 5 to 90
  seconds after it, rather than in the same second as everyone else's checker.

A **check failed** that persists for every container from one registry while
other lookups work usually means something on the host is blocking or
rate-limiting DNS for it.

## What an update does

1. **Pull** every selected image first, so a registry problem can't leave the
   stack half-updated. If the pulled image is the one already running, nothing
   happens.
2. **Safety checks.** A Postgres image whose `PG_MAJOR` differs (for example
   17 → 18) is refused, because a major upgrade needs dump/restore, not a
   container swap ([Service: Postgres](Service-Postgres.md)). The digest is
   then blocked.
3. **Back up first** (on by default). Each suite with an app being replaced is
   backed up, with trigger `pre-update`. A Postgres update backs up every
   suite that has databases. An app whose part of that backup failed is not
   updated unless the request sets `force`. Pre-update versions are kept apart
   from the suite's own retention ([Pre-update versions](Backups-and-Restore.md#pre-update-versions)).

   wranglarr then checks that this backup could put the app's data back with
   the app stopped (step 6). If it couldn't, a failed update couldn't be
   undone, so the app is skipped unless the request sets `force`. That's the
   case when the backup can't be read, or for an arr on SQLite whose config
   folder isn't mounted into wranglarr, or is mounted read-only. The stock
   `compose.yaml` mounts the arrs' folders read-only, which is all the arrs on
   Postgres need; for one on SQLite, drop the `:ro` from its line.
4. **Recreate.** wranglarr inspects the old container and creates a new one
   with the same settings:
   - Kept: mounts, environment, labels (including the compose labels, so
     `docker compose` still owns it), restart policy, capabilities, devices,
     published ports, networks and aliases.
   - Reattached: anonymous volumes.
   - Dropped: values the container only had because the *old image* defined
     them (env, labels, cmd, entrypoint, exposed ports), so the new image's
     defaults apply, as with `docker compose up`.

   The old container is stopped and parked as `<name>-wranglarr-old` until the
   new one is up.
5. **Wait for healthy**, up to `health_timeout` seconds (default 300):
   - A container with a Docker healthcheck must report `healthy`.
   - A container without one must stay running.
   - Then the app's own API must answer. For Postgres, it must accept
     connections.
6. **On failure**, the new container is removed. If wranglarr had asked
   Docker to start it, the new version may already have migrated the app's
   database or config files, which the old version can't read. That counts
   even when Docker's answer never came back. So the app's data goes back
   from the pre-update backup first, with the app stopped:
   - Postgres databases, through `pg_restore`;
   - for an arr on SQLite, its database file (`radarr.db` and the like) from
     the backup zip. Its `-wal`, `-shm` and `-journal` files go, since they
     belong to the newer database, and the replaced files are kept in
     wranglarr's state folder in case you need them. The rest of the config
     folder (posters, logs, the app's own backups) stays as it is;
   - for the download clients and the other apps without a backup API, their
     config folder.

   Then the old container is renamed back and started, and wranglarr waits
   for it to come up healthy. An arr on Postgres gets its own backup zip back
   through its API once it's up. The tag is pointed back at the old image so a
   later `docker compose up` doesn't switch to the rejected one, and the new
   digest is blocked. Other containers on the same tag aren't tried on that
   image in the same run.

   **When the data can't go back, nothing is started on it.** If the backup
   can't be read any more, or a restore fails halfway, the old container
   stays stopped and parked as `<name>-wranglarr-old`. Starting either
   version could make things worse: the old one on data the new one migrated,
   or either on half-restored data. The run log says what had been put back
   before it stopped, and the container's row on the Updates page says what
   to do. If Docker won't remove the new container, wranglarr stops it, and
   the row says whether it might still be running. Restore the backup by hand
   ([Backups and Restore](Backups-and-Restore.md)), then rename the parked
   container back and start it.

   Recovery distinguishes the container coming back from its data being restored.
   If the API disappears before config upload, the report says the upload was not
   attempted. If an upload's response is lost, its outcome stays unknown even if
   the app answers again; inspect its configuration before retrying. A refused
   upload from an app that still answers is reported separately from an app that
   fails to return after restarting.

   A failed update also leaves a **failure report**: see below.
7. **Shared network.** Apps on `network_mode: service:vpn` live in the VPN
   container's network namespace, which is tied to its container id. When
   `arrs-vpn` is replaced, every container in its namespace is recreated onto
   the new one automatically (history action `recreate`). Without this they'd
   lose the network, which is the "VPN on, apps have no network" entry in
   [Troubleshooting](Troubleshooting.md). The new VPN stays either way. If an
   app can't be recreated onto it, that app is marked **failed**, the run ends
   **partial**, and the app has no network until you run
   `docker compose up -d <app>` in the stack folder.

   A namespace-only recreation preserves each dependent's exact running image,
   even if its tag was pulled elsewhere or it follows a different track. A
   dependent selected for an image update goes through its own backed-up update
   step after following the VPN; held or unselected dependents are not upgraded.

Order within one run: VPN, then Postgres, then extras, then apps. If the VPN or
Postgres fails, the run stops there.

## Failure reports

When an update fails, the new container's last log lines and exit state are
kept before it's removed. wranglarr turns them into a report you can file
the problem with: the app, the image, the versions, the error, the exit code,
whether it ran out of memory, its health check output, what happened
afterwards, and the log lines that matter (the errors and their stack traces,
or the last lines when nothing looks like an error). "Afterwards" says how
Docker shows the container once wranglarr is done: the old version back and
healthy, back but unhealthy or not started, or left stopped for you to
recover. When the new container couldn't be removed, it says so, and whether
it may still be running, which is the first thing to sort out. It also says
whether the data went back, and whether the new version is now skipped.

Before anything is kept, wranglarr takes these out of every part of it,
including the image name and versions:

- every API key, password and username wranglarr knows, and the Postgres and
  VPN keys;
- anything shaped like a secret: `apikey=`, `token=`, `password=`, `passkey=`
  and the like (quoted values whole), `Authorization` and other login headers,
  cookies, logins in URLs, JWTs, and long runs of key characters (the arrs'
  32-character keys, tracker passkeys);
- email addresses, public IP addresses (your LAN's stay), your domain (from
  the reverse proxy settings and the apps' URLs) with every name under it,
  other domain names in any case, a private registry's name, and the user
  name in home folders.

This is a best effort, not a promise. A secret in a shape wranglarr doesn't
know can get through, and log lines can still name films, series or files in
your library. So the button below asks you to read the report first.

Each report has a **fingerprint**, `wf-` and ten characters. It comes from
the app, the image, the new version, the error and the first error line, with
times, numbers and ids left out, so the same failure gets the same
fingerprint on every machine. The issue title carries it, so one search finds
an earlier report of the same thing. Each failure keeps its own report, so
two containers failing the same way in one run each have theirs, and a report
says how often its fingerprint has been seen here.

The run log and the `update_failed` notification point to the report's page,
`/updates/report/<run>-<container>`, also linked from the container's row on
the Updates page. Its **Check whether it's been reported** button asks you to
confirm you've read the report, copies it to your clipboard and asks GitHub
for issues in the app's repo, open or closed, with that fingerprint:

- **Reported already:** the oldest such issue opens. Paste the report in a
  comment there, or add a thumbs-up.
- **Not yet:** a new issue opens with the title, fingerprint, app, versions
  and the report filled in. A report too long for a link is pasted by hand.

wranglarr asks GitHub itself, so the search carries the fingerprint and
nothing about your setup. A browser asking would send GitHub your wranglarr's
address. For the same reason, links from wranglarr to other sites don't send
them the page they came from (`Referrer-Policy: same-origin`). The page also
has the plain search and new-issue links, and the run log has the search
link, for doing it by hand.

Problems with wranglarr's apps, Postgres and the VPN go to wranglarr's repo.
An extra container's go to the repo its image names in its
`org.opencontainers.image.source` label, with a note that it isn't
wranglarr's; one whose image names none gets a report and no links. The last
25 reports are kept; **Forget it** on a report's page removes one.

## Rollback

**Roll back** is offered while the host still has the image from before the
last successful update. It tags that image back onto the container's tag
(`lscr.io/linuxserver/radarr:latest` → the previous image), recreates the
container on it, and **holds** the container, so the next auto-update doesn't
undo the rollback. Release the hold when a fixed image is out.

An app may migrate its database on the new version, and the old one can then
refuse to start. So the Roll back dialog offers to **restore its data** from
the backup taken just before that update, ticked by default and showing that
backup's date. Everything the app changed since then is lost: new imports,
added items, settings. The data goes back while the container is stopped,
before the old version starts, and an arr's own backup zip after it's up.

For an arr on SQLite, its database file goes back with the app stopped, as
after a failed update (step 6 above). That needs its config folder mounted
into wranglarr read-write; without it, a rollback with the data is refused
and says so. Roll back without the data, or mount the folder.

If the rollback fails once it has begun putting data back, neither version is
started on that data: the container stays stopped and parked as
`<name>-wranglarr-old`, and the run log says what had been put back. Recover
it as below.

When that backup isn't kept any more, the dialog says so and the rollback only
swaps the program. Restore another version by hand if the app doesn't start
([Backups and Restore](Backups-and-Restore.md)).

`docker image prune` removes untagged old images, and with them the ability
to roll back.

After a track change, the container's tag is the new track's: rolling back from
`develop` tags the stable image as `radarr:develop` on this host, so `compose up`
keeps the rolled-back program. The next pull of `develop` replaces that tag.

### An interrupted update

While it swaps containers, wranglarr parks the old one as
`<name>-wranglarr-old`. If wranglarr is stopped in the middle (host restart,
`docker kill`), the run is marked failed when it starts again, and the parked
container stays. The same goes for an update or rollback that stopped because
the data couldn't go back. The Updates page says so on the container's row, and an
update or rollback that would swap it again is refused until it's sorted out by
hand: if the container works, remove `<name>-wranglarr-old`; if not, remove the
container, put its data back if the row says to, and rename `<name>-wranglarr-old`
back to its name, then start it.

## Keeping wranglarr itself up to date

wranglarr can't swap its own container from inside it, since stopping that
container stops wranglarr. It hands the job to a short-lived helper instead.

**Update now.** When a check finds a newer wranglarr image, the dashboard
shows a banner with the release notes link and an **Update now** button.
wranglarr's row on the Updates page gets the same button. You have to be
signed in on wranglarr's pages to press it: an API key can't start it. The
`self_update_available` notification says the same and gives the shell
command as a fallback.

**On a schedule.** In **Automation**, tick **Update wranglarr** and set its
schedule (`self_update` and `self_schedule` in `[updates]`, default off and
`30 5 * * *`). The schedule runs a check first and updates only when there's
a newer image that wasn't rejected before. Neither *Update all* nor the
stack's auto-update ever includes wranglarr.

**What happens** (one run under Activity, suite `system`, kind `self-update`):

1. wranglarr pulls its image. If nothing new came down, the run ends there.
2. It copies its state database (`VACUUM INTO`) and `wranglarr.toml` into
   `<state folder>/self-update/<time>/`. The newest three copies are kept.
3. It starts a helper container, `<name>-wranglarr-updater`, from the **new**
   image. The helper gets the Docker socket and wranglarr's state folder,
   wranglarr's user, groups and security options, and nothing else: no
   network, no capabilities, a read-only root. It is removed when it exits.
4. The helper stops wranglarr and parks it as `<name>-wranglarr-old`, the
   same swap any other container gets (step 4 of
   [What an update does](#what-an-update-does)). It runs `wranglarr-init` (the
   one-shot that prepares the folders) on the new image, then creates the new
   wranglarr with the old one's settings.
5. The new wranglarr counts as up when `wranglarr health` answers inside it
   within `health_timeout`. The helper then removes the parked containers.
6. The new wranglarr reads the helper's report from the state folder. It
   copies the helper's log into the run, records the history row, sends the
   usual update notification and finishes the run. The run's page picks up by
   itself once wranglarr is back.

**If the new one doesn't come up**, the helper removes it, puts the state
database back from the copy (the new version may have changed it), restores
the old `wranglarr-init`, renames the old wranglarr back and starts it. It
points the tag back at the old image and blocks the new digest, so the
schedule skips that image. **Try again** on the row forces it. The old
wranglarr then finishes the run as **failed**.

When wranglarr is running, a helper that dies without a final report fails the
run, as does a report past its deadline. wranglarr ignores and removes a report
older than a day, or one it can't read. If no wranglarr is running, inspect the
helper's report and parked containers before attempting recovery.

**When it isn't offered.** The row then says why, and the banner shows the
shell command instead:

- Docker control is off, or wranglarr isn't running in a container it can
  find (by `HOSTNAME`).
- The image is one you built (`wranglarr:local`) or is pinned to a digest
  (`image@sha256:…`). There's no newer image to pull for those.
- The Docker socket or the state folder isn't a mount of wranglarr's container.

Then update it from the stack folder:

```sh
docker compose pull wranglarr && docker compose up -d wranglarr
```

or, for an image you built, build it again
([The wranglarr image](Setup.md#the-wranglarr-image)).

On rootless Podman this works the same through Podman's socket. The helper
finds `wranglarr-init` from the labels `docker compose` puts on wranglarr's
container. `podman-compose` doesn't add those labels, so there the helper
skips that step. The folders `wranglarr-init` set up stay as they were. That
matters only when a release changes what it sets up, and its release notes then
say so under **Before you update**.

If the new wranglarr fails and its saved state cannot be restored, the helper
leaves the old container stopped under `<name>-wranglarr-old`. It retains the
snapshot and `self-update.json` report in wranglarr's state folder. Inspect the
report and restore a verified state snapshot before renaming and starting the
old container; starting it against state changed by the failed candidate may
be unsafe.

## Automation

| Setting | Default | |
| --- | --- | --- |
| Enabled | on | off = no scheduled checks or updates |
| Check schedule | `0 */6 * * *` | cron, container `TZ`; blank = manual only |
| Auto-update | off | apply every *update available* (not held, not blocked) on the schedule below |
| Auto-update schedule | `0 5 * * *` | runs a fresh check first |
| Back up first | on | |
| Update wranglarr | off | update wranglarr itself on the schedule below ([above](#keeping-wranglarr-itself-up-to-date)) |
| wranglarr's schedule | `30 5 * * *` | runs a fresh check first; skips an image that was rejected before |

Values come from `[updates]` in `wranglarr.toml`. Changes in the UI are stored
in the state db and win over the file until **reset to config file**.

Don't run Watchtower or another auto-updater against the same containers: it
would replace them without the backup and health checks, and break the VPN
namespace link described above. That includes wranglarr's own container: use
**Update wranglarr** instead.

## `[updates]` reference

```toml
[updates]
enabled = true
check_schedule = "0 */6 * * *"
auto_update = false
auto_schedule = "0 5 * * *"
backup_first = true
pre_update_keep = 1         # pre-update backups kept per suite, apart from its retention
pre_update_settle_days = 7  # updates closer than this are a run, which keeps its first backup too
self_update = false         # update wranglarr itself on self_schedule
self_schedule = "30 5 * * *"
health_timeout = 300        # seconds
include_postgres = true
include_vpn = true
extra = []                  # more container names to manage, e.g. ["prowlarr", "overseerr"]
exclude = []                # never touch these
```

`[postgres] container` (default `arrs-postgres`) and `[vpn] container`
(default `arrs-vpn`) name the infrastructure containers.

## API

```
GET    /api/updates                    containers + status, settings, next runs, history
POST   /api/updates/check              check now (synchronous) -> same shape
POST   /api/updates/apply              {"containers": ["radarr"], "force": false} | {} = all available -> {"run": id}
POST   /api/updates/rollback           {"container": "radarr", "restore_data": true} -> {"run": id}
POST   /api/updates/hold               {"container": "radarr", "hold": true}
POST   /api/updates/self               update wranglarr itself (a signed-in browser only) -> {"run": id}
PUT    /api/updates/settings           {"enabled":true,"check_schedule":"…","auto_update":false,"auto_schedule":"…","backup_first":true,"self_update":false,"self_schedule":"…"}
DELETE /api/updates/settings           back to the config file
PUT    /api/updates/tracks/{container} {"tag":"develop"} | {"tag":null} = follow deployed image
DELETE /api/updates/tracks/{container} back to that container's file default
GET    /api/updates/reports            the kept failure reports, newest first
GET    /api/updates/reports/{key}      one, with its Markdown, title and links
GET    /api/updates/reports/{key}/reported  the issue it was already reported in, or {"issue": null}
DELETE /api/updates/reports/{key}      forget it
```

Update and rollback runs appear under Activity with suite `system`.
