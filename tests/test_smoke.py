"""Smoke: the main pages and read APIs answer 200 with every app unreachable."""
import pytest

from harness import Server, base_toml, suite_toml


@pytest.fixture(scope="module")
def srv(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("smoke")
    toml = base_toml(tmp) + suite_toml("media", [
        dict(name="radarr", kind="radarr", url="http://127.0.0.1:1", api_key="x"),
        dict(name="qbittorrent", kind="qbittorrent", url="http://127.0.0.1:1", username="a", password="b"),
        dict(name="seerr", kind="seerr", url="http://127.0.0.1:1", api_key="x")])
    s = Server(tmp, toml).start()
    yield s
    s.stop()


@pytest.mark.parametrize("path", ["/", "/suites", "/suites/media", "/activity", "/updates", "/indexers", "/notifications",
                                  "/connections", "/system", "/proxy", "/configs", "/static/style.css",
                                  "/api/suites", "/api/suites/media", "/api/runs", "/api/downloads", "/api/notify",
                                  "/api/connections", "/api/updates", "/api/indexers", "/api/links", "/api/configs",
                                  "/api/home/settings", "/api/home/feeds", "/api/app-logins", "/api/proxy", "/api/vpn",
                                  "/api/connect", "/api/health"])
def test_page(srv, path):
    with srv.client() as c:
        r = c.get(path)
        assert r.status_code == 200, (path, r.status_code, r.text[:300])
        if path.startswith("/api/"):
            r.json()


def test_unreachable_apps_are_reported(srv):
    with srv.client() as c:
        d = c.get("/api/downloads").json()
        q = next(x for x in d["clients"] if x["name"] == "qbittorrent")
        assert q["ok"] is False and q["error"], q
        assert isinstance(c.post("/api/notify/check").json(), dict)
        seerr = c.get("/api/connections").json()["seerr"]
        assert isinstance(seerr, dict)
        h = c.get("/api/health").json()
        assert h["ok"] is True and h["docker"] is False and "version" in h, h
