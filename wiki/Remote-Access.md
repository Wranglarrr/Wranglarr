# Remote access: Tailscale and Cloudflare

Two different jobs often get called "VPN":

| | What it does | Here |
| --- | --- | --- |
| **The stack's VPN** (gluetun) | the apps' own traffic *out* to trackers, indexers and news servers goes through your VPN provider | [VPN: Shared Tunnel](VPN-Shared-Tunnel.md) |
| **Remote access** (Tailscale, Cloudflare Tunnel, a reverse proxy) | *you* reach the UIs from your phone or from work | this page |

They don't interfere. Remote access arrives at the ports published on the host,
the same ones you use on your LAN, and those work the same with the VPN on.
Don't put Tailscale or cloudflared inside the gluetun container: then your own
access would depend on the VPN provider, and stop whenever the tunnel drops.

## Tailscale

The easiest way in from outside: nothing is opened to the internet, and only
your own devices can connect.

1. Install Tailscale on the machine the stack runs on (on TrueNAS, the Tailscale
   app from the Apps catalog) and on your phone or laptop, signed in to the
   same account.
2. Open the apps at the machine's Tailscale name with the same ports as at
   home: `http://nas:8787` for wranglarr, `http://nas:7878` for Radarr.
   (`nas` is the machine's name in the Tailscale admin console, with MagicDNS on.
   Its `100.x.y.z` address works too.)

With [AuthTunnel](App-Logins.md), use one name for everything:
the sign-in cookie belongs to the name you signed in at, so sign in to
wranglarr at `http://nas:8787` and the apps at `http://nas:<port>` find it.
Nothing to set in `.env`.

### HTTPS, for passkeys

[Passkeys](Login.md#passkeys-need-https-and-a-host-name) need HTTPS on a host
name. `tailscale serve` gives the machine a real certificate under your
tailnet's name, still reachable from your devices only:

```sh
tailscale serve --bg 8787                                # wranglarr at https://nas.<tailnet>.ts.net
tailscale serve --bg --https=7878 http://127.0.0.1:7878  # Radarr at https://nas.<tailnet>.ts.net:7878
```

Repeat the second line for each app you want over HTTPS, with its port. Every
app stays on the one name, so the AuthTunnel cookie still reaches them all. A
passkey made at the `ts.net` name works only there; add another one under
Account for the name you use at home.

### Mobile apps

nzb360, LunaSea, Helmarr and the like connect to `http://nas:<port>` over
Tailscale like the browser. With AuthTunnel on, the gate lets through what
carries the app's own credentials: the arrs' and SABnzbd's API key, NZBGet's
login. qBittorrent signs in with a cookie instead, so a mobile app can't reach
it through the gate.

## Cloudflare Tunnel

`cloudflared` makes an outgoing connection to Cloudflare and publishes the apps
at names in your domain (`https://radarr.example.com`), without opening a port
on your router. Unlike Tailscale, those names are public: anyone can reach the
login page.

1. In the Cloudflare dashboard, create a tunnel and run its `cloudflared`
   container next to the stack. Put it on the `arrs` network, or run it on the
   host.
2. wranglarr writes the `ingress:` rules for you: **Reverse proxy** in the
   sidebar, Proxy **Cloudflare Tunnel**. Pick "by service name" when cloudflared
   is on the `arrs` network. Details in [Reverse proxy](Reverse-Proxy.md).
3. Add a DNS name for each app (`cloudflared tunnel route dns <tunnel> radarr.example.com`).

**A login in front of every app is a must.** Use AuthTunnel with
`AUTH_COOKIE_DOMAIN=example.com` and `AUTH_LOGIN_URL=https://wranglarr.example.com`,
and publish wranglarr through the tunnel too. The Reverse proxy page warns
when these don't match.

**Cloudflare Access** can add Cloudflare's own sign-in (email code, Google,
GitHub) in front, before anything reaches your stack. It also stops the mobile
apps, which can't get past it. Either leave the apps' API paths out of the Access
policy, or use Tailscale for the mobile apps.

**LAN only** apps are left out of the tunnel's rules: a Cloudflare Tunnel is
public by design.

**Don't stream video through Cloudflare.** Cloudflare's terms limit serving
video through its proxy and tunnels. Keep Jellyfin, Plex and Emby off it and
reach them over Tailscale or their own remote access. The arrs, wranglarr and
Seerr are fine.

## Cloudflare DNS with your own reverse proxy

If your domain's DNS is at Cloudflare and points at your own reverse proxy
(Caddy, Traefik, Nginx Proxy Manager):

- **Grey cloud** (DNS only): the proxy sees each visitor's own address, and
  **LAN only** works as it should.
- **Orange cloud** (proxied): every visitor comes from a Cloudflare address, so
  **LAN only** would let no one in. Set your proxy's trusted proxies to
  [Cloudflare's ranges](https://www.cloudflare.com/ips/) first, so it reads the
  real address from the header Cloudflare adds.
- Let's Encrypt's DNS challenge with a Cloudflare API token gives a certificate
  even for names that only resolve at home.

## Which to use

| You want | Use |
| --- | --- |
| to reach everything from your own devices, nothing public | Tailscale |
| HTTPS and passkeys, nothing public | Tailscale with `tailscale serve` |
| a link for someone without Tailscale (Seerr for family) | Cloudflare Tunnel or a reverse proxy, for Seerr only; the rest over Tailscale |
| everything at nice public names | Cloudflare Tunnel or a reverse proxy, with AuthTunnel |
