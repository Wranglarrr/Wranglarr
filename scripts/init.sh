#!/usr/bin/env bash
# First-time bootstrap.
#
#  1. Start the config.xml-based arrs that are enabled in COMPOSE_PROFILES
#     WITHOUT Postgres (compose.init.yaml) so each writes its own config.xml.
#  2. Stop them.
#  3. Patch every config.xml to point at Postgres (scripts/patch_config.py),
#     and the download clients' save paths at /data (scripts/patch_downloads.py)
#     so the arrs see finished downloads at the same path (imports, hardlinks).
#     Download clients get the same treatment: generate config, then patch.
#  4. Seed wranglarr's config file if missing.
#  5. Bring up the real stack (compose.yaml).  Postgres creates the databases
#     on first init; the apps then create their schemas.  Bazarr needs no
#     patching (Postgres settings come from the environment).
#
#  6. Set the apps' logins the way APPS_AUTH in .env says (wiki/App-Logins.md):
#     apps = one login in every app, tunnel = AuthTunnel (wranglarr-gate signs
#     you in), manual = leave them alone.
#
# Re-running on an already initialised appdata dir skips step 1 and just
# re-applies the patch (useful after changing POSTGRES_PASSWORD in .env).
#
#   ./scripts/init.sh --auth    only step 6: after changing APPS_AUTH and the
#                               logins (wranglarr-setup --auth writes them)
set -euo pipefail

if ((BASH_VERSINFO[0] < 4)); then
  echo "init: needs Bash 4 or newer (this is $BASH_VERSION). On macOS: brew install bash, then run it again." >&2
  exit 1
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.."

AUTH_ONLY=no
for arg in "$@"; do
  case "$arg" in
    --auth) AUTH_ONLY=yes ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) echo "init: unknown option $arg (try --help)" >&2; exit 2 ;;
  esac
done

# docker compose, or podman compose when there is no docker CLI.
# Override with COMPOSE="podman-compose" etc.
if [[ -z "${COMPOSE:-}" ]]; then
  if command -v docker >/dev/null 2>&1; then COMPOSE="docker compose"
  elif command -v podman >/dev/null 2>&1; then COMPOSE="podman compose"
  else echo "init: neither docker nor podman found" >&2; exit 1; fi
fi
export PODMAN_COMPOSE_WARNING_LOGS=false
dc() { $COMPOSE "$@"; }

if [[ ! -f .env ]]; then
  echo "init: no .env found; copy .env.example to .env and edit it first" >&2
  exit 1
fi
# Pull values without sourcing .env (passwords/cron strings are not shell-safe).
envget() { sed -n "s/^[[:space:]]*$1=//p" .env | tail -n1 | sed 's/^"\(.*\)"$/\1/; s/^'"'"'\(.*\)'"'"'$/\1/'; }
APPDATA="$(envget APPDATA)"; APPDATA="${APPDATA:-./appdata}"
POSTGRES_USER="$(envget POSTGRES_USER)"
PROFILES="$(envget COMPOSE_PROFILES)"; PROFILES="${PROFILES:-radarr,sonarr,lidarr}"

