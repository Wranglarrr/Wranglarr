"""The login: first-run setup with the one-time code, CSRF/Origin guard, lockout, TOTP
(replay refused), recovery codes, API keys, sessions, passkey refusals, password change.

The steps run in order against one server (login on)."""
import sqlite3
import time

import pytest

from harness import Server, base_toml, suite_toml, totp

PW = "correct horse battery"
COOKIE = "wranglarr_session"


@pytest.fixture(scope="module")
def srv(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("auth")
    toml = base_toml(tmp) + suite_toml("media", [dict(name="radarr", kind="radarr", url="http://127.0.0.1:1", api_key="x")])
    s = Server(tmp, toml, auth=True).start()
    s.ctx = {}
    yield s
    s.stop()


@pytest.fixture(scope="module")
def c(srv):
    with srv.client() as c:
        yield c


def test_first_run(srv, c):
    O = srv.origin
    r = c.get("/")
    assert r.status_code == 303 and r.headers["location"] == "/setup"
    assert c.get("/api/suites").status_code == 401
    assert c.get("/api/health").json() == {"ok": True}, "health without sign-in: no details"
    assert c.get("/setup").status_code == 200
    code = srv.setup_code()
    body = {"code": code, "username": "admin", "password": PW}
    assert c.post("/api/auth/setup", json=body).status_code == 403, "setup POST without Origin refused (CSRF)"
    assert c.post("/api/auth/setup", json=dict(body, code="AAAA-BBBB-CCCC"), headers=O).status_code == 403, "wrong setup code"
    r = c.post("/api/auth/setup", json=dict(body, password="short"), headers=O)
    assert r.status_code == 400 and "error" in r.json(), "weak password refused"
    r = c.post("/api/auth/setup", json=body, headers=O)
    assert r.status_code == 200 and COOKIE in r.cookies, r.text
    sc = r.headers["set-cookie"].lower()
    assert "httponly" in sc and "samesite=strict" in sc, sc
    assert c.get("/api/suites").status_code == 200
    assert c.get("/api/health").json().get("version"), "health with session: details"
    assert c.post("/api/auth/setup", json=dict(body, username="x", password=PW + " 2"), headers=O).status_code == 409


def test_guard(srv, c):
    O = srv.origin
    assert c.post("/api/connect", json={"mode": "new"}).status_code == 403, "state change without Origin refused"
    assert c.post("/api/connect", json={"mode": "new"}, headers={"Origin": "http://evil.example"}).status_code == 403
    assert c.get("/account").status_code == 200
    assert c.post("/api/auth/logout", headers=O).status_code == 200
    assert c.get("/api/suites").status_code == 401, "logout ends the session"
    r = c.get("/suites/x")
    assert r.status_code == 303 and r.headers["location"].startswith("/login?next="), r.headers.get("location")


def test_lockout(srv, c):
    O = srv.origin
    for i in range(5):
        r = c.post("/api/auth/login", json={"username": "admin", "password": f"wrong{i}"}, headers=O)
    assert r.status_code in (401, 429), "5 wrong passwords"
    r = c.post("/api/auth/login", json={"username": "admin", "password": PW}, headers=O)
    assert r.status_code == 429, "then locked out, even with the right password"
    assert "error" in r.json()
    srv.restart()                                       # "restart wranglarr to clear it"
    r = c.post("/api/auth/login", json={"username": "ADMIN", "password": PW}, headers=O)
    assert r.status_code == 200 and COOKIE in r.cookies, "login (username case-insensitive)"


def test_totp(srv, c):
    O = srv.origin
    assert c.post("/api/auth/totp/begin", json={"password": "nope"}, headers=O).status_code == 403
    b = c.post("/api/auth/totp/begin", json={"password": PW}, headers=O).json()
    assert b["qr_svg"].startswith("<svg") and b["uri"].startswith("otpauth://totp/wranglarr:admin?"), b["uri"]
    secret = b["secret"].replace(" ", "")
    if 30 - time.time() % 30 < 4:                       # keep the steps below inside one 30 s window
        time.sleep(30 - time.time() % 30 + 0.2)
    t = int(time.time() // 30)
    wrong = "000000" if totp(secret, t) != "000000" else "111111"
    assert c.post("/api/auth/totp/enable", json={"code": wrong}, headers=O).status_code == 400
    rec = c.post("/api/auth/totp/enable", json={"code": totp(secret, t)}, headers=O).json()["recovery"]
    assert len(rec) == 10
    c.post("/api/auth/logout", headers=O)
    r = c.post("/api/auth/login", json={"username": "admin", "password": PW}, headers=O)
    assert r.json().get("mfa") and COOKIE not in r.cookies, "password alone -> asks for the code, no session yet"
    assert c.get("/api/suites").status_code == 401
    assert c.post("/api/auth/login/totp", json={"code": totp(secret, t)}, headers=O).status_code == 401, "replay refused"
    assert c.post("/api/auth/login/totp", json={"code": totp(secret, t + 1)}, headers=O).status_code == 200, "next code"
    c.post("/api/auth/logout", headers=O)
    c.post("/api/auth/login", json={"username": "admin", "password": PW}, headers=O)
    assert c.post("/api/auth/login/totp", json={"code": totp(secret, t + 1)}, headers=O).status_code == 401, "same code twice"
    assert c.post("/api/auth/login/totp", json={"code": rec[0].upper()}, headers=O).status_code == 200, "recovery code"
    c.post("/api/auth/logout", headers=O)
    c.post("/api/auth/login", json={"username": "admin", "password": PW}, headers=O)
    assert c.post("/api/auth/login/totp", json={"code": rec[0]}, headers=O).status_code == 401, "recovery code once only"
    assert c.post("/api/auth/login/totp", json={"code": rec[1]}, headers=O).status_code == 200
    me = c.get("/api/auth/me").json()
    assert me["totp"] and me["recovery_left"] == 8, me
    srv.ctx["rec"] = rec


def test_api_keys(srv, c):
    O = srv.origin
    k = c.post("/api/auth/api-keys", json={"name": "ha", "password": PW}, headers=O).json()
    assert k["key"].startswith("wr_"), k
    with srv.client() as raw:
        assert raw.get("/api/suites", headers={"X-Api-Key": k["key"]}).status_code == 200, "API key works without a session"
        assert raw.post("/api/connect", json={"mode": "new"}, headers={"X-Api-Key": k["key"]}).status_code in (200, 202, 409), \
            "API key POST needs no Origin"
        assert raw.get("/api/suites", headers={"X-Api-Key": "wr_nope"}).status_code == 401
        assert raw.post("/api/auth/api-keys", json={"name": "x", "password": PW},
                        headers={"X-Api-Key": k["key"]}).status_code == 403, "API key can't manage the account"
        assert "hash" not in str(c.get("/api/auth/me").json()["api_keys"])
        # only a hash of the key is stored: the key itself is nowhere in the state database
        db = srv.tmp / "state" / "wranglarr.db"
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        try:
            dump = "\n".join(con.iterdump())
        finally:
            con.close()
        assert k["key"] not in dump and k["key"][3:] not in dump
        assert c.delete(f"/api/auth/api-keys/{k['id']}", headers=O).status_code == 200
        assert raw.get("/api/suites", headers={"X-Api-Key": k["key"]}).status_code == 401, "revoked key stops working"


def test_sessions(srv, c):
    O, rec = srv.origin, srv.ctx["rec"]
    with srv.client() as other:
        other.post("/api/auth/login", json={"username": "admin", "password": PW}, headers=O)
        other.post("/api/auth/login/totp", json={"code": rec[2]}, headers=O)
        assert other.get("/api/suites").status_code == 200, "second browser signed in"
        assert len(c.get("/api/auth/me").json()["sessions"]) == 2
        c.post("/api/auth/sessions/end-others", headers=O)
        assert other.get("/api/suites").status_code == 401 and c.get("/api/suites").status_code == 200


def test_passkeys_refused_without_a_name_or_https(srv, c):
    r = c.post("/api/auth/passkey/register-options", json={"password": PW},
               headers={"Host": "192.168.1.5:8787", "Origin": "http://192.168.1.5:8787"})
    assert r.status_code == 400, ("passkeys refused on an IP address", r.text)
    # a host name over plain http: the ceremony starts, but finishing it is refused before the credential is looked at
    lan = {"Host": "wranglarr.lan:8787", "Origin": "http://wranglarr.lan:8787"}
    r = c.post("/api/auth/passkey/register-options", json={"password": PW}, headers=lan)
    assert r.status_code == 200 and r.json()["options"]["rp"]["id"] == "wranglarr.lan", r.text
    r = c.post("/api/auth/passkey/register", json={"flow": r.json()["flow"], "credential": {}}, headers=lan)
    assert r.status_code == 400 and "HTTPS" in r.json()["error"], ("passkeys refused over plain http (not localhost)", r.text)


def test_password_change(srv, c):
    O = srv.origin
    assert c.post("/api/auth/password", json={"current": "wrong wrong wrong", "new": "another long passphrase"},
                  headers=O).status_code == 403
    assert c.post("/api/auth/password", json={"current": PW, "new": "another long passphrase"}, headers=O).status_code == 200
    with srv.client() as fresh:
        assert fresh.post("/api/auth/login", json={"username": "admin", "password": PW}, headers=O).status_code == 401
        r = fresh.post("/api/auth/login", json={"username": "admin", "password": "another long passphrase"}, headers=O)
        assert r.status_code == 200 and r.json().get("mfa"), r.text
