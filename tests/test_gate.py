"""AuthTunnel (APPS_AUTH=tunnel): the domain-wide session cookie, /api/auth/gate (what
wranglarr-gate asks nginx-side before every app request, incl. the apps' own API keys),
the login page's `next` handling, logout clearing the domain cookie, and qBittorrent's
own login checked by wranglarr when AUTH_QBITTORRENT_LOGIN is on (its API has no key)."""
from http.cookies import SimpleCookie
from urllib.parse import quote

import httpx
import pytest

from harness import Server, base_toml, free_port, port_open, start_mock, suite_toml

PW = "correct horse battery"
QB = {"username": "admin", "password": "a-long-qbittorrent-secret"}
GATE_SECRET = "the-gates-own-secret"
HOST = "wranglarr.example.com"
H = {"Host": HOST, "Origin": f"https://{HOST}", "X-Forwarded-Proto": "https"}


@pytest.fixture(scope="module")
def srv(tmp_path_factory):
    s = tunnel_server(tmp_path_factory.mktemp("gate"))
    yield s
    s.stop()


def tunnel_server(tmp, qb=None, style="sid", qb_password=None, **env):
    """wranglarr in tunnel mode, with a mock qBittorrent (mocks/mock_qb_gate.py) whose login
    is `qb` (or `qb_password`, when it should differ from wranglarr's copy) in `style`."""
    tmp.mkdir(parents=True, exist_ok=True)
    login = qb or QB
    port = free_port()
    mock = start_mock(tmp, "mock-qb", "mock_qb_gate.py", port, qb_password or login["password"], style,
                      ready=lambda: port_open(port))
    toml = base_toml(tmp) + suite_toml("media", [
        dict(name="radarr", kind="radarr", url="http://127.0.0.1:1", api_key="x"),
        dict(name="qbittorrent", kind="qbittorrent", url=f"http://127.0.0.1:{port}", **login)])
    env = {**dict(APPS_AUTH="tunnel", AUTH_COOKIE_DOMAIN="example.com", AUTH_LOGIN_URL=f"https://{HOST}",
                  AUTH_QBITTORRENT_LOGIN="on", WRANGLARR_GATE_SECRET=GATE_SECRET), **env}
    try:
        s = Server(tmp, toml, auth=True, env=env).start()
    except Exception:
        mock.stop()
        raise
    stop = s.stop

    def stop_both():
        stop()
        mock.stop()
    s.stop, s.qb_port, s.qb_mock = stop_both, port, mock
    return s


def qb_sessions(s):
    """The mock qBittorrent's live sessions and the ones logged out."""
    return httpx.get(f"http://127.0.0.1:{s.qb_port}/__sessions", timeout=5).json()


def make_account(s):
    with s.client() as c:
        r = c.post("/api/auth/setup", json={"code": s.setup_code(), "username": "admin", "password": PW},
                   headers=s.origin)
        assert r.status_code == 200, r.text


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


def gate(srv, headers, token=None, cookie=""):
    with srv.client() as c:              # a fresh client each time: cookies only when given
        h = dict(G, **headers)
        cookies = [f"wranglarr_session={token}"] if token else []
        if cookie:
            cookies.append(cookie)
        if cookies:
            h["Cookie"] = "; ".join(cookies)
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


# ---- qBittorrent: its own login, checked by wranglarr ------------------------------------------
# AUTH_QBITTORRENT_LOGIN=on. What a phone app does through the gate: POST /api/v2/auth/login,
# keep the session cookie, send it with every API call. The gate sends the login to
# /api/auth/gate/qbittorrent/login with WRANGLARR_GATE_SECRET and the client's address;
# wranglarr checks it, opens a session in qBittorrent (the mock here) and hands its cookie on.
# Every request then goes to /api/auth/gate with the client's cookies, and the path as
# X-Wranglarr-Path.

FROM_GATE = {"X-Wranglarr-Gate": GATE_SECRET, "X-Wranglarr-Client": "198.51.100.7",
             "X-Original-Proto": "http", "X-Original-Host": "nas:8090"}


def qb_login(srv, form, headers=None, gate_headers=FROM_GATE):
    with srv.client() as c:              # an app: no Origin, no wranglarr session
        return c.post("/api/auth/gate/qbittorrent/login", data=form, headers=dict(gate_headers, **(headers or {})))


def qb_sid(srv, form=QB, headers=None):
    r = qb_login(srv, form, headers)
    assert r.status_code == 200 and r.text == "Ok.", r.text
    raw = next(h for h in r.headers.get_list("set-cookie") if h.startswith("SID="))
    return SimpleCookie(raw)["SID"].value


def qb_gate(srv, sid, app="qbittorrent", path="/api/v2/torrents/info", token=None, name="SID"):
    h = {"X-Wranglarr-App": app, "X-Original-Host": "nas:8090", "X-Original-URI": path or "/"}
    if path is not None:
        h["X-Wranglarr-Path"] = path
    return gate(srv, h, token, f"{name}={sid}" if sid else "")


def admin_login(s, password):
    with s.client() as c:
        return c.post("/api/auth/login", json={"username": "admin", "password": password}, headers=s.origin)


