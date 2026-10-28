"""Black-box harness: runs a wranglarr server (Python or Rust, any binary) as a separate
process against mocks, and talks to it over HTTP only.  See tests/README.md."""
from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import re
import shlex
import signal
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path

import httpx

TESTS = Path(__file__).resolve().parent
REPO = TESTS.parent
MOCKS = TESTS / "mocks"
BIN = TESTS / "bin"
WRANGLARR_BIN = REPO / "wranglarr" / "target" / "debug" / "wranglarr"  # cargo build in wranglarr/
PY = sys.executable                    # runs the mocks (stdlib only)

ARR_KEY = "radarrkey0000000000000000000000ab"   # mock_arr's default API key


def command(var: str, default: str) -> list[str]:
    """The server under test: $WRANGLARR_CMD / $WRANGLARR_SETUP_CMD (a shell-style command line),
    else the debug build's `wranglarr <default>`."""
    v = os.environ.get(var, "").strip()
    return shlex.split(v) if v else [str(WRANGLARR_BIN), default]


WRANGLARR_CMD = command("WRANGLARR_CMD", "serve")
SETUP_CMD = command("WRANGLARR_SETUP_CMD", "setup")

# variables from the developer's shell that would change the server's behaviour
_STRIP_PREFIX = ("WRANGLARR_", "APPS_", "AUTH_", "COMPOSE_", "NZBGET_", "QBITTORRENT_", "POSTGRES_")
_STRIP = {"HOSTNAME", "PUID", "PGID", "UMASK", "DATA_ROOT", "DATA_ROOT_2", "TZ", "NO_COLOR", "VIRTUAL_ENV", "PYTHONPATH"}

LIVE: list["Proc"] = []                # for the failure report in conftest.py


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def base_env(**extra) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(_STRIP_PREFIX) and k not in _STRIP}
    env.update(PATH=f"{BIN}:{env.get('PATH', '/usr/bin:/bin')}", NO_COLOR="1", TZ="UTC", PYTHONUNBUFFERED="1")
    env.update({k: str(v) for k, v in extra.items() if v is not None})
    return env


class Proc:
    """A process we started, in its own process group, with its output in a log file."""

    def __init__(self, name: str, argv: list[str], log: Path, env: dict | None = None, cwd: Path | None = None):
        self.name, self.argv, self.log, self.env, self.cwd = name, argv, log, env, cwd
        self.p: subprocess.Popen | None = None

    def start(self) -> "Proc":
        self.log.parent.mkdir(parents=True, exist_ok=True)
        fh = open(self.log, "ab")
        self.p = subprocess.Popen(self.argv, stdout=fh, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                  env=self.env, cwd=self.cwd, start_new_session=True)
        fh.close()
        LIVE.append(self)
        return self

    def alive(self) -> bool:
        return self.p is not None and self.p.poll() is None

    def stop(self, timeout: float = 8) -> None:
        if self.p is None:
            return
        if self.p.poll() is None:
            try:
                os.killpg(self.p.pid, signal.SIGTERM)      # our own group (start_new_session)
            except ProcessLookupError:
                pass
            try:
                self.p.wait(timeout)
            except subprocess.TimeoutExpired:
                os.killpg(self.p.pid, signal.SIGKILL)
                self.p.wait(5)
        if self in LIVE:
            LIVE.remove(self)
        self.p = None

    def text(self) -> str:
        try:
            return self.log.read_text(errors="replace")
        except OSError:
            return ""

    def tail(self, n: int = 40) -> str:
        return "\n".join(self.text().splitlines()[-n:])


def wait_until(fn, timeout: float = 20, every: float = 0.1, what: str = "condition", proc: Proc | None = None):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        if proc is not None and not proc.alive():
            raise RuntimeError(f"{proc.name} exited while waiting for {what}:\n{proc.tail()}")
        try:
            v = fn()
            if v:
                return v
        except Exception as e:          # noqa: BLE001
            last = e
        time.sleep(every)
    raise TimeoutError(f"timed out waiting for {what} (last error: {last})" + (f"\n{proc.tail()}" if proc else ""))


