"""Container updates against mock Docker: check, update (settings carried over), the
Postgres major-bump refusal, the VPN namespace dependents, automatic rollback of a
crashing image, manual rollback + hold, settings, pages, and Docker being off.

The steps in this file run in order against one server and one mock Docker."""
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

import pytest

from harness import ARR_KEY, MockArr, MockDocker, MockDownloads, Server, base_toml, suite_toml, wait_run


# what the broken radarr prints: secrets, a public address and a domain mixed in
CRASH_LOG = (
    "[Info] Bootstrap: starting up as /home/johnny/.config\n"
    f"[Info] Api: key {ARR_KEY} loaded, qbittorrent password=qbpass\n"
    "[Info] Http: https://radarr.mymedia.example.org/api from 203.0.113.9\n"
    f"[Fatal] ConsoleApp: Migration 231 failed (pid 4242), apikey={ARR_KEY}\n"
    "   at NzbDrone.Core.Datastore.Migration.Framework.MigrationController.Migrate(String connectionString)\n"
    "   at Radarr.Host.Bootstrap.Start()\n"
    "[Info] Bootstrap: exiting, vpn key gluekey\n"
)


class MockGitHub:
    """GitHub's issue search: `issues` maps a fingerprint to the issue that has it."""

    def __init__(self):
        self.issues, self.asked = {}, []
        gh = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a): pass

            def do_GET(self):
                u = urlparse(self.path)
                q = parse_qs(u.query).get("q", [""])[0]
                gh.asked.append({"path": u.path, "q": q, "origin": self.headers.get("Origin"),
                                 "agent": self.headers.get("User-Agent")})
                items = [i for fid, i in gh.issues.items() if f'"{fid}"' in q]
                body = json.dumps({"total_count": len(items), "items": items}).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json"); self.end_headers()
                self.wfile.write(body)

        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.url = f"http://127.0.0.1:{self.srv.server_port}"
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()

    def stop(self):
        self.srv.shutdown()


def build(tmp):
    """Mock Docker + Radarr + qBittorrent/gluetun and a server managing them."""
    (tmp / "arrconfig/radarr").mkdir(parents=True)
    (tmp / "arrconfig/radarr/config.xml").write_text(f"<Config><Port>7878</Port><ApiKey>{ARR_KEY}</ApiKey><UrlBase></UrlBase></Config>")
    (tmp / "arrconfig/qbittorrent").mkdir(parents=True)
    (tmp / "arrconfig/qbittorrent/qBittorrent.conf").write_text("[Preferences]\n")
    dock, arr, dl = MockDocker(tmp), MockArr(tmp), MockDownloads(tmp)
    toml = base_toml(tmp) + (
        f'[docker]\nsocket = "{dock.sock}"\n'
        f'[vpn]\ncontrol_url = "http://127.0.0.1:{dl.gluetun}"\napi_key = "gluekey"\n'
        '[updates]\ncheck_schedule = "0 */6 * * *"\nhealth_timeout = 20\nextra = ["wranglarr", "ghost"]\n\n'
    ) + suite_toml("media", [dict(name="radarr", kind="radarr", url=arr.url, api_key_file=f"{tmp}/arrconfig/radarr/config.xml",
                                  databases=["radarr-main", "radarr-log"], container="radarr")], keep_last=5) \
      + suite_toml("downloads", [dict(name="qbittorrent", kind="qbittorrent", url=f"http://127.0.0.1:{dl.qbit}",
                                      username="admin", password="qbpass", config_dir=f"{tmp}/arrconfig/qbittorrent",
                                      container="qbittorrent")])
    gh = MockGitHub()
    srv = Server(tmp, toml, env={"WRANGLARR_GITHUB_API": gh.url}).start()
    return type("Env", (), dict(tmp=tmp, dock=dock, arr=arr, dl=dl, srv=srv, gh=gh, c=srv.client(),
                                procs=(srv, dl, arr, dock, gh)))


@pytest.fixture(scope="module")
def env(tmp_path_factory):
    e = build(tmp_path_factory.mktemp("updates"))
    yield e
    e.c.close()
    for p in e.procs:
        p.stop()


