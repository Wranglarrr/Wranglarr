"""Failure-oriented restore checks: stopped writers, recovery preservation and plan progress."""
import json

import pytest

from harness import sha256, wait_run, write_manifest
from test_updates import build


@pytest.fixture
def env(tmp_path):
    e = build(tmp_path)
    yield e
    e.c.close()
    for p in e.procs:
        p.stop()


def database_version(e, version="20260101T000000Z"):
    vdir = write_manifest(e.tmp / "store", "media", version, {"apps": {}})
    (vdir / "radarr").mkdir()
    dump = vdir / "radarr/main.dump"
    dump.write_bytes(b"database archive")
    write_manifest(e.tmp / "store", "media", version, {"apps": {"radarr": {
        "databases": [{"name": name, "file": "radarr/main.dump", "sha256": sha256(dump)}
                      for name in ["radarr-main", "radarr-log"]]
    }}})
    return version


def test_failed_stop_never_restores_a_live_database(env):
    v = database_version(env)
    env.dock.http.post("/_mock/fail_stop", json={"name": "radarr"}).raise_for_status()
    r = env.c.post(f"/api/suites/media/versions/{v}/restore", json={"app": "radarr", "config": False})
    run = wait_run(env.c, r.json()["run"])
    assert run["status"] == "failed" and "restore refused" in run["log"], run
    assert env.dock.containers()["radarr"]["State"]["Running"]
    pglog = env.tmp / "pg.log"
    assert not pglog.exists() or "pg_restore" not in pglog.read_text()


def test_database_restore_requires_docker_control(env):
    v = database_version(env)
    env.srv.stop()
    env.srv.config.write_text(env.srv.config.read_text().replace('container = "radarr"', 'container = ""'))
    env.srv.start()
    r = env.c.post(f"/api/suites/media/versions/{v}/restore", json={"app": "radarr", "config": False})
    run = wait_run(env.c, r.json()["run"])
    assert run["status"] == "failed" and "live restore is refused" in run["log"], run
    pglog = env.tmp / "pg.log"
    assert not pglog.exists() or "pg_restore" not in pglog.read_text()


def test_plan_keeps_missing_items_and_never_replays_completed_restore(env):
    v = database_version(env)
    missing = "20260202T000000Z"
    plan = env.tmp / "state/restore-plan.json"
    plan.write_text(json.dumps({"items": [
        {"suite": "media", "app": "radarr", "version": v, "databases": True, "config": False},
        {"suite": "media", "app": "radarr", "version": missing, "databases": True, "config": False},
    ]}))
    run = wait_run(env.c, env.c.post("/api/restore-plan/run").json()["run"])
    assert run["status"] == "partial", run
    assert run["result"]["done"] == [f"radarr@{v}"]
    assert run["result"]["unavailable"] == [f"radarr@{missing}"]
    assert [i["version"] for i in json.loads(plan.read_text())["items"]] == [missing]
    assert env.c.post("/api/restore-plan/run").status_code == 409
    database_version(env, missing)
    run = wait_run(env.c, env.c.post("/api/restore-plan/run").json()["run"])
    assert run["status"] == "ok", run
    assert run["result"]["done"] == [f"radarr@{missing}"]
    assert not plan.exists()
    assert (env.tmp / "pg.log").read_text().count("pg_restore") == 4


def test_plan_with_interrupted_item_requires_review(env):
    v = database_version(env)
    plan = env.tmp / "state/restore-plan.json"
    plan.write_text(json.dumps({"items": [{"suite": "media", "app": "radarr", "version": v, "started": True}]}))
    response = env.c.post("/api/restore-plan/run")
    assert response.status_code == 409 and "interrupted" in response.text
    assert json.loads(plan.read_text())["items"][0]["started"]


def test_failed_file_restore_keeps_container_stopped_and_recovery_journal(env):
    backup = wait_run(env.c, env.c.post("/api/suites/downloads/backup").json()["run"])
    assert backup["status"] == "ok", backup
    v = backup["result"]["version"]
    live = env.tmp / "arrconfig/qbittorrent"
    transaction = live / ".wranglarr-restore-transaction"
    transaction.mkdir()
    (transaction / "recovery.json").write_text("previous recovery")
    (live / "qbittorrent.conf").write_text("preserve live")
    run = wait_run(env.c, env.c.post(f"/api/suites/downloads/versions/{v}/restore", json={"app": "qbittorrent"}).json()["run"])
    assert run["status"] == "failed" and "manual recovery" in run["log"], run
    assert not env.dock.containers()["qbittorrent"]["State"]["Running"]
    assert (transaction / "recovery.json").read_text() == "previous recovery"
    assert (live / "qbittorrent.conf").read_text() == "preserve live"


def test_incomplete_restore_selection_is_rejected_before_stop(env):
    v = database_version(env)
    manifest = env.tmp / "store/media" / v / "manifest.json"
    m = json.loads(manifest.read_text())
    m["apps"]["radarr"]["databases"].pop()
    manifest.write_text(json.dumps(m))
    n0 = len(env.dock.state()["events"])
    run = wait_run(env.c, env.c.post(f"/api/suites/media/versions/{v}/restore",
                                   json={"app": "radarr", "config": False}).json()["run"])
    assert run["status"] == "failed" and "match configured databases" in run["log"], run
    assert not any("/stop" in x for x in env.dock.state()["events"][n0:])
    run = wait_run(env.c, env.c.post(f"/api/suites/media/versions/{v}/restore",
                                   json={"app": "radarr", "databases": False}).json()["run"])
    assert run["status"] == "failed" and "config restore is missing" in run["log"], run


def test_arr_config_restore_restarts_the_app_and_keeps_this_stacks_database_login(env):
    """The arr answers the upload with restartRequired and waits for the restart; a
    backup from another install carries that install's Postgres password."""
    import io
    import zipfile
    from harness import ARR_KEY
    live = env.tmp / "arrconfig/radarr/config.xml"
    live.write_text(f"<Config><Port>7878</Port><ApiKey>{ARR_KEY}</ApiKey><UrlBase></UrlBase>"
                    "<PostgresHost>postgres</PostgresHost><PostgresPassword>this-stack</PostgresPassword></Config>")
    version = "20260303T000000Z"
    vdir = write_manifest(env.tmp / "store", "media", version, {"apps": {}})
    (vdir / "radarr").mkdir()
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        z.writestr("config.xml", f"<Config><ApiKey>{ARR_KEY}</ApiKey><PostgresHost>postgres</PostgresHost>"
                                 "<PostgresPassword>other-install</PostgresPassword></Config>")
    zf = vdir / "radarr/radarr_backup.zip"
    zf.write_bytes(buf.getvalue())
    write_manifest(env.tmp / "store", "media", version, {"apps": {"radarr": {
        "config_zip": {"file": "radarr/radarr_backup.zip", "sha256": sha256(zf), "format": "zip"}}}})
    before = env.arr.state()["restarts"]
    r = env.c.post(f"/api/suites/media/versions/{version}/restore", json={"app": "radarr", "databases": False})
    run = wait_run(env.c, r.json()["run"])
    assert run["status"] == "ok", run["log"]
    assert "keeps this stack's database connection (PostgresPassword)" in run["log"], run["log"]
    assert "this-stack" not in run["log"] and "other-install" not in run["log"], "no passwords in the log"
    st = env.arr.state()
    assert st["restored"] and st["restarts"] == before + 1, st
