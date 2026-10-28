# Environment variables (`.env`)

Every key read by the compose files. "Used by" names the service(s) whose
definition references it. Keys with a `?` default are required when that
file is in use. See the per-service pages for what each value does inside
the container.

## Global

| Key | Default | Used by | Description |
| --- | --- | --- | --- |
| `TZ` | `UTC` | all | container timezone |
| `PUID` | `1000` | all apps, vpn, `patch-config`, wranglarr | user id the apps and wranglarr run as, and that owns their files. Never `0`; see [Users and permissions](Setup.md#users-and-permissions) |
| `PGID` | `1000` | all apps, vpn, `patch-config`, wranglarr | group id; a group shared with whoever else manages the media |
| `WRANGLARR_AUTH` | `on` | wranglarr | the login for the web UI and API. `off` turns it off; only behind a reverse proxy that asks for a login itself. See [Login](Login.md) |
| `DOCKER_GID` | `0` | wranglarr | group of the Docker socket, so wranglarr can use it without root. `init.sh` fills it in: `stat -c %g /var/run/docker.sock` on Linux, `0` for rootless Podman/Docker and Docker Desktop. See [Security](Security.md) |
| `UMASK` | `002` | all apps, wranglarr | umask for files the apps create (`002` = group-writable) |
| `COMPOSE_PROFILES` | `radarr,sonarr,lidarr` | compose, postgres (`ARR_APPS`), wranglarr (`WRANGLARR_ENABLED_APPS`) | which services exist: any of `radarr sonarr lidarr prowlarr bazarr qbittorrent sabnzbd nzbget seerr` |
| `WRANGLARR_IMAGE` | `ghcr.io/wranglarrr/wranglarr:latest` | wranglarr, wranglarr-init, wranglarr-apps-auth | the image they run; `wranglarr:local` for one you built ([The wranglarr image](Setup.md#the-wranglarr-image)) |
| `COMPOSE_FILE` | `compose.yaml` | compose | the VPN layout: `:compose.vpn.yaml` (download clients) plus `:compose.vpn-prowlarr.yaml` or `:compose.vpn-all.yaml` for one shared tunnel, or `:compose.vpn-per-app.yaml` ([VPN: Shared Tunnel](VPN-Shared-Tunnel.md#which-apps-go-in-the-tunnel)). `init.sh` adds `compose.authtunnel.yaml` last |
| `APPDATA` | `./appdata` | all | root for per-app config, Postgres data, wranglarr state, backups |
| `DATA_ROOT` | **required** | all apps | mounted at `/data` |
| `DATA_ROOT_2` | | all apps (commented) | optional second drive → `/data2` |
| `TORRENTS_DIR` | `/data/torrents` | `init.sh`, `patch-config` | qBittorrent's save path inside the containers (one subfolder per category = per arr); written into `qBittorrent.conf` only while that is still at its default; keep it under `/data` |
| `USENET_DIR` | `/data/usenet` | `init.sh`, `patch-config` | SABnzbd / NZBGet download folders: `<dir>/incomplete`, `<dir>/complete/<arr>`; same rule |
| `SABNZBD_NZB_BACKUP` | `nzb-backup` | `patch-config` | SABnzbd's `nzb_backup_dir`, relative to its `/config`: a copy of every NZB added, so backups hold them. Empty = off |
| `DOCKER_SOCKET` | `/var/run/docker.sock` | wranglarr | host side of the engine socket wranglarr uses for restores and [updates](Updates.md); Podman: `init.sh` sets it to Podman's own socket (rootless: `/run/user/<uid>/podman/podman.sock`) |

## Postgres → [Service: Postgres](Service-Postgres.md)

| Key | Default | Used by | Description |
| --- | --- | --- | --- |
| `POSTGRES_IMAGE` | `postgres:17` | postgres | pin the major; never `latest` |
| `POSTGRES_USER` | **required** | postgres, bazarr, wranglarr, `patch-config` | role that owns every app database |
| `POSTGRES_PASSWORD` | **required** | same | its password |
| `POSTGRES_HOST` | `postgres` | `patch-config` | host written into config.xml |
| `POSTGRES_PORT` | `5432` | `patch-config` | port written into config.xml |
| `POSTGRES_HOST_PORT` | `5432` | postgres (commented `ports:`) | host port if you choose to publish Postgres |

## Published ports and images

| Key | Default | Used by | Description |
| --- | --- | --- | --- |
| `RADARR_PORT` | `7878` | radarr / vpn | host port for the web UI |
| `SONARR_PORT` | `8989` | sonarr / vpn | |
| `LIDARR_PORT` | `8686` | lidarr / vpn | |
| `PROWLARR_PORT` | `9696` | prowlarr / vpn | |
| `BAZARR_PORT` | `6767` | bazarr / vpn | |
| `QBITTORRENT_PORT` | `8090` | qbittorrent / vpn | maps to 8090 inside; keep them equal (qBittorrent's Host header validation) |
| `SABNZBD_PORT` | `8085` | sabnzbd / vpn | maps to 8080 inside |
| `NZBGET_PORT` | `6789` | nzbget / vpn | |
| `SEERR_PORT` | `5055` | seerr | never inside the VPN ([Service: Seerr](Service-Seerr.md)) |
| `WRANGLARR_PORT` | `8787` | wranglarr | |
| `WRANGLARR_URL_BASE` | empty | wranglarr | browser path prefix, e.g. `/wranglarr`. The proxy must pass it through unchanged. Bare endpoints remain available for internal callers; cookies stay `Path=/`. Must not shadow a built-in route such as `/api` |
| `HOST_IP` | found by setup or `init.sh` | wranglarr | this machine's IP address on your network (not a domain name): where the dashboard's Direct IP apps open when you came through the reverse proxy, and where the proxy config reaches the published ports. The wizard fills it in when you open it by IP, `init.sh` when it's blank. Change it only if this `.env` goes to another machine |
| `FORWARDED_ALLOW_IPS` | `127.0.0.1` | wranglarr | comma-separated exact proxy IPs trusted for `X-Forwarded-For`, `X-Forwarded-Host`, `X-Forwarded-Proto`; empty trusts none. Use the immediate peer as seen inside wranglarr. `*` trusts every known peer and requires proxy-only port access. See [Login](Login.md#passkeys) |
| `RADARR_IMAGE` … `NZBGET_IMAGE` | linuxserver images (hotio in `compose.vpn-per-app.yaml`) | each app | image override, one per app: `RADARR_IMAGE`, `SONARR_IMAGE`, `LIDARR_IMAGE`, `PROWLARR_IMAGE`, `BAZARR_IMAGE`, `QBITTORRENT_IMAGE`, `SABNZBD_IMAGE`, `NZBGET_IMAGE` |
| `SEERR_IMAGE` | `ghcr.io/seerr-team/seerr:latest` | seerr | |
| `GLUETUN_IMAGE` | `qmcgaw/gluetun:v3.41.3` | vpn | |

## Credentials

| Key | Default | Used by | Description |
| --- | --- | --- | --- |
| `QBITTORRENT_USER`, `QBITTORRENT_PASSWORD` | `admin`, empty | wranglarr, init | qBittorrent web UI login; wranglarr and the arrs log in with it. With `APPS_AUTH` `apps` or `tunnel`, init writes it into `qBittorrent.conf`; with `manual`, set it in qBittorrent's UI first (it starts with a temporary one, see its logs) and copy it here |
| `GLUETUN_API_KEY` | required with `compose.vpn.yaml` | vpn (control server role), wranglarr | lets wranglarr read VPN status |
| `NZBGET_USER` | `nzbget` | nzbget, wranglarr | web login; the image applies it at launch (`nzbget.conf` keeps its default), wranglarr logs in with it |
| `NZBGET_PASS` | `tegbzn6789` | nzbget, wranglarr | change it |

## App logins → [App Logins](App-Logins.md)

Set by the setup wizard (`wranglarr-setup --auth` to change them) and applied by
`./scripts/init.sh` (`--auth` to apply only these). wranglarr only reads them.

| Key | Default | Used by | Description |
| --- | --- | --- | --- |
| `APPS_AUTH` | `manual` | init, wranglarr | `apps`: one login in every app; `tunnel`: AuthTunnel, wranglarr-gate asks for your wranglarr sign-in; `manual`: left alone |
| `APPS_LOGIN_USER`, `APPS_LOGIN_PASSWORD` | `admin`, empty | init (`wranglarr-apps-auth`) | `apps`: the login. The wizard makes `QBITTORRENT_*` and `NZBGET_*` the same |
| `APPS_AUTH_REQUIRED` | `enabled` | init | `apps`: the arrs ask `enabled` (everywhere) or `disabledForLocalAddresses` |
| `AUTH_COOKIE_DOMAIN` | empty | wranglarr | `tunnel`: the sign-in cookie's domain, e.g. `example.com`, so it reaches `radarr.example.com` … |
| `AUTH_LOGIN_URL` | empty | wranglarr | `tunnel`: wranglarr's name on your reverse proxy for the gate's login redirect, e.g. `https://wranglarr.example.com` or `https://myhouse.com/wranglarr` (including `WRANGLARR_URL_BASE`, not this machine's IP), used for apps opened under `AUTH_COOKIE_DOMAIN`; otherwise, and when empty, the app's address on `WRANGLARR_PORT` |
| `GATE_IMAGE` | `docker.io/nginxinc/nginx-unprivileged:1.27-alpine` | wranglarr-gate | `tunnel`: the gate's image |

## VPN, shared tunnel (`compose.vpn.yaml`) → [VPN: Shared Tunnel](VPN-Shared-Tunnel.md)

| Key | Default | gluetun variable | Description |
| --- | --- | --- | --- |
| `VPN_SERVICE_PROVIDER` | `custom` | `VPN_SERVICE_PROVIDER` | `custom`, `mullvad`, `protonvpn`, `private internet access`, `airvpn`, `ivpn`, `nordvpn`, `surfshark`, `windscribe`, … |
| `VPN_TYPE` | `wireguard` | `VPN_TYPE` | `wireguard` or `openvpn` |
| `WIREGUARD_PRIVATE_KEY` | | same | your peer's private key |
| `WIREGUARD_PRESHARED_KEY` | | same | optional |
| `WIREGUARD_ADDRESSES` | | same | tunnel address, e.g. `10.2.0.2/32` |
| `WIREGUARD_PUBLIC_KEY` | | same | custom provider: the server's key |
| `WIREGUARD_ENDPOINT_IP` | | same | custom provider: server **IP** (no hostnames) |
| `WIREGUARD_ENDPOINT_PORT` | `51820` | same | |
| `WIREGUARD_MTU` | `1320` | same | lower if large transfers stall |
| `SERVER_COUNTRIES` / `SERVER_CITIES` / `SERVER_HOSTNAMES` / `SERVER_REGIONS` | | same | server filters for named providers (`SERVER_REGIONS` for PIA/ProtonVPN) |
| `VPN_USER` / `VPN_PASSWORD` | | `OPENVPN_USER` / `OPENVPN_PASSWORD` | provider account; OpenVPN always, PIA/ProtonVPN WireGuard port forwarding too |
| `VPN_PORT_FORWARDING` | `off` | same | ask the provider for an inbound port (PIA, ProtonVPN, Perfect Privacy, PrivateVPN) |
| `VPN_PORT_FORWARDING_PROVIDER` | | same | only when using `custom` with one of those providers' configs |
| `VPN_PORT_FORWARD_ONLY` | | `PORT_FORWARD_ONLY` | `on`: only servers that can forward a port (PIA, ProtonVPN's P2P servers) |
| `OPENVPN_PROTOCOL` | `udp` | same | `udp` / `tcp` |
| `OPENVPN_CUSTOM_CONFIG` | | same | `/gluetun/custom.conf` for your own `.ovpn` at `appdata/vpn/custom.conf` |
| `OPENVPN_CIPHERS` | | same | override if the provider needs it |
| `VPN_LAN_NETWORK` | | `FIREWALL_OUTBOUND_SUBNETS` | LAN CIDRs the apps may reach directly (the Docker network is always allowed) |
| `DNS_UPSTREAM_RESOLVER_TYPE` | `dot` | same | `dot`, `doh`, `plain` |
| `DNS_UPSTREAM_RESOLVERS` | `cloudflare` | same | comma list |
| `BLOCK_MALICIOUS` | `on` | same | DNS blocklist |
| `HEALTH_VPN_DURATION_INITIAL` | `30s` | same | how long to wait before declaring the tunnel unhealthy at start |
| `UPDATER_PERIOD` | `24h` | same | server-list refresh |

## VPN, per app (`compose.vpn-per-app.yaml`) → [VPN: Per App](VPN-Per-App.md)

| Key | Default | Description |
| --- | --- | --- |
| `RADARR_VPN`, `SONARR_VPN`, `LIDARR_VPN`, `PROWLARR_VPN`, `BAZARR_VPN`, `QBITTORRENT_VPN`, `SABNZBD_VPN`, `NZBGET_VPN` | `false` | per app `VPN_ENABLED` |
| `VPN_PROVIDER` | `generic` | `generic`, `proton`, `pia` |
| `VPN_CONF` | `wg0` | name of the WireGuard config under `appdata/<app>/wireguard/` |
| `VPN_LAN_NETWORK` | **required** | LAN CIDR(s) allowed to reach the web UI |
| `VPN_LAN_LEAK_ENABLED` | `false` | let all LAN-bound traffic bypass the tunnel (dangerous) |
| `VPN_EXPOSE_PORTS_ON_LAN` | | extra ports to expose, `7878/tcp,…` |
| `VPN_AUTO_PORT_FORWARD` | `false` | `true` or a port number; proton/pia |
| `VPN_PORT_REDIRECTS` | | `from@to/tcp` redirects |
| `VPN_HEALTHCHECK_ENABLED` | `true` | stop the container if connectivity fails |
| `VPN_NAMESERVERS` | | `wg`, `1.1.1.1`, `1.1.1.1@853#cloudflare-dns.com`, … |
| `VPN_INTERFACE_PREFIXES` | | override `eth,enp` detection of local interfaces |
| `VPN_PIA_USER` / `VPN_PIA_PASS` / `VPN_PIA_PREFERRED_REGION` / `VPN_PIA_DIP_TOKEN` / `VPN_PIA_PORT_FORWARD_PERSIST` | | PIA account, region, dedicated-IP token, keep forwarded port |
| `PRIVOXY_ENABLED` | `false` | HTTP proxy on 8118 inside the tunnel |
| `UNBOUND_ENABLED` / `UNBOUND_NAMESERVERS` | `false` / | recursive resolver when the VPN is off |

## Set by compose, not by you

These are container-level values the compose files set from the above; listed
so you know they exist.

| Container | Variable | Value |
| --- | --- | --- |
| postgres | `POSTGRES_DB` | `${POSTGRES_USER}` (a default db; the app dbs come from `initdb/`) |
| postgres | `ARR_APPS` | `${COMPOSE_PROFILES}` |
| bazarr | `POSTGRES_ENABLED`, `POSTGRES_HOST`, `POSTGRES_PORT`, `POSTGRES_DATABASE`, `POSTGRES_USERNAME`, `POSTGRES_PASSWORD`, `PGSSLCERT` | `true`, `postgres`, `5432`, `bazarr`, `${POSTGRES_USER}`, `${POSTGRES_PASSWORD}`, `/tmp/postgresql.crt` |
| qbittorrent | `WEBUI_PORT`, `TORRENTING_PORT` | `8090`, `6881` |
| wranglarr | `WRANGLARR_ENABLED_APPS`, `WRANGLARR_CONFIG`, `POSTGRES_USER`, `POSTGRES_PASSWORD`, `GLUETUN_API_KEY`, `QBITTORRENT_USER`, `QBITTORRENT_PASSWORD`, `NZBGET_USER`, `NZBGET_PASS`, `APPS_AUTH`, `APPS_LOGIN_USER`, `APPS_AUTH_REQUIRED`, `AUTH_COOKIE_DOMAIN`, `AUTH_LOGIN_URL`, `WRANGLARR_PORT`, `WRANGLARR_URL_BASE`, `HOST_IP` | `${COMPOSE_PROFILES}` (+`,vpn` with the VPN override), `/config/wranglarr.toml`, … |
| vpn | `HTTP_CONTROL_SERVER_AUTH_DEFAULT_ROLE` | `{"auth":"apikey","apikey":"${GLUETUN_API_KEY}"}` |
| patch-config | `ARR_APPS`, `DL_APPS` | the enabled config.xml apps and download clients, set by `init.sh` |
| patch-config | `APPS_AUTH`, `QBITTORRENT_USER`, `QBITTORRENT_PASSWORD` | from `.env`: qBittorrent's login (and, for `tunnel`, the whitelist that lets the gate in) |
| wranglarr-apps-auth | wranglarr's, plus `APPS_LOGIN_PASSWORD` | the one-shot `init.sh` runs to set the apps' logins |
| wranglarr-gate | `NGINX_ENTRYPOINT_LOCAL_RESOLVERS`, `NGINX_ENVSUBST_OUTPUT_DIR`, `NGINX_ENVSUBST_FILTER`, `NZBGET_USER`, `NZBGET_PASS` | `1`, `/tmp/gate`, `^(NGINX_LOCAL_RESOLVERS\|WR_)`, from `.env` (the gate signs you in to NZBGet) |
| patch-config | `TORRENTS_DIR`, `USENET_DIR`, `SABNZBD_NZB_BACKUP` | from `.env`, defaults above |
| hotio apps | `WEBUI_PORTS` | the app's port, for the hotio firewall |