def statuses(c):
    return {t["container"]: t["status"] for t in c.get("/api/updates").json()["targets"]}


def target(c, name):
    return next(t for t in c.get("/api/updates").json()["targets"] if t["container"] == name)


def env_of(container):
    return dict(e.split("=", 1) for e in container["Config"]["Env"])


def test_check(env):
    c = env.c
    o = c.get("/api/updates").json()
    assert {t["container"] for t in o["targets"]} == {"arrs-vpn", "arrs-postgres", "radarr", "qbittorrent", "wranglarr", "ghost"}
    assert o["next"]["check"], o["next"]                       # check_schedule is scheduled
    o = c.post("/api/updates/check").json()
    st = {t["container"]: t["status"] for t in o["targets"]}
    assert st == {"arrs-vpn": "current", "arrs-postgres": "available", "radarr": "available", "qbittorrent": "available",
                  "wranglarr": "local", "ghost": "missing"}, st
    assert o["available"] == 3
    page = c.get("/updates")
    assert page.status_code == 200 and "update available" in page.text and 'class="badge"' in page.text
    assert c.get("/").text.count('class="badge"') == 1
    assert c.get("/api/health").json()["updates_available"] == 3


def test_update_keeps_settings_refuses_pg_major(env):
    c, dock = env.c, env.dock
    before = dock.containers()
    rid = c.post("/api/updates/apply", json={"containers": ["radarr", "qbittorrent", "arrs-postgres"]}).json()["run"]
    r = wait_run(c, rid)
    res = r["result"]["results"]
    assert res == {"arrs-postgres": "refused", "radarr": "updated", "qbittorrent": "updated"}, (res, r["log"])
    assert r["status"] == "partial"
    after = dock.containers()
    rd0, rd1 = before["radarr"], after["radarr"]
    assert rd1["Id"] != rd0["Id"] and rd1["State"]["Running"]
    env_ = env_of(rd1)
    assert env_["PUID"] == "1000" and env_["TZ"] == "Europe/London" and env_["NEW_IMAGE_DEFAULT"] == "1", env_
    assert rd1["HostConfig"]["Binds"] == rd0["HostConfig"]["Binds"]
    assert rd1["HostConfig"]["RestartPolicy"] == {"Name": "unless-stopped"}
    assert rd1["Config"]["Labels"]["com.docker.compose.service"] == "radarr"
    assert rd1["Config"]["Labels"]["com.docker.compose.image"] == rd1["Image"]
    assert rd1["Config"]["Labels"]["org.opencontainers.image.version"] == "5.15.1.9463-ls251"
    assert rd1["Networks"]["arrs"]["Aliases"][0] == "radarr" and rd0["Id"][:12] not in rd1["Networks"]["arrs"]["Aliases"]
    assert "Hostname" not in rd1["body"], rd1["body"].get("Hostname")
    anon0 = [m for m in rd0["Mounts"] if m["Type"] == "volume"]
    assert [m["Source"] for m in rd1["Mounts"] if m["Destination"] == "/config"] == ["/srv/appdata/radarr"] and not anon0
    qb1 = after["qbittorrent"]
    assert qb1["HostConfig"]["NetworkMode"] == f"container:{after['arrs-vpn']['Id']}"
    # a user env value equal to the image's default is kept; image-only values follow the new image
    assert "TZ=UTC" in qb1["body"]["Env"] and not [e for e in qb1["body"]["Env"] if e.startswith(("PATH=", "HOME="))], qb1["body"]["Env"]
    assert "radarr-wranglarr-old" not in after and after["arrs-postgres"]["Id"] == before["arrs-postgres"]["Id"]
    assert after["arrs-postgres"]["State"]["Running"]
    assert c.get("/api/suites/media/versions").json()[0]["trigger"] == "pre-update"
    assert c.get("/api/suites/downloads/versions").json()[0]["trigger"] == "pre-update"
    st = statuses(c)
    assert st["radarr"] == "current" and st["qbittorrent"] == "current" and st["arrs-postgres"] == "blocked", st
    assert dock.state()["tags"]["postgres:17"] == before["arrs-postgres"]["Image"], "postgres tag not restored"