declare -A PORT=([radarr]=7878 [sonarr]=8989 [lidarr]=8686 [prowlarr]=9696)
# the file each download client writes on first start
declare -A DLCONF=([qbittorrent]=qBittorrent/qBittorrent.conf [sabnzbd]=sabnzbd.ini [nzbget]=nzbget.conf)
XML_APPS=(); DL=()
for p in ${PROFILES//,/ }; do
  [[ -n "${PORT[$p]:-}" ]] && XML_APPS+=("$p")
  [[ -n "${DLCONF[$p]:-}" ]] && DL+=("$p")
done
export ARR_APPS="${XML_APPS[*]}" DL_APPS="${DL[*]}"
DATA_ROOT="$(envget DATA_ROOT)"
TORRENTS_DIR="$(envget TORRENTS_DIR)"; TORRENTS_DIR="${TORRENTS_DIR:-/data/torrents}"
USENET_DIR="$(envget USENET_DIR)"; USENET_DIR="${USENET_DIR:-/data/usenet}"
export TORRENTS_DIR USENET_DIR

# ---- app logins (APPS_AUTH) ------------------------------------------------------------
# shellcheck source=scripts/authtunnel.sh
source scripts/authtunnel.sh
APPS_AUTH="$(envget APPS_AUTH)"; APPS_AUTH="${APPS_AUTH:-manual}"
TUNNEL_WAS=no; [[ -f compose.authtunnel.yaml ]] && TUNNEL_WAS=yes

# set KEY=VALUE in .env (replace the last uncommented line, or append)
envset() {
  if grep -q "^[[:space:]]*$1=" .env; then
    local tmp; tmp="$(mktemp)"
    awk -v k="$1" -v v="$2" 'BEGIN{n=0} {a[NR]=$0; if ($0 ~ "^[[:space:]]*" k "=") n=NR} END{for(i=1;i<=NR;i++) print (i==n ? k "=" v : a[i])}' .env > "$tmp"
    cat "$tmp" > .env; rm -f "$tmp"
  else
    printf '%s=%s\n' "$1" "$2" >> .env
  fi
}

check_auth() {
  case "$APPS_AUTH" in
    apps|tunnel|manual) ;;
    *) echo "init: APPS_AUTH=$APPS_AUTH in .env: use apps, tunnel or manual" >&2; exit 1 ;;
  esac
  if [[ "$APPS_AUTH" == apps && -z "$(envget APPS_LOGIN_PASSWORD)" ]]; then
    echo "init: APPS_AUTH=apps needs APPS_LOGIN_USER and APPS_LOGIN_PASSWORD in .env" >&2; exit 1
  fi
  local wa; wa="$(envget WRANGLARR_AUTH | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  if [[ "$APPS_AUTH" == tunnel && "$wa" =~ ^(off|false|0|no)$ ]]; then
    echo "init: APPS_AUTH=tunnel needs wranglarr's own sign-in: WRANGLARR_AUTH=$(envget WRANGLARR_AUTH) turns it off, which would open every app to anyone" >&2; exit 1
  fi
  if [[ "$APPS_AUTH" == manual && "$TUNNEL_WAS" == yes ]]; then
    echo "init: the apps are behind wranglarr-gate now (AuthTunnel) and have no login of their own." >&2
    echo "init: Switching straight to manual would put them on the network with none. Use APPS_AUTH=apps" >&2
    echo "init: (one login in every app); after that, manual leaves them as they are." >&2
    exit 1
  fi
  if [[ "$APPS_AUTH" == apps ]]; then
    local u; u="$(envget APPS_LOGIN_USER)"; u="${u:-admin}"
    [[ ",$PROFILES," == *",qbittorrent,"* && ( "$(envget QBITTORRENT_USER)" != "$u" || "$(envget QBITTORRENT_PASSWORD)" != "$(envget APPS_LOGIN_PASSWORD)" ) ]] \
      && echo "init: note: QBITTORRENT_USER / QBITTORRENT_PASSWORD differ from the apps' login; qBittorrent gets those" >&2
    [[ ",$PROFILES," == *",nzbget,"* && ( "$(envget NZBGET_USER)" != "$u" || "$(envget NZBGET_PASS)" != "$(envget APPS_LOGIN_PASSWORD)" ) ]] \
      && echo "init: note: NZBGET_USER / NZBGET_PASS differ from the apps' login; NZBGet gets those" >&2
  fi
  return 0
}

