"""Mock *arr (Radarr/Sonarr-style API) for the black-box tests.

    mock_arr.py PORT [--key KEY] [--api v3] [--empty]

What it does:
  - /ping, /api/<v>/system/status, the collection counts a backup reads, the Backup
    command (queued on the first poll, completed on the second), /backup/manual/<zip>
    (a zip with config.xml and a "CONSISTENT" radarr.db), backup restore upload, restart,
    download clients (test + add + update).
  - Any other /api/<v>/<resource>[/<id>] is a small in-memory REST store (GET list/one,
    POST adds with a new id, PUT replaces, DELETE removes), so community configs can be
    applied and undone against it. --empty starts the store empty instead of the
    collection counts a backup expects.

Test controls (no API key needed):
    GET    /_mock/state     what happened: backups, restores, restarts, requests, the store
    POST   /_mock/seed      {"resource": [rows], ...} replaces those resources;
                            {"static": {"/api/v3/x": value}} serves fixed GET answers
    POST   /_mock/fault     {"method": "PUT", "prefix": "/api/v3/customformat", "status": 500,
                             "body": {...}, "delay": 0}: matching requests sleep `delay` s,
                            then (if status is set) answer status/body instead
    DELETE /_mock/fault     clears the faults
"""
import argparse
import io
import json
import threading
import time
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

ap = argparse.ArgumentParser()
ap.add_argument("port", type=int)
ap.add_argument("--key", default="radarrkey0000000000000000000000ab")
ap.add_argument("--api", default="v3")
ap.add_argument("--empty", action="store_true")
args = ap.parse_args()
KEY, API = args.key, f"/api/{args.api}"

LOCK = threading.RLock()
state = {"polls": 0, "backups": [], "clients": [], "restored": [], "restarts": 0, "nid": 10, "requests": []}
STORE: dict[str, list] = {} if args.empty else {
    "indexer": [1, 2], "qualityprofile": [1, 2, 3], "importlist": [], "notification": [1],
    "tag": [1, 2, 3, 4], "customformat": [1] * 12}
STATIC = {
    "system/status": {"version": "5.14.0.9383", "databaseType": "postgreSQL", "databaseVersion": "17.2",
                      "migrationVersion": 241, "instanceName": "Radarr", "branch": "master", "osName": "alpine"},
    "movie": [{"hasFile": True, "monitored": True, "sizeOnDisk": 5_000_000_000},
              {"hasFile": False, "monitored": True, "sizeOnDisk": 0},
              {"hasFile": True, "monitored": False, "sizeOnDisk": 2_000_000_000}],
    "rootfolder": [{"freeSpace": 900_000_000_000}],
    "health": [{"type": "warning", "message": "No indexers"}],
}
FAULTS: list[dict] = []


