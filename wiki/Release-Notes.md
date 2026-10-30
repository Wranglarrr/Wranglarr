# Release Notes

What each wranglarr release adds, changes and fixes, newest first. Read the
entry before you update. **Before you update** lists anything you have to do
by hand, such as a setting that was renamed.

The image tags are listed in [The wranglarr image](Setup.md#the-wranglarr-image),
and [Keeping wranglarr itself up to date](Updates.md#keeping-wranglarr-itself-up-to-date)
explains how to update. The `edge` image follows the main branch and has no entry
here until its changes are released.

<!--
How to write an entry:
- One `## <version> — <YYYY-MM-DD>` heading per release, newest first. The
  version is the one in wranglarr/Cargo.toml. The image workflow refuses to
  publish a `v<version>` tag until this page has a dated heading for it, and it
  uses that section as the GitHub release's text.
- Until the release, the heading says `— not released yet`.
- Sections, in this order and only when they have something in them:
  `### Before you update`, `### New`, `### Changed`, `### Fixed`.
- Write for someone running the stack: what they'll notice and what they have to
  do, not how the code changed.
-->

## 1.1.1 — not released yet

### Before you update

- **An arr still on SQLite isn't updated** unless its config folder is
  mounted into wranglarr read-write. The stock `compose.yaml` mounts the
  arrs' folders read-only (`/arrconfig/radarr:ro`), which is enough for the
  arrs on Postgres. For one on SQLite, drop the `:ro` from its line and
  recreate wranglarr, or move it to Postgres
  ([Importing existing apps](Importing-Existing-Apps.md)). Otherwise a failed
  update couldn't put its database back, so the update is skipped and says
  why.

### Changed

- **A smaller image.** wranglarr is now built for size: its program is about a
  quarter smaller, so the image downloads faster. Backups, logins and
  compression run as fast as before.
- **Pre-update backups are kept apart from scheduled ones,** so a week of
  nightly backups never pushes out the backup a rollback needs. The newest
  one stays. During a run of quick updates (catching up through several
  versions), the backup from before the first stays until the last version has
  run for a week. `pre_update_keep` and `pre_update_settle_days` under
  `[updates]` change both. The pre-update backups you already have, made by
  1.1.0 or 1.0.0, aren't touched by the new rule: they stay counted with the
  scheduled ones, under the suite's `keep_last` and `keep_days`, until they
  age out as they would have before.
- **Roll back can put the data back too.** Its dialog offers to restore the
  app's data from the backup taken just before that update, ticked by default
  and showing that backup's date. A newer version may have migrated the
  database, which the old version can't read.

- **A failed update leaves a report to file it with.** The new container's
  last log lines and exit state are kept before it's removed, with keys,
  passwords, public addresses and domains taken out as far as wranglarr can
  tell, so read it before you send it. It says how the app was left: back on
  the old version, back but unhealthy, or stopped for you to recover. The run
  log, the notification and the Updates page link to the report's page. Its
  button copies the report and looks the failure up by its fingerprint: the
  issue it was already reported in opens, or a new one with the report filled
  in.
- **An update is skipped when a failure couldn't be undone.** Before it
  swaps a container, wranglarr checks that the backup it just made could put
  the data back. An arr on SQLite needs its config folder mounted into
  wranglarr read-write for that. Use `force` to update anyway.
- **Links out of wranglarr no longer send its address** to the site they go
  to, so a link to GitHub or release notes doesn't name your domain.

### Fixed

- **A failed update could leave the old version on migrated data.** When a new
  version started, changed the app's database or config files, and then
  failed its health check, the old version was put back on that changed data
  and could refuse to start. The data now goes back from the pre-update
  backup before the old version starts. For an arr on SQLite that's its
  database file, put back with the app stopped, since an old version that
  can't read the newer database never comes up to take its backup zip.
  When the data can't go back (the backup can't be read, or a restore fails
  halfway), nothing is started on it: the container stays stopped as
  `<name>-wranglarr-old`, and the Updates page says what had been put back and
  what to do. A rollback that fails while putting data back does the same.
- **Two containers on one tag:** after the first failed to update, the second
  could be "updated" onto the old image. A failed image is now skipped for the
  rest of the run.

## 1.1.0 — 2026-10-28

Fixes from the first days of 1.0.0, mostly around reverse proxies and the VPN,
plus one place to set where each app opens.

### Before you update

- **Using "a path each" on the Reverse proxy page?** The paths now go under
  wranglarr's name, so they share its certificate and sign-in cookie: with
  wranglarr at `wranglarr.example.com`, Radarr moves from
  `https://example.com/radarr` to `https://wranglarr.example.com/radarr`. To
  keep `example.com/radarr`, put wranglarr at **The domain itself** or **Path on
  the domain** under Dashboard → Links (for the latter, set the matching
  `WRANGLARR_URL_BASE`). Either way, copy the new config into your proxy after
  updating. Each app's URL Base (`/radarr`) stays the same.
- **Optional:** add `HOST_IP=<this machine's IP>` to `.env`, or run
  `./scripts/init.sh` to fill it in, and recreate wranglarr. The links and the
  proxy config use it for the apps that aren't proxied. Without it, wranglarr
  uses the address under **This machine** on the Links panel, or the Reverse
  proxy page's.

### New

- **Dashboard → Links sets where each app opens**, for the cards and the
  reverse proxy config alike. wranglarr itself is at a name under your domain
  (`wranglarr.example.com`), at the domain itself (`example.com`), at a path
  on the domain (`example.com/wranglarr`), or not proxied. Per app:
  **Subdomain** (`radarr.example.com`), **wranglarr path**
  (under wranglarr's name: `wranglarr.example.com/radarr`, or
  `example.com/radarr` with wranglarr at the domain itself), **Direct IP** (this
  machine's address and the app's port) or an address of your own, and
  **local only** to keep a proxied app to your own network. Each app's published
  port is shown beside it.
- With AuthTunnel on, the Links panel shows which apps sign in through
  wranglarr and which keep their own login, and warns when the proxy domain,
  `AUTH_COOKIE_DOMAIN` and `AUTH_LOGIN_URL` don't line up.
- **`HOST_IP`**: the setup wizard and `./scripts/init.sh` find this machine's IP
  and write it to `.env`. Change it only if you take that `.env` to another
  machine.
- **wranglarr at a path on your domain**, e.g. `https://myhouse.com/wranglarr`:
  set `WRANGLARR_URL_BASE=/wranglarr` and choose **Path on the domain** under
  Dashboard → Links. The apps on a path sit beside it
  (`https://myhouse.com/radarr`) and share its certificate and sign-in. With
  AuthTunnel, `AUTH_LOGIN_URL` includes the path. Passkeys keep working, and
  installs that don't set it need no change.
  [How to set it up](Reverse-Proxy.md#wranglarr-at-a-path-on-the-domain).
- **`proxy-setup`** prints the reverse proxy config from a shell, straight into
  a file: `docker compose exec -T wranglarr proxy-setup > Caddyfile.wranglarr`
  (`--kind nginx` for another proxy). See
  [Reverse proxy](Reverse-Proxy.md#from-a-shell).

### Changed

- The Reverse proxy page keeps the proxy's own settings (which proxy, how it
  reaches the apps). The domain, the local network and each app's place moved
  to Dashboard → Links, and the page lists what it sends where. The "Open apps
  at" choice is gone: each app's row says where it opens. Settings saved by
  1.0.0 carry over: a Public app stays proxied, a LAN only app is proxied and
  **local only**, and a Not proxied app is **Direct IP**.
- **Update checks ask the registries more gently.** A "too many requests"
  (`429 toomanyrequests` from GHCR, which serves the `lscr.io` images) or a
  dropped DNS lookup is retried three times before the container shows
  **check failed**. A registry's answer is reused for 10 minutes across the
  scheduled jobs and the check after each update, and a schedule set to a fixed
  minute (`0 */6 * * *`) starts a few seconds to a minute and a half after it.
  **Check now** still asks afresh.
- The wranglarr image labels point at the source repository and the docs, so
  update checkers can find the release notes. A locally built image has them
  too.
- **VPN port forwarding is wired up.** With `VPN_PORT_FORWARDING=on`, gluetun
  now pushes the forwarded port into qBittorrent and binds it to the tunnel
  (tick "Bypass authentication for clients on localhost" in qBittorrent).
  `VPN_PORT_FORWARD_ONLY=on` picks only servers that can forward a port.
  wranglarr also checks every minute that qBittorrent still listens on the
  forwarded port, and sets it again after qBittorrent restarts on its own (its
  image resets the port to 6881, and gluetun only pushes it when it changes).
- **Importing existing apps** explains copying the arrs' `MediaCover` folder,
  so a library brought over doesn't show grey posters for weeks.
- Built on Rust 1.99.1 and Alpine 3.24.

### Fixed

- **qBittorrent 5.3 kept asking for a login** after setup, when its config file
  still had 4.x settings in it. The login is now written where 5.x reads it.
- **Opened through the proxy, posters and cards linked to wranglarr's name and
  the app's port** (`wranglarr.example.com:7878`). They now open each app at its
  name on the proxy, or at this machine's address when the app isn't proxied.
- **Download-client cards linked to proxy names that didn't exist.** The cards
  now open each app where it's set under Links, whatever DNS says at the time:
  choose **Direct IP** for an app you haven't added to your proxy.
- **A Links save that failed could leave half of it saved.** The addresses and
  each app's place are now checked and saved together. Two apps on one route, a
  path wranglarr uses itself, and settings your proxy can't do are refused
  before a config is written.
- In the proxy configs, IPv6 addresses are bracketed, and a path like `/radarr`
  no longer also catches `/radarr4k`.
- **AuthTunnel: apps opened by IP couldn't sign in** once `AUTH_LOGIN_URL` was
  set: the sign-in happened on the domain, and its cookie never reached the IP.
  Opened by IP, an app now signs in at wranglarr on that same address.
  `AUTH_LOGIN_URL` is only used under `AUTH_COOKIE_DOMAIN`.
- With AuthTunnel and no domain on the Reverse proxy page, the links follow
  `AUTH_COOKIE_DOMAIN`.
- **The VPN compose files failed on some Compose versions** (2.35, for one),
  which drop `!reset` when it comes through a YAML merge. Each service now
  spells it out.
- **VPN: "has Redirect on" warnings you couldn't clear.** With Prowlarr in the
  tunnel and the arrs outside it, wranglarr asked you to turn Redirect off for
  Usenet indexers, which Prowlarr refuses ("Redirect must be enabled for Usenet
  indexers"), and for torrent indexers where the box is greyed out. It now warns
  only for torrent indexers where Redirect is on and can be turned off.

## 1.0.0 — 2026-10-27

The first public release.

### New

- **Scheduled, versioned backups.** Each app's databases (`pg_dump`), its own
  config backup and a copy of `/config` are grouped into suites. Each suite has a
  cron schedule, keep-last and keep-days retention, a size history, and shows
  what changed since the last version.
- **Restore per app, or without wranglarr.** Restore an app's databases, its
  config or both. Checksums are checked first. Every version carries a pinned
  `compose.yaml`, so it also runs on its own with `docker compose up`.
- **Container updates with a safety net.** wranglarr backs up first, recreates
  the container with the same settings, waits for it to be healthy, and puts the
  old one back if it isn't. It also does rollback, holds and nightly
  auto-update, and won't take a Postgres major upgrade unasked.
- **wranglarr updates itself.** Press **Update now** on the dashboard or the
  Updates page, or turn on its own schedule. A helper swaps the container,
  and if the new version doesn't come up it puts the old one back, along with
  wranglarr's state.
- **A setup wizard.** It writes `.env` and `wranglarr.toml`, picks the services,
  and brings in apps you already run.
- **Wired on first start.** Download clients are added to every arr, and the arrs
  to Prowlarr, Bazarr and Seerr. Each link is tested before it's saved.
- **Live status.** The arrs and Prowlarr tell wranglarr when something changes
  (a health warning cleared, an indexer fixed, an import), and the open pages
  update straight away. **API keys** lists every app's key with copy buttons, and
  a key changed in an app reconnects the apps that keep a copy of it.
- **Community configs.** Apply shared quality and naming settings to your arrs,
  and undo them later.
- **Notifications**: problems, finished runs and available updates, sent to the
  channels you choose.
- **Media servers and Seerr**, **indexers in one place**, and **reverse-proxy
  config** written for Caddy, nginx and Traefik.
- **A login built in**, with two-factor, passkeys and API keys.
- Images for amd64 and arm64, running as an unprivileged user, on Docker
  Compose, rootless Podman and TrueNAS SCALE.
  It takes `PUID`, `PGID`, `TZ`, `UMASK` and `FILE__` secrets like the
  LinuxServer.io images do: started as root, it hands its folders to
  `PUID:PGID` and runs as that user.