def test_qbittorrent_login_opens_a_session_the_gate_accepts(srv):
    r = qb_login(srv, QB)
    assert r.status_code == 200 and r.text == "Ok.", r.text
    raw = next(h for h in r.headers.get_list("set-cookie") if h.startswith("SID="))
    assert "httponly" in raw.lower() and "secure" not in raw.lower(), raw
    sid = SimpleCookie(raw)["SID"].value
    assert sid in qb_sessions(srv)["live"], "the session is qBittorrent's own"
    assert qb_gate(srv, sid).status_code == 204, "an API call with the session goes through"
    assert qb_gate(srv, "made-up").status_code == 401, "a made-up session doesn't"
    assert qb_gate(srv, sid, app="radarr").status_code == 401, "nor does it open another app"
    assert qb_gate(srv, sid, name="QBT_SID_8090").status_code == 204, "5.1's name for it"
    with srv.client() as c:
        assert c.post("/api/auth/gate/qbittorrent/logout", headers={"Cookie": f"SID={sid}"}).status_code == 404, \
            "a logout not from the gate"
        assert qb_gate(srv, sid).status_code == 204
        r = c.post("/api/auth/gate/qbittorrent/logout", headers=dict(FROM_GATE, Cookie=f"SID={sid}"))
        assert r.status_code == 200
    assert qb_gate(srv, sid).status_code == 401, "logged out"
    assert sid in qb_sessions(srv)["ended"], "and ended in qBittorrent too"


def test_a_session_qbittorrent_handed_out_itself_doesnt_open_the_gate(srv):
    # qBittorrent lets the gate's network in without a login, so a session it opened for anyone
    # who asked it directly mustn't count: only one wranglarr opened after checking the login
    r = httpx.post(f"http://127.0.0.1:{srv.qb_port}/api/v2/auth/login", data=QB, timeout=5)
    own = SimpleCookie(r.headers["set-cookie"])["SID"].value
    assert qb_gate(srv, own).status_code == 401
    with srv.client() as c:              # nor can its logout end someone else's session
        sid = qb_sid(srv)
        c.post("/api/auth/gate/qbittorrent/logout", headers=dict(FROM_GATE, Cookie=f"SID={own}"))
        assert qb_gate(srv, sid).status_code == 204
        assert own in qb_sessions(srv)["live"], "a logout leaves sessions the gate never took to qBittorrent"


def test_qbittorrent_5_1_answers_204_with_its_own_cookie_name(tmp_path):
    s = tunnel_server(tmp_path, style="qbt")
    try:
        r = qb_login(s, QB)
        name = f"QBT_SID_{s.qb_port}"
        assert r.status_code == 204, (r.status_code, r.text)
        raw = next(h for h in r.headers.get_list("set-cookie") if h.startswith(f"{name}="))
        sid = SimpleCookie(raw)[name].value
        assert qb_gate(s, sid, name=name).status_code == 204
        assert qb_gate(s, sid, name="SID").status_code == 204, "the value is what counts"
    finally:
        s.stop()


def test_when_qbittorrent_opens_no_session_the_app_is_told(tmp_path):
    # .env's qBittorrent login doesn't match qBittorrent's: the client's login was right, but
    # there's no session to give it
    s = tunnel_server(tmp_path / "a", qb_password="what-qbittorrent-really-has")
    try:
        r = qb_login(s, QB)
        assert r.status_code == 502 and "set-cookie" not in r.headers, (r.status_code, r.text)
        assert "qBittorrent refused the login in .env" in s.log()
    finally:
        s.stop()
    s = tunnel_server(tmp_path / "b")
    try:
        s.qb_mock.stop()                 # qBittorrent is down
        r = qb_login(s, QB)
        assert r.status_code == 502 and "set-cookie" not in r.headers, (r.status_code, r.text)
    finally:
        s.stop()


def test_over_https_the_sid_is_secure(srv):
    r = qb_login(srv, QB, {"X-Original-Proto": "https", "X-Original-Host": "qbittorrent.example.com"})
    raw = next(h for h in r.headers.get_list("set-cookie") if h.startswith("SID="))
    assert "; secure" in raw.lower(), raw


def test_a_wrong_qbittorrent_login_fails_like_qbittorrent(srv):
    r = qb_login(srv, dict(QB, password="nope"))
    assert r.status_code == 200 and r.text == "Fails.", r.text
    assert not any(h.startswith("SID=") for h in r.headers.get_list("set-cookie"))
    assert qb_login(srv, {"username": "admin"}).text == "Fails.", "no password"
    assert qb_login(srv, {"password": QB["password"]}).text == "Fails.", "no user"
    with srv.client() as c:
        assert c.post("/api/auth/gate/radarr/login", data=QB, headers=FROM_GATE).status_code == 404, \
            "radarr has API keys for that"


def test_only_the_gate_can_send_a_login(srv):
    # wranglarr's own port is published too: without the gate's secret nobody picks the address
    # their guesses count against, or gets a session
    for h in [{}, {"X-Wranglarr-Gate": "wrong"}, {"X-Wranglarr-Gate": ""}]:
        r = qb_login(srv, QB, gate_headers={"X-Wranglarr-Client": "203.0.113.9", **h})
        assert r.status_code == 404, (h, r.text)


