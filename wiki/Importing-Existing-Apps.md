# Bringing in apps you already run

You already have Radarr, Sonarr, Prowlarr or the others running: in Docker,
installed on Windows or Linux, on a NAS or on another machine. This page
moves them into the stack and keeps their library, history, profiles, custom
formats, indexers, tags and API keys.

Each app takes three steps:

1. **Its config**: the old config folder goes into `appdata/<app>/` before
   `init.sh` runs.
2. **Its database**: the stack keeps the apps' data in Postgres. Most
   installs use SQLite (a `radarr.db` file): one command per app copies the
   rows across. Already on Postgres? Then it's a dump and a restore
   ([Already on Postgres](#already-on-postgres)).
3. **Its paths and connections**: the stack sees your media at `/data/...`,
   and the other apps at new addresses.

Not worth keeping? Start fresh and import the library instead
([below](#start-fresh-and-import-the-library)).

## Before you start

- **SQLite: same version on both sides.** Copying a SQLite database only
  works when the new app runs the exact version the old one ran: the tables
  have to match. The stack runs each app's newest image, so **update the old
  app first** (System → Updates), then check System → Status → Version on
  both. Can't update the old one? See [pinning the version](#the-old-app-cant-be-updated).
  Already on Postgres, the new app only needs to be the same version or newer.
- **Back it up.** In each old app: System → Backup → **Backup now**, and
  keep the zip. Nothing below changes the old install, but keep the backup
  until the new one has run for a while.
- **Note the old paths.** Settings → Media Management → Root folders, and
  each download client's folders. You'll need them [later](#3-paths).
- **Let the downloads finish**, or note what's still downloading. The new
  download clients start with a new category per arr (see
  [Download clients](#download-clients)).
- **Stop the old apps** before the new ones start. Two Radarrs on one library
  both grab and import, and their ports clash.

## 1. Put the old config in place

Do this **before** the first `./scripts/init.sh` (or before re-running it for
an app you're adding). `init.sh` only creates a config for an app that has
none, so it keeps yours and only adds the Postgres settings.

| App | Copy into | What |
| --- | --- | --- |
| Radarr, Sonarr, Lidarr, Readarr, Whisparr, Prowlarr | `appdata/<app>/` | `config.xml` and `<app>.db` (e.g. `radarr.db`) |
| Bazarr | `appdata/bazarr/` | `config/config.yaml` and `db/bazarr.db`, keeping the folders |
| Seerr (Overseerr, Jellyseerr) | `appdata/seerr/` | `settings.json` and `db/db.sqlite3` |

Next to a database there may be a `-wal` and a `-shm` file (`radarr.db-wal`,
`radarr.db-shm`). **Copy them too.** SQLite keeps its latest writes in the
`-wal` file until it folds them into the database. Without it you'd copy an
older state, or for Seerr, which leaves almost everything there, a nearly
empty one.

`appdata` is `APPDATA` in `.env` (`./appdata` by default). Where the old
files are:

| Old install | Folder |
| --- | --- |
| Docker | whatever was mounted at `/config` (Seerr: `/app/config`) |
| Windows | `C:\ProgramData\Radarr` (Sonarr, Lidarr, Prowlarr the same) |
| Linux package or script | `/var/lib/radarr`, or `~/.config/Radarr` |
| macOS | `~/.config/Radarr` |
| Bazarr outside Docker | the `data` folder next to Bazarr |

Only have the backup zip? Unzip it into `appdata/<app>/`: it holds
`config.xml` and, on SQLite, `<app>.db`. The [setup wizard](Setup.md)'s
**Existing backups** step finds a zip under `appdata/<app>/Backups/` and copies
its `config.xml` for you; the `.db` you unzip yourself.

The rest of the old folder (`logs`, `Backups`) isn't needed, with one exception:
for an arr, copy `MediaCover` too, or most of its posters will be missing for
weeks ([Posters](#posters)).

**Check four settings** in an arr's `config.xml`, because the stack expects
its defaults:

| Element | Must be | Why |
| --- | --- | --- |
| `<Port>` | `7878` Radarr, `8989` Sonarr, `8686` Lidarr, `9696` Prowlarr | Docker publishes that port inside the container |
| `<UrlBase>` | empty | wranglarr and the other apps call the root address |
| `<EnableSsl>` | `False` | TLS belongs to your [reverse proxy](Reverse-Proxy.md) |
| `PostgresMainDbConnectionString`, `PostgresLogDbConnectionString` | absent | `init.sh` writes `PostgresHost`, `PostgresUser` and the rest; it leaves a config with connection strings alone, and the app won't start with both |

Keep `<ApiKey>`: wranglarr reads it from the file, and anything else that
used the old key keeps working.

Then run `./scripts/init.sh` as in [Setup](Setup.md). It adds the Postgres
settings and starts everything. Each app starts **empty** on Postgres: it
creates its tables, and wranglarr wires it to the others. The next step
replaces that empty database with your old one.

## 2. Copy the database

Once per app, after `init.sh` has started it once. From the stack directory:

```sh
APP=radarr        # or sonarr, lidarr, readarr, whisparr, prowlarr
PGU=$(docker compose exec -T postgres printenv POSTGRES_USER)
PGP=$(docker compose exec -T postgres printenv POSTGRES_PASSWORD)

docker compose stop $APP
docker compose exec -T postgres psql -q -U "$PGU" -d "$APP" < postgres/migrate/empty-tables.sql
docker run --rm --network arrs -v "$PWD/appdata/$APP:/old" ghcr.io/roxedus/pgloader \
  --with "quote identifiers" --with "data only" /old/$APP.db "postgresql://$PGU:$PGP@postgres:5432/$APP"
docker compose exec -T postgres psql -q -U "$PGU" -d "$APP" < postgres/migrate/reset-sequences.sql
docker compose start $APP
```

What each line does:

1. **Stop** the app, so nothing writes while the rows move.
2. **Empty its tables.** [`empty-tables.sql`](../postgres/migrate/empty-tables.sql)
   clears the defaults the new install started with. The old database brings
   its own, and loading on top of them would make duplicates.
3. **Copy the rows** with [pgloader](https://github.com/dimitri/pgloader), in
   the build the Servarr wiki recommends (the one in Debian and Ubuntu is too
   old for Postgres 17). `quote identifiers` keeps the tables' capital
   letters; `data only` leaves the tables the app made alone. It runs on the
   stack's network (`arrs`), so it reaches Postgres as `postgres`. It gets the
   whole folder, so SQLite can read the `-wal` file too.
4. **Reset the id counters.** [`reset-sequences.sql`](../postgres/migrate/reset-sequences.sql)
   moves each one past the highest id copied in. Without it, the first film
   you add fails with *duplicate key value violates unique constraint*.
5. **Start** the app with your old library.

pgloader prints a `WARNING` per column whose type differs between SQLite and
Postgres (`bigint` and `integer`, say). Those are expected. What matters is
the table it ends with: the **errors** column should be `0` on every row, and
the last line is `Total import time ✓`.

The log database (`logs.db`) isn't copied: it's the app's own log, and it
starts again.

**Podman**: use `podman` and `podman compose`; the network is still `arrs`.
**TrueNAS** and other setups that need `sudo docker`: run the lines with `sudo`.
**`APPDATA` elsewhere**: replace `$PWD/appdata` with it.

When it's running on your data, move the old files out of the way so nobody
mistakes them for the live ones: `mkdir appdata/radarr/sqlite-old && mv appdata/radarr/radarr.db* appdata/radarr/sqlite-old/`.

### Bazarr

The same steps, with Bazarr's database name and four extra options for its
dates (from [Bazarr's guide](https://wiki.bazarr.media/Additional-Configuration/PostgreSQL-Database/)).
Bazarr needs to be 1.1.5 or newer on the old side.

```sh
docker compose stop bazarr
docker compose exec -T postgres psql -q -U "$PGU" -d bazarr < postgres/migrate/empty-tables.sql
docker run --rm --network arrs -v "$PWD/appdata/bazarr/db:/old" ghcr.io/roxedus/pgloader \
  --with "quote identifiers" --with "data only" \
  --cast "column table_blacklist.timestamp to timestamp" --cast "column table_blacklist_movie.timestamp to timestamp" \
  --cast "column table_history.timestamp to timestamp" --cast "column table_history_movie.timestamp to timestamp" \
  /old/bazarr.db "postgresql://$PGU:$PGP@postgres:5432/bazarr"
docker compose exec -T postgres psql -q -U "$PGU" -d bazarr < postgres/migrate/reset-sequences.sql
docker compose start bazarr
```

### Seerr

Seerr keeps two lists of its own database changes, one for SQLite and one for
Postgres, in a table called `migrations`. The Postgres list it wrote on its
first start has to stay, and the old SQLite one must not be copied over it.
[`empty-tables.sql`](../postgres/migrate/empty-tables.sql) leaves that table
alone, and [`seerr.load`](../postgres/migrate/seerr.load) tells pgloader to
skip it. The pgloader build is the one [Seerr's guide](https://docs.seerr.dev/extending-seerr/database-config)
names:

```sh
docker compose stop seerr
docker compose exec -T postgres psql -q -U "$PGU" -d seerr < postgres/migrate/empty-tables.sql
docker run --rm --network arrs -e PGU="$PGU" -e PGP="$PGP" \
  -v "$PWD/appdata/seerr/db:/old" -v "$PWD/postgres/migrate/seerr.load:/seerr.load:ro" \
  ghcr.io/ralgar/pgloader:pr-1531 pgloader /seerr.load
docker compose exec -T postgres psql -q -U "$PGU" -d seerr < postgres/migrate/reset-sequences.sql
docker compose start seerr
```

**Coming from Overseerr or Jellyseerr**: Seerr upgrades their data the first
time it starts on it ([Seerr's migration guide](https://docs.seerr.dev/migration-guide)),
but here it starts on Postgres and never sees the old file. Upgrade the file
first, on SQLite, with the same Seerr version the stack runs:

```sh
docker run --rm -d --name seerr-upgrade --init -v "$PWD/appdata/seerr:/app/config" ghcr.io/seerr-team/seerr:latest
docker logs -f seerr-upgrade      # wait for "Server ready on port 5055", then Ctrl-C
docker stop seerr-upgrade
```

Then run the Seerr steps above.

### Already on Postgres

Then there's nothing to convert: dump each database on the old server and
restore it, whole, into the stack's Postgres. The new app can be **newer** than
the old one: when it starts it upgrades the database, as any update would. A
Radarr 4.7 database restored under Radarr 6 comes up with its library, after
running the migrations in between. No pgloader, and the id counters come
with the dump.

**Keep the old Postgres server?** No: the stack runs its own. wranglarr's
backups dump the databases there, its restores recreate them there, and
Bazarr and Seerr are wired to it. Your old server can go once the apps run
here.

**1. Dump** each app's main database on the old server, with the app
stopped. The names are in the old `config.xml` (`PostgresMainDb`: often
`radarr-main`, as in the Servarr wiki, or just `radarr`); Bazarr's in its `config.yaml` under `postgresql`,
Jellyseerr's in `DB_NAME`.

```sh
# Postgres installed on the old machine
pg_dump -Fc -h localhost -U <old user> <main db> > radarr.dump
# Postgres in a container there
docker exec <old postgres container> pg_dump -Fc -U <old user> <main db> > radarr.dump
```

The log database (`PostgresLogDb`) can stay behind: the new app starts a new one.

**2. Config**, as in [step 1](#1-put-the-old-config-in-place), then `init.sh`.
The old `config.xml` still points at the old server; `init.sh` overwrites
`PostgresHost`, `PostgresUser`, `PostgresPassword` and the database names
with the stack's. Remove `PostgresMainDbConnectionString` and
`PostgresLogDbConnectionString` if the old one used them.

**3. Restore** into the stack, from the stack directory. The app's databases
are made empty again first, because `init.sh` already started the app on
them:

```sh
APP=radarr        # or sonarr, lidarr, readarr, whisparr, prowlarr
PGU=$(docker compose exec -T postgres printenv POSTGRES_USER)

docker compose stop $APP
docker compose exec -T postgres psql -q -U "$PGU" -d postgres \
  -c "DROP DATABASE \"$APP\"" -c "CREATE DATABASE \"$APP\"" \
  -c "DROP DATABASE \"$APP-log\"" -c "CREATE DATABASE \"$APP-log\""
docker compose exec -T postgres pg_restore -U "$PGU" -d "$APP" --no-owner --no-privileges < $APP.dump
docker compose start $APP
```

`--no-owner --no-privileges` makes the stack's user the owner of everything,
whatever the old server's user was called. Watch the first start with
`docker compose logs -f $APP`: after an upgrade it lists each migration it
runs, then *Application started*.

Bazarr and Seerr have one database each, `bazarr` and `seerr`:

```sh
docker compose stop bazarr
docker compose exec -T postgres psql -q -U "$PGU" -d postgres -c 'DROP DATABASE bazarr' -c 'CREATE DATABASE bazarr'
docker compose exec -T postgres pg_restore -U "$PGU" -d bazarr --no-owner --no-privileges < bazarr.dump
docker compose start bazarr
```

The same for `seerr`, from a Jellyseerr or Seerr database: Seerr upgrades a
Jellyseerr one on its first start ([Seerr's migration guide](https://docs.seerr.dev/migration-guide)).

**Both on one machine?** Skip the file and pipe one into the other:

```sh
docker exec <old postgres container> pg_dump -Fc -U <old user> <main db> \
  | docker compose exec -T postgres pg_restore -U "$PGU" -d radarr --no-owner --no-privileges
```

**Other Postgres versions.** The old server can run any version: a dump from
Postgres 16 or 18 restores into the stack's 17. If `pg_restore` ever says
*unsupported version in file header*, dump as plain SQL instead
(`pg_dump -Fp ... > radarr.sql`) and load it with
`docker compose exec -T postgres psql -U "$PGU" -d radarr < radarr.sql`
into the emptied database.

## 3. Paths

Every app in the stack sees your data at `/data` (`DATA_ROOT` in `.env`):

```
/data/media/movies    Radarr's root folder
/data/media/tv        Sonarr's
/data/media/music     Lidarr's
/data/torrents/<arr>  qBittorrent's downloads, one folder per arr
/data/usenet/...      SABnzbd's and NZBGet's
```

The old install probably used other paths: `/movies`, `/tv`, `/downloads`, or
`D:\Movies` on Windows. Its database still has them, so after the copy every
film points at a folder that doesn't exist (a *missing root folder* warning,
on the [Dashboard](Dashboard.md) too).

**Move the library under `DATA_ROOT`** if it isn't there yet. On the same
disk that's a rename, not a copy. Then, in each arr:

1. Settings → Media Management → **Add Root Folder**: `/data/media/movies`.
2. Movies (Sonarr: Series, Lidarr: Artists) → **Select** → select all →
   **Edit** → Root Folder: the new one → **Apply**.
3. Asked whether to move the files: **No, I'll move the files myself**. They
   are already there, under the new name.
4. Remove the old root folder.

Downloads and the library on one filesystem is what lets the arrs
**hardlink** instead of copying: an import takes no time and no extra space,
and the torrent keeps seeding. So keep them on the same disk or dataset
(TrueNAS: [Datasets](TrueNAS.md#1-datasets)).

Can't move the files? Mount the old folder where the old app had it, in a
`compose.override.yaml` next to `compose.yaml`:

```yaml
services:
  radarr:
    volumes:
      - /mnt/old/movies:/movies
```

That works with no other change. Imports into it are copies, though, since
it's another mount.

**Remote path mappings** (Settings → Download Clients) aren't needed in the
stack: every app sees the downloads at the same `/data` path. Delete them.

## 4. Connections

The old database also brought the old connections, pointing where the apps
used to be: a download client at `192.168.1.5:8080`, Prowlarr's apps at
`http://localhost:7878`. Open wranglarr → **System → App connections**:

- A connection wranglarr recognises (same kind, and named like its own:
  *qBittorrent*, *SABnzbd*, *NZBGet*, *Radarr*...) shows as **differs:
  points at ...**. Press **Connect apps**: it sets the host, port, login and
  category, and keeps the rest of the entry.
- An old connection under another name stays as it is, and wranglarr adds
  its own next to it. Delete the old one in the app.

Then:

- **Prowlarr**: once its apps point at the new addresses, Settings → Apps →
  **Sync App Indexers** puts the indexers back into each arr.
- **Bazarr** and **Seerr** are covered by Connect apps too (their Sonarr
  and Radarr). In Bazarr, delete the old path mappings: the paths are the
  same everywhere now.
- **Plex, Jellyfin, Emby, Kodi** connections in the arrs (Settings →
  Connect) point at the old address too. Fix them there, or add the server on
  the [Connections](Connections.md) page.
- **Logins**: the old database brings the old app logins. Run
  `./scripts/init.sh --auth` to set the stack's again ([App logins](App-Logins.md)).

### Download clients

wranglarr gives each arr its own category in each client, named after the
arr (`radarr`), downloading into `/data/torrents/radarr`. Connect apps sets
that category on the arr's entry too. A download that was still running
under the old category (`movies`, say) isn't seen by the arr anymore: it
won't be imported. So let the queue finish in the old client first, or
import the stragglers by hand (Wanted → Manual Import).

The clients themselves are quicker to set up again than to move:

- **qBittorrent**: to keep seeding the old torrents, copy the old
  `qBittorrent/BT_backup` folder and `qBittorrent.conf` into
  `appdata/qbittorrent/qBittorrent/` before `init.sh`. The old file keeps
  its old download folder, which doesn't exist in the stack: set Options →
  Downloads → **Default Save Path** to `/data/torrents/`. Then select the
  torrents → **Set location** → their folder under `/data/torrents`, and
  qBittorrent rechecks them.
- **SABnzbd, NZBGet**: add your usenet servers again. An old config file
  brings its old folders with it, and `init.sh` only sets folders that are
  still at the image's default.

## 5. Check, then back up

In each app:

- System → Status: **Database** says PostgreSQL, and the version is the one
  you expected.
- The library, Activity → History, and your profiles are there.
- System → Health is empty, or only lists things you're still fixing.
- The library shows posters, not grey placeholders ([Posters](#posters)).

Then in wranglarr, **Suites → Back up now** for each suite. That first backup
is the one to come back to if anything goes wrong later.

## Posters

Radarr, Sonarr and Lidarr (Readarr and Whisparr too) keep the posters,
fanart and banners they show in `appdata/<app>/MediaCover/`: one folder per
film, series or artist, named by its id in the database (Lidarr adds
`Albums/`, one folder per album). The images aren't in the database, and
wranglarr's [backups](Backups-and-Restore.md) leave them out.

An arr fetches a missing image only when it refreshes that item. It
refreshes an item that's still airing or coming out often. A film that's out,
or a series that has ended, can wait weeks. So a library copied in without
`MediaCover` shows grey placeholders for most of it until then.

### Copy them across

The ids come with the database ([step 2](#2-copy-the-database), or
[Already on Postgres](#already-on-postgres)), so the old folder fits the new
library as it is. From the stack directory, once per arr, at any time (the
app can keep running):

```sh
APP=radarr        # or sonarr, lidarr, readarr, whisparr
OLD=/path/to/old/config   # the old install's config folder (step 1)
sudo cp -a --no-clobber "$OLD/MediaCover/." "appdata/$APP/MediaCover/"
sudo chown -R "$(grep ^PUID= .env | cut -d= -f2):$(grep ^PGID= .env | cut -d= -f2)" "appdata/$APP/MediaCover"
```

- `--no-clobber` keeps the images the new app has fetched already, which are
  the newer ones.
- `chown` gives the files to the app's user (`PUID`/`PGID`), so it can
  replace them on its next refresh.
- Reload the page in the browser.

It's a few hundred MB for a few hundred series, and a few GB for a large
film library. Put `appdata` on a disk with room for it.

Only after a database copy: an app [started fresh](#start-fresh-and-import-the-library)
numbers its items anew, and the old folders would land on the wrong items.
Library Import fetches the posters itself.

### Check and refill

Compare the folder with the library:

```sh
ls appdata/radarr/MediaCover | wc -l     # about the number of films
ls appdata/sonarr/MediaCover | wc -l     # about the number of series
ls appdata/lidarr/MediaCover | wc -l     # about the number of artists, plus Albums
```

Far fewer? Either:

- **Copy from the old install** while you still have it, as above. It's
  quick, and nothing is asked of the metadata servers.
- **Have the app fetch them**: Movies (Sonarr: Series, Lidarr: Artists) →
  **Update all**. That refreshes every item's metadata, not only the
  images, so on a big library it runs for a while (System → Tasks shows it).
  For a single item: open it → **Refresh**.

The same holds after restoring a [backup](Backups-and-Restore.md) onto a new
machine: copy `MediaCover` across from the old one too.

## Start fresh and import the library

When the old install isn't worth keeping (very old, broken, or you just want
a clean slate), skip the database:

1. Leave `appdata/<app>/` empty and run `init.sh`: the app starts new.
2. Settings → Media Management → add `/data/media/movies` as a root folder.
3. Radarr: Movies → **Library Import**; Sonarr: Series → **Library Import**.
   Pick the folder, check the matches, import. Lidarr scans a root folder
   when you add it; Library → **Unmapped Files** lists what it couldn't
   match.

You keep the files and lose the history, custom formats and profiles. For
the profiles, [Recyclarr](https://recyclarr.dev) or the
[TRaSH Guides](https://trash-guides.info) set them up again quickly.

## The old app can't be updated

The versions must match, so run the **old** version in the stack for the copy.
Pin its image in `.env` to the old version's tag **before** `init.sh`, for
example:

```
RADARR_IMAGE=lscr.io/linuxserver/radarr:5.14.0
```

linuxserver.io tags each version (`5.14.0`, and `5.14.0.9383-ls245` for an
exact build); they're listed on each image's page
([Radarr's](https://hub.docker.com/r/linuxserver/radarr/tags)). Then run
`init.sh`, copy the database as above and check it. Finally remove the line
from `.env` and run `docker compose up -d radarr`: the newer version upgrades the
database itself when it starts, as any update would.

Already ran `init.sh` with the newer version? Its tables are the new
version's. Make the databases again so the pinned version creates its own:

```sh
docker compose stop radarr
docker compose exec -T postgres psql -U "$PGU" -d postgres \
  -c 'DROP DATABASE "radarr"' -c 'CREATE DATABASE "radarr"' \
  -c 'DROP DATABASE "radarr-log"' -c 'CREATE DATABASE "radarr-log"'
docker compose up -d radarr      # the pinned version creates its tables; give it a minute
```

Then copy the database (step 2).

## When something goes wrong

| What you see | Why | What to do |
| --- | --- | --- |
| pgloader: `duplicate key value violates unique constraint` | the tables weren't emptied | stop the app, run the five lines again from the top |
| pg_restore: `relation ... already exists` | the database wasn't empty: `init.sh` had started the app on it | run the restore lines again: they drop and recreate the databases first |
| The app won't start after a restore: its database is newer than the app | the old app was newer than the stack's image | let the stack catch up (`docker compose pull $APP`), or [pin](#the-old-app-cant-be-updated) the old version's tag |
| The copy worked but recent changes are missing, or Seerr is nearly empty | the `-wal` file was left behind | copy `<db>-wal` and `<db>-shm` next to the database, run step 2 again |
| pgloader: `column ... does not exist`, or the app fails to start after the copy | the old and new versions differ | [match the versions](#the-old-app-cant-be-updated) and let that version make its tables, then step 2 again |
| `duplicate key value` when you add something in the app | the id counters weren't reset | run `reset-sequences.sql` again |
| pgloader: `could not translate host name "postgres"` | it isn't on the stack's network | check the name with `docker network ls` (it's `arrs`), and that Postgres is running |
| pgloader stops with `Heap exhausted` on a big database | pgloader's memory | add `--with "prefetch rows = 100" --with "batch size = 1MB"` |
| password authentication failed | the password has characters a URL can't carry (`@ : / ? #`) | write them URL-encoded (`@` is `%40`) in the `postgresql://` line |
| The app starts empty | it's reading another database | System → Status shows PostgreSQL? If not, `config.xml` lacks the `Postgres*` lines: run `init.sh` again |
| *Missing root folder* warnings | the old paths | [Paths](#3-paths) |
| Grey placeholders instead of posters for much of the library | `MediaCover` wasn't copied, and the arr only fetches an image when it refreshes that item | [Posters](#posters) |
| Old connections still in the apps | they came with the database | [Connections](#4-connections) |

If it won't come right, nothing is lost: the old install and its backup zip
are untouched, and step 2 can run as often as you like (it empties the tables
first).
