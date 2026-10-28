"""Retention counts good versions only: POST /api/suites/{s}/prune on versions
crafted on disk.  Keep the newest keep_last good ones (and/or younger than keep_days),
always the newest good one; a failed / partial / unfinished version goes once a newer
good one exists."""
import pytest

from harness import Server, base_toml, suite_toml, write_manifest


@pytest.fixture(scope="module")
def srv(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("prune")
    app = dict(name="radarr", kind="radarr", url="http://127.0.0.1:1", api_key="x")
    toml = base_toml(tmp) + "".join(suite_toml(n, [dict(app, name=f"radarr-{n}")], keep_last=1, keep_days=0)
                                    for n in ("s", "f", "d", "n"))
    s = Server(tmp, toml).start()
    s.store = tmp / "store"
    yield s
    s.stop()


def craft(srv, suite, versions):
    for v, status in versions:
        write_manifest(srv.store, suite, v, {"status": status, "apps": {}})


def left(c, suite):
    return [m["version"] for m in c.get(f"/api/suites/{suite}/versions").json()]


def test_failed_versions_dont_count(srv):
    craft(srv, "s", [("20260101T000000Z", "ok"), ("20260102T000000Z", "failed"),
                     ("20260103T000000Z", "ok"), ("20260104T000000Z", "failed")])
    with srv.client() as c:
        gone = c.post("/api/suites/s/prune").json()["pruned"]
        assert left(c, "s") == ["20260104T000000Z", "20260103T000000Z"], "newest good + the newer failed one stay"
        assert sorted(gone) == ["20260101T000000Z", "20260102T000000Z"], gone
        assert not (srv.store / "s" / "20260101T000000Z").exists()


def test_failures_dont_push_out_the_only_good_version(srv):
    craft(srv, "f", [("20260101T000000Z", "ok"), ("20260201T000000Z", "failed"), ("20260202T000000Z", "failed"),
                     ("20260203T000000Z", "failed")])
    with srv.client() as c:
        assert c.post("/api/suites/f/prune").json()["pruned"] == []
        assert left(c, "f") == ["20260203T000000Z", "20260202T000000Z", "20260201T000000Z", "20260101T000000Z"]


def test_keep_days_and_unfinished(srv):
    """keep_days alone: old good versions go, the newest good one never does; a folder
    without a manifest (unfinished) older than a good version goes too."""
    craft(srv, "d", [("20200101T000000Z", "ok"), ("20200102T000000Z", "partial"), ("20200103T000000Z", "ok")])
    (srv.store / "d" / "20191231T000000Z").mkdir()           # no manifest: "incomplete"
    with srv.client() as c:
        assert c.put("/api/suites/d/settings", json={"keep_last": 0, "keep_days": 30}).status_code == 200
        gone = c.post("/api/suites/d/prune").json()["pruned"]
        assert sorted(gone) == ["20191231T000000Z", "20200101T000000Z", "20200102T000000Z"], gone
        assert left(c, "d") == ["20200103T000000Z"]


def test_names_that_arent_versions_are_left_alone(srv):
    craft(srv, "n", [("20260101T000000Z", "ok"), ("20260102T000000Z", "ok"), ("20260102T000000Z-1", "ok")])
    (srv.store / "n" / "my-notes").mkdir()
    with srv.client() as c:
        gone = c.post("/api/suites/n/prune").json()["pruned"]
        assert sorted(gone) == ["20260101T000000Z", "20260102T000000Z"], gone
        assert left(c, "n") == ["my-notes", "20260102T000000Z-1"]
