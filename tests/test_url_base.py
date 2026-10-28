"""A browser URL base coexists with bare health, gate and webhook endpoints."""
import base64
import hashlib
import html
import json
import re
import sqlite3
from urllib.parse import parse_qs, quote, urlsplit

import pytest

from harness import Server, base_toml, suite_toml, wait_until

PW = "correct horse battery"


@pytest.fixture
def srv(tmp_path):
    toml = base_toml(tmp_path) + suite_toml("media", [
        dict(name="radarr", kind="radarr", url="http://127.0.0.1:1", api_key="x")])
    s = Server(tmp_path, toml, auth=True, env=dict(
        WRANGLARR_URL_BASE="/w/", APPS_AUTH="tunnel", AUTH_COOKIE_DOMAIN="myhouse.com",
        AUTH_LOGIN_URL="https://myhouse.com/w")).start()
    yield s
    s.stop()


def sign_up(srv, c):
    r = c.post("/w/api/auth/setup", headers=srv.origin, json={
        "code": srv.setup_code(), "username": "admin", "password": PW})
    assert r.status_code == 200, r.text
    assert "path=/;" in r.headers["set-cookie"].lower()
    return r.cookies["wranglarr_session"]


def assert_browser_urls_prefixed(text):
    urls = re.findall(r'(?:href|src|action)="(/[^\"]*)"', text)
    assert urls and all(u.startswith("/w/") for u in urls), urls
    assert '<meta name="wranglarr-base" content="/w">' in text


def test_bare_and_prefixed_pages_and_login_round_trip(srv):
    with srv.client() as c:
        for root in ("/", "/w", "/w/"):
            r = c.get(root)
            assert r.status_code == 303 and r.headers["location"] == "/w/setup"
        for prefix in ("", "/w"):
            assert c.get(prefix + "/api/health").status_code == 200
            assert c.get(prefix + "/static/app.js").status_code == 200
            assert_browser_urls_prefixed(c.get(prefix + "/setup").text)
        sign_up(srv, c)
        for path in ("/", "/w/", "/w/suites", "/w/suites/media", "/w/system", "/w/account", "/w/proxy"):
            r = c.get(path)
            assert r.status_code == 200, (path, r.text)
            assert_browser_urls_prefixed(r.text)
        assert 'src="/w/api/apps/radarr/logo"' in c.get("/w/suites").text, "imported logo macro keeps the base"
        assert c.get("/whatever/api/health").status_code == 404, "strip only on a segment boundary"
        assert c.get("/w/w/api/health").status_code == 404, "strip exactly once"

        # The event stream reaches its real route through the outer middleware.
        with c.stream("GET", "/w/api/events/stream") as r:
            assert r.status_code == 200 and "text/event-stream" in r.headers["content-type"]
            first = next(line for line in r.iter_lines() if line.startswith("data:"))
            assert json.loads(first[5:])["type"] == "hello"
        assert c.post("/w/api/auth/logout", headers=srv.origin).status_code == 200
        destination = "/w/suites/media?filter=a%26b"
        r = c.get(destination)
        assert r.status_code == 303
        assert urlsplit(r.headers["location"]).path == "/w/login"
        assert parse_qs(urlsplit(r.headers["location"]).query)["next"] == [destination]
        r = c.get(r.headers["location"])
        assert 'data-next="' + html.escape(destination, quote=True) + '"' in r.text
        assert c.post("/w/api/auth/login", json={"username": "admin", "password": PW}, headers=srv.origin).status_code == 200
        r = c.get("/w/login", params={"next": destination})
        assert r.status_code == 303 and r.headers["location"] == destination
        assert c.get("/w/login", params={"next": "//evil.example/"}).headers["location"] == "/w/"
        assert c.get("/login", params={"next": "/suites"}).headers["location"] == "/w/suites"
        # Referer can include the base; CSRF compares origins, never full page URLs.
        assert c.post("/w/api/auth/logout", headers={"Referer": srv.url + "/w/account"}).status_code == 200


