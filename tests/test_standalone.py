"""The standalone restore stack written into every backup version (compose.yaml, .env,
RESTORE.md, postgres/initdb), from the containers as mock Docker describes them, with
AuthTunnel (UIs on 127.0.0.1 only) and without (APPS_AUTH=apps)."""
import stat

import pytest

from harness import ARR_KEY, MockArr, MockDocker, MockDownloads, Server, base_toml, suite_toml, wait_run


@pytest.fixture(scope="module", params=["tunnel", "apps"])
def env(request, tmp_path_factory):
    tmp = tmp_path_factory.mktemp(f"standalone-{request.param}")
    (tmp / "qb/qBittorrent").mkdir(parents=True)
    (tmp / "qb/qBittorrent/qBittorrent.conf").write_text("[Preferences]\n")
    dock, arr, dl = MockDocker(tmp, "standalone"), MockArr(tmp), MockDownloads(tmp)
    toml = base_toml(tmp, pg_host="postgres") + f'[docker]\nsocket = "{dock.sock}"\n\n' \
        + suite_toml("media", [dict(name="radarr", kind="radarr", url=arr.url, api_key=ARR_KEY, databases=["radarr"],
                                    container="radarr")]) \
        + suite_toml("downloads", [dict(name="qbittorrent", kind="qbittorrent", url=f"http://127.0.0.1:{dl.qbit}",
                                        username="admin", password="qbpass", config_dir=f"{tmp}/qb", container="qbittorrent")])
    srv = Server(tmp, toml, env=dict(APPS_AUTH=request.param)).start()
    c = srv.client()
    yield type("Env", (), dict(mode=request.param, store=tmp / "store", c=c))
    c.close()
    for p in (srv, dl, arr, dock):
        p.stop()


def backup(env, suite):
    rid = env.c.post(f"/api/suites/{suite}/backup").json()["run"]
    r = wait_run(env.c, rid)
    assert r["status"] == "ok", r["log"]
    v = r["result"]["version"]
    return env.c.get(f"/api/suites/{suite}/versions/{v}").json(), env.store / suite / v


def test_media(env):
    m, vdir = backup(env, "media")
    assert {"compose.yaml", ".env", "RESTORE.md"} <= set(m["standalone"]), m["standalone"]
    assert m["apps"]["radarr"]["container"]["image"] == "lscr.io/linuxserver/radarr:latest"
    y, r = (vdir / "compose.yaml").read_text(), (vdir / "RESTORE.md").read_text()
    assert (vdir / "postgres/initdb/10-restore.sh").exists(), "the stock postgres image recreates the databases"
    assert stat.S_IMODE((vdir / ".env").stat().st_mode) == 0o600
    if env.mode == "tunnel":
        assert '"127.0.0.1:7878:7878"' in y, [l for l in y.splitlines() if "7878" in l]
        assert "AuthTunnel" in r, "RESTORE.md explains the logins"
    else:
        assert '"7878:7878"' in y and "127.0.0.1:7878" not in y, [l for l in y.splitlines() if "7878" in l]
        assert "AuthTunnel" not in r


def test_downloads(env):
    m, vdir = backup(env, "downloads")
    info = m["apps"]["qbittorrent"]["container"]
    assert info["vpn"] is True and sorted(info["ports"]) == ["6881:6881", "6881:6881/udp", "8090:8090"], info
    y, r = (vdir / "compose.yaml").read_text(), (vdir / "RESTORE.md").read_text()
    assert "profiles: [downloads]" in y and "VPN" in r, "the per-app VPN client is opt-in"
    if env.mode == "tunnel":
        assert '"127.0.0.1:8090:8090"' in y and '"6881:6881"' in y and '"6881:6881/udp"' in y, \
            [l for l in y.splitlines() if "80" in l or "6881" in l]
    else:
        assert '"8090:8090"' in y and "127.0.0.1:8090" not in y
