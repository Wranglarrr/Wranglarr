"""Mock Docker Engine API on a unix socket: enough to exercise wranglarr's updater,
restores (stop/start) and the standalone restore stack (container inspect).

    mock_docker.py SOCKET [SEED]       SEED: updates (default) | standalone

Test controls (same socket, no Docker equivalent):
    GET  /_mock/state          containers (incl. the create body each was made from), tags, events
    POST /_mock/registry       {"ref", "digest", "version", "env"?, "health"?, "crash"?}: what a pull of ref gets
    POST /_mock/fail_create    {"name": "x"}: creating a container called x answers 500 ({"name": null} clears)
"""
import hashlib, json, os, socketserver, sys, threading, time, uuid
from http.server import BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs, unquote

def hid(s): return "sha256:" + hashlib.sha256(s.encode()).hexdigest()
def cid(): return uuid.uuid4().hex + uuid.uuid4().hex

IMAGES, TAGS, REGISTRY, CONTAINERS, EVENTS = {}, {}, {}, {}, []
FAIL_CREATE: set = set()
FAIL_STOP: set = set()
LOCK = threading.RLock()          # one request at a time: handlers mutate the dicts above


def locked(fn):
    def run(self):
        with LOCK:
            return fn(self)
    return run
NETS = {"arrs": {}}

def add_image(ref, digest, version, env=(), health=None, crash=False, exposed=None):
    iid = hid(ref + digest)
    repo = ref.rsplit(":", 1)[0]
    labels = {"org.opencontainers.image.version": version, "maintainer": "x"}
    if crash: labels["mock.crash"] = "1"
    IMAGES[iid] = {"Id": iid, "RepoTags": [ref], "RepoDigests": [f"{repo}@{digest}"], "Created": "2026-10-01T00:00:00Z",
                   "Config": {"Env": ["PATH=/usr/bin", "HOME=/root", *env], "Labels": labels, "Cmd": ["/init"],
                              "ExposedPorts": exposed or {}, "Volumes": {"/config": {}}, "Healthcheck": health}}
    for i in IMAGES.values():
        if ref in i["RepoTags"] and i["Id"] != iid: i["RepoTags"].remove(ref)
    TAGS[ref] = iid
    return iid

def resolve_image(ref):
    if ref in IMAGES: return ref
    if ref in TAGS: return TAGS[ref]
    for i in IMAGES:
        if i.startswith(ref) or i.startswith("sha256:" + ref): return i
    return None

def find(x):
    if x in CONTAINERS: return x
    for k, c in CONTAINERS.items():
        if c["Id"].startswith(x): return k
    return None

def make_container(name, body, iid):
    img = IMAGES[iid]["Config"]
    hc = body.get("HostConfig") or {}
    mode = hc.get("NetworkMode", "")
    if mode.startswith("container:"):
        if body.get("Hostname"): return None, "conflicting options: hostname and the network mode"
        if body.get("ExposedPorts") or hc.get("PortBindings"): return None, "conflicting options: port exposing and the container sharing network namespace"
        target = find(mode.split(":", 1)[1])
        if not target: return None, f"No such container: {mode}"
    env = {e.split("=", 1)[0]: e for e in img["Env"]}
    env.update({e.split("=", 1)[0]: e for e in body.get("Env") or []})
    labels = dict(img["Labels"]); labels.update(body.get("Labels") or {})
    i = cid()
    c = {"Id": i, "Name": "/" + name, "Image": iid,
         "Config": {"Image": body["Image"], "Env": list(env.values()), "Labels": labels, "Cmd": body.get("Cmd") or img["Cmd"],
                    "Hostname": body.get("Hostname") or (i[:12] if not mode.startswith("container:") else ""),
                    "ExposedPorts": {**img["ExposedPorts"], **(body.get("ExposedPorts") or {})},
                    "Volumes": {**img["Volumes"], **(body.get("Volumes") or {})},
                    "Healthcheck": body.get("Healthcheck") or img["Healthcheck"]},
         "HostConfig": hc, "State": {"Running": False, "Restarting": False, "ExitCode": 0},
         "Mounts": [{"Type": "bind", "Source": b.split(":")[0], "Destination": b.split(":")[1], "RW": True} for b in hc.get("Binds") or []]
                   + [{"Type": "volume", "Name": m["Source"], "Destination": m["Target"], "RW": not m.get("ReadOnly")} for m in hc.get("Mounts") or []],
         "NetworkSettings": {"Networks": {}}, "_body": body}
    declared = {m["Destination"] for m in c["Mounts"]}
    for v in c["Config"]["Volumes"]:
        if v not in declared:
            c["Mounts"].append({"Type": "volume", "Name": "anon-" + uuid.uuid4().hex[:8], "Destination": v, "RW": True})
    for net, ep in ((body.get("NetworkingConfig") or {}).get("EndpointsConfig") or {}).items():
        c["NetworkSettings"]["Networks"][net] = {"Aliases": (ep.get("Aliases") or []) + [i[:12]]}
    return c, None

