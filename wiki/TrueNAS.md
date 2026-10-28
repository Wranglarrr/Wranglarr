# TrueNAS SCALE

The stack runs on TrueNAS SCALE **24.10 (Electric Eel) and later**, which
run apps on Docker. Earlier releases ran apps on Kubernetes and are not
supported.

There are two ways to run it:

- **From the TrueNAS shell (recommended).** It's the same as any Linux host, with `sudo` in front. `init.sh`, the VPN overlays, profiles and wranglarr's updates all work as documented. The containers don't appear on the Apps screen.
- **From the Apps screen** ("Install via YAML"). Optional, after the shell route. The stack then shows up as one app that you can start and stop there. See [the Apps screen](#optional-the-apps-screen).

> This page follows TrueNAS's documented behaviour, but it has not yet been
> run on a TrueNAS machine. If something differs on yours, please report it.

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

Later commands are the usual ones with `sudo`:

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

TrueNAS's **Apps → Discover Apps → ⋮ → Install via YAML** takes one compose
file. That screen doesn't read `.env` and runs no `init.sh`. So set the stack
up from the shell first (steps 1–4), then hand TrueNAS the finished
configuration:

```sh
cd /mnt/tank/apps/arrs
sudo docker compose down
sudo docker compose config > truenas-app.yaml
```

`config` reads `COMPOSE_FILE` from `.env`, so a VPN layout is included.

The result is the stack as it runs now:

- only the services enabled in `COMPOSE_PROFILES`;
- every value from `.env` filled in;
- absolute paths;
- wranglarr's image as `WRANGLARR_IMAGE` names it. The Apps screen can pull the published image; one you built (`wranglarr:local`) must stay on this machine.

Paste it into *Install via YAML*, give the app a name (`arrs`) and save.

- `truenas-app.yaml` contains your Postgres password and API keys, and so does the YAML TrueNAS stores. Delete the file once the app is saved.
- After changing `.env` (a new service, a password), run the three commands again and paste the result into the app's **Edit**.
- Don't start the stack from the shell as well. Use the app's Stop and Start.
- TrueNAS names the compose project `ix-arrs`. Containers keep the names from `compose.yaml` (`radarr`, `wranglarr`, …), which is what wranglarr looks for.

## Sources

- [Installing custom apps](https://apps.truenas.com/managing-apps/installing-custom-apps/) (TrueNAS docs)
- [Install via YAML screen](https://www.truenas.com/docs/scale/25.10/scaleuireference/apps/installcustomappscreens/) (TrueNAS 25.10 docs)
- [Docker Compose: install via YAML](https://forums.truenas.com/t/docker-compose-install-via-yaml-thank-you-ix/22343) (forum)
- [ACL permissions with Docker Compose](https://forums.truenas.com/t/docker-compose-acl-permissins-hell/52140) (forum)