def test_vpn_update_moves_dependents(env):
    c, dock = env.c, env.dock
    dock.registry(ref="qmcgaw/gluetun:v3.41.3", digest="sha256:" + "a2" * 32, version="v3.41.3",
                  health={"Test": ["CMD", "true"]})
    c.post("/api/updates/check")
    qb_before = dock.containers()["qbittorrent"]["Id"]
    rid = c.post("/api/updates/apply", json={}).json()["run"]
    r = wait_run(c, rid)
    assert "arrs-postgres" not in r["result"]["results"], "blocked postgres retried by update-all"
    assert r["status"] == "ok", (r["status"], r["log"])
    assert r["result"]["results"].get("arrs-vpn") == "updated" and r["result"]["results"].get("qbittorrent") == "recreated", r["result"]
    a = dock.containers()
    assert a["qbittorrent"]["HostConfig"]["NetworkMode"] == f"container:{a['arrs-vpn']['Id']}" and a["qbittorrent"]["Id"] != qb_before
    assert a["arrs-vpn"]["HostConfig"]["PortBindings"] == {"7878/tcp": [{"HostPort": "7878"}]}
    assert a["arrs-vpn"]["Networks"]["arrs"]["Aliases"][:2] == ["vpn", "radarr-alias"]


def test_crashing_image_rolls_back(env):
    c, dock = env.c, env.dock
    dock.registry(ref="lscr.io/linuxserver/radarr:latest", digest="sha256:" + "b3" * 32, version="5.16.0-broken", crash=True,
                  log=CRASH_LOG)
    c.post("/api/updates/check")
    good = dock.containers()["radarr"]
    restored = len(env.arr.state()["restored"])
    rid = c.post("/api/updates/apply", json={"containers": ["radarr"]}).json()["run"]
    r = wait_run(c, rid)
    s = dock.state()
    a = s["containers"]
    assert r["status"] == "failed" and a["radarr"]["Id"] == good["Id"] and a["radarr"]["State"]["Running"], r["log"]
    # the new version ran, so it may have migrated the data: it goes back from the
    # pre-update backup before the old one starts, and its config zip after
    log = r["log"]
    assert "may have migrated radarr's data: putting it back from" in log, log
    assert log.index("restored database radarr-main") < log.index("radarr: old container restored"), log
    assert len(env.arr.state()["restored"]) == restored + 1, "the config zip went back through the old radarr"
    assert not [n for n in a if n.endswith("-wranglarr-old")]
    assert s["tags"]["lscr.io/linuxserver/radarr:latest"] == good["Image"], "radarr tag still on the broken image"
    assert statuses(c)["radarr"] == "blocked"

    # a report to check for and file the failure with, with the secrets taken out
    rep = r["result"]["reports"]["radarr"]
    fid, key = rep["id"], rep["key"]
    assert fid.startswith("wf-") and len(fid) == 13, rep
    assert rep["search"].startswith("https://github.com/Wranglarrr/Wranglarr/issues?q=") and fid in rep["search"], rep
    assert key == f'{r["id"]}-radarr' and rep["state"] == "running", rep
    assert f"radarr: a report ({fid}) is at /updates/report/{key}: check there whether it's been reported" in log, log
    assert "or search for it by hand: https://github.com/Wranglarrr/Wranglarr/issues" in log, log
    full = c.get(f"/api/updates/reports/{key}").json()
    md = full["markdown"]
    for secret in (ARR_KEY, "qbpass", "gluekey", "radarr.mymedia.example.org", "203.0.113.9", "johnny"):
        assert secret not in md, (secret, md)
    assert "Migration 231 failed" in md and "at NzbDrone.Core.Datastore.Migration" in md, md
    assert "starting up" not in md, "only the error lines and their stack are kept"
    assert full["title"] == f"Update failed: Radarr 5.15.1.9463-ls251 → 5.16.0-broken [{fid}]", full["title"]
    assert full["links"]["new_issue"].startswith(
        "https://github.com/Wranglarrr/Wranglarr/issues/new?template=update-failure.yml"), full["links"]
    assert full["data"] == "The data was put back from the pre-update backup.", full
    assert full["data_recovery"] == {
        "state": "verified", "api": "verified",
        "completed": ["database radarr-main", "database radarr-log", "API config"],
    }, full
    assert full["recovery_phase"] is None, full
    assert full["state"] == "running" and full["blocked"] is True, full
    assert full["outcome"].startswith("radarr is back on 5.15.1.9463-ls251. 5.16.0-broken is skipped from now on."), full
    assert full["exit_code"] == 1 and full["seen"] == 1, full
    assert target(c, "radarr")["report"]["key"] == key
    page = c.get(f"/updates/report/{key}")
    assert page.status_code == 200 and fid in page.text, page.text[:500]
    assert f'data-report-check data-key="{key}"' in page.text, page.text
    assert page.headers["referrer-policy"] == "same-origin", "links out don't carry wranglarr's address"

    # has it been reported? wranglarr asks, so GitHub doesn't see this page's address
    assert c.get(f"/api/updates/reports/{key}/reported").json() == {"issue": None}
    asked = env.gh.asked[-1]
    assert asked["path"] == "/search/issues" and asked["origin"] is None, asked
    assert asked["q"] == f'repo:Wranglarrr/Wranglarr is:issue "{fid}"' and asked["agent"].startswith("wranglarr/"), asked
    env.gh.issues[fid] = {"number": 42, "title": full["title"], "state": "closed",
                          "html_url": "https://github.com/Wranglarrr/Wranglarr/issues/42"}
    assert c.get(f"/api/updates/reports/{key}/reported").json()["issue"] == {
        "number": 42, "title": full["title"], "state": "closed", "url": "https://github.com/Wranglarrr/Wranglarr/issues/42"}
    assert c.get("/api/updates/reports/wf-0000000000/reported").status_code == 404

    # the same failure again (on another day it would be another time and pid):
    # the same fingerprint, seen twice, and each occurrence keeps its own report
    r = wait_run(c, c.post("/api/updates/apply", json={"containers": ["radarr"]}).json()["run"])
    rep2 = r["result"]["reports"]["radarr"]
    assert r["status"] == "failed" and rep2["id"] == fid and rep2["key"] != key, r["result"]
    again = c.get(f'/api/updates/reports/{rep2["key"]}').json()
    assert again["seen"] == 2 and again["first_seen"] == full["at"], again
    reports = c.get("/api/updates/reports").json()["reports"]
    assert [(x["key"], x["id"]) for x in reports] == [(rep2["key"], fid), (key, fid)], reports
    assert c.get(f"/api/updates/reports/{key}").json()["run_id"] == full["run_id"]
    assert c.get("/api/updates/reports/wf-0000000000").status_code == 404


