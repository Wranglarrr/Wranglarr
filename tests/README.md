# Black-box tests

These tests start a wranglarr server as a separate process, point it at mock Docker,
mock arrs, mock download clients and fake Postgres tools, and talk to it only over HTTP
(plus reading the files it writes: the backup store, `.env`, `wranglarr.toml`, its state
database, its log). Nothing imports `wranglarr`, so the suite is the behaviour
contract: a change that breaks it changes what users see.

No containers, no network beyond 127.0.0.1: every process listens on a free high port
(Docker on a unix socket) under pytest's temp folder, and is stopped at the end.

## Running

From the repo root: build wranglarr, then run pytest from any Python 3.11+ with
[requirements.txt](requirements.txt) (pytest, httpx):

```sh
cargo build --manifest-path wranglarr/Cargo.toml
python3 -m venv tests/.venv && tests/.venv/bin/pip install -r tests/requirements.txt   # once
tests/.venv/bin/python -m pytest tests          # or: tests/.venv/bin/python tests/run.py
tests/.venv/bin/python -m pytest tests -k gate  # one area
WRANGLARR_TEST_SLOW=1 tests/.venv/bin/python -m pytest tests   # + the cron-driven auto-update test (~90 s)
```

pytest prints the commands under test in its header and a pass/fail summary at the end;
a failing test also shows the tail of the server's and the mocks' logs.

### Against another binary

```sh
WRANGLARR_CMD="/path/to/wranglarr serve" WRANGLARR_SETUP_CMD="/path/to/wranglarr setup" \
  tests/.venv/bin/python -m pytest tests
```

Both are shell-style command lines (default: `wranglarr/target/debug/wranglarr serve` and
`… setup`).

## What the server under test must do

- **wranglarr** reads its config from `$WRANGLARR_CONFIG`, listens on `[server] bind`/`port`,
  answers `GET /api/health` with 200 once it's ready, and stops on SIGTERM. Its output
  (stdout + stderr) goes to a log file; the auth tests read the one-time setup code from
  it (the last `XXXX-XXXX-XXXX`, upper-case hex, after startup or `GET /setup`).
- A sign-in lockout lives in memory: the auth test clears it by restarting the server
  (same state database, same port).
- Postgres is only ever reached through `pg_dump`, `pg_dumpall`, `pg_restore`, `psql`,
  `pg_isready` from `PATH`; the harness puts [bin/](bin/) first.
- **wranglarr-setup** takes `--target DIR --bind 127.0.0.1 --port N`, needs
  `DIR/compose.yaml`, takes its token from `$WRANGLARR_SETUP_TOKEN`, and serves
  `/static/style.css` without the token (the harness waits on that).