def start(c):
    img = IMAGES[c["Image"]]["Config"]
    if img["Labels"].get("mock.crash"):
        c["State"] = {"Running": False, "Restarting": False, "ExitCode": 1}; return
    mode = c["HostConfig"].get("NetworkMode", "")
    if mode.startswith("container:") and not find(mode.split(":", 1)[1]):
        raise ValueError("cannot join network of a non running container")
    c["State"] = {"Running": True, "Restarting": False, "ExitCode": 0, "_started": time.time()}
    if c["Config"].get("Healthcheck"): c["State"]["Health"] = {"Status": "starting"}

def tick():
    for c in CONTAINERS.values():
        h = c["State"].get("Health")
        if h and h["Status"] == "starting" and time.time() - c["State"]["_started"] > 0.3: h["Status"] = "healthy"

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def address_string(self): return "uds"
    def send(self, code, obj=None, raw=None):
        self.send_response(code); self.send_header("Content-Type", "application/json"); self.end_headers()
        if raw is not None: self.wfile.write(raw)
        elif obj is not None: self.wfile.write(json.dumps(obj).encode())
    def body(self):
        n = int(self.headers.get("Content-Length") or 0); return json.loads(self.rfile.read(n)) if n else {}
    @locked
    def do_GET(self):
        tick(); u = urlparse(self.path); p = unquote(u.path)
        if p == "/_ping": return self.send(200, raw=b"OK")
        if p == "/_mock/state": return self.send(200, snapshot())
        if p == "/containers/json":
            return self.send(200, [{"Id": c["Id"], "Names": [c["Name"]], "Image": c["Config"]["Image"], "HostConfig": {"NetworkMode": c["HostConfig"].get("NetworkMode", "default")}} for c in CONTAINERS.values()])
        if p.startswith("/containers/") and p.endswith("/json"):
            k = find(p[len("/containers/"):-5])
            if not k: return self.send(404, {"message": "No such container"})
            return self.send(200, {k2: v for k2, v in CONTAINERS[k].items() if not k2.startswith("_")})
        if p.startswith("/images/") and p.endswith("/json"):
            i = resolve_image(p[len("/images/"):-5])
            return self.send(200, IMAGES[i]) if i else self.send(404, {"message": "No such image"})
        if p.startswith("/distribution/") and p.endswith("/json"):
            ref = p[len("/distribution/"):-5]
            if ref not in REGISTRY: return self.send(401, {"message": "unauthorized or not found"})
            return self.send(200, {"Descriptor": {"digest": REGISTRY[ref]["digest"]}})
        self.send(404, {"message": "nope " + p})
    @locked
    def do_POST(self):
        tick(); u = urlparse(self.path); p = unquote(u.path); q = {k: v[0] for k, v in parse_qs(u.query).items()}
        EVENTS.append(f"POST {p} {u.query}")
        if p == "/_mock/registry":
            b = self.body(); REGISTRY[b.pop("ref")] = b; return self.send(200, {})
        if p == "/_mock/fail_stop":
            n = self.body().get("name")
            FAIL_STOP.clear()
            if n: FAIL_STOP.add(n)
            return self.send(200, {})
        if p == "/_mock/fail_create":
            n = self.body().get("name")
            FAIL_CREATE.clear()
            if n: FAIL_CREATE.add(n)
            return self.send(200, {})
        if p == "/images/create":
            ref = f"{q['fromImage']}:{q['tag']}"
            r = REGISTRY.get(ref)
            if not r: return self.send(200, raw=b'{"status":"Pulling"}\n{"error":"manifest unknown"}\n')
            cur = TAGS.get(ref)
            if not cur or r["digest"] not in IMAGES[cur]["RepoDigests"][0]:
                add_image(ref, r["digest"], r["version"], r.get("env", ()), r.get("health"), r.get("crash", False))
            return self.send(200, raw=b'{"status":"Pulling"}\n{"status":"Digest: x"}\n')
        if p.startswith("/images/") and p.endswith("/tag"):
            i = resolve_image(p[len("/images/"):-4]); ref = f"{q['repo']}:{q['tag']}"
            if not i: return self.send(404, {"message": "No such image"})
            for im in IMAGES.values():
                if ref in im["RepoTags"]: im["RepoTags"].remove(ref)
            IMAGES[i]["RepoTags"].append(ref); TAGS[ref] = i; return self.send(201)
        if p == "/containers/create":
            name = q["name"]; b = self.body()
            if name in FAIL_CREATE: return self.send(500, {"message": f"mock: creating {name} fails"})
            if name in CONTAINERS: return self.send(409, {"message": f"Conflict. The container name \"/{name}\" is already in use"})
            i = resolve_image(b["Image"])
            if not i: return self.send(404, {"message": "No such image: " + b["Image"]})
            c, err = make_container(name, b, i)
            if err: return self.send(400, {"message": err})
            CONTAINERS[name] = c; return self.send(201, {"Id": c["Id"], "Warnings": []})
        if p.startswith("/networks/") and p.endswith("/connect"):
            net = p.split("/")[2]; b = self.body(); k = find(b["Container"])
            CONTAINERS[k]["NetworkSettings"]["Networks"][net] = {"Aliases": (b["EndpointConfig"].get("Aliases") or [])}
            return self.send(200, {})
        if p.startswith("/containers/"):
            parts = p.split("/"); k = find(parts[2]); action = parts[3]
            if not k: return self.send(404, {"message": "No such container"})
            c = CONTAINERS[k]
            if action == "stop":
                if k in FAIL_STOP: return self.send(500, {"message": "mock stop failed"})
                if not c["State"]["Running"]: return self.send(304)
                c["State"] = {"Running": False, "Restarting": False, "ExitCode": 0}; return self.send(204)
            if action == "start":
                if c["State"]["Running"]: return self.send(304)
                try: start(c)
                except ValueError as e: return self.send(500, {"message": str(e)})
                return self.send(204)
            if action == "rename":
                new = q["name"]
                if new in CONTAINERS: return self.send(409, {"message": "name in use"})
                CONTAINERS[new] = CONTAINERS.pop(k); CONTAINERS[new]["Name"] = "/" + new; return self.send(204)
        self.send(404, {"message": "nope " + p})
    @locked
    def do_DELETE(self):
        u = urlparse(self.path); p = unquote(u.path); EVENTS.append(f"DELETE {p} {u.query}")
        k = find(p.split("/")[2])
        if not k: return self.send(404, {"message": "No such container"})
        del CONTAINERS[k]; return self.send(204)