def zipbytes():
    b = io.BytesIO()
    with zipfile.ZipFile(b, "w") as z:
        z.writestr("config.xml", f"<Config><ApiKey>{KEY}</ApiKey></Config>")
        z.writestr("radarr.db", "CONSISTENT")
    return b.getvalue()


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def send(self, code, obj=None, raw=None, ctype="application/json"):
        body = raw if raw is not None else (json.dumps(obj).encode() if obj is not None else b"")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def auth(self):
        q = parse_qs(urlparse(self.path).query)
        if self.headers.get("X-Api-Key") == KEY or q.get("apikey", [None])[0] == KEY:
            return True
        self.send(401, {"error": "Unauthorized"})
        return False

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    # ---- dispatch ---------------------------------------------------------------
    def handle_any(self, method):
        p = urlparse(self.path).path
        data = self.body()
        if p.startswith("/_mock/"):
            with LOCK:
                return self.control(method, p, data)
        with LOCK:
            state["requests"].append(f"{method} {p}")
            fault = next((f for f in FAULTS if f.get("method", method) == method and p.startswith(f.get("prefix", ""))), None)
        if fault:
            time.sleep(float(fault.get("delay") or 0))
            if fault.get("status"):
                return self.send(int(fault["status"]), fault.get("body", {"message": "mock fault"}))
        if p == "/ping":
            return self.send(200, {"status": "OK"})
        if not self.auth():
            return
        with LOCK:
            return getattr(self, method.lower())(p, data)

    def do_GET(self): self.handle_any("GET")
    def do_POST(self): self.handle_any("POST")
    def do_PUT(self): self.handle_any("PUT")
    def do_DELETE(self): self.handle_any("DELETE")

    def control(self, method, p, data):
        if p == "/_mock/state" and method == "GET":
            return self.send(200, dict(state, store=STORE))
        if p == "/_mock/seed" and method == "POST":
            j = json.loads(data)
            STATIC.update({k.split(API + "/", 1)[-1]: v for k, v in (j.pop("static", None) or {}).items()})
            STORE.update(j)
            return self.send(200, {})
        if p == "/_mock/fault":
            if method == "DELETE":
                FAULTS.clear()
            else:
                FAULTS.append(json.loads(data))
            return self.send(200, {})
        return self.send(404, {"error": "no such control " + p})

    # ---- the app ------------------------------------------------------------------
    def rest(self, p):
        """/api/v3/customformat/2 -> ("customformat", 2); ("customformat/schema", None) for sub paths."""
        if not p.startswith(API + "/"):
            return None, None
        rest = p[len(API) + 1:]
        head, _, tail = rest.rpartition("/")
        if head and tail.isdigit():
            return head, int(tail)
        return rest, None

    def get(self, p, data):
        res, rid = self.rest(p)
        if res == "downloadclient" and rid is None:
            return self.send(200, state["clients"])
        if res == "system/backup":
            return self.send(200, state["backups"])
        if res == "command" and rid == 42:
            state["polls"] += 1
            return self.send(200, {"id": 42, "status": "queued" if state["polls"] < 2 else "completed"})
        if p.startswith("/backup/manual/"):
            return self.send(200, raw=zipbytes(), ctype="application/zip")
        if res in STATIC and rid is None:
            return self.send(200, STATIC[res])
        if res in STORE:
            rows = STORE[res]
            if rid is None:
                return self.send(200, rows)
            hit = next((r for r in rows if isinstance(r, dict) and r.get("id") == rid), None)
            return self.send(200, hit) if hit else self.send(404, {"message": "NotFound"})
        self.send(404, {"error": "nope " + p})

    def post(self, p, data):
        res, rid = self.rest(p)
        if res == "command":
            name = f"radarr_backup_v5.14.0.9383_2026.10.21_{len(state['backups'])}.zip"
            state["backups"].append({"id": 100 + len(state["backups"]), "name": name, "path": f"/backup/manual/{name}",
                                     "type": "manual", "size": 1234, "time": f"2026-10-21T10:0{len(state['backups']) % 10}:00Z"})
            state["polls"] = 0
            return self.send(201, {"id": 42, "status": "queued"})
        if res == "system/backup/restore/upload":
            state["restored"].append(len(data))
            return self.send(200, {"restartRequired": True})
        if res == "system/restart":
            state["restarts"] += 1
            return self.send(200, {"restarting": True})
        if res == "downloadclient/test":
            j = json.loads(data)
            fields = {f["name"]: f["value"] for f in j["fields"]}
            if fields.get("movieCategory") != "radarr":
                return self.send(400, [{"errorMessage": "bad category"}])
            return self.send(200, {})
        if res == "downloadclient":
            j = json.loads(data)
            j["id"] = state["nid"]; state["nid"] += 1
            state["clients"].append(j)
            return self.send(201, j)
        if res and rid is None:
            j = json.loads(data or b"{}")
            j["id"] = state["nid"]; state["nid"] += 1
            STORE.setdefault(res, []).append(j)
            return self.send(201, j)
        self.send(404, {"error": "nope " + p})

    def put(self, p, data):
        res, rid = self.rest(p)
        j = json.loads(data or b"{}")
        rows = state["clients"] if res == "downloadclient" else STORE.get(res)
        for i, r in enumerate(rows or []):
            if isinstance(r, dict) and r.get("id") == rid:
                rows[i] = dict(j, id=rid)
                return self.send(202, rows[i])
        self.send(404, {"message": "NotFound"})

    def delete(self, p, data):
        res, rid = self.rest(p)
        if res == "system/backup":
            state["backups"] = [b for b in state["backups"] if b["id"] != rid]
            return self.send(200, {})
        rows = STORE.get(res)
        if rows is not None and any(isinstance(r, dict) and r.get("id") == rid for r in rows):
            STORE[res] = [r for r in rows if not (isinstance(r, dict) and r.get("id") == rid)]
            return self.send(200, {})
        self.send(404, {"message": "NotFound"})


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", args.port), H).serve_forever()
