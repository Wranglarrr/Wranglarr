"""Config snapshots in backups and their restore, through the API: the natural-format copy of
/config (logs, caches, lock files left out; config.sha256 in `sha256sum -c` format), the
SQLite-mode overlay from the app's own backup zip, restoring a download client's folder
(container stopped and started, old contents set aside), tampering caught, a non-folder
snapshot refused, unreadable files reported, and a restore that can't reach the app
failing before anything is stopped.

The steps in this file run in order against one server."""
import os
import shutil

import pytest

from harness import (ARR_KEY, MockArr, MockDocker, MockDownloads, Server, base_toml, sha256, suite_toml,
                     wait_run, write_manifest)

QB_CONF = "[Preferences]\nWebUI\\Port=8090\n"


@pytest.fixture(scope="module")
def env(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("snapshots")
    live = tmp / "live"                                    # qBittorrent's /config
    for d in ("qBittorrent/logs", "qBittorrent/BT_backup", "qBittorrent/GeoDB"):
        (live / d).mkdir(parents=True)
    (live / "qBittorrent/qBittorrent.conf").write_text(QB_CONF)
    (live / "qBittorrent/BT_backup/abc.fastresume").write_bytes(b"d8:resume")
    (live / "qBittorrent/logs/qbittorrent.log").write_text("noise")
    (live / "qBittorrent/GeoDB/x.mmdb").write_bytes(b"0" * 1000)
    (live / "qBittorrent/lockfile").write_text("123")
    rad = tmp / "radarr"                                   # Radarr's /config, SQLite mode
    (rad / "MediaCover/1").mkdir(parents=True)
    (rad / "logs").mkdir()
    (rad / "logs/radarr.txt").write_text("log")
    (rad / "config.xml").write_text(f"<Config><ApiKey>{ARR_KEY}</ApiKey></Config>")
    (rad / "radarr.db").write_text("LIVE-INCONSISTENT")
    (rad / "MediaCover/1/poster.jpg").write_bytes(b"x" * 5000)
    perms = tmp / "perms"                                  # files wranglarr can't read
    (perms / "sub").mkdir(parents=True)
    (perms / "a.xml").write_text("x")
    (perms / "secret.xml").write_text("y")
    (perms / "sub/b").write_text("z")
    dock, arr, dl = MockDocker(tmp), MockArr(tmp), MockDownloads(tmp)
    toml = base_toml(tmp) + f'[docker]\nsocket = "{dock.sock}"\n\n' \
        + suite_toml("downloads", [dict(name="qbittorrent", kind="qbittorrent", url=f"http://127.0.0.1:{dl.qbit}",
                                        username="admin", password="qbpass", config_dir=str(live), container="qbittorrent")]) \
        + suite_toml("media", [dict(name="radarr", kind="radarr", url=arr.url, api_key=ARR_KEY, config_dir=str(rad))]) \
        + suite_toml("perms", [dict(name="radarr2", kind="radarr", url=arr.url, api_key=ARR_KEY, config_dir=str(perms))]) \
        + suite_toml("c7", [dict(name="radarr7", kind="radarr", url=arr.url, api_key_file=f"{tmp}/missing/config.xml",
                                 container="radarr")])
    srv = Server(tmp, toml).start()
    c = srv.client()
    yield type("Env", (), dict(tmp=tmp, live=live, store=tmp / "store", dock=dock, c=c, ctx={}))
    c.close()
    os.chmod(perms / "secret.xml", 0o644)
    os.chmod(perms / "sub", 0o755)
    for p in (srv, dl, arr, dock):
        p.stop()


def backup(c, suite):
    rid = c.post(f"/api/suites/{suite}/backup").json()["run"]
    r = wait_run(c, rid)
    return r, c.get(f"/api/suites/{suite}/versions/{r['result']['version']}").json()


def files_under(d):
    return sorted(p.relative_to(d).as_posix() for p in d.rglob("*") if p.is_file())


def check_sums(vdir, sums):
    """What `sha256sum -c` does, run from the version folder."""
    for line in (vdir / sums).read_text().splitlines():
        digest, _, rel = line.partition("  ")
        assert sha256(vdir / rel) == digest, rel


def test_download_client_snapshot(env):
    r, m = backup(env.c, "downloads")
    assert r["status"] == "ok", r["log"]
    snap = m["apps"]["qbittorrent"]["config_snapshot"]
    assert snap["path"] == "qbittorrent/config" and snap["files"] == 2 and snap["skipped"] >= 3, snap
    vdir = env.store / "downloads" / m["version"]
    assert files_under(vdir / "qbittorrent/config") == ["qBittorrent/BT_backup/abc.fastresume", "qBittorrent/qBittorrent.conf"]
    check_sums(vdir, snap["checksums"])
    resume = next(i for i in snap["payload"]["items"] if i["label"].startswith("resume files"))
    assert resume["count"] == 1, snap["payload"]
    env.ctx["qb_version"] = m["version"]


def test_sqlite_mode_overlay(env):
    """No databases configured: the live .db is skipped and the app's own zip's copy laid over."""
    r, m = backup(env.c, "media")
    assert r["status"] == "ok", r["log"]
    app = m["apps"]["radarr"]
    assert app["config_zip"] and app["config_zip"]["format"] == "zip", app
    cfg = env.store / "media" / m["version"] / "radarr/config"
    assert (cfg / "radarr.db").read_text() == "CONSISTENT"
    assert not (cfg / "MediaCover").exists() and not (cfg / "logs").exists() and (cfg / "config.xml").exists()


def test_restore_download_client(env):
    c, live = env.c, env.live
    (live / "qBittorrent/qBittorrent.conf").write_text("changed")
    n0 = len(env.dock.state()["events"])
    rid = c.post(f"/api/suites/downloads/versions/{env.ctx['qb_version']}/restore", json={"app": "qbittorrent"}).json()["run"]
    r = wait_run(c, rid)
    assert r["status"] == "ok", r["log"]
    assert (live / "qBittorrent/qBittorrent.conf").read_text() == QB_CONF
    assert (live / "qBittorrent/BT_backup/abc.fastresume").exists()
    keep = env.tmp / "live.pre-wranglarr"
    assert (keep / "qBittorrent/qBittorrent.conf").read_text() == "changed", "the replaced contents are set aside"
    ev = env.dock.state()["events"][n0:]
    assert any("/containers/qbittorrent/stop" in e for e in ev) and any("/containers/qbittorrent/start" in e for e in ev), ev
    assert env.dock.containers()["qbittorrent"]["State"]["Running"]


def test_tampered_snapshot_refused(env):
    c = env.c
    snap = env.store / "downloads" / env.ctx["qb_version"] / "qbittorrent/config/qBittorrent/qBittorrent.conf"
    snap.write_text("tampered")
    (env.live / "qBittorrent/qBittorrent.conf").write_text("live now")
    rid = c.post(f"/api/suites/downloads/versions/{env.ctx['qb_version']}/restore", json={"app": "qbittorrent"}).json()["run"]
    r = wait_run(c, rid)
    assert r["status"] == "failed" and "checksum mismatch" in r["log"], r["log"]
    assert (env.live / "qBittorrent/qBittorrent.conf").read_text() == "live now", "nothing was replaced"
    assert env.dock.containers()["qbittorrent"]["State"]["Running"], "started again after the failed restore"
    snap.write_text(QB_CONF)


def test_snapshot_must_be_a_folder(env):
    """An old-style config.tar.gz (or any file) where the snapshot folder should be is refused."""
    v = "20200101T000000Z"
    vdir = write_manifest(env.store, "downloads", v, {"status": "ok", "apps": {"qbittorrent": {
        "name": "qbittorrent", "kind": "qbittorrent", "databases": [], "errors": [],
        "config_snapshot": {"path": "qbittorrent/config.tar.gz", "files": 1}}}})
    (vdir / "qbittorrent").mkdir()
    (vdir / "qbittorrent/config.tar.gz").write_bytes(b"\x1f\x8b not really")
    rid = env.c.post(f"/api/suites/downloads/versions/{v}/restore", json={"app": "qbittorrent"}).json()["run"]
    r = wait_run(env.c, rid)
    assert r["status"] == "failed" and "not a config snapshot" in r["log"], r["log"]
    assert (env.live / "qBittorrent/qBittorrent.conf").read_text() == "live now"
    shutil.rmtree(vdir)


@pytest.mark.skipif(os.geteuid() == 0, reason="root reads everything")
def test_unreadable_files_reported(env):
    """A file or folder that can't be read makes the backup incomplete, and says which."""
    perms = env.tmp / "perms"
    os.chmod(perms / "secret.xml", 0)
    os.chmod(perms / "sub", 0)
    try:
        r, m = backup(env.c, "perms")
    finally:
        os.chmod(perms / "secret.xml", 0o644)
        os.chmod(perms / "sub", 0o755)
    app = m["apps"]["radarr2"]
    bad = app["config_snapshot"]["unreadable"]
    assert any("secret.xml" in b for b in bad) and any("sub" in b for b in bad), bad
    assert any("couldn't be read" in e for e in app["errors"]), app["errors"]
    assert m["status"] != "ok" and r["status"] != "ok", (m["status"], r["status"])


def test_restore_fails_before_stopping_when_the_app_is_unusable(env):
    """The app's client can't be made (API key file missing): the restore fails
    before the container is stopped, so the app keeps running."""
    v = "20260101T000000Z"
    vdir = write_manifest(env.store, "c7", v, {"status": "ok", "apps": {}})
    (vdir / "radarr7").mkdir()
    (vdir / "radarr7/radarr.dump").write_bytes(b"d")
    write_manifest(env.store, "c7", v, {"status": "ok", "apps": {"radarr7": {
        "name": "radarr7", "kind": "radarr", "errors": [],
        "databases": [{"name": "radarr", "file": "radarr7/radarr.dump", "sha256": sha256(vdir / "radarr7/radarr.dump")}]}}})
    n0 = len(env.dock.state()["events"])
    rid = env.c.post(f"/api/suites/c7/versions/{v}/restore", json={"app": "radarr7"}).json()["run"]
    r = wait_run(env.c, rid)
    assert r["status"] == "failed", r["log"]
    ev = env.dock.state()["events"][n0:]
    assert not any("/stop" in e for e in ev), ev
    assert env.dock.containers()["radarr"]["State"]["Running"]
    pglog = env.tmp / "pg.log"
    assert "pg_restore" not in (pglog.read_text() if pglog.exists() else ""), "no database was touched"
