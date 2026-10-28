#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Smoke-test a built wranglarr image on this machine's platform:
#
#   scripts/smoke-image.sh IMAGE VERSION
#
# The binary reports VERSION and its tools are there; the server starts as
# root with PUID/PGID set, answers /api/health, and has switched to PUID:PGID.
# DOCKER=podman runs it with Podman. Exits non-zero at the first failure.
set -eu
image=${1:?usage: smoke-image.sh IMAGE VERSION}
version=${2:?usage: smoke-image.sh IMAGE VERSION}
docker=${DOCKER:-docker}
name="wranglarr-smoke-$$"
dir=$(mktemp -d)

fail() {
  echo "smoke-image: $*" >&2
  if $docker container inspect "$name" >/dev/null 2>&1; then
    $docker logs "$name" >&2 2>&1 || true
  fi
  exit 1
}
cleanup() {
  $docker rm -f "$name" >/dev/null 2>&1 || true
  # the container handed these to PUID; remove them as it
  $docker run --rm -v "$dir:/w" --entrypoint sh "$image" -c 'rm -rf /w/config /w/backups' >/dev/null 2>&1 || true
  rm -rf "$dir" 2>/dev/null || true
}
trap cleanup EXIT

got=$($docker run --rm "$image" wranglarr --version)
[ "$got" = "wranglarr $version" ] || fail "version: got '$got', want 'wranglarr $version'"
$docker run --rm "$image" sh -c 'pg_dump --version >/dev/null && wranglarr-setup --help >/dev/null' \
  || fail "pg_dump or wranglarr-setup missing"

mkdir -p "$dir/config" "$dir/backups"
cat > "$dir/config/wranglarr.toml" <<'EOF'
[server]
bind = "0.0.0.0"
port = 8787

[storage]
root = "/backups"
state_db = "/config/wranglarr.db"

[[suite]]
name = "smoke"
[[suite.app]]
name = "radarr"
kind = "radarr"
url = "http://127.0.0.1:9"
api_key = "00000000000000000000000000000000"
EOF

# started as root (no --user), as a plain docker run of a LinuxServer.io-style image
$docker run -d --name "$name" -e PUID=1000 -e PGID=1000 -e NO_COLOR=1 \
  -v "$dir/config:/config" -v "$dir/backups:/backups" "$image" >/dev/null
ok=
for _ in $(seq 60); do
  if $docker exec "$name" wget -qO- http://127.0.0.1:8787/api/health 2>/dev/null | grep -q '"ok":true'; then
    ok=1
    break
  fi
  sleep 1
done
[ -n "$ok" ] || fail "/api/health didn't answer within 60 s"
# shellcheck disable=SC2016 # an awk program, not shell
uid=$($docker exec "$name" awk '/^Uid:/ {print $2}' /proc/1/status)
[ "$uid" = 1000 ] || fail "runs as uid $uid, not PUID 1000"
$docker logs "$name" 2>&1 | grep -q 'now running as 1000:1000' || fail "no 'now running as 1000:1000' in the log"
echo "smoke-image: $image ok (wranglarr $version, healthy, root -> 1000:1000)"
