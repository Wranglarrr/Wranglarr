# VPN: shared tunnel (gluetun)

One container, `vpn` ([qmcgaw/gluetun](https://github.com/qdm12/gluetun)),
owns a WireGuard or OpenVPN tunnel, a kill-switch firewall and a DNS-over-TLS
resolver. The apps you put in the tunnel are attached to its network namespace
(`network_mode: service:vpn`): all their traffic leaves through the tunnel, and
none leaves if it drops. You still open their UIs the way you did before.

## Which apps go in the tunnel

| In the tunnel | `COMPOSE_FILE` in `.env` | Pick it when |
| --- | --- | --- |
| **every app** (the wizard's default) | `compose.yaml:compose.vpn.yaml:compose.vpn-all.yaml` | you want nothing to think about: whatever indexers the arrs use, nothing they do leaves without the tunnel |
| **download clients and Prowlarr** | `compose.yaml:compose.vpn.yaml:compose.vpn-prowlarr.yaml` | every indexer is in Prowlarr. The arrs then talk only to Prowlarr, the download clients and metadata servers (TMDB, TheTVDB, MusicBrainz), so they can use your own connection |
| **download clients only** | `compose.yaml:compose.vpn.yaml` | only the swarm and news-server traffic needs hiding; searching indexers from your own address is fine with you |

The download clients are qBittorrent, SABnzbd and NZBGet. Seerr is never in the
tunnel: it only talks to your own apps, your media server and metadata servers.
The wizard asks under **Network & VPN → In the tunnel**.

**Download clients and Prowlarr** holds only while the arrs reach every indexer
through Prowlarr:

- **No indexers of their own.** An indexer added in an arr by hand, or from
  wranglarr's [Indexers](Indexers.md) page, is queried by the arr itself,
  outside the tunnel. Add it in Prowlarr instead.
- **Redirect off** for each torrent indexer in Prowlarr. With Redirect on,
  Prowlarr hands the arr the indexer's own download link, and the arr fetches
  the `.torrent` from there, outside the tunnel. Usenet indexers are the
  exception: Prowlarr always redirects their grabs and won't save Redirect off
  for them, so the arrs fetch each `.nzb` from your own address. The download
  itself still goes through the tunnel, from SABnzbd or NZBGet.

wranglarr checks both on every dashboard refresh. Anything that goes around the
tunnel shows up as a health warning on that app's card, starting with "VPN:".
**System → VPN** lists which apps are in the tunnel and which aren't.

### Switching later

Change `COMPOSE_FILE` (or re-run the wizard), then recreate the stack. The
`vpn` container changes, and the apps attached to it have to be recreated with it:

```sh
docker compose down && docker compose up -d
```

With [AuthTunnel](App-Logins.md) on, run `./scripts/init.sh --auth` in between:
the gate's file depends on which apps' ports live on `vpn`.

## What changes when it's on

| | Without | In the tunnel |
| --- | --- | --- |
| App web UI ports | published on each app | published on `vpn` (same host ports). With AuthTunnel, on `wranglarr-gate` as always |
| App → app, both in the tunnel | by service name | `localhost:<port>` (same namespace); [App Connections](App-Connections.md) writes it that way |
| Anything else → an app in the tunnel | `http://qbittorrent:8090` | unchanged: `vpn` carries the network alias of every app in it |
| App in the tunnel → `postgres`, apps outside it | by name | unchanged: gluetun ≥ 3.41 forwards container-name lookups to Docker's DNS, and its firewall lets the Docker network through |
| LAN → app UI | direct | through the port published on `vpn`: gluetun lets in what comes from the Docker network and sends the replies back the same way |
| App → LAN device (NAS, …) | direct | blocked unless the subnet is in `VPN_LAN_NETWORK` |
| Startup | apps wait for Postgres | apps also wait for `vpn` to be healthy |
| wranglarr | — | gets `vpn` in `WRANGLARR_ENABLED_APPS` and the list of apps in the tunnel in `WRANGLARR_VPN_APPS`; shows tunnel state, public IP and forwarded port |

## What was tested

Each layout was run with the stack's own compose files, AuthTunnel on and off,
against a test WireGuard server and a test web server that reports the address
a request comes from:

| Check | Result |
| --- | --- |
| UIs from the host and from the LAN, through `vpn` or through the gate | reachable, as without the VPN |
| Where an app's requests come from | apps in the tunnel: the VPN's exit address. Apps outside it: your own connection |
| Names | every app reaches `postgres` and every other app by service name, in or out of the tunnel |
| DNS | apps in the tunnel resolve through gluetun (`/etc/resolv.conf` is `127.0.0.1`), which sends lookups over the tunnel |
| Tunnel down | apps in it reach nothing outside, by name or by IP, and public names don't resolve. Container names, `postgres` and the UIs keep working |
| Tunnel back | gluetun reconnects by itself, about 15 seconds after the server returns |

One thing it can't stop: a program in the tunnel that sends a DNS query to
Docker's own resolver *by address* gets an answer from your host's resolver,
because the Docker network is allowed. The apps here don't: they use the
system resolver, which is gluetun.

## Providers and protocols

| Mode | `.env` |
| --- | --- |
| Custom WireGuard peer | `VPN_SERVICE_PROVIDER=custom`, `VPN_TYPE=wireguard`, `WIREGUARD_PRIVATE_KEY`, `WIREGUARD_ADDRESSES`, `WIREGUARD_PUBLIC_KEY`, `WIREGUARD_ENDPOINT_IP` (an IP, not a hostname), `WIREGUARD_ENDPOINT_PORT`, optional `WIREGUARD_PRESHARED_KEY` — or leave the keys blank and drop a `wg0.conf` at `appdata/vpn/wireguard/wg0.conf` |
| Named provider, WireGuard | `VPN_SERVICE_PROVIDER=mullvad` (or `protonvpn`, `private internet access`, `airvpn`, `ivpn`, `nordvpn`, `surfshark`, `windscribe`, …), `WIREGUARD_PRIVATE_KEY`, `WIREGUARD_ADDRESSES`, server filters `SERVER_COUNTRIES` / `SERVER_CITIES` / `SERVER_HOSTNAMES` / `SERVER_REGIONS` |
| Named provider, OpenVPN (legacy) | `VPN_TYPE=openvpn`, `VPN_USER`, `VPN_PASSWORD`, `OPENVPN_PROTOCOL` |
| Custom `.ovpn` | `VPN_SERVICE_PROVIDER=custom`, `VPN_TYPE=openvpn`, file at `appdata/vpn/custom.conf` with `remote` set to an IP and any certs beside it referenced as `/gluetun/…`, `OPENVPN_CUSTOM_CONFIG=/gluetun/custom.conf` |

Not supported by gluetun: IPsec/IKEv2, L2TP, PPTP, SSTP.

Provider pages with the exact server-filter values:
https://github.com/qdm12/gluetun-wiki/tree/main/setup/providers

## Port forwarding (torrent connectability)

`VPN_PORT_FORWARDING=on` asks the provider for an inbound port (PIA, ProtonVPN,
Perfect Privacy, PrivateVPN; PIA/Proton also need `VPN_USER`/`VPN_PASSWORD`).
The port is shown in wranglarr's header. With PIA or ProtonVPN, set
`VPN_PORT_FORWARD_ONLY=on` too, so gluetun picks a server that can forward one
(on ProtonVPN, the P2P servers). gluetun pushes the port into qBittorrent and
binds it to the tunnel's interface each time it changes; tick "Bypass
authentication for clients on localhost" in qBittorrent's Web UI options so it
can (see [Service: qBittorrent](Service-qBittorrent.md)).

## Control server

gluetun listens on port 8000 inside its namespace (not published). wranglarr
reaches it as `http://vpn:8000` with the `X-API-Key` header; the key is
`GLUETUN_API_KEY`, set through `HTTP_CONTROL_SERVER_AUTH_DEFAULT_ROLE`. Note
the default role covers every route, including the ones that can stop the
tunnel, so keep the key out of anything you don't trust. Routes used:
`/v1/vpn/status`, `/v1/publicip/ip`, `/v1/portforward`.

## Caveats

- If the `vpn` **container** is recreated by hand (`docker compose up` after editing it), the apps in it lose their network and must be restarted: `docker compose restart qbittorrent sabnzbd nzbget` (plus `prowlarr`, and `radarr sonarr lidarr bazarr` with every app in the tunnel). wranglarr's [Updates](Updates.md) do that for you. A dropped **tunnel** is different: gluetun reconnects by itself.
- Podman ignores the health check built into the gluetun image, so `compose.vpn.yaml` spells it out. The apps wait for it.
- Two apps cannot listen on the same port inside the namespace; that's why qBittorrent uses 8090.
- `VPN_LAN_NETWORK` must not overlap the tunnel's own range.
- Needs `/dev/net/tun` on the host and Docker Compose ≥ 2.24 (`!reset`).
- The VPN is for the apps' own traffic going out. For you reaching the apps from outside (Tailscale, Cloudflare Tunnel, a reverse proxy), see [Remote access](Remote-Access.md): it works the same with the VPN on.
