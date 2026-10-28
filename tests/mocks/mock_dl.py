"""One process serving mock qBittorrent, SABnzbd, NZBGet and the gluetun control server.

    mock_dl.py QBIT_PORT SAB_PORT NZBGET_PORT GLUETUN_PORT      (0: don't serve that one)

Logins: qBittorrent admin / qbpass; SABnzbd API key sabkey0000000000000000000000000ab;
NZBGet nzbget / secretpw; gluetun X-API-Key gluekey."""
import json, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs
state = {"restarts": 0}
class Base(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def out(self, code, body=b"", ctype="application/json"):
        if not isinstance(body, bytes): body = json.dumps(body).encode()
        self.send_response(code); self.send_header("Content-Type", ctype); self.end_headers(); self.wfile.write(body)
    def body(self):
        n = int(self.headers.get("Content-Length") or 0); return self.rfile.read(n) if n else b""
class Qbit(Base):
    def do_POST(self):
        p = urlparse(self.path).path; data = self.body().decode()
        if p == "/api/v2/auth/login":
            if "username=admin" in data and "password=qbpass" in data: self.send_response(200); self.send_header("Set-Cookie", "SID=abc"); self.end_headers(); self.wfile.write(b"Ok."); return
            return self.out(200, b"Fails.", "text/plain")
        self.out(404)
    def do_GET(self):
        p = urlparse(self.path).path
        if p == "/api/v2/app/version": return self.out(200, b"v5.0.3", "text/plain")
        if self.headers.get("Cookie", "").find("SID=abc") < 0: return self.out(403, b"Forbidden", "text/plain")
        if p == "/api/v2/app/webapiVersion": return self.out(200, b"2.11.2", "text/plain")
        if p == "/api/v2/torrents/info": return self.out(200, [{"state": "downloading", "completed": 100}, {"state": "stalledUP", "completed": 5000}, {"state": "pausedDL", "completed": 0}])
        self.out(404)
class Sab(Base):
    def do_GET(self):
        u = urlparse(self.path); q = parse_qs(u.query); mode = q.get("mode", [""])[0]
        if u.path != "/api": return self.out(404)
        if mode == "version": return self.out(200, {"version": "4.4.1"})
        if q.get("apikey", [""])[0] != "sabkey0000000000000000000000000ab": return self.out(200, {"status": False, "error": "API Key Incorrect"})
        if mode == "queue": return self.out(200, {"queue": {"noofslots_total": 4, "paused": False}})
        if mode == "history": return self.out(200, {"history": {"noofslots": 120}})
        if mode == "warnings": return self.out(200, {"warnings": []})
        self.out(404)
class Nzb(Base):
    def do_POST(self):
        if self.headers.get("Authorization") != "Basic bnpiZ2V0OnNlY3JldHB3": return self.out(401, b"", "text/plain")
        req = json.loads(self.body()); m = req["method"]
        res = {"version": "24.3", "listgroups": [1, 2], "history": [1] * 7, "status": {"DownloadPaused": False, "DownloadedSizeMB": 2048}}.get(m)
        self.out(200, {"result": res, "error": None, "id": req.get("id")})
class Glue(Base):
    def do_GET(self):
        if self.headers.get("X-API-Key") != "gluekey": return self.out(401, b"Unauthorized", "text/plain")
        p = urlparse(self.path).path
        r = {"/v1/vpn/status": {"status": "running"}, "/v1/publicip/ip": {"public_ip": "203.0.113.7"}, "/v1/portforward": {"port": 51234}}
        self.out(200, r[p]) if p in r else self.out(404)
for cls, port in zip((Qbit, Sab, Nzb, Glue), map(int, sys.argv[1:5])):
    if port: threading.Thread(target=ThreadingHTTPServer(("127.0.0.1", port), cls).serve_forever, daemon=True).start()
threading.Event().wait()
