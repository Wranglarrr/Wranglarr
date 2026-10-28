"""The first-run setup wizard (wranglarr-setup): the one-time token, secrets masked in the
answers it sends back and restored on write, and every check done before .env is touched."""
import shutil
import stat
import tomllib

import pytest

from harness import REPO, SetupServer

KEEP = "•••••• unchanged ••••••"          # what the wizard shows instead of a secret already in .env


@pytest.fixture
def wizard(tmp_path):
    """wizard(**files) -> a running wranglarr-setup on a fresh stack folder holding those files."""
    started = []

    def make(token="testtoken", **files):
        stack = tmp_path / f"stack{len(started)}"
        stack.mkdir()
        (stack / "compose.yaml").write_text("")
        for name, text in files.items():
            p = stack / name.replace("__", "/")
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        s = SetupServer(tmp_path, stack, token=token).start()
        s.stack = stack
        started.append(s)
        return s
    yield make
    for s in started:
        s.stop()


def test_token(wizard):
    w = wizard(token="abc123")
    with w.client(signed_in=False) as c:
        r = c.get("/")
        assert r.status_code == 401 and "?t=" in r.text
        assert c.get("/api/setup/defaults").status_code == 401
        assert c.post("/api/setup/write", json={}).status_code == 401
        assert c.get("/static/style.css").status_code == 200
        r = c.get("/?t=nope")
        assert r.status_code == 401 and "match" in r.text
        r = c.get("/?t=abc123")
        sc = r.headers["set-cookie"].lower()
        assert r.status_code == 303 and "wranglarr_setup" in sc and "samesite=strict" in sc and "httponly" in sc, sc
        c.cookies.set("wranglarr_setup", "abc123")
        assert c.get("/").status_code == 200
        assert c.get("/api/setup/defaults").status_code == 200


def test_secrets_masked_and_kept(wizard, tmp_path):
    env = ("POSTGRES_PASSWORD=supersecretpw\nWIREGUARD_PRIVATE_KEY=" + "A" * 43 + "=\nTZ=UTC\n"
           "COMPOSE_PROFILES=radarr\nAPPS_LOGIN_PASSWORD=appspassword123\n")
    w = wizard(**{".env": env})
    with w.client() as c:
        d = c.get("/api/setup/defaults").json()["existing_env"]
        assert d["POSTGRES_PASSWORD"] == KEEP and d["WIREGUARD_PRIVATE_KEY"] == KEEP and "supersecret" not in str(d), d
        assert d["TZ"] == "UTC", "plain values are shown"
        assert c.get("/docs").status_code == 404 and c.get("/openapi.json").status_code == 404
        r = c.post("/api/setup/check-vpn", json={"mode": "shared", "vpn_type": "wireguard", "provider": "mullvad",
                                                "private_key": KEEP})
        assert r.json()["errors"] == [], ("check-vpn uses the key from .env", r.json())
        ans = {"services": ["radarr"], "postgres_password": KEEP, "apps_auth": "manual", "data_root": str(tmp_path)}
        r = c.post("/api/setup/preview", json=ans)
        assert r.status_code == 200 and "POSTGRES_PASSWORD=••••••" in r.json().get("env", ""), r.text[:300]
        assert c.post("/api/setup/write", json={"answers": ans}).status_code == 409, ".env exists: needs overwrite"
        r = c.post("/api/setup/write", json={"answers": ans, "overwrite": True})
        new = (w.stack / ".env").read_text()
        assert r.status_code == 200 and "supersecretpw" in new and KEEP not in new, r.text[:300]
        assert stat.S_IMODE((w.stack / ".env").stat().st_mode) == 0o600
        assert list(w.stack.glob(".env.bak-*")), "the old .env is kept"
        (w.stack / "appdata-old").mkdir()
        z = w.stack / "appdata-old" / "x.zip"
        z.write_bytes(b"PK")
        r = c.post("/api/setup/write", json={"answers": dict(ans, appdata="./appdata"), "overwrite": True,
                                             "legacy_configs": {"radarr": str(z)}})
        assert r.status_code == 400 and "inside appdata" in r.text, ("a sibling of appdata is refused", r.text[:200])


def test_write_checks_everything_first(wizard, tmp_path):
    """Refused writes name the field and leave .env alone; the backup
    schedule and retention end up in the new wranglarr.toml; $ in a password is quoted."""
    example = (REPO / "wranglarr" / "wranglarr.example.toml").read_text()
    w = wizard(**{"wranglarr__wranglarr.example.toml": example})
    base = {"services": ["radarr"], "postgres_password": "pa$$'word", "appdata": "./appdata", "data_root": str(tmp_path),
            "apps_auth": "manual"}
    with w.client() as c:
        r = c.post("/api/setup/write", json={"answers": base})
        assert r.status_code == 400 and "postgres password" in r.text, r.text
        assert not (w.stack / ".env").exists(), "nothing written on a refused write"
        r = c.post("/api/setup/write", json={"answers": dict(base, postgres_password="pa$word"),
                                             "legacy_configs": {"radarr": "/nope.zip"}})
        assert r.status_code == 400 and "not inside appdata" in r.text and not (w.stack / ".env").exists(), r.text
        r = c.post("/api/setup/write", json={"answers": dict(base, postgres_password="pa$word", backup_cron="nonsense")})
        assert r.status_code == 400 and "backup schedule" in r.text and not (w.stack / ".env").exists(), r.text
        r = c.post("/api/setup/write", json={"answers": dict(base, postgres_password="pa$word", backup_cron="0 2 * * *",
                                                             keep_last=5, keep_days=9)})
        assert r.status_code == 200, r.text[:300]
    assert "POSTGRES_PASSWORD='pa$word'" in (w.stack / ".env").read_text()
    cfg = tomllib.loads((w.stack / "appdata/wranglarr/wranglarr.toml").read_text())
    assert cfg["suite"][0]["schedule"] == "0 2 * * *", cfg["suite"][0]
    assert all(s["keep_last"] == 5 and s["keep_days"] == 9 for s in cfg["suite"]), cfg["suite"]


def test_legacy_config_from_a_zip(wizard, tmp_path):
    """A zip inside appdata: its config.xml lands in the app's folder (never over a live one)."""
    import zipfile
    w = wizard()
    (w.stack / "appdata/old").mkdir(parents=True)
    z = w.stack / "appdata/old/radarr_backup.zip"
    with zipfile.ZipFile(z, "w") as zz:
        zz.writestr("config.xml", "<Config><ApiKey>legacy</ApiKey></Config>")
    ans = {"services": ["radarr"], "postgres_password": "pw-with-no-specials", "appdata": "./appdata",
           "data_root": str(tmp_path), "apps_auth": "manual"}
    with w.client() as c:
        r = c.post("/api/setup/write", json={"answers": ans, "legacy_configs": {"radarr": str(z)}})
        assert r.status_code == 200, r.text[:300]
    assert "legacy" in (w.stack / "appdata/radarr/config.xml").read_text()
    shutil.rmtree(w.stack / "appdata/old")
