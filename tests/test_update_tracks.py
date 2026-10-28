"""Release tracks select registry tags, survive restarts, and preserve held VPN dependents."""
import pytest

from harness import wait_run
from test_updates import build, target


@pytest.fixture
def env(tmp_path):
    e = build(tmp_path)
    yield e
    e.c.close()
    for p in e.procs:
        p.stop()


def test_track_check_apply_persistence_and_reset(env):
    c, dock = env.c, env.dock
    ref = "lscr.io/linuxserver/radarr:develop"
    dock.registry(ref=ref, digest="sha256:" + "f1" * 32, version="7.0-dev")
    before = dock.containers()["radarr"]
    r = c.put("/api/updates/tracks/radarr", json={"tag": "develop"})
    assert r.status_code == 200, r.text
    assert target(c, "radarr")["status"] == "unchecked"
    assert target(c, "radarr")["track"]["image"] == ref
    assert dock.containers()["radarr"]["Id"] == before["Id"], "saving must not recreate"
    env.srv.restart()
    assert target(c, "radarr")["track"]["source"] == "ui"
    assert target(c, "radarr")["track"]["tag"] == "develop"
    c.post("/api/updates/check").raise_for_status()
    assert target(c, "radarr")["status"] == "available"
    run = c.post("/api/updates/apply", json={"containers": ["radarr"]}).json()["run"]
    result = wait_run(c, run)
    assert result["status"] == "ok", result["log"]
    assert "pre-update" in result["log"]
    assert dock.containers()["radarr"]["Config"]["Image"] == ref
    assert target(c, "radarr")["status"] == "current"
    assert c.delete("/api/updates/tracks/radarr").status_code == 200
    assert target(c, "radarr")["track"]["tag"] is None
    assert target(c, "radarr")["track"]["source"] == "container"
    # Reset follows the currently deployed track; it does not silently downgrade.
    assert target(c, "radarr")["track"]["image"] == ref


def test_config_track_blank_override_and_validation(env):
    c = env.c
    env.srv.stop()
    with env.srv.config.open("a") as f:
        f.write('\n[updates.tracks]\nradarr = "nightly"\n')
    env.srv.start()
    assert target(c, "radarr")["track"]["tag"] == "nightly"
    assert target(c, "radarr")["track"]["source"] == "config"
    assert c.put("/api/updates/tracks/radarr", json={"tag": None}).status_code == 200
    assert target(c, "radarr")["track"]["tag"] is None
    assert c.delete("/api/updates/tracks/radarr").status_code == 200
    assert target(c, "radarr")["track"]["tag"] == "nightly"
    for tag in ["../evil", "other/image:dev", "x@y", "-dev", "a b", "x" * 129]:
        r = c.put("/api/updates/tracks/radarr", json={"tag": tag})
        assert r.status_code == 400, (tag, r.text)
    assert c.put("/api/updates/tracks/radarr", json={"tag": []}).status_code == 422
    assert c.put("/api/updates/tracks/unknown", json={"tag": "dev"}).status_code == 404
    assert target(c, "radarr")["track"]["tag"] == "nightly", "invalid input must not change the selection"
    page = c.get("/updates")
    assert page.status_code == 200 and 'value="develop"' in page.text
    assert "Save track" in page.text


def test_vpn_recreation_keeps_held_app_image_and_reference(env):
    c, dock = env.c, env.dock
    c.post("/api/updates/hold", json={"container": "qbittorrent", "hold": True}).raise_for_status()
    before = dock.containers()["qbittorrent"]
    dock.registry(ref="qmcgaw/gluetun:v3.41.3", digest="sha256:" + "a9" * 32, version="vpn-new")
    # A newer application tag is present locally, independent of wranglarr.
    dock.http.post("/images/create", params={"fromImage": "lscr.io/linuxserver/qbittorrent", "tag": "latest"}).raise_for_status()
    run = c.post("/api/updates/apply", json={"containers": ["arrs-vpn"]}).json()["run"]
    result = wait_run(c, run)
    assert result["status"] == "ok", result["log"]
    after = dock.containers()["qbittorrent"]
    assert after["Id"] != before["Id"], "the network namespace must change"
    assert after["Image"] == before["Image"], "a namespace change must not upgrade a held app"
    row = target(c, "qbittorrent")
    assert row["held"]
    assert row["image"] == before["Config"]["Image"], "retain the tracking tag despite creation by image id"
    assert row["status"] == "available", "the next check still follows its named tag"


def test_wranglarr_development_track_is_checked_and_left_to_update_now(env):
    c, dock = env.c, env.dock
    repo = "ghcr.io/wranglarrr/wranglarr"
    dock.registry(ref=f"{repo}:latest", digest="sha256:" + "e1" * 32, version="1.0")
    dock.registry(ref=f"{repo}:edge", digest="sha256:" + "e2" * 32, version="1.1-dev")
    dock.http.post("/images/create", params={"fromImage": repo, "tag": "latest"}).raise_for_status()
    dock.http.post("/containers/create", params={"name": "wranglarr-devtest"}, json={"Image": f"{repo}:latest"}).raise_for_status()
    dock.http.post("/containers/wranglarr-devtest/start").raise_for_status()
    own = dock.containers()["wranglarr-devtest"]
    env.srv.stop()
    env.srv.env["HOSTNAME"] = own["Id"][:12]
    env.srv.start()
    r = c.put("/api/updates/tracks/wranglarr-devtest", json={"tag": "edge"})
    assert r.status_code == 200, r.text
    c.post("/api/updates/check").raise_for_status()
    row = target(c, "wranglarr-devtest")
    assert row["status"] == "self" and row["newer"], row
    assert [x["tag"] for x in row["track"]["choices"]] == ["latest", "edge"]
    assert f"{repo}:edge" not in dock.state()["tags"], "checks must not pull"
    assert f"WRANGLARR_IMAGE={repo}:edge" in c.get("/updates").text
    # the ordinary updater never takes wranglarr's own container: that's Update now
    r = c.post("/api/updates/apply", json={"containers": ["wranglarr-devtest"]})
    assert r.status_code in (404, 409), r.text
    assert dock.containers()["wranglarr-devtest"]["Id"] == own["Id"]
