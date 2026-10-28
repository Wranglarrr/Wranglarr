# Security

wranglarr needs a lot of access to do its job: the Docker socket, every
app's config, and the backups. This page covers what that access exposes,
what the stack does to limit it, and what's left to you, starting with the
service user every app should run as.

## What's exposed

**The Docker socket is root on the host.** Whoever can talk to it can start
a container that mounts `/` and change anything on the machine. wranglarr
needs it for three things:

- stopping and starting apps around a restore;
- [container updates](Updates.md);
- creating download folders as the right user ([App Connections](App-Connections.md)).

Running wranglarr as a normal user doesn't change what the socket allows. It
limits what the wranglarr *process* can do on its own: it can't change its
own code, can't read files that aren't its user's, and has no special
privileges.

**wranglarr's web UI controls all of that.** Anyone signed in can:

- stop and start containers;
- restore old configs;
- delete backups;
- push indexers to your arrs;
- pull and recreate containers through Updates.

So the [login](Login.md) matters:

- turn on two-factor, or use a passkey;
- keep API keys to the tools that need them;
- don't set `WRANGLARR_AUTH=off` unless a reverse proxy asks for a login in front of it.

Keep it off the internet even so:

- don't forward its port on your router;
- for remote access, use a VPN to your network (WireGuard, Tailscale), or a reverse proxy with HTTPS ([Remote access](Remote-Access.md)). HTTPS is also what makes [passkeys](Login.md#passkeys-need-https-and-a-host-name) work.

**With AuthTunnel, wranglarr's login guards every app too** ([App logins](App-Logins.md)).
The apps have no login of their own then; wranglarr-gate lets a request through
only with a wranglarr session or the app's own API key. So:

- the same advice applies twice over: two-factor or a passkey;
- `init.sh` refuses AuthTunnel with `WRANGLARR_AUTH=off`;
- the apps are still reachable without a login inside the Docker network (`arrs`), which is how they reach each other. Don't attach other containers to it, or publish the apps' ports another way. A reverse proxy container should point at `wranglarr-gate`, as the generated config does ([Reverse proxy](Reverse-Proxy.md)).
- the gate is `nginx-unprivileged`: read-only, no capabilities, its own user.

**The apps' shared login** (`APPS_AUTH=apps`) sits in `.env` as `APPS_LOGIN_PASSWORD`,
in `sabnzbd.ini` in plain text, and in Bazarr's config as an unsalted MD5: use
one of its own, not wranglarr's.

**The setup wizard** (`wranglarr-setup`, port 8788) has no accounts. On start
it prints a link with a random code (`/?t=…`), and only a browser that opened
that link can use it. It runs only while you're setting up, so stop it
(Ctrl-C) once it has written `.env`.

**The backups hold secrets.** Each version has:

- every app's API key;
- your Postgres password, in the standalone stack's `.env`;
- dumps of every database.

Treat `appdata/backups` like a password store: keep it off shared folders, and encrypt any copy that leaves the machine.

**Apps running as root (`PUID=0`).** If an app is compromised, it can then
write anywhere it's mounted, including all of `/data`. Your media also fills
up with root-owned files. Every app should run as a normal user (next
section). The setup wizard and `init.sh` warn about `0`.

## What the stack does

| | |
| --- | --- |
| **wranglarr runs as `PUID:PGID`** | set with `user:` in `compose.yaml`, never root |
| **no capabilities** | `cap_drop: [ALL]`, `no-new-privileges` |
| **read-only image** | `read_only: true`: the code can't be changed while it runs. Only its own folders (`/config`, `/backups`), the app configs it restores, and a `/tmp` in memory are writable. |
| **the socket through its group** | `group_add: [DOCKER_GID]` gives only this container the socket's group. Your service user is **not** in the host's `docker` group. |
| **root only in `wranglarr-init`** | a one-shot container that runs before wranglarr each time the stack starts. It hands `appdata/wranglarr` and `appdata/backups` to `PUID:PGID`, then exits. It has the chown capabilities and nothing else, and no network. |

`init.sh` writes `DOCKER_GID` into `.env`:

- On Linux it's the group of the socket. Check it with `stat -c %g /var/run/docker.sock`, or `getent group docker`.
- Under rootless Podman, rootless Docker and Docker Desktop it's `0`. There, the socket shows up as root-group inside containers, and that root is your own user or the VM's.

If the System page shows **Docker control: off** while the socket is mounted,
`DOCKER_GID` is wrong.

**System → Status → Runs as** shows what wranglarr runs as. It turns red if
it's root on the host.

**Started as root anyway** (a plain `docker run` with no `--user`, as with a
LinuxServer.io image): when `PUID` is set and isn't `0`, wranglarr hands its
folders (`/config`, the backups, the state database's folder) to `PUID:PGID`
first, then runs as that user, with `PGID`, the Docker socket's group and
`DOCKER_GID` as its groups. This includes group `0` when that is the socket's
mapped group under rootless Podman or Docker Desktop. Its log says
`started as root: now running as 1000:1000 (PUID/PGID)`. A folder already owned
by `PUID:PGID` isn't walked again. Without `PUID` it stays root.

If wranglarr still runs as root, it prints an amber warning box at the top of
its log (`docker compose logs wranglarr`). The box repeats the points above
and says how to fix it. `wranglarr-init` prints one when `PUID` or `PGID` is
`0`. Set `NO_COLOR=1` for plain text.

## A service user for the stack

Make one user, with no login, that owns your media and runs every app,
wranglarr included. Its uid and gid become `PUID` and `PGID`.

- **One user for every app:** Radarr can then rename what qBittorrent downloaded, and Bazarr can write subtitles next to it.
- **A group you can join:** add your own login to it, keep `UMASK=002`, and you can manage the files too.
- **Not in the `docker` group:** that would give anyone who becomes that user root. The stack grants the socket to the wranglarr container alone (above).

The examples call it `media`.

### Debian, Ubuntu, Raspberry Pi OS

```sh
sudo adduser --system --group --no-create-home media
id media                          # uid=… gid=…  ->  PUID / PGID
```

### Fedora, RHEL, Rocky, Alma, Arch, openSUSE

```sh
sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin media
id media
```

### Alpine

```sh
sudo addgroup -S media
sudo adduser -S -D -H -G media -s /sbin/nologin media
id media
```

### Then, on any Linux

Give it the data drive, let your own login in, and keep new folders in the
group:

```sh
sudo chown -R media:media /mnt/data
sudo chmod -R g+w /mnt/data
sudo find /mnt/data -type d -exec chmod g+s {} +    # new files inherit the group
sudo usermod -aG media "$USER"                      # log out and back in afterwards
```

Don't `chown` the whole `appdata` folder:

- `appdata/postgres` belongs to the Postgres container's own user;
- the linuxserver images fix their own `/config` folders at start;
- `wranglarr-init` fixes wranglarr's.

Put the ids in `.env` (the setup wizard asks for them):

```sh
PUID=<uid of media>
PGID=<gid of media>
UMASK=002
```

Then run `./scripts/init.sh` as usual. It adds `DOCKER_GID`.

### TrueNAS SCALE

TrueNAS already has one: the **apps** user and group, `568:568`. Use it with
the datasets' *Apps* ACL preset, or give the datasets to it.

For your own user instead, go to **Credentials → Users → Add**:

- *Create New Primary Group*;
- no home directory;
- shell `nologin`;
- SMB access only if you want to reach the media over SMB.

Its uid and gid are on the Users list. Setting up the datasets is covered in
[TrueNAS SCALE](TrueNAS.md#1-datasets). Run `sudo ./scripts/init.sh` from the
shell, which finds `DOCKER_GID` itself.

### macOS (Docker Desktop)

No user to create. Containers run inside Docker Desktop's Linux VM, and files
they write to folders on your Mac belong to your Mac user whatever `PUID` is.
Use your own ids:

```sh
id -u    # PUID, e.g. 501
id -g    # PGID, e.g. 20
```

`DOCKER_GID` is `0` (init.sh sets it).

### Windows (Docker Desktop with WSL 2)

Run the stack from inside your WSL distro (Ubuntu by default). Keep the
repository and `appdata` on the distro's own filesystem (`~/arrs`, not
`/mnt/c/...`), which is faster and keeps Linux permissions. Then:

- **With Docker Desktop:** use your WSL user's ids (`id -u`, `id -g`, usually `1000`). `DOCKER_GID` is `0`.
- **With Docker Engine installed inside the distro** (no Desktop): it's a Linux host. Follow the [Debian/Ubuntu](#debian-ubuntu-raspberry-pi-os) steps above.

### Rootless Podman or rootless Docker

Use your own user. A rootless engine is the strongest setup here: the socket
only has your user's rights, so neither wranglarr nor a compromised app can
reach root. `DOCKER_GID` is `0`.

Your user's subordinate ids own the apps' files on the host. Use
`podman unshare` (or `rootlesskit` for Docker) to change or delete them, for
example `podman unshare rm -rf appdata/backups/media/<version>`.

## Going further

- **No socket at all.** Delete the `docker.sock` line from the wranglarr service (and `[docker]` in `wranglarr.toml`). You lose:
  - restores of download clients;
  - stopping apps around database restores;
  - Updates;
  - folder creation in App connections.

  Backups and database restores still work.
- **A socket proxy** (such as `tecnativa/docker-socket-proxy`) limits which API calls get through. Updates still need to create containers, so it narrows the exposure less than it seems.
- **Pin the image.** Set `WRANGLARR_IMAGE` to an exact release (`ghcr.io/wranglarrr/wranglarr:1.0.0`), or build `wranglarr:local` yourself from a tagged release ([The wranglarr image](Setup.md#the-wranglarr-image)).
