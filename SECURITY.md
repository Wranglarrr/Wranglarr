# Security policy

## Reporting a vulnerability

Please report it privately: open
[a security advisory](https://github.com/Wranglarrr/Wranglarr/security/advisories/new)
(Security tab -> Report a vulnerability). Don't open a public issue for it.

Include what you found, the version (`wranglarr --version` or the image tag), and the
steps or setup that show it. The fix is worked out in the advisory, and the advisory is
published with credit to you once a fixed release is out, unless you'd rather not be
named.

## Supported versions

Fixes go into a patch release of the latest minor version (currently 1.1.x). Older
versions don't get backports: update to the latest release.

## Scope

In scope: the wranglarr binary and image, the compose files, the setup wizard, and the
lariat installer and its checks (signatures, digests, the activation steps).

Out of scope, report these upstream: the arr apps and other images the stack runs
(Radarr, Sonarr, Prowlarr, Postgres, Gluetun and the rest), and lariat packages from
third-party providers, which belong to their publishers.

How the stack is locked down, and what it exposes, is in
[Security](https://github.com/Wranglarrr/Wranglarr/wiki/Security).