- Pages: a few tests look for text in the HTML (`class="badge"`, `update available`,
  `update blocked`, `Try again`, `data-hold="radarr" data-on="1"`, `Docker control is off`,
  the setup page's `?t=`); the templates carry over, so these should too.

## Environment

What the harness sets for the server (everything else from your shell is passed through,
minus `WRANGLARR_*`, `APPS_*`, `AUTH_*`, `COMPOSE_*`, `HOSTNAME`, `PUID`/`PGID`/`UMASK`, `TZ`
and a few others that would change behaviour):

| Variable | Value | Why |
| --- | --- | --- |
| `WRANGLARR_CONFIG` | the generated `wranglarr.toml` | the config |
| `WRANGLARR_AUTH` | `off`, unless the test needs the login | most tests drive the API directly |
| `WRANGLARR_PRE_RESTORE` | `<tmp>/pre-restore` | where a restore sets aside what it replaced when the folder beside the config isn't writable (default `/config/pre-restore`) |
| `WRANGLARR_UPDATE_POLL` | `0.1` | **test-only knob**: seconds between health polls while a recreated container comes up (default 2) |
| `WRANGLARR_UPDATE_STABLE` | `0.5` | **test-only knob**: how long a container without a healthcheck must keep running to count as up (default 15) |
| `PATH` | `tests/bin` first | the fake Postgres tools |
| `FAKE_PG_LOG` | `<tmp>/pg.log` | where the fake tools log their calls (read by `test_snapshots.py`) |
| `NO_COLOR`, `TZ` | `1`, `UTC` | plain log lines; one time zone |
| `APPS_AUTH`, `AUTH_COOKIE_DOMAIN`, `AUTH_LOGIN_URL` | per test | AuthTunnel (`test_gate.py`, `test_proxy.py`, `test_standalone.py`) |
| `WRANGLARR_SETUP_TOKEN` | `testtoken` / per test | the wizard's one-time token |

The two **test-only knobs** exist for these tests (in `wranglarr/src/updates.rs`);
unset, nothing changes. Without them the update tests take minutes.

## Files

| File | Covers |
| --- | --- |
| [harness.py](harness.py) | starting/stopping processes, free ports, the `Server` / `SetupServer` wrappers, mock clients, `wait_run`, TOTP, crafting versions on disk |
| [conftest.py](conftest.py), [pytest.ini](pytest.ini), [run.py](run.py) | pytest glue: header, logs on failure, the one-command runner |
| [test_updates.py](test_updates.py) | update check, update with settings carried over (env, binds, labels, networks, anonymous volumes), Postgres major bump refused + blocked, VPN update recreating its namespace dependents, crashing image rolled back, manual rollback + hold, settings + scheduled jobs (`next`), pages, a dependent that can't be recreated, user env equal to the image default kept, Docker off |
| [test_updates_auto.py](test_updates_auto.py) | (slow, opt-in) the auto-update cron: held and blocked skipped, pre-update backup, nothing on the second pass |
| [test_update_tracks.py](test_update_tracks.py) | per-container release tracks, tag validation, config/UI precedence, persistence, check/apply, wranglarr's development track, and preserving held apps during VPN recreation |
| [test_auth.py](test_auth.py) | first-run setup code, CSRF/Origin guard, cookie flags, lockout, TOTP + replay, recovery codes, API keys (only the hash stored), sessions, passkeys refused on an IP / plain http, password change |
| [test_gate.py](test_gate.py) | AuthTunnel: domain cookie, `/api/auth/gate` (session, the apps' own API keys), login `next` handling, logout |
| [test_proxy.py](test_proxy.py) | reverse proxy config for every kind × style, with and without AuthTunnel; validation |
| [test_smoke.py](test_smoke.py) | every page and read API answers 200 with all apps down |
| [test_feeds.py](test_feeds.py) | home page lists: missing/upcoming (Sonarr, Radarr, Lidarr), Bazarr wanted, Prowlarr indexers, home settings |
| [test_restore_safety.py](test_restore_safety.py) | failed stops, complete restore selection, interrupted file recovery, and durable restore-plan progress |
| [test_snapshots.py](test_snapshots.py) | config snapshots (what's left out, `config.sha256`), SQLite-mode overlay, restoring a download client's folder, tampering caught, a non-folder snapshot refused, unreadable files reported, a restore failing before the stop |
| [test_prune.py](test_prune.py) | retention on crafted versions: failed ones don't count, keep_days, unfinished folders, non-version names |
| [test_standalone.py](test_standalone.py) | the standalone restore stack in each version, tunnel vs apps mode |
| [test_api_edges.py](test_api_edges.py) | edges of the API: CSRF with login off, `{"error"}` shapes, partial PUTs, hidden versions, versions locked during a run, notification channels, indexer sources, history ids |
| [test_community.py](test_community.py) | community config check/apply, partial undo keeps what failed |
| [test_setup_wizard.py](test_setup_wizard.py) | wizard token, secrets masked and restored, checks before `.env` is written, legacy config from a zip |
| [mocks/mock_docker.py](mocks/mock_docker.py) | Docker Engine API on a unix socket; seeds `updates` and `standalone`; `/_mock/state`, `/_mock/registry`, `/_mock/fail_create` |
| [mocks/mock_arr.py](mocks/mock_arr.py) | an arr: status, backups, restore, download clients, a small REST store; `/_mock/state`, `/_mock/seed`, `/_mock/fault` (errors and delays) |
| [mocks/mock_dl.py](mocks/mock_dl.py) | qBittorrent, SABnzbd, NZBGet and gluetun's control server in one process |
| [mocks/mock_feeds.py](mocks/mock_feeds.py) | Sonarr, Radarr, Lidarr, Bazarr, Prowlarr with home-page data relative to now |
| [bin/](bin/) | fake `pg_dump`, `pg_dumpall`, `pg_restore`, `psql`, `pg_isready` |

Tests in one file share a server and run in order (`test_updates.py`, `test_auth.py`,
`test_snapshots.py` build on their earlier steps), so run a whole file rather than a
single step of those.