def test_manual_rollback_and_hold(env):
    c, dock = env.c, env.dock
    t = target(c, "radarr")
    assert t["can_rollback"] and t["rollback_to"] == "5.14.0.9383-ls250", t
    # the backup from just before that update, offered to put the data back with it
    assert t["rollback_backup"] and t["rollback_backup"]["version"], t
    restored = len(env.arr.state()["restored"])
    rid = c.post("/api/updates/rollback", json={"container": "radarr", "restore_data": True}).json()["run"]
    r = wait_run(c, rid)
    a = dock.containers()
    assert r["status"] == "ok", r["log"]
    assert r["result"]["data_from"] == t["rollback_backup"]["version"], r["result"]
    log = r["log"]
    assert log.index("putting radarr's data back from the pre-update backup") < log.index("radarr: rollback done"), log
    assert "restored database radarr-main" in log, log
    assert len(env.arr.state()["restored"]) == restored + 1, "its config zip went back through the old radarr"
    assert a["radarr"]["Config"]["Labels"]["org.opencontainers.image.version"] == "5.14.0.9383-ls250"
    assert a["radarr"]["Config"]["Image"] == "lscr.io/linuxserver/radarr:latest"
    t = target(c, "radarr")
    assert t["held"] and not t["can_rollback"] and t["status"] == "blocked", t
    r2 = c.post("/api/updates/apply", json={})            # held + blocked: nothing left to update
    assert r2.status_code == 409, r2.text
    assert "error" in r2.json()
    c.post("/api/updates/hold", json={"container": "radarr", "hold": False})
    assert not target(c, "radarr")["held"]
    assert c.post("/api/updates/hold", json={"container": "nope"}).status_code == 404
    assert c.post("/api/updates/apply", json={"containers": ["nope"]}).status_code == 404


