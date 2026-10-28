"""The home page lists (/api/home/feeds): missing and upcoming for Sonarr / Radarr / Lidarr,
Bazarr's wanted subtitles, Prowlarr's indexers (failing first), and the home settings."""
import pytest

from harness import Server, base_toml, free_port, port_open, start_mock, suite_toml, wait_until


@pytest.fixture(scope="module")
def srv(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("feeds")
    ports = [free_port() for _ in range(5)]
    mock = start_mock(tmp, "mock-feeds", "mock_feeds.py", *ports)
    wait_until(lambda: all(map(port_open, ports)), 10, what="mock feeds")
    apps = [dict(name=k, kind=k, url=f"http://127.0.0.1:{p}", api_key="feedkey")
            for k, p in zip(("sonarr", "radarr", "lidarr", "bazarr", "prowlarr"), ports)]
    s = Server(tmp, base_toml(tmp) + suite_toml("media", apps)).start()
    yield s
    s.stop()
    mock.stop()


def feeds(c, **settings):
    """The lists, with the home settings reset and then set to `settings`."""
    c.delete("/api/home/settings")
    if settings:
        r = c.put("/api/home/settings", json=settings)
        assert r.status_code == 200, r.text
    j = c.get("/api/home/feeds", params={"refresh": "true"}).json()
    return {app: {lst["key"]: lst for lst in v["lists"]} for app, v in j["apps"].items()}


def test_missing(srv):
    with srv.client() as c:
        f = feeds(c, limit=5, missing_days=30)
        m = f["sonarr"]["missing"]
        assert m["total"] == 2 and m["all"] == 99 and [i["sub"] for i in m["items"]] == ["S01E01 · e1", "S01E02 · e2"], m
        m = feeds(c, limit=5, missing_days=0)["sonarr"]["missing"]           # 0 days: all of it
        assert m["total"] == 99 and len(m["items"]) == 5, m
        m = f["radarr"]["missing"]                       # not out yet doesn't count; only the recent ones
        assert m["total"] == 1 and m["all"] == 2 and m["items"][0]["title"] == "New (2026)" and m["items"][0]["day"], m


def test_upcoming(srv):
    with srv.client() as c:
        u = feeds(c, limit=5, release_days=30)["radarr"]["upcoming"]   # films: release_days, not days
        assert [i["sub"] for i in u["items"]] == ["in cinemas", "on disc"] and u["items"][0]["path"] == "/movie/film", u
        f = feeds(c, limit=5)
        u = f["radarr"]["upcoming"]                                      # default 90 days reaches the digital release
        assert [i["sub"] for i in u["items"]] == ["in cinemas", "on disc", "digital release"] and u["days"] == 90, u
        u = feeds(c, limit=5, release_next=2)["radarr"]["upcoming"]    # only the next N
        assert len(u["items"]) == 2 and u["total"] == 3 and u["next"] == 2, u
        e = f["sonarr"]["upcoming"]["items"][0]                          # an episode
        assert e["tag"] == "downloaded" and not e["day"] and e["sub"] == "S02E03 · T" and e["path"] == "/series/x", e
        a = f["lidarr"]["upcoming"]["items"][0]                          # an album
        assert a["title"] == "Band" and a["sub"] == "LP (ep)" and a["day"] and a["path"] == "/album/abc", a


def test_bazarr_and_prowlarr(srv):
    with srv.client() as c:
        f = feeds(c, limit=5)
        b = f["bazarr"]["missing"]
        assert b["total"] == 4 and b["items"][0]["tag"] == "English, French" and b["items"][0]["path"] == "/series/4", b
        assert b["items"][0]["sub"] == "1x02 · Two"
        assert b["items"][1] == {**b["items"][1], "title": "Film", "tag": "de", "path": "/movies/9"}, b
        p = f["prowlarr"]["indexers"]
        assert [i["title"] for i in p["items"]] == ["Z bad", "B ok", "A off"] and p["note"] == "1 failing", p
        assert [i["state"] for i in p["items"]] == ["err", "", "muted"]


def test_switching_lists_off(srv):
    with srv.client() as c:
        f = feeds(c, upcoming=False, missing=False, indexers=False)
        assert all(not lists for lists in f.values()), f


def test_settings(srv):
    with srv.client() as c:
        c.delete("/api/home/settings")
        r = c.put("/api/home/settings", json={"days": "500", "missing_days": "-3", "limit": "1", "details": "always",
                                             "queue": False, "release_days": "9999", "release_next": "0"})
        s = r.json()
        assert s["release_days"] == 365 and s["release_next"] == 1, s
        assert s["days"] == 90 and s["missing_days"] == 0 and s["limit"] == 3 and s["details"] == "always" and s["queue"] is False, s
        assert c.get("/api/home/settings").json() == s
        r = c.put("/api/home/settings", json={"details": "sideways"})
        assert r.status_code == 400 and "hover or always" in r.json()["error"], r.text
        r = c.put("/api/home/settings", json={"days": "lots"})
        assert r.status_code == 400 and "error" in r.json(), r.text
        d = c.delete("/api/home/settings").json()
        assert d["days"] == 7 and d["limit"] == 25 and d["details"] == "hover", d