# compose.authtunnel.yaml in COMPOSE_FILE exactly when APPS_AUTH=tunnel
tunnel_files() {
  local cf; cf="$(envget COMPOSE_FILE)"
  local parts=() p out=()
  IFS=: read -r -a parts <<< "${cf:-compose.yaml}"
  for p in "${parts[@]}"; do [[ -n "$p" && "$p" != compose.authtunnel.yaml ]] && out+=("$p"); done
  if [[ "$APPS_AUTH" == tunnel ]]; then
    tunnel_write; out+=(compose.authtunnel.yaml)
  else
    tunnel_remove
  fi
  local new; new="$(IFS=:; echo "${out[*]}")"
  if [[ "$new" != "${cf:-compose.yaml}" ]]; then
    envset COMPOSE_FILE "$new"; echo "init: COMPOSE_FILE=$new"
  fi
}

# qBittorrent's login lives in its config file: write it while qBittorrent is stopped
patch_qb() {
  [[ ",$PROFILES," == *",qbittorrent,"* && "$APPS_AUTH" != manual ]] || return 0
  dc stop qbittorrent >/dev/null 2>&1 || true
  # (no `down` afterwards: compose.init.yaml shares the stack's project, so it would take the apps down too)
  ARR_APPS="" DL_APPS=qbittorrent dc -f compose.init.yaml run --rm patch-config
}

apps_auth() {   # the wranglarr-apps-auth one-shot; $@ = --only apps|clients
  [[ "$APPS_AUTH" == manual ]] && return 0
  echo "init: setting the apps' logins (APPS_AUTH=$APPS_AUTH)"
  if ! dc run --rm wranglarr-apps-auth wranglarr-apps-auth "$@"; then
    echo "init: some apps' logins weren't set (above); fix the cause and re-run: ./scripts/init.sh --auth" >&2
    return 1
  fi
}

check_auth

if [[ "$AUTH_ONLY" == yes ]]; then
  if [[ "$APPS_AUTH" == tunnel ]]; then
    # the gate takes the ports first, then the apps drop their own logins
    tunnel_files
    patch_qb
    dc up -d --remove-orphans
    dc restart wranglarr-gate >/dev/null    # it reads its config only at start
    apps_auth
  else
    # leaving the tunnel: give the apps their logins back before their ports reopen
    [[ "$APPS_AUTH" == apps ]] && apps_auth --only apps
    patch_qb
    tunnel_files
    dc up -d --remove-orphans
    [[ "$APPS_AUTH" == apps ]] && apps_auth --only clients
  fi
  cat <<MSG

Done: APPS_AUTH=$APPS_AUTH. wranglarr's home page shows each app's login.
MSG
  exit 0
fi
tunnel_files

if [[ "$(envget PUID)" == 0 || "$(envget PGID)" == 0 ]]; then
  echo "init: warning: PUID/PGID is 0, so every app runs as root and creates root-owned files" >&2
  echo "init:          on your media. Use a normal user (id -u / id -g), e.g. 1000:1000, or 568:568 on TrueNAS." >&2
fi

# Podman has no /var/run/docker.sock: mount its own API socket instead.
if [[ -z "$(envget DOCKER_SOCKET)" && "$COMPOSE" == podman* ]]; then
  psock="$(podman info --format '{{.Host.RemoteSocket.Path}}' 2>/dev/null)"; psock="${psock#unix://}"
  if [[ -n "$psock" ]]; then
    printf '\n# Podman API socket wranglarr uses for restores and updates (init.sh)\nDOCKER_SOCKET=%s\n' "$psock" >> .env
    echo "init: DOCKER_SOCKET=$psock written to .env"
    [[ -S "$psock" ]] || echo "init: Podman's socket isn't running: systemctl --user enable --now podman.socket (rootful: sudo systemctl enable --now podman.socket)" >&2
  fi
fi