def test_settings_and_pages(env):
    c = env.c
    bad = c.put("/api/updates/settings", json={"check_schedule": "nope", "auto_schedule": ""})
    assert bad.status_code == 400 and "error" in bad.json()
    ok = c.put("/api/updates/settings", json={"check_schedule": "15 * * * *", "auto_update": True,
                                             "auto_schedule": "0 4 * * 1", "backup_first": True}).json()
    assert ok["source"] == "ui"
    nxt = c.get("/api/updates").json()["next"]
    assert nxt["auto"] and nxt["check"], nxt                    # both jobs scheduled
    c.put("/api/updates/settings", json={"enabled": False, "check_schedule": "15 * * * *"})
    assert not c.get("/api/updates").json()["next"]["check"]   # off: no check job
    assert c.delete("/api/updates/settings").json()["source"] == "config"
    pg = c.get("/updates").text
    assert "update blocked" in pg and "Try again" in pg and 'data-hold="radarr" data-on="1"' in pg
    runs = c.get("/api/runs").json()
    rid = runs[0]["id"]
    for path in ("/updates", "/activity", "/", "/system", f"/runs/{rid}"):
        assert c.get(path).status_code == 200, path
    hist = c.get("/api/updates").json()["history"]
    assert [(h["container"], h["action"], h["status"]) for h in hist][:4] == [
        ("radarr", "rollback", "ok"), ("radarr", "update", "failed"), ("radarr", "update", "failed"),
        ("qbittorrent", "recreate", "ok")], hist[:4]


def test_failed_vpn_dependent_is_reported(env):
    """The VPN comes up on the new image but its dependent can't be recreated:
    the run says so and the log says how to recover."""
    c, dock = env.c, env.dock
    dock.registry(ref="qmcgaw/gluetun:v3.41.3", digest="sha256:" + "a3" * 32, version="v3.41.4",
                  health={"Test": ["CMD", "true"]})
    dock.fail_create("qbittorrent")
    c.post("/api/updates/check")
    rid = c.post("/api/updates/apply", json={"containers": ["arrs-vpn"]}).json()["run"]
    r = wait_run(c, rid)
    dock.fail_create(None)
    res = r["result"]["results"]
    assert res.get("arrs-vpn") == "updated" and res.get("qbittorrent") == "failed", (res, r["log"])
    assert r["status"] == "partial", r["status"]
    assert "compose up -d qbittorrent" in r["log"], r["log"]


@pytest.fixture(scope="module")
def nodocker(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("updates-nodocker")
    toml = base_toml(tmp) + suite_toml("media", [dict(name="radarr", kind="radarr", url="http://127.0.0.1:1",
                                                      api_key="x", container="radarr")])
    srv = Server(tmp, toml).start()
    yield srv
    srv.stop()


def test_without_docker(nodocker):
    with nodocker.client() as c:
        p = c.get("/updates")
        assert p.status_code == 200 and "Docker control is off" in p.text
        r = c.post("/api/updates/check")
        assert r.status_code == 409 and "error" in r.json(), r.text
        r = c.post("/api/updates/apply", json={"containers": ["radarr"]})
        assert r.status_code == 409, r.text
        assert c.post("/api/updates/apply", json={"containers": ["nope"]}).status_code in (404, 409)
        assert c.get("/api/health").json()["updates_available"] == 0
        assert c.get("/api/health").json()["docker"] is False
