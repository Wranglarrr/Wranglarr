"""Automatic updates on their cron schedule: held and blocked containers are skipped, the
pre-update backup runs, and a second pass with nothing left queues nothing.

Slow (it waits for two minute boundaries), so it runs only with WRANGLARR_TEST_SLOW=1."""
import os
import time

import pytest

from harness import wait_run, wait_until
from test_updates import build

pytestmark = pytest.mark.skipif(os.environ.get("WRANGLARR_TEST_SLOW") != "1", reason="slow: set WRANGLARR_TEST_SLOW=1")


@pytest.fixture(scope="module")
def env(tmp_path_factory):
    e = build(tmp_path_factory.mktemp("updates-auto"))
    yield e
    e.c.close()
    for p in e.procs:
        p.stop()


def update_runs(c):
    return [r for r in c.get("/api/runs", params={"suite": "system", "limit": 50}).json() if r["kind"] == "update"]


def test_auto_update(env):
    c = env.c
    env.dock.registry(ref="lscr.io/linuxserver/radarr:develop", digest="sha256:" + "f4" * 32, version="7.0-dev")
    assert c.put("/api/updates/tracks/radarr", json={"tag": "develop"}).status_code == 200
    assert c.post("/api/updates/hold", json={"container": "qbittorrent"}).status_code == 200
    r = c.put("/api/updates/settings", json={"auto_update": True, "auto_schedule": "* * * * *", "check_schedule": ""})
    assert r.status_code == 200, r.text
    assert c.get("/api/updates").json()["next"]["auto"]
    run = wait_until(lambda: (update_runs(c) or [None])[0], 75, 1, what="the scheduled update run")
    r = wait_run(c, run["id"])
    assert r["result"]["results"] == {"arrs-postgres": "refused", "radarr": "updated"}, (r["result"], r["log"])
    assert "pre-update" in r["log"]
    assert env.dock.containers()["radarr"]["Config"]["Image"] == "lscr.io/linuxserver/radarr:develop"
    # next minute: postgres blocked, qbittorrent held, radarr current -> nothing queued
    time.sleep(62 - time.time() % 60 + 5)
    assert len(update_runs(c)) == 1, update_runs(c)