class S(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

def seed_updates():
    gl = add_image("qmcgaw/gluetun:v3.41.3", "sha256:" + "a1" * 32, "v3.41.3", health={"Test": ["CMD", "/gluetun-entrypoint", "healthcheck"]}, exposed={"8000/tcp": {}})
    rd = add_image("lscr.io/linuxserver/radarr:latest", "sha256:" + "b1" * 32, "5.14.0.9383-ls250", env=("PUID=911",), exposed={"7878/tcp": {}})
    # TZ=UTC in both the image and the container: a user value equal to the image default is kept
    qb = add_image("lscr.io/linuxserver/qbittorrent:latest", "sha256:" + "c1" * 32, "5.0.3-r0-ls360", env=("TZ=UTC",), exposed={"8080/tcp": {}})
    pg = add_image("postgres:17", "sha256:" + "d1" * 32, "", env=("PG_MAJOR=17",), health=None)
    lc = add_image("wranglarr:local", "sha256:" + "e1" * 32, "0.1.0"); IMAGES[lc]["RepoDigests"] = []
    REGISTRY.update({
        "qmcgaw/gluetun:v3.41.3": {"digest": "sha256:" + "a1" * 32, "version": "v3.41.3"},
        "lscr.io/linuxserver/radarr:latest": {"digest": "sha256:" + "b2" * 32, "version": "5.15.1.9463-ls251", "env": ("PUID=911", "NEW_IMAGE_DEFAULT=1")},
        "lscr.io/linuxserver/qbittorrent:latest": {"digest": "sha256:" + "c2" * 32, "version": "5.0.4-r0-ls361"},
        "postgres:17": {"digest": "sha256:" + "d2" * 32, "version": "", "env": ("PG_MAJOR=18",)},
    })
    compose = lambda svc: {"com.docker.compose.project": "arrs", "com.docker.compose.service": svc, "com.docker.compose.image": "x"}
    for name, ref, iid, hc, env, aliases in (
        ("arrs-vpn", "qmcgaw/gluetun:v3.41.3", gl, {"NetworkMode": "arrs", "PortBindings": {"7878/tcp": [{"HostPort": "7878"}]}, "CapAdd": ["NET_ADMIN"], "RestartPolicy": {"Name": "unless-stopped"}}, ["VPN_TYPE=wireguard"], ["vpn", "radarr-alias"]),
        ("radarr", ref_r := "lscr.io/linuxserver/radarr:latest", rd, {"NetworkMode": "arrs", "Binds": ["/srv/appdata/radarr:/config", "/mnt/data:/data"], "RestartPolicy": {"Name": "unless-stopped"}}, ["PUID=1000", "TZ=Europe/London"], ["radarr"]),
        ("arrs-postgres", "postgres:17", pg, {"NetworkMode": "arrs", "Binds": ["/srv/appdata/postgres:/var/lib/postgresql/data"]}, ["POSTGRES_USER=arr"], ["postgres"]),
        ("wranglarr", "wranglarr:local", lc, {"NetworkMode": "arrs"}, [], ["wranglarr"]),
    ):
        b = {"Image": ref, "Env": env, "Labels": compose(name), "HostConfig": hc, "NetworkingConfig": {"EndpointsConfig": {"arrs": {"Aliases": aliases}}}}
        c, err = make_container(name, b, iid); assert not err, err; CONTAINERS[name] = c; start(c)
    vpn_id = CONTAINERS["arrs-vpn"]["Id"]
    b = {"Image": "lscr.io/linuxserver/qbittorrent:latest", "Env": ["WEBUI_PORT=8090", "TZ=UTC"], "Labels": compose("qbittorrent"), "HostConfig": {"NetworkMode": f"container:{vpn_id}", "Binds": ["/srv/appdata/qbittorrent:/config"]}}
    c, err = make_container("qbittorrent", b, qb); assert not err, err; CONTAINERS["qbittorrent"] = c; start(c)

def seed_standalone():
    """Two plain containers for the standalone restore stack: radarr without published ports,
    and a hotio qbittorrent with its own VPN and its ports published."""
    rd = add_image("lscr.io/linuxserver/radarr:latest", "sha256:" + "b1" * 32, "5.14.0.9383-ls250", exposed={"7878/tcp": {}})
    qb = add_image("ghcr.io/hotio/qbittorrent:release", "sha256:" + "f1" * 32, "release-5.0.3", exposed={"8080/tcp": {}})
    compose = lambda svc: {"com.docker.compose.project": "arrs", "com.docker.compose.service": svc}
    for name, ref, iid, env, hc in (
        ("radarr", "lscr.io/linuxserver/radarr:latest", rd, ["PUID=1000", "PGID=1000", "TZ=UTC"],
         {"NetworkMode": "arrs", "Binds": ["/srv/appdata/radarr:/config"]}),
        ("qbittorrent", "ghcr.io/hotio/qbittorrent:release", qb, ["PUID=1000", "VPN_ENABLED=true"],
         {"NetworkMode": "arrs", "Binds": ["/srv/appdata/qbittorrent:/config"],
          "PortBindings": {"8090/tcp": [{"HostPort": "8090"}], "6881/tcp": [{"HostPort": "6881"}], "6881/udp": [{"HostPort": "6881"}]}}),
    ):
        b = {"Image": ref, "Env": env, "Labels": compose(name), "HostConfig": hc,
             "NetworkingConfig": {"EndpointsConfig": {"arrs": {"Aliases": [name]}}}}
        c, err = make_container(name, b, iid); assert not err, err; CONTAINERS[name] = c; start(c)


def snapshot():
    return {"containers": {k: {"Id": v["Id"], "Image": v["Image"], "Config": v["Config"], "HostConfig": v["HostConfig"],
                               "State": v["State"], "Mounts": v["Mounts"], "Networks": v["NetworkSettings"]["Networks"],
                               "body": v["_body"]} for k, v in CONTAINERS.items()},
            "tags": TAGS, "events": EVENTS}


if __name__ == "__main__":
    {"updates": seed_updates, "standalone": seed_standalone}[sys.argv[2] if len(sys.argv) > 2 else "updates"]()
    sock = sys.argv[1]
    if os.path.exists(sock): os.unlink(sock)
    S(sock, H).serve_forever()
