"""AuthTunnel (APPS_AUTH=tunnel): the domain-wide session cookie, /api/auth/gate (what
wranglarr-gate asks nginx-side before every app request, incl. the apps' own API keys),
the login page's `next` handling, and logout clearing the domain cookie."""
from http.cookies import SimpleCookie
from urllib.parse import quote

import pytest

from harness import Server, base_toml, suite_toml

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


def test_gate_app_keys(srv, session):
    ga = {"X-Wranglarr-App": "radarr"}
    assert gate(srv, {**ga, "X-Wranglarr-Key": "x"}).status_code == 204, "the app's real API key (header)"
    assert gate(srv, {**ga, "X-Wranglarr-Key-Arg": "x"}).status_code == 204, "the app's real API key (apikey=)"
    assert gate(srv, {**ga, "X-Wranglarr-Key": "made-up"}).status_code == 401
    assert gate(srv, {"X-Wranglarr-App": "sonarr", "X-Wranglarr-Key": "x"}).status_code == 401, "an app wranglarr doesn't know"


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