# wranglarr runs as PUID:PGID and reaches the container engine's socket through
# the socket's group: find that group once and keep it in .env.  Rootless
# Podman/Docker and Docker Desktop show the socket as root-group inside
# containers, hence 0 there.
if [[ -z "$(envget DOCKER_GID)" ]]; then
  sock="$(envget DOCKER_SOCKET)"; sock="${sock:-/var/run/docker.sock}"
  gid=""
  if [[ "$COMPOSE" == podman* ]]; then
    [[ "$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null)" == true ]] && gid=0
  elif docker info --format '{{.OperatingSystem}} {{.SecurityOptions}}' 2>/dev/null | grep -qiE 'docker desktop|rootless'; then
    gid=0
  fi
  if [[ -z "$gid" && -S "$sock" ]]; then gid="$(stat -c %g "$sock" 2>/dev/null || stat -f %g "$sock" 2>/dev/null || true)"; fi
  if [[ -n "$gid" ]]; then
    printf '\n# group of the Docker socket, so wranglarr can use it without root (init.sh)\nDOCKER_GID=%s\n' "$gid" >> .env
    echo "init: DOCKER_GID=$gid written to .env"
  else
    echo "init: could not find the Docker socket's group; set DOCKER_GID in .env (stat -c %g $sock)" >&2
  fi
fi

# This machine's IP address on the network, for wranglarr's links and the reverse
# proxy config: found once and kept in .env (the setup wizard may have filled it in).
if [[ -z "$(envget HOST_IP)" ]]; then
  ip=""
  if command -v ip >/dev/null; then
    ip="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit }}')"
  fi
  if [[ -z "$ip" ]] && command -v ipconfig >/dev/null; then      # macOS
    ip="$(ipconfig getifaddr "$(route -n get default 2>/dev/null | awk '/interface:/ {print $2}')" 2>/dev/null || true)"
  fi
  if [[ -z "$ip" ]] && command -v hostname >/dev/null; then
    ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
  fi
  if [[ -n "$ip" ]]; then
    if grep -q '^[[:space:]]*HOST_IP=' .env; then
      awk -v ip="$ip" '/^[[:space:]]*HOST_IP=/ { print "HOST_IP=" ip; next } { print }' .env > .env.tmp && cat .env.tmp > .env && rm -f .env.tmp
    else
      printf '\n# this machine'"'"'s IP address on the network; change it only if this .env goes to another machine (init.sh)\nHOST_IP=%s\n' "$ip" >> .env
    fi
    echo "init: HOST_IP=$ip written to .env (this machine's IP; change it only if this .env goes to another machine)"
  else
    echo "init: could not find this machine's IP; set HOST_IP in .env for the dashboard's links" >&2
  fi
fi

mkdir -p "$APPDATA"/{postgres,backups,wranglarr,radarr,sonarr,lidarr,prowlarr,bazarr,qbittorrent,sabnzbd,nzbget,seerr,vpn/wireguard}
# Seerr runs as PUID:PGID itself (no PUID variable of its own), so its folder must be theirs
if [[ ",$PROFILES," == *",seerr,"* ]]; then
  PUID_="$(envget PUID)"; PGID_="$(envget PGID)"
  if (( EUID == 0 )); then
    chown "${PUID_:-1000}:${PGID_:-1000}" "$APPDATA/seerr"
  elif [[ "$COMPOSE" == podman* && "$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null)" == true ]]; then
    podman unshare chown "${PUID_:-1000}:${PGID_:-1000}" "$APPDATA/seerr" 2>/dev/null || true
  elif [[ "$(stat -c %u "$APPDATA/seerr")" != "${PUID_:-1000}" ]]; then
    echo "init: $APPDATA/seerr must belong to ${PUID_:-1000}:${PGID_:-1000} (sudo chown ${PUID_:-1000}:${PGID_:-1000} $APPDATA/seerr)" >&2
  fi
fi

need_init=()
for app in "${XML_APPS[@]}"; do
  if [[ -f "$APPDATA/$app/config.xml" ]]; then
    echo "init: $app already has a config.xml, will only patch it"
  else
    need_init+=("$app")
  fi
done

