# VPN: per app (hotio images)

The alternative to the shared tunnel: each app gets hotio's image, which has
a WireGuard client built in, and its own tunnel. Use it when apps need
different exit servers. (To put only some apps on the VPN, the shared tunnel
can do that too, with one connection: see [which apps go in the
tunnel](VPN-Shared-Tunnel.md#which-apps-go-in-the-tunnel).)

```
COMPOSE_FILE=compose.yaml:compose.vpn-per-app.yaml      # .env
RADARR_VPN=true                                         # per app
VPN_LAN_NETWORK=192.168.1.0/24                          # required
```

What the override does per app: image → `ghcr.io/hotio/<app>`, `hostname
<app>.internal`, `cap_add: NET_ADMIN`, IPv6 disabled, `WEBUI_PORTS` for the
hotio firewall, and the `VPN_*` block below. The `/config` layout is the same
as linuxserver's, so `config.xml`, wranglarr and `init.sh` are unaffected. An
app with `<APP>_VPN=false` runs the hotio image without the tunnel.

Each app needs its own WireGuard peer: one key cannot be used by several
tunnels at once. Put it at `appdata/<app>/wireguard/wg0.conf` (`generic`,
`proton`) or let `pia` download it from your credentials.

## Variables (all apps share them except `<APP>_VPN`)

| `.env` | hotio variable | Default | Meaning |
| --- | --- | --- | --- |
| `RADARR_VPN` … `NZBGET_VPN` | `VPN_ENABLED` | `false` | start the tunnel for that app |
| `VPN_PROVIDER` | same | `generic` | `generic`, `proton`, `pia` |
| `VPN_CONF` | same | `wg0` | config name under `wireguard/` |
| `VPN_LAN_NETWORK` | same | required | LAN CIDR(s) allowed to reach the web UI; never the Docker bridge |
| `VPN_LAN_LEAK_ENABLED` | same | `false` | let all LAN-bound traffic bypass the tunnel (dangerous) |
| `VPN_EXPOSE_PORTS_ON_LAN` | same | | extra ports, `7878/tcp,…` |
| `VPN_AUTO_PORT_FORWARD` | same | `false` | `true` or a port; proton/pia |
| `VPN_PORT_REDIRECTS` | same | | `3000@3001/tcp` style redirects |
| `VPN_HEALTHCHECK_ENABLED` | same | `true` | bring the container down if connectivity fails |
| `VPN_NAMESERVERS` | same | | `wg`, `1.1.1.1`, `1.1.1.1@853#cloudflare-dns.com` |
| `VPN_INTERFACE_PREFIXES` | same | | override `eth,enp` |
| `VPN_PIA_USER`, `VPN_PIA_PASS` | same | | PIA account |
| `VPN_PIA_PREFERRED_REGION` | same | | region id (list appears in `wireguard/` after first start) |
| `VPN_PIA_DIP_TOKEN` | same | | dedicated-IP token |
| `VPN_PIA_PORT_FORWARD_PERSIST` | same | `false` | keep the forwarded port until it expires |
| `PRIVOXY_ENABLED` | same | `false` | HTTP proxy on 8118 |
| `UNBOUND_ENABLED`, `UNBOUND_NAMESERVERS` | same | `false`, | recursive DNS when the VPN is off |

Reference: https://hotio.dev/containers/radarr/#wireguard (identical block on
every hotio image used here, including Bazarr and the download clients).

wranglarr has no VPN status in this mode (there is no single control server);
the per-app health check already stops a container whose tunnel is down.
