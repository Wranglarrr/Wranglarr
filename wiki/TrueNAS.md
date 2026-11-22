# TrueNAS SCALE

The stack runs on TrueNAS SCALE **24.10 (Electric Eel) and later**, which
run apps on Docker. Earlier releases ran apps on Kubernetes and are not
supported.

There are two ways to run it:

- **From the TrueNAS shell (recommended).** It's the same as any Linux host, with `sudo` in front. `init.sh`, the VPN overlays, profiles and wranglarr's updates all work as documented. The containers don't appear on the Apps screen.
- **From the Apps screen.** `init.sh --truenas` sets the stack up from the shell, then installs it as one app that you start and stop there. See [the Apps screen](#optional-the-apps-screen).

> The shell route and an app installed with *Install via YAML* have been run
> on TrueNAS SCALE. `init.sh --truenas` follows TrueNAS's API documentation.
> If something differs on yours, please report it.

## 1. Datasets

In **Datasets**, on a data pool (`tank` below), create two datasets:

| Dataset | Holds | Preset |
| --- | --- | --- |
| `tank/apps/arrs` | the stack: this repository, `.env`, `appdata/` (app configs, Postgres, backups) | **Generic** |
| `tank/data` | downloads and media: `torrents/`, `usenet/`, `media/movies`, `media/tv`, `media/music` | **Generic**, or **SMB** if you share the media over SMB |

Two rules:

- **Make `torrents`, `usenet` and `media` plain folders inside `tank/data`, not child datasets.** Each ZFS dataset is its own filesystem, and a hardlink can't cross from one to another. With child datasets every import becomes a full copy and seeding torrents take up twice the space. The setup wizard flags this.
- **Don't use the SMB preset for `tank/apps/arrs`.** It blocks `chmod`, and Postgres won't initialise its data folder without it.

**Who owns them.** The containers run as `PUID:PGID`. Either:

- use TrueNAS's built-in **apps** user, `568:568`. Give both datasets to it: Datasets → the dataset → Permissions → Edit, set the owner user and group to `apps`, tick *Apply recursively*. Or
- use your own TrueNAS user's uid and gid, or a dedicated one ([Security](Security.md#truenas-scale)). Credentials → Users shows them.

Your stack folder is then `/mnt/tank/apps/arrs` and `DATA_ROOT` is `/mnt/tank/data`.

Don't put either one in your home directory or anywhere outside `/mnt/<pool>`. That's TrueNAS's boot pool, and upgrades replace it. The wizard flags that too.

## 2. Get the stack

Open **System → Shell**, or SSH in as your admin user. Each `docker` command
needs `sudo`.

```sh
cd /mnt/tank/apps
sudo git clone https://github.com/Wranglarrr/Wranglarr arrs
cd arrs
```

If `git` isn't there, run it from a container:

```sh
sudo docker run --rm -v /mnt/tank/apps:/git alpine/git clone https://github.com/Wranglarrr/Wranglarr arrs
```

## 3. Run the setup wizard

```sh
sudo docker run --rm -it -p 8788:8788 \
  -v /mnt/tank/apps/arrs:/stack \
  -v /mnt/tank/data:/data \
  ghcr.io/wranglarrr/wranglarr:latest wranglarr-setup
```

That pulls the published image. To run one you built yourself, see
[The wranglarr image](Setup.md#the-wranglarr-image).

It prints a link ending in `?t=` and a code. Open it with your TrueNAS
address in place of `localhost`: **http://\<truenas-ip\>:8788/?t=\<code\>**. The [Quickstart](Quickstart.md#2-run-the-setup-wizard)
walks through each step. On TrueNAS:

- **System:** PUID and PGID start as `568` when the wizard sees a ZFS dataset on `/data`. Change them if you used your own user in step 1.
- **Storage:** `APPDATA` stays `./appdata`. `DATA_ROOT` is `/mnt/tank/data`. The check shows the dataset name, and warns about child datasets, the boot pool, or a dataset `PUID:PGID` can't write.
- **Network:** pick ports that nothing else uses. TrueNAS's own UI takes 80 and 443, and your other apps have theirs (Apps → each app → *Ports*).

Stop the wizard with Ctrl-C once it has written the files.

## 4. Run `init.sh`

```sh
sudo ./scripts/init.sh
```

It does the same as on any other host (see [Setup](Setup.md)). Running as
root, it also gives the download folders it creates (`torrents/radarr`,
`usenet/complete/sonarr`, …) to `PUID:PGID`, so the apps can write them.
To run the stack from the Apps screen instead, use
`sudo ./scripts/init.sh --truenas` ([the Apps screen](#optional-the-apps-screen)).

Later commands, when the stack runs from the shell, are the usual ones with `sudo`:

```sh
sudo docker compose ps
sudo docker compose logs qbittorrent | grep -i password
sudo docker compose up -d wranglarr
```

Everything from [step 4 of the Quickstart](Quickstart.md#4-logins-you-set-yourself-leave-them-to-me-only)
on is the same. Open wranglarr at **http://\<truenas-ip\>:8787**.

## After a reboot

The containers have `restart: unless-stopped`, so they start again when
TrueNAS starts its Docker service. That only happens once an apps pool is
set (**Apps → Configuration → Choose Pool**), even if you run no TrueNAS
apps.

Some 25.10 builds have been reported not to start Docker at boot. If the
stack is down after a reboot, check **Apps**. Unsetting and setting the pool
again, or a second reboot, has fixed it for others
([forum thread](https://forums.truenas.com/t/needs-to-restart-docker-after-startup/45580)).

## Backups and updates

- **Backups** go to `appdata/backups` on the `tank/apps/arrs` dataset. A periodic snapshot task on that dataset, plus a replication task to another pool or machine, gives you an off-box copy. Each version folder runs without wranglarr ([Backups and Restore](Backups-and-Restore.md#without-wranglarr)).
- **Downloads and media** on `tank/data` aren't part of wranglarr's backups. Snapshot or replicate that dataset as you would any media share.
- **Image updates:** use wranglarr's Updates page. Only the containers listed in `wranglarr.toml` are touched, never TrueNAS's own apps. If you run the stack from the Apps screen, don't also update it there. See [Container Updates](Updates.md).

## Optional: the Apps screen

An app on TrueNAS's Apps screen runs one compose file, and doesn't read
`.env` or run `init.sh`. So run `init.sh` with `--truenas` in step 4, in place
of the plain command:

```sh
cd /mnt/tank/apps/arrs
sudo ./scripts/init.sh --truenas
```

It sets the stack up as usual, then stops it in the shell and installs it as
the custom app `arrs` through TrueNAS's API (`midclt`): the same as
*Install via YAML*, with nothing to copy and paste. `--truenas=NAME` gives the
app another name (lowercase letters, digits and hyphens).

The app gets the stack as it runs now:

- only the services enabled in `COMPOSE_PROFILES`, without their `profiles:`, so the app starts all of them;
- every value from `.env` filled in, and the `COMPOSE_FILE` overlays (VPN, AuthTunnel);
- absolute paths;
- wranglarr's image as `WRANGLARR_IMAGE` names it. The Apps screen can pull the published image; one you built (`wranglarr:local`) must stay on this machine.

Then:

- Start and stop the stack from **Apps → Installed**, not with `docker compose` in the shell.
- After changing `.env` (a new service, a password), stop the app and run `sudo ./scripts/init.sh --truenas` again. It updates the app and starts it. While the app runs, `init.sh` refuses to start, since both would use the same container names.
- The configuration TrueNAS stores holds your Postgres password and API keys, like `.env`.
- TrueNAS names the compose project `ix-arrs`. Containers keep the names from `compose.yaml` (`radarr`, `wranglarr`, …), which is what wranglarr looks for. Logs: `sudo docker logs wranglarr`.

Pasting into **Install via YAML** by hand works too, but copy the YAML from a
file opened in an editor, not from a terminal: copying out of a terminal can
change the whitespace YAML depends on.

## Sources

- [Installing custom apps](https://apps.truenas.com/managing-apps/installing-custom-apps/) (TrueNAS docs)
- [Install via YAML screen](https://www.truenas.com/docs/scale/25.10/scaleuireference/apps/installcustomappscreens/) (TrueNAS 25.10 docs)
- [Docker Compose: install via YAML](https://forums.truenas.com/t/docker-compose-install-via-yaml-thank-you-ix/22343) (forum)
- [ACL permissions with Docker Compose](https://forums.truenas.com/t/docker-compose-acl-permissins-hell/52140) (forum)
