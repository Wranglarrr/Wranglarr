"""A mock qBittorrent for AuthTunnel's app sign-ins: a new session for every right login.

    mock_qb_gate.py PORT PASSWORD STYLE

STYLE `sid`: qBittorrent before 5.1 (200 `Ok.` and a SID cookie); `qbt`: 5.1 and later
(204 and a QBT_SID_<port> cookie). The user is admin. GET /__sessions lists the live
sessions and the ones logged out, for the tests to check."""
import json, secrets, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PORT, PASSWORD, STYLE = int(sys.argv[1]), sys.argv[2], sys.argv[3]
NAME = "SID" if STYLE == "sid" else f"QBT_SID_{PORT}"
live, ended = set(), []


class Qb(BaseHTTPRequestHandler):
    def log_message(self, *a): pass

    def out(self, code, body=b"", headers=()):
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Type", "application/json" if body.startswith(b"{") else "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def session(self):
        for part in self.headers.get("Cookie", "").split(";"):
            k, _, v = part.strip().partition("=")
            if k == NAME and v in live:
                return v
        return None

    def do_POST(self):
        path = urlparse(self.path).path
        n = int(self.headers.get("Content-Length") or 0)
        form = {k: v[0] for k, v in parse_qs(self.rfile.read(n).decode()).items()}
        if path == "/api/v2/auth/login":
            if form.get("username") != "admin" or form.get("password") != PASSWORD:
                return self.out(200, b"Fails.")
            sid = secrets.token_urlsafe(24)
            live.add(sid)
            cookie = [("Set-Cookie", f"{NAME}={sid}; HttpOnly; SameSite=Strict; path=/")]
            return self.out(200, b"Ok.", cookie) if STYLE == "sid" else self.out(204, b"", cookie)
        if path == "/api/v2/auth/logout":
            sid = self.session()
            if sid:
                live.discard(sid)
                ended.append(sid)
            return self.out(200)
        self.out(404)

    def do_GET(self):
        if urlparse(self.path).path == "/__sessions":
            return self.out(200, json.dumps({"live": sorted(live), "ended": ended}).encode())
        self.out(200 if self.session() else 403, b"[]" if self.session() else b"Forbidden")


ThreadingHTTPServer(("127.0.0.1", PORT), Qb).serve_forever()