if (( ${#need_init[@]} )); then
  echo "init: generating config.xml for: ${need_init[*]}"
  dc -f compose.init.yaml up -d "${need_init[@]}"
  for app in "${need_init[@]}"; do
    echo -n "init: waiting for $app to come up"
    for _ in $(seq 1 60); do
      if dc -f compose.init.yaml exec -T "$app" \
           curl -sf "http://localhost:${PORT[$app]}/ping" >/dev/null 2>&1 \
         && grep -q '<ApiKey>' "$APPDATA/$app/config.xml" 2>/dev/null; then
        echo " ok"; break
      fi
      echo -n "."; sleep 2
    done
    if ! grep -q '<ApiKey>' "$APPDATA/$app/config.xml" 2>/dev/null; then
      echo; echo "init: $app never wrote config.xml; check: $COMPOSE -f compose.init.yaml logs $app" >&2
      dc -f compose.init.yaml down
      exit 1
    fi
  done
  dc -f compose.init.yaml down
fi

dl_init=()
for app in "${DL[@]}"; do
  [[ -f "$APPDATA/$app/${DLCONF[$app]}" ]] || dl_init+=("$app")
done
if (( ${#dl_init[@]} )); then
  echo "init: generating default config for: ${dl_init[*]}"
  dc -f compose.init.yaml up -d "${dl_init[@]}"
  for app in "${dl_init[@]}"; do
    echo -n "init: waiting for $app"
    for _ in $(seq 1 60); do
      [[ -f "$APPDATA/$app/${DLCONF[$app]}" ]] && { echo " ok"; break; }
      echo -n "."; sleep 2
    done
    [[ -f "$APPDATA/$app/${DLCONF[$app]}" ]] || echo " (no config yet; patch it later by re-running init)"
  done
  sleep 5        # let them finish writing before they are stopped
  dc -f compose.init.yaml down
fi

# Download folders on the shared mount (host side of /data), including one per
# arr and Prowlarr: wranglarr gives each its own category (= folder) in every client.
dirs=()
for app in "${DL[@]}"; do
  case "$app" in
    qbittorrent) dirs+=("$TORRENTS_DIR" "$TORRENTS_DIR/incomplete")
                 for a in "${XML_APPS[@]}"; do dirs+=("$TORRENTS_DIR/$a"); done ;;
    *) dirs+=("$USENET_DIR/incomplete" "$USENET_DIR/complete")
       for a in "${XML_APPS[@]}"; do dirs+=("$USENET_DIR/complete/$a"); done ;;
  esac
done
if [[ -n "$DATA_ROOT" ]]; then
  made=(); new=()
  for d in "$TORRENTS_DIR" "$USENET_DIR" "${dirs[@]}"; do
    [[ "$d" == /data/* ]] || continue          # only paths on the shared mount
    h="$DATA_ROOT/${d#/data/}"
    [[ -d "$h" ]] || new+=("$h")
    if mkdir -p "$h" 2>/dev/null; then made+=("$h")
    else echo "init: could not create $h (create it, owned by PUID:PGID)" >&2; fi
  done
  # Run as root (sudo on TrueNAS SCALE, or a root shell): the folders just made
  # are root-owned, and the apps run as PUID:PGID.  Hand the new ones over.
  if (( EUID == 0 )) && (( ${#new[@]} )); then
    PUID_="$(envget PUID)"; PGID_="$(envget PGID)"
    chown "${PUID_:-1000}:${PGID_:-1000}" "${new[@]}" 2>/dev/null \
      || echo "init: could not chown the new download folders to ${PUID_:-1000}:${PGID_:-1000}" >&2
  fi
  # Rootless Podman maps your user to root inside the containers, so folders you
  # create are root-owned in there and the apps (running as PUID) can't write
  # them.  Hand them to PUID:PGID inside the user namespace.
  if [[ "$COMPOSE" == podman* ]] && (( ${#made[@]} )) \
     && [[ "$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null)" == true ]]; then
    PUID_="$(envget PUID)"; PGID_="$(envget PGID)"
    podman unshare chown "${PUID_:-1000}:${PGID_:-1000}" "$DATA_ROOT" "$DATA_ROOT"/{torrents,usenet} "${made[@]}" 2>/dev/null || true
  fi
fi

if (( ${#XML_APPS[@]} + ${#DL[@]} )); then
  echo "init: patching configs (Postgres: ${XML_APPS[*]:-none}; download paths: ${DL[*]:-none})"
  # apps rewrite their config on shutdown (qBittorrent always does), so a running
  # one would undo the patch: stop them; the final `up -d` starts them again
  dc stop "${XML_APPS[@]}" "${DL[@]}" >/dev/null 2>&1 || true
  dc -f compose.init.yaml run --rm patch-config
  dc -f compose.init.yaml down --remove-orphans >/dev/null 2>&1 || true
fi

if [[ ! -f "$APPDATA/wranglarr/wranglarr.toml" ]]; then
  cp wranglarr/wranglarr.example.toml "$APPDATA/wranglarr/wranglarr.toml"
  echo "init: seeded $APPDATA/wranglarr/wranglarr.toml (edit to add shared download clients/notifications)"
fi

# wranglarr's image: the published one is pulled by `up`; one built here (WRANGLARR_IMAGE
# without a registry, e.g. wranglarr:local) is built from ./wranglarr if it isn't there yet
WIMG="$(envget WRANGLARR_IMAGE)"
if [[ -n "$WIMG" && "${WIMG%%[:@]*}" != */* ]]; then
  ENGINE="${COMPOSE%% *}"; [[ "$ENGINE" == podman-compose ]] && ENGINE=podman; [[ "$ENGINE" == docker-compose ]] && ENGINE=docker
  if ! $ENGINE image inspect "$WIMG" >/dev/null 2>&1; then
    echo "init: building $WIMG from ./wranglarr"
    $ENGINE build -t "$WIMG" ./wranglarr
  fi
fi

# Postgres creates the apps' databases only on its very first start: make the
# ones for apps enabled since then (safe to repeat)
PROBLEMS=()
if dc up -d --wait postgres >/dev/null 2>&1; then
  if out="$(dc exec -T postgres /docker-entrypoint-initdb.d/01-create-arr-dbs.sh 2>&1)"; then
    printf '%s\n' "$out" | grep -v "already exists" || :
  else
    printf '%s\n' "$out" >&2
    PROBLEMS+=("creating the apps' databases failed (above). Fix it and run ./scripts/init.sh again.")
  fi
else
  echo "init: Postgres didn't come up" >&2
  PROBLEMS+=("Postgres didn't come up, so databases for newly enabled apps may be missing. Check: $COMPOSE logs postgres")
fi

# where wranglarr answers: WRANGLARR_PORT is a port or address:port
WPORT="$(envget WRANGLARR_PORT)"; WPORT="${WPORT:-8787}"; WHOST=localhost
if [[ "$WPORT" == *:* ]]; then WHOST="${WPORT%:*}"; WPORT="${WPORT##*:}"; fi
[[ "$WHOST" == 0.0.0.0 ]] && WHOST=localhost

echo "init: starting the full stack (profiles: $PROFILES)"
dc up -d --remove-orphans
if [[ "$APPS_AUTH" == tunnel ]]; then dc restart wranglarr-gate >/dev/null; fi
apps_auth || PROBLEMS+=("some apps' logins weren't set (above). Fix the cause and run ./scripts/init.sh --auth")

cat <<MSG

$( ((${#PROBLEMS[@]})) && echo "Started, but not everything worked (see the end)." || echo "Done.")  wranglarr: http://$WHOST:$WPORT

Check the apps came up on Postgres:
  $COMPOSE logs ${XML_APPS[*]}$([[ ",$PROFILES," == *",bazarr,"* ]] && echo " bazarr") 2>/dev/null | grep -i postgres | head -20
  $COMPOSE exec postgres psql -U "$POSTGRES_USER" -l

The SQLite files the init run created (e.g. $APPDATA/radarr/radarr.db) are
unused now and can be deleted.
MSG

if ((${#PROBLEMS[@]})); then
  echo >&2
  for p in "${PROBLEMS[@]}"; do echo "init: problem: $p" >&2; done
  exit 1
fi
