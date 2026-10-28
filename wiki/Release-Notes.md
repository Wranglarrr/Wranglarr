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

## 1.1.0 — not released yet

Fixes from the first days of 1.0.0, mostly around opening the apps through a
reverse proxy, plus one place to set where each app opens.

### Before you update

- **Using "a path each" on the Reverse proxy page?** The paths now go under
  wranglarr's name, so they share its certificate and sign-in cookie: with
  wranglarr at `wranglarr.example.com`, Radarr moves from
  `https://example.com/radarr` to `https://wranglarr.example.com/radarr`. To
  keep `example.com/radarr`, put wranglarr at **The domain itself** or **Path on
  the domain** under Dashboard → Links (for the latter, set the matching
  `WRANGLARR_URL_BASE`). Either way,
  copy the new config into your proxy after updating. Each app's URL Base
  (`/radarr`) stays the same.
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
- **Wranglarr at a path on your domain.** Set `WRANGLARR_URL_BASE=/wranglarr`
  and choose **Path on the domain** in Dashboard → Links to serve wranglarr at
  `https://myhouse.com/wranglarr`. The proxy passes the prefix through unchanged.
  App paths are siblings (`https://myhouse.com/radarr`), sharing the certificate
  and sign-in cookie. With AuthTunnel, include the base in `AUTH_LOGIN_URL`.
  Browser links, login redirects, assets and live status use the base; internal
  gate checks, webhooks and healthchecks keep their bare URLs. Cookies stay
  `Path=/`, and passkeys still belong to the hostname. The default is blank,
  so existing root and subdomain installs need no change.
  [Setup instructions](Reverse-Proxy.md#wranglarr-at-a-path-on-the-domain).
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
- Built on Rust 1.99.1 and Alpine 3.24.

### Fixed

- **qBittorrent 5.3 kept asking for a login** after setup, when its config file
  still had 4.x settings in it. The login is now written where 5.x reads it.
- **Opened through the proxy, posters and cards linked to wranglarr's name and
  the app's port** (`wranglarr.example.com:7878`). They now open each app at its
  name on the proxy, or at this machine's address when the app isn't proxied.
- **App links follow explicit placement.** Choose Direct IP for apps not on the
  proxy. DNS failures and shared login pages no longer change configured links.
- **Links saves are atomic.** Browser addresses and proxy placement are validated
  together; a failed save changes neither. Conflicting routes, reserved paths and
  unsupported proxy/access combinations are rejected before a config is generated.
- IPv6 upstream addresses are bracketed correctly, and path routes match path
  boundaries rather than similarly named prefixes.
- **AuthTunnel: apps opened by IP couldn't sign in** once `AUTH_LOGIN_URL` was
  set: the sign-in happened on the domain, and its cookie never reached the IP.
  Opened by IP, an app now signs in at wranglarr on that same address.
  `AUTH_LOGIN_URL` is only used under `AUTH_COOKIE_DOMAIN`.
- With AuthTunnel and no domain on the Reverse proxy page, the links follow
  `AUTH_COOKIE_DOMAIN`.

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
