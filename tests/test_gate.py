"""AuthTunnel (APPS_AUTH=tunnel): the domain-wide session cookie, /api/auth/gate (what
wranglarr-gate asks nginx-side before every app request, incl. the apps' own API keys),
the login page's `next` handling, and logout clearing the domain cookie."""
from http.cookies import SimpleCookie
from urllib.parse import quote

import pytest

from harness import MockDownloads, Server, base_toml, free_port, suite_toml

PW = "correct horse battery"
HOST = "wranglarr.example.com"
H = {"Host": HOST, "Origin": f"https://{HOST}", "X-Forwarded-Proto": "https"}


@pytest.fixture(scope="module")
def srv(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("gate")
    toml = base_toml(tmp) + suite_toml("media", [dict(name="radarr", kind="radarr", url="http://127.0.0.1:1", api_key="x")])
    s = Server(tmp, toml, auth=True, env=dict(APPS_AUTH="tunnel", AUTH_COOKIE_DOMAIN="example.com",
                                             AUTH_LOGIN_URL=f"https://{HOST}")).start()
    yield s
    s.stop()


@pytest.fixture(scope="module")
def session(srv):
    with srv.client() as c:
        c.get("/setup", headers={"Host": HOST})
        r = c.post("/api/auth/setup", json={"code": srv.setup_code(), "username": "admin", "password": PW}, headers=H)
        assert r.status_code == 200, r.text
        raw = next(h for h in r.headers.get_list("set-cookie") if h.startswith("wranglarr_session="))
        sc = raw.lower()
        assert "domain=example.com" in sc and "samesite=lax" in sc and "secure" in sc, \
            f"tunnel: cookie for the whole domain, Lax, Secure ({sc})"
        # parsed by hand: the cookie is for example.com, and this client talks to 127.0.0.1
        return SimpleCookie(raw)["wranglarr_session"].value


G = {"X-Original-Proto": "https", "X-Original-Host": "radarr.example.com", "X-Original-URI": "/movie/1?a=b"}


def gate(srv, headers, token=None):
    with srv.client() as c:              # a fresh client each time: cookies only when given
        h = dict(G, **headers)
        if token:
            h["Cookie"] = f"wranglarr_session={token}"
        return c.get("/api/auth/gate", headers=h)


def test_gate_session(srv, session):
    assert gate(srv, {}, session).status_code == 204
    r = gate(srv, {})
    want = f"https://{HOST}/login?next=" + quote("https://radarr.example.com/movie/1?a=b", safe="")
    assert r.status_code == 401 and r.headers["x-wranglarr-login"] == want, r.headers.get("x-wranglarr-login")
    assert gate(srv, {"X-Api-Key": "x"}).status_code == 401, "an X-Api-Key header doesn't count as a session"


def test_gate_by_ip_signs_in_on_that_address(srv, session):
    # an app opened by IP and port: the domain cookie from the proxy's sign-in would never
    # reach it, so the gate sends it to wranglarr on the same address (WRANGLARR_PORT)
    r = gate(srv, {"X-Original-Proto": "http", "X-Original-Host": "192.168.1.10:7878", "X-Original-URI": "/"})
    want = "http://192.168.1.10:8787/login?next=" + quote("http://192.168.1.10:7878/", safe="")
    assert r.status_code == 401 and r.headers["x-wranglarr-login"] == want, r.headers.get("x-wranglarr-login")
    with srv.client() as c:              # signed in there, the login page sends you back to the app
        r = c.get("/login", params={"next": "http://192.168.1.10:7878/"},
                  headers={"Host": "192.168.1.10:8787", "Cookie": f"wranglarr_session={session}"})
        assert r.status_code == 303 and r.headers["location"] == "http://192.168.1.10:7878/", r.headers.get("location")


def test_gate_app_keys(srv, session):
    ga = {"X-Wranglarr-App": "radarr"}
    assert gate(srv, {**ga, "X-Wranglarr-Key": "x"}).status_code == 204, "the app's real API key (header)"
    assert gate(srv, {**ga, "X-Wranglarr-Key-Arg": "x"}).status_code == 204, "the app's real API key (apikey=)"
    assert gate(srv, {**ga, "X-Wranglarr-Key": "made-up"}).status_code == 401
    assert gate(srv, {"X-Wranglarr-App": "sonarr", "X-Wranglarr-Key": "x"}).status_code == 401, "an app wranglarr doesn't know"


@pytest.fixture
def qb_app(tmp_path):
    """AuthTunnel over a mock qBittorrent; its own server, so a lockout stays in the test."""
    dl = MockDownloads(tmp_path)
    toml = base_toml(tmp_path) + suite_toml("media", [dict(
        name="qbittorrent", kind="qbittorrent", url=f"http://127.0.0.1:{dl.qbit}", username="admin", password="qbpass")])
    s = Server(tmp_path, toml, auth=True, env=dict(APPS_AUTH="tunnel")).start()
    yield s
    s.stop()
    dl.stop()


LOGIN = "/api/auth/gate/qbittorrent/login"


def app_login(srv, username="admin", password="qbpass"):
    with srv.client() as c:              # as an app sends it: a form, no Origin, no cookies
        return c.post(LOGIN, data={"username": username, "password": password})


def app_gate(srv, cookie=None, app="qbittorrent"):
    with srv.client() as c:
        h = {"X-Wranglarr-App": app, "X-Original-Host": "nas:8090", "X-Original-URI": "/api/v2/app/version"}
        return c.get("/api/auth/gate", headers=dict(h, Cookie=cookie) if cookie else h)


def test_qbittorrent_app_signs_in_through_the_gate(qb_app):
    assert app_gate(qb_app).status_code == 401, "not signed in"
    r = app_login(qb_app)
    assert r.status_code == 200 and r.text == "Ok.", r.text
    cookie = r.headers["set-cookie"].split(";")[0]
    assert cookie == "SID=abc", "qBittorrent's own session, so its answers don't replace it"
    assert app_gate(qb_app, cookie).status_code == 204
    assert app_gate(qb_app, "SID=made-up").status_code == 401
    assert app_gate(qb_app, cookie, app="radarr").status_code == 401, "only for qBittorrent"


@pytest.mark.parametrize("username,password", [("admin", "wrong"), ("other", "qbpass"), ("admin", ""), ("", "")])
def test_qbittorrent_app_login_is_checked(qb_app, username, password):
    # the mock would refuse these too; the real one, whitelisted, says Ok. to anything
    r = app_login(qb_app, username, password)
    assert r.status_code == 401 and r.text == "Fails.", r.text
    assert "set-cookie" not in r.headers


def test_qbittorrent_app_login_locks_out(qb_app):
    for _ in range(5):
        assert app_login(qb_app, password="wrong").status_code == 401
    r = app_login(qb_app)
    assert r.status_code == 403 and "set-cookie" not in r.headers, "the right password doesn't get through a lockout"


def test_qbittorrent_app_login_when_it_is_down(tmp_path):
    toml = base_toml(tmp_path) + suite_toml("media", [dict(
        name="qbittorrent", kind="qbittorrent", url=f"http://127.0.0.1:{free_port()}", username="admin", password="qbpass")])
    s = Server(tmp_path, toml, auth=True, env=dict(APPS_AUTH="tunnel")).start()
    try:
        r = app_login(s)
        assert r.status_code == 502 and "set-cookie" not in r.headers, r.text
    finally:
        s.stop()


def test_qbittorrent_app_login_needs_authtunnel(tmp_path):
    toml = base_toml(tmp_path) + suite_toml("media", [dict(
        name="qbittorrent", kind="qbittorrent", url="http://127.0.0.1:1", username="admin", password="qbpass")])
    s = Server(tmp_path, toml, auth=True).start()
    try:
        assert app_login(s).status_code == 404
    finally:
        s.stop()


@pytest.mark.parametrize("nxt,want", [
    ("https://radarr.example.com/movie/1", "https://radarr.example.com/movie/1"),
    ("https://evil.example/", "/"),
    ("https://example.com.evil.net/", "/"),
    ("//evil.example/", "/"),
    ("https://sub.radarr.example.com/", "https://sub.radarr.example.com/"),
    ("javascript:alert(1)", "/"),
    ("https:\\\\evil.example", "/"),
])
def test_login_next(srv, session, nxt, want):
    with srv.client() as c:
        r = c.get("/login", params={"next": nxt}, headers={"Host": HOST, "Cookie": f"wranglarr_session={session}"})
        assert r.status_code == 303 and r.headers["location"] == want, (nxt, r.status_code, r.headers.get("location"))


def test_logout_clears_domain_cookie(srv, session):
    with srv.client() as c:
        r = c.post("/api/auth/logout", headers={**H, "Cookie": f"wranglarr_session={session}"})
        assert r.status_code == 200
        sc = ", ".join(r.headers.get_list("set-cookie")).lower()
        assert "domain=example.com" in sc, sc
    assert gate(srv, {}, session).status_code == 401, "the session is gone"


def test_links_follow_the_tunnels_domain(srv, session):
    # no Reverse proxy page set up: AUTH_COOKIE_DOMAIN gives each app a name under it
    with srv.client() as c:              # a session of its own: the logout test ends the shared one
        r = c.post("/api/auth/login", json={"username": "admin", "password": PW}, headers=H)
        assert r.status_code == 200, r.text
        raw = next(h for h in r.headers.get_list("set-cookie") if h.startswith("wranglarr_session="))
        token = SimpleCookie(raw)["wranglarr_session"].value

    def links(host):
        with srv.client() as c:
            r = c.get("/api/links", headers={"Host": host, "Cookie": f"wranglarr_session={token}"})
            assert r.status_code == 200, r.text
            return r.json()["links"]["radarr"]
    assert links(HOST) == {"url": "https://radarr.example.com/", "how": "proxy",
                           "note": "its name on the reverse proxy"}
    assert links("192.168.1.10:8787")["how"] != "proxy", "opened by IP: the apps open by IP and port"
