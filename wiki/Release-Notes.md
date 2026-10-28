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