def test_gate_redirects_and_bare_webhooks(srv):
    with srv.client() as c:
        sign_up(srv, c)
        assert c.post("/w/api/events/wire", headers=srv.origin).status_code == 200
        # No cookie: these endpoints remain public, with their own credentials.
        with srv.client() as internal:
            headers = {"X-Original-Proto": "https", "X-Original-Host": "myhouse.com", "X-Original-URI": "/radarr/"}
            r = internal.get("/api/auth/gate", headers=headers)
            assert r.status_code == 401
            assert r.headers["x-wranglarr-login"] == "https://myhouse.com/w/login?next=" + quote("https://myhouse.com/radarr/", safe="")
            r = internal.get("/api/auth/gate", headers={"X-Original-Proto": "http", "X-Original-Host": "192.168.1.10:7878"})
            assert r.headers["x-wranglarr-login"].startswith("http://192.168.1.10:8787/w/login?next=")
            assert internal.get("/api/health").status_code == 200

            def event_secret():
                with sqlite3.connect(srv.tmp / "state/wranglarr.db") as db:
                    row = db.execute("SELECT value FROM kv WHERE key='events:secret'").fetchone()
                return json.loads(row[0]) if row else None
            secret = wait_until(event_secret, 10, what="event credentials")
            token = hashlib.sha256(f"wranglarr-events:{secret}:radarr".encode()).hexdigest()[:32]
            auth = "Basic " + base64.b64encode(f"radarr:{token}".encode()).decode()
            r = internal.post("/api/hooks/radarr", json={"eventType": "Test"}, headers={"Authorization": auth})
            assert r.status_code == 200, r.text
            assert internal.post("/api/hooks/radarr", json={"eventType": "Test"}).status_code == 401
        status = c.get("/w/api/events").json()
        assert "/w/api/hooks/" not in status["url"], "generated internal hooks stay bare"


def test_proxy_placement_and_links_use_sibling_paths(srv):
    with srv.client() as c:
        token = sign_up(srv, c)
        for kind in ("caddy", "nginx", "traefik-file", "traefik-labels", "caddy-labels", "cloudflared", "npm"):
            r = c.put("/w/api/proxy", headers=srv.origin, json={
                "kind": kind, "domain": "myhouse.com", "upstream": "docker", "apps": {
                    "wranglarr": {"at": "path", "sub": "w", "local": False},
                    "radarr": {"at": "path", "local": False}}})
            assert r.status_code == 200, r.text
            out = r.json()
            plan = {p["name"]: p for p in out["plan"]}
            assert (plan["wranglarr"]["fqdn"], plan["wranglarr"]["path"], plan["wranglarr"]["strip"]) == ("myhouse.com", "/w", False)
            assert plan["radarr"]["path"] == "/radarr" and plan["radarr"]["fqdn"] == "myhouse.com"
            assert out["valid"] and "/w/radarr" not in out["text"]
            if kind == "npm":
                assert "Forward Hostname/IP wranglarr" in out["text"], "NPM has a default target even with no root route"
        links = c.get("/w/api/links", headers={"Host": "myhouse.com", "Cookie": f"wranglarr_session={token}"}).json()
        assert links["links"]["radarr"]["url"] == "https://myhouse.com/radarr/"
        me = links["panel"]["apps"][0]
        assert me["at"] == "path" and me["now"]["url"] == "https://myhouse.com/w/"
        assert me["direct"] is None or me["direct"].endswith(":8787/w/")


def test_javascript_prefixes_api_and_stream_urls_once():
    import shutil
    import subprocess
    from harness import REPO

    node = shutil.which("node")
    if not node:
        pytest.skip("Node is needed for the JavaScript URL regression check")
    script = r"""
const fs = require('node:fs'), vm = require('node:vm'), assert = require('node:assert/strict');
const calls = [], listeners = {};
const ctx = vm.createContext({
  document: {
    querySelector: s => s.includes('wranglarr-base') ? {content: '/w'} : s.startsWith('[id^=') ? {} : null,
    querySelectorAll: () => [], getElementById: () => null,
    addEventListener: (name, f) => { listeners[name] = f; }
  },
  location: {pathname: '/w/suites', search: '', href: 'https://myhouse.com/w/suites'},
  window: {EventSource: true}, EventSource: class { constructor(url) { calls.push(url); } close() {} },
  fetch: async url => { calls.push(url); return {ok: true, status: 200, json: async () => ({})}; },
  matchMedia: () => ({addEventListener() {}}), addEventListener() {}, URL, setTimeout, clearTimeout
});
vm.runInContext(fs.readFileSync(process.argv[1], 'utf8'), ctx);
(async () => {
  const w = vm.runInContext('W', ctx);
  assert.equal(w.url('/api/health'), '/w/api/health');
  assert.equal(w.url('/w/api/health'), '/w/api/health');
  assert.equal(w.url('/whatever'), '/w/whatever');
  assert.equal(w.url('https://myhouse.com/radarr/'), 'https://myhouse.com/radarr/');
  await w.api('GET', '/api/health');
  listeners.DOMContentLoaded();
  assert.deepEqual(calls, ['/w/api/health', '/w/api/events/stream']);
})().catch(e => { console.error(e); process.exitCode = 1; });
"""
    result = subprocess.run([node, "-e", script, str(REPO / "wranglarr/static/app.js")], capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