# ---- mocks ----------------------------------------------------------------------------
def start_mock(tmp: Path, name: str, script: str, *args, ready=None) -> Proc:
    p = Proc(name, [PY, "-I", str(MOCKS / script), *map(str, args)], tmp / "logs" / f"{name}.log", env=base_env()).start()
    if ready:
        wait_until(ready, 10, what=f"{name} to listen", proc=p)
    return p


def port_open(port: int) -> bool:
    with socket.socket() as s:
        s.settimeout(0.2)
        return s.connect_ex(("127.0.0.1", port)) == 0


class MockDocker:
    def __init__(self, tmp: Path, seed: str = "updates"):
        self.sock = tmp / "docker.sock"
        self.proc = start_mock(tmp, "mock-docker", "mock_docker.py", self.sock, seed, ready=self.sock.exists)
        self.http = httpx.Client(transport=httpx.HTTPTransport(uds=str(self.sock)), base_url="http://docker", timeout=10)

    def state(self) -> dict:
        return self.http.get("/_mock/state").json()

    def containers(self) -> dict:
        return self.state()["containers"]

    def registry(self, **entry):
        self.http.post("/_mock/registry", json=entry).raise_for_status()

    def fail_create(self, name: str | None):
        self.http.post("/_mock/fail_create", json={"name": name}).raise_for_status()

    def stop(self):
        self.http.close()
        self.proc.stop()


class MockArr:
    def __init__(self, tmp: Path, name: str = "mock-arr", *extra):
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.proc = start_mock(tmp, name, "mock_arr.py", self.port, *extra, ready=lambda: port_open(self.port))
        self.http = httpx.Client(base_url=self.url, timeout=10)

    def state(self) -> dict:
        return self.http.get("/_mock/state").json()

    def seed(self, **res):
        self.http.post("/_mock/seed", json=res).raise_for_status()

    def fault(self, **f):
        self.http.post("/_mock/fault", json=f).raise_for_status()

    def clear_faults(self):
        self.http.delete("/_mock/fault").raise_for_status()

    def stop(self):
        self.http.close()
        self.proc.stop()


class MockDownloads:
    """qBittorrent, SABnzbd, NZBGet and gluetun in one process (mocks/mock_dl.py)."""

    def __init__(self, tmp: Path):
        self.qbit, self.sab, self.nzbget, self.gluetun = (free_port() for _ in range(4))
        self.proc = start_mock(tmp, "mock-dl", "mock_dl.py", self.qbit, self.sab, self.nzbget, self.gluetun,
                               ready=lambda: all(map(port_open, (self.qbit, self.sab, self.nzbget, self.gluetun))))

    def stop(self):
        self.proc.stop()


# ---- the server under test --------------------------------------------------------------
class Server:
    """wranglarr (WRANGLARR_CMD) with a generated config.  `toml` is the config without [server]."""

    def __init__(self, tmp: Path, toml: str, env: dict | None = None, auth: bool = False):
        self.tmp = tmp
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.origin = {"Origin": self.url}
        self.config = tmp / "wranglarr.toml"
        self.config.write_text(f'[server]\nbind = "127.0.0.1"\nport = {self.port}\n\n{toml}')
        (tmp / "pre-restore").mkdir(exist_ok=True)
        (tmp / "state").mkdir(exist_ok=True)
        self.env = base_env(WRANGLARR_CONFIG=self.config, FAKE_PG_LOG=tmp / "pg.log",
                            WRANGLARR_PRE_RESTORE=tmp / "pre-restore",
                            WRANGLARR_AUTH=None if auth else "off",
                            WRANGLARR_UPDATE_POLL="0.1", WRANGLARR_UPDATE_STABLE="0.5", **(env or {}))
        self.proc = Proc("wranglarr", WRANGLARR_CMD, tmp / "logs" / "wranglarr.log", env=self.env, cwd=tmp)

    def start(self) -> "Server":
        self.proc.start()
        wait_until(lambda: httpx.get(self.url + "/api/health", timeout=2).status_code == 200, 30,
                   what="wranglarr /api/health", proc=self.proc)
        return self

    def stop(self):
        self.proc.stop()

    def restart(self):
        self.stop()
        self.start()

    def client(self, **kw) -> httpx.Client:
        kw.setdefault("timeout", 60)
        kw.setdefault("follow_redirects", False)
        return httpx.Client(base_url=self.url, **kw)

    def log(self) -> str:
        return self.proc.text()

    def setup_code(self) -> str:
        """The one-time setup code wranglarr prints in its log while it has no account."""
        rx = re.compile(r"\b([0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4})\b")
        return wait_until(lambda: (rx.findall(self.log()) or [None])[-1], 10, what="the setup code in the log")


