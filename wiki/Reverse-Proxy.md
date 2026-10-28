# Reverse proxy

To reach the stack from outside your network, or by name instead of
`IP:port`, put a reverse proxy in front of it (or use Tailscale: see
[Remote access](Remote-Access.md)). wranglarr writes the config for
you: **Reverse proxy** in the sidebar. It changes nothing itself. You paste the
result into your proxy.

![The Reverse proxy page](images/proxy/01-proxy.png)

## Choices

| Setting | |
| --- | --- |
| **Proxy** | Caddy, nginx, Traefik (file provider or Docker labels), caddy-docker-proxy (Docker labels), Cloudflare Tunnel, Nginx Proxy Manager |
| **Domain** | the domain your DNS points at the proxy, e.g. `example.com` |
| **Apps at** | a name each, `https://radarr.example.com` (works for every app), or a path each, `https://example.com/radarr` |
| **The proxy reaches them** | at this machine's address and the published ports (the proxy runs on the host, or on another machine), or by service name (the proxy is a container on the stack's `arrs` network) |
| **LAN** | which addresses count as local for **LAN only**; blank = every private range |

Then, per app and for wranglarr itself:

| | |
| --- | --- |
| **Public** | reachable from anywhere |
| **LAN only** | proxied, at its name, but only for addresses on your network (an IP allowlist in the proxy) |
| **Not proxied** | only at `http://<this machine>:<port>`, as now |

The quick buttons cover the usual cases: everything public; the arrs public and
the download clients LAN only; everything LAN only.

When proxying wranglarr, set `FORWARDED_ALLOW_IPS` in `.env` to the proxy's exact
source IP as seen by wranglarr, then recreate its container. It defaults to
`127.0.0.1`; Docker and remote proxies usually need another address. The proxy
must replace forwarded host and scheme headers, and wranglarr's port must be
private to those proxies. These settings let wranglarr recognize HTTPS for
passkeys and secure cookies, and the browser's host for origin checks.
See [Login](Login.md#passkeys) for details.

## The login in front

Opening an app to the internet needs a login in front of it:

- **AuthTunnel** ([App logins](App-Logins.md)): the proxy points at the same
  ports, which now belong to wranglarr-gate, so every app asks for your wranglarr
  sign-in. Set `AUTH_COOKIE_DOMAIN` to your domain and `AUTH_LOGIN_URL` to
  wranglarr's name (`wranglarr-setup --auth`), and proxy wranglarr too. The
  page warns when they don't match. With Docker labels or by service name, the
  config points at `wranglarr-gate`, never at the apps directly.
- **One login for every app**: set **ask for it everywhere**. Through a proxy,
  every visitor can look local to the app, so "not on the local network" means
  no login.
- **Manual**: make sure each public app has a login. Dashboard → Logins shows the
  ones that don't.

Seerr is the exception: it has its own sign-in with your media server's
accounts and is meant for people without a wranglarr account, so it's never
put behind wranglarr-gate.

## Per proxy

**Caddy.** Add the blocks to your Caddyfile, `caddy reload`. Caddy gets the
certificates itself. LAN only is `abort` for anything outside `remote_ip`.

**nginx.** One file, `arrs.conf`, that goes inside `http { }`:

- Debian / Ubuntu (`sites-available`):
  ```sh
  sudo cp arrs.conf /etc/nginx/sites-available/arrs
  sudo ln -s /etc/nginx/sites-available/arrs /etc/nginx/sites-enabled/arrs
  ```
- elsewhere: `/etc/nginx/conf.d/arrs.conf`.

Get the certificates **before** enabling it: nginx refuses to start while an
`ssl_certificate` file is missing. The top of the file lists one
`sudo certbot certonly --nginx -d <name>` per name; with a wildcard certificate,
point the `ssl_` lines at its folder instead. Then
`sudo nginx -t && sudo systemctl reload nginx`. The port-80 block redirects to
HTTPS but leaves `/.well-known/acme-challenge/` alone, so renewals keep
working. LAN only is `allow` / `deny all`. HTTP/2 is commented out: `http2 on;`
needs nginx 1.25.1, newer than Debian 12 (1.22) and Ubuntu 24.04 (1.24) ship.

**Traefik, file provider.** Save in the folder `providers.file.directory` points
at; Traefik picks it up. Entry point and certificate resolver names are settings
on the page (`websecure`, `letsencrypt` by default). LAN only is the
`ipAllowList` middleware (Traefik v3; it's `ipWhiteList` on v2).

**Traefik or caddy-docker-proxy, Docker labels.** The page writes
`compose.proxy.yaml`: labels on the right containers (the apps, the VPN container
when they share its network, or wranglarr-gate with AuthTunnel), and the
proxy's network. Save it next to `compose.yaml`, add it to `COMPOSE_FILE` in
`.env` (`compose.yaml:compose.proxy.yaml`), `docker compose up -d`. `init.sh`
keeps it there when it adds or removes AuthTunnel's file.

**Cloudflare Tunnel.** Merge the `ingress:` rules into cloudflared's
`config.yml`, add the DNS names (`cloudflared tunnel route dns <tunnel> radarr.example.com`),
restart cloudflared. A tunnel is public by design, so LAN-only apps are left out;
put Cloudflare Access in front if you want them through it.

**Nginx Proxy Manager.** It's set up in its web UI; the page lists the steps per
host (forward host and port, websockets, the LAN-only access list).

## Paths instead of names

`https://example.com/radarr` needs one name and one certificate, but each app has
to know its path:

- Radarr, Sonarr, Lidarr, Prowlarr: Settings → General → **URL Base** `/radarr`, restart;
- Bazarr: Settings → General → URL Base; SABnzbd: Config → General → URL Base;
- then wranglarr's own address for that app has to include it: in
  `appdata/wranglarr/wranglarr.toml`, `url = "http://radarr:7878/radarr"`.

qBittorrent and NZBGet have no URL Base; the config strips the path for them.
wranglarr and Seerr always get a name of their own (`wranglarr.example.com`,
`seerr.example.com`): neither works under a path.

## Things to know

- **SABnzbd** refuses host names it doesn't know. The configs send it its own
  address instead of yours, so you don't have to edit `host_whitelist`.
- **LAN only** goes by the visitor's address as your proxy sees it. Behind a CDN
  (Cloudflare's orange cloud) that's the CDN's address, so every visitor is
  outside; configure your proxy's trusted proxies first.
- **Passkeys** belong to the name you added them at. One added at
  `192.168.1.10` won't work at `wranglarr.example.com`; add another under Account
  once you're signed in there.
- **Posters** open the proxied names when you've opened wranglarr through the
  proxy (at the domain above or a name under it). For a proxy set up without
  this page, set Dashboard → Links to `https://{name}.example.com`.

## HTTP API

```
GET  /api/proxy     settings, the plan per app and the rendered config
PUT  /api/proxy     {"kind":"caddy","style":"subdomain"|"path","domain":"example.com",
                     "upstream":"host"|"docker","upstream_host":"","lan":"",
                     "apps":{"radarr":{"expose":"public"|"lan"|"off","sub":""}}}
```
