"""Edges of the API: CSRF with the login off, errors in the {"error"}
shape, partial PUTs that keep the rest, hidden/missing versions, versions locked while a
run uses the suite, notification channels, indexer sources, community history ids."""
import pytest

from harness import ARR_KEY, MockArr, Server, base_toml, suite_toml, wait_until


@pytest.fixture(scope="module")
def env(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("api-edges")
    slow = MockArr(tmp, "mock-sonarr-slow", "--empty")
    toml = base_toml(tmp) \
        + suite_toml("media", [dict(name="radarr", kind="radarr", url="http://127.0.0.1:1", api_key="x")]) \
        + suite_toml("other", [dict(name="sonarr", kind="sonarr", url=slow.url, api_key=ARR_KEY)])
    srv = Server(tmp, toml).start()
    c = srv.client()
    yield type("Env", (), dict(tmp=tmp, srv=srv, c=c, slow=slow, store=tmp / "store"))
    c.close()
    srv.stop()
    slow.stop()


def test_csrf_with_login_off(env):
    c = env.c
    r = c.post("/api/suites/media/prune", headers={"Origin": "https://evil.example"})
    assert r.status_code == 403 and "error" in r.json(), r.text
    r = c.post("/api/suites/media/prune", headers={"Referer": "https://evil.example/page"})
    assert r.status_code == 403, "a cross-site Referer counts too"
    assert c.post("/api/suites/media/prune").status_code == 200, "a script (no Origin) is fine"
    assert c.post("/api/suites/media/prune", headers=env.srv.origin).status_code == 200, "same origin is fine"


def test_error_shapes(env):
    c = env.c
    r = c.get("/api/nope")
    assert r.status_code == 404 and "error" in r.json(), r.text
    r = c.delete("/api/suites")
    assert r.status_code == 405 and "error" in r.json(), r.text
    r = c.get("/no-such-page")
    assert r.status_code == 404 and "<html" in r.text.lower()
    r = c.put("/api/suites/media/settings", json={"keep_last": "lots"})
    assert r.status_code == 422 and "keep_last" in r.json().get("error", ""), r.text
    assert c.get("/api/suites/nope").status_code == 404


def test_partial_puts_keep_the_rest(env):
    c = env.c
    c.put("/api/suites/media/settings", json={"schedule": "15 4 * * *", "enabled": True, "keep_last": 9, "keep_days": 0})
    j = c.put("/api/suites/media/settings", json={"keep_last": 5}).json()
    assert j["schedule"] == "15 4 * * *" and j["keep_last"] == 5, j
    c.put("/api/updates/settings", json={"check_schedule": "0 5 * * *", "auto_update": False, "auto_schedule": "",
                                         "backup_first": True})
    j = c.put("/api/updates/settings", json={"auto_update": True}).json()
    assert j["check_schedule"] == "0 5 * * *" and j["auto_update"] is True, j
    r = c.put("/api/proxy", json={"kind": "caddy", "domain": "example.com"})
    assert r.status_code == 200, r.text[:200]
    j = c.put("/api/proxy", json={"lan": "10.0.0.0/8"}).json()
    assert j["settings"]["domain"] == "example.com" and j["settings"]["lan"] == "10.0.0.0/8", str(j)[:200]
    assert c.put("/api/proxy", json={"apps": ["radarr"]}).status_code == 400, "wrong type"
    assert c.put("/api/proxy", json={"domain": 5}).status_code == 400, "domain as a number"


def test_versions_that_dont_exist(env):
    c = env.c
    assert c.get("/api/suites/media/versions/.hidden").status_code == 404
    assert c.get("/suites/media/versions/.hidden").status_code == 404
    assert c.post("/api/suites/media/versions/.hidden/restore", json={"app": "radarr"}).status_code == 404
    assert c.delete("/api/suites/media/versions/20200101T000000Z").status_code == 404
    assert c.post("/api/suites/media/versions/20200101T000000Z/restore", json={"app": "nope"}).status_code == 404


def test_versions_locked_while_a_run_uses_the_suite(env):
    c = env.c
    (env.store / "other" / "20200101T000000Z").mkdir(parents=True)
    env.slow.fault(method="GET", prefix="/api/v3/system/status", delay=4)     # the backup takes a while
    try:
        rid = c.post("/api/suites/other/backup").json()["run"]
        wait_until(lambda: c.get(f"/api/runs/{rid}").json()["status"] == "running", 10, what="the backup to start")
        r = c.post("/api/suites/other/prune")
        assert r.status_code == 409 and "error" in r.json(), r.text
        assert c.delete("/api/suites/other/versions/20200101T000000Z").status_code == 409
        assert c.post("/api/suites/other/backup").status_code == 409, "one run per suite"
        wait_until(lambda: c.get(f"/api/runs/{rid}").json()["status"] not in ("queued", "running"), 30,
                   what="the backup to finish")
    finally:
        env.slow.clear_faults()
    # the backup's own prune took the empty (incomplete) version away; free now, so a delete works
    (env.store / "other" / "20200101T000000Z").mkdir(parents=True, exist_ok=True)
    r = c.delete("/api/suites/other/versions/20200101T000000Z")
    assert r.status_code == 200 and r.json() == {"deleted": "20200101T000000Z"}, r.text


def test_notification_channels(env):
    c = env.c
    r = c.post("/api/notify/channels", json={"kind": "webhook", "name": "hook", "config": {"url": "http://127.0.0.1:1/x"},
                                             "events": []})
    assert r.status_code == 201, r.text[:200]
    cid = r.json()["channel"]["id"]
    r = c.post("/api/notify/test", json={"id": cid})
    j = r.json()
    assert r.status_code == 200 and "needed" not in (j.get("error") or ""), ("test by id uses the saved channel", j)
    assert c.post("/api/notify/test", json={"id": "nope"}).status_code == 404
    assert c.post("/api/notify/channels", json={"kind": "webhook", "config": ["x"]}).status_code == 400, "config as a list"
    assert c.post("/api/notify/channels", json={"kind": "webhook", "config": {"url": "http://a/x"},
                                                "arr": "radarr"}).status_code == 400, "arr as a string"
    assert any(ch["id"] == cid for ch in c.get("/api/notify").json()["channels"])
    assert c.delete(f"/api/notify/channels/{cid}").status_code == 200
    assert not any(ch["id"] == cid for ch in c.get("/api/notify").json()["channels"])


def test_indexer_sources(env):
    c = env.c
    r = c.post("/api/indexers/sources", json={"type": "torznab", "name": "t", "url": "http://127.0.0.1:1/api",
                                              "apply_to": [], "min_seeders": 0})
    assert r.status_code in (200, 202), r.text
    src = next((s for s in c.get("/api/indexers").json()["sources"] if s["name"] == "t"), {})
    assert src.get("min_seeders") == 0, ("min_seeders 0 kept", src)
    r = c.post("/api/indexers/sources", json={"type": "torznab", "name": "t2", "url": "http://127.0.0.1:1/api", "min_seeders": "x"})
    assert r.status_code == 422 and "min_seeders" in r.text, r.text[:150]


def test_community_history_unknown(env):
    assert env.c.delete("/api/configs/history/nope").status_code == 404
    assert env.c.post("/api/configs/history/nope/undo").status_code == 404