def test_a_login_from_another_site_is_refused(srv):
    # a page elsewhere that makes your browser sign in (or guess) through the gate
    for h in [{"Origin": "https://evil.example"}, {"Referer": "https://evil.example/x"}, {"Origin": "null"}]:
        r = qb_login(srv, QB, h)
        assert r.status_code == 403 and "SID=" not in r.headers.get("set-cookie", ""), (h, r.text)
    assert qb_login(srv, QB, {"Origin": "http://nas:8090"}).text == "Ok.", "qBittorrent's own page"


@pytest.mark.parametrize("path", ["/api/v2/app/setPreferences", "/api/v2/app/shutdown",
                                  "/api/v2/search/installPlugin", "/api/v2//search/INSTALLPLUGIN/",
                                  "/api/v2/search/enablePlugin", "/api/v2/search/updatePlugins",
                                  "/api/v2/search/uninstallPlugin", "", None])
def test_an_app_session_cant_run_programs(srv, session, path):
    # "run external program" (setPreferences), search plugins (Python) and shutdown stay wranglarr-only
    sid = qb_sid(srv)
    assert qb_gate(srv, sid, path=path).status_code == 403, "an app session"
    if path:
        with srv.client() as c:       # a session of its own: another test signs `session` out
            r = c.post("/api/auth/login", json={"username": "admin", "password": PW}, headers=H)
            raw = next(h for h in r.headers.get_list("set-cookie") if h.startswith("wranglarr_session="))
        token = SimpleCookie(raw)["wranglarr_session"].value
        assert qb_gate(srv, "", path=path, token=token).status_code == 204, "signed in to wranglarr"


def test_app_sessions_still_reach_the_rest(srv):
    sid = qb_sid(srv)
    for path in ["/api/v2/app/preferences", "/api/v2/torrents/add", "/api/v2/transfer/toggleSpeedLimitsMode",
                 "/api/v2/search/start"]:
        assert qb_gate(srv, sid, path=path).status_code == 204, path


def test_qbittorrent_guesses_lock_that_address_out_of_app_sign_ins_only(tmp_path):
    s = tunnel_server(tmp_path)
    try:
        make_account(s)
        here = dict(FROM_GATE, **{"X-Wranglarr-Client": "127.0.0.1"})
        for i in range(5):
            assert qb_login(s, dict(QB, password=f"nope{i}"), gate_headers=here).text == "Fails."
        r = qb_login(s, QB, gate_headers=here)
        assert r.status_code == 403 and "banned" in r.text, "locked out, even with the right login"
        assert qb_login(s, QB, {"X-Wranglarr-Client": "127.0.0.2"}).text == "Ok.", "counted per address"
        assert admin_login(s, PW).status_code == 200, "wranglarr's own sign-in from that address isn't locked"
        assert "127.0.0.1 (qbittorrent app sign-in) locked out" in s.log()
    finally:
        s.stop()


def test_an_old_password_sent_again_counts_once(tmp_path):
    s = tunnel_server(tmp_path)
    try:
        for _ in range(7):     # a phone retrying the password from before it changed
            assert qb_login(s, dict(QB, password="the-old-password")).text == "Fails."
        assert qb_login(s, QB).text == "Ok."
    finally:
        s.stop()


def test_a_right_app_login_doesnt_reset_wranglarrs_own_count(tmp_path):
    s = tunnel_server(tmp_path)
    try:
        make_account(s)
        for i in range(4):
            assert admin_login(s, f"wrong{i}").status_code == 401
        assert qb_login(s, QB, gate_headers=dict(FROM_GATE, **{"X-Wranglarr-Client": "127.0.0.1"})).text == "Ok."
        admin_login(s, "wrong4")
        assert admin_login(s, PW).status_code == 429, "the fifth wrong admin password still locks it"
    finally:
        s.stop()


def test_off_by_default(tmp_path):
    s = tunnel_server(tmp_path, AUTH_QBITTORRENT_LOGIN="")
    try:
        assert qb_login(s, QB).status_code == 404
        with s.client() as c:
            assert c.post("/api/auth/gate/qbittorrent/logout", headers=FROM_GATE).status_code == 404
    finally:
        s.stop()


def test_a_weak_qbittorrent_password_opens_nothing_unless_allowed(tmp_path):
    weak = {"username": "admin", "password": "qb-secret"}
    s = tunnel_server(tmp_path / "a", qb=weak)
    try:
        r = qb_login(s, weak)
        assert r.text == "Fails." and "SID=" not in r.headers.get("set-cookie", ""), r.text
        assert "its password in .env is empty or weak" in s.log()
    finally:
        s.stop()
    s = tunnel_server(tmp_path / "b", qb=weak, AUTH_QBITTORRENT_ALLOW_WEAK="on")
    try:
        assert qb_login(s, weak).text == "Ok.", "AUTH_QBITTORRENT_ALLOW_WEAK"
    finally:
        s.stop()