class SetupServer:
    """wranglarr-setup (WRANGLARR_SETUP_CMD) on a stack folder, with a known token."""

    def __init__(self, tmp: Path, target: Path, token: str = "testtoken", env: dict | None = None):
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.token = token
        self.env = base_env(WRANGLARR_SETUP_TOKEN=token, **(env or {}))
        argv = SETUP_CMD + ["--target", str(target), "--bind", "127.0.0.1", "--port", str(self.port)]
        self.proc = Proc("wranglarr-setup", argv, tmp / "logs" / f"setup-{self.port}.log", env=self.env, cwd=tmp)

    def start(self) -> "SetupServer":
        self.proc.start()
        wait_until(lambda: httpx.get(self.url + "/static/style.css", timeout=2).status_code == 200, 30,
                   what="wranglarr-setup to listen", proc=self.proc)
        return self

    def stop(self):
        self.proc.stop()

    def client(self, signed_in: bool = True, **kw) -> httpx.Client:
        kw.setdefault("timeout", 30)
        kw.setdefault("follow_redirects", False)
        # like the wizard's page: changes need its exact Origin
        kw.setdefault("headers", {"Origin": self.url})
        c = httpx.Client(base_url=self.url, **kw)
        if signed_in:
            c.cookies.set("wranglarr_setup", self.token)
        return c


# ---- helpers ------------------------------------------------------------------------------
def wait_run(c: httpx.Client, rid: int, timeout: float = 120) -> dict:
    """Poll /api/runs/{id} until it's finished."""
    def done():
        r = c.get(f"/api/runs/{rid}").json()
        return r if r["status"] not in ("queued", "running") else None
    return wait_until(done, timeout, 0.2, what=f"run {rid} to finish")


def totp(secret_b32: str, counter: int) -> str:
    """RFC 6238 (SHA-1, 6 digits, 30 s steps), as an authenticator app computes it."""
    secret_b32 = secret_b32.replace(" ", "").upper()
    key = base64.b32decode(secret_b32 + "=" * (-len(secret_b32) % 8))
    h = hmac.new(key, struct.pack(">Q", counter), hashlib.sha1).digest()
    o = h[-1] & 0x0F
    return f"{(struct.unpack('>I', h[o:o + 4])[0] & 0x7FFFFFFF) % 1_000_000:06d}"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_manifest(store: Path, suite: str, version: str, manifest: dict) -> Path:
    """Craft a backup version on disk the way wranglarr lays it out (<root>/<suite>/<version>/manifest.json)."""
    d = store / suite / version
    d.mkdir(parents=True, exist_ok=True)
    (d / "manifest.json").write_text(json.dumps({"suite": suite, "version": version, **manifest}, indent=2))
    return d


def suite_toml(name: str, apps: list[dict], **opts) -> str:
    """A [[suite]] block; each app is a dict of its keys."""
    def val(v):
        return json.dumps(v)
    out = f'[[suite]]\nname = "{name}"\n' + "".join(f"{k} = {val(v)}\n" for k, v in opts.items())
    for a in apps:
        out += "[[suite.app]]\n" + "".join(f"{k} = {val(v)}\n" for k, v in a.items())
    return out + "\n"


def base_toml(tmp: Path, pg_host: str = "127.0.0.1", connect_auto: bool = False) -> str:
    return (f'[storage]\nroot = "{tmp}/store"\nstate_db = "{tmp}/state/wranglarr.db"\n'
            f'[postgres]\nhost = "{pg_host}"\nport = 5432\nuser = "arr"\npassword = "x"\n'
            f'[connect]\nauto = {"true" if connect_auto else "false"}\n\n')
