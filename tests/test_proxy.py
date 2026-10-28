"""Reverse proxy config: every proxy kind x style renders, with and without AuthTunnel
(APPS_AUTH=tunnel sends the gated apps through wranglarr-gate), plus a few spot checks."""
import copy

import pytest

from harness import Server, base_toml, suite_toml

KINDS = ("caddy", "nginx", "traefik-file", "traefik-labels", "caddy-labels", "cloudflared", "npm")
APPS = {"radarr": {"expose": "public"}, "sabnzbd": {"expose": "lan"}, "qbittorrent": {"expose": "lan", "sub": "qb"},
        "wranglarr": {"expose": "public"}}


@pytest.fixture(scope="module", params=["", "tunnel"], ids=["plain", "tunnel"])
def srv(request, tmp_path_factory):
    tmp = tmp_path_factory.mktemp("proxy")
    toml = base_toml(tmp) + suite_toml("media", [
        dict(name="radarr", kind="radarr", url="http://radarr:7878", api_key="x"),
        dict(name="sabnzbd", kind="sabnzbd", url="http://sabnzbd:8080", api_key="x"),
        dict(name="qbittorrent", kind="qbittorrent", url="http://qbittorrent:8090", username="a", password="b")])
    s = Server(tmp, toml, env=dict(APPS_AUTH=request.param)).start()
    s.tunnel = bool(request.param)
    yield s
    s.stop()


@pytest.mark.parametrize("style", ["subdomain", "path"])
@pytest.mark.parametrize("kind", KINDS)
def test_render(srv, kind, style):
    docker = "labels" in kind
    apps = copy.deepcopy(APPS)
    if kind == "cloudflared":
        for name in ("sabnzbd", "qbittorrent"):
            apps[name]["expose"] = "off"
    elif kind == "npm" and style == "path":
        for app in apps.values():
            app["expose"] = "public"
    with srv.client() as c:
        r = c.put("/api/proxy", json={"kind": kind, "style": style, "domain": "example.com",
                                      "upstream": "docker" if docker else "host", "upstream_host": "192.168.1.10",
                                      "apps": apps})
        assert r.status_code == 200, r.text
        j = r.json()
        assert j["settings"]["kind"] == kind
        assert j["settings"]["apps"]["qbittorrent"] == {"at": "direct" if kind == "cloudflared" else "", "local": apps["qbittorrent"]["expose"] != "public", "sub": "qb"}, "the older form still reads"
        text = j["text"]
        assert text.strip() and isinstance(j["notes"], list)
        assert "wranglarr.example.com" in text, "wranglarr always gets a name of its own"
        assert "radarr" in text
        if kind == "cloudflared":               # local apps explicitly use Direct IP
            assert "qb.example.com" not in text and "sabnzbd" not in text
        else:
            assert ("qb.example.com" if style == "subdomain" else "/qb") in text, "the custom name for qbittorrent"
        if not docker:
            assert "192.168.1.10" in text, "host upstream: the address the proxy reaches the apps at"
        if docker and kind == "traefik-labels":
            assert ("wranglarr-gate:" in text) == srv.tunnel, "AuthTunnel: the apps' labels go on wranglarr-gate"
        assert c.get("/api/proxy").json()["settings"]["kind"] == kind, "saved"


def test_bad_values(srv):
    with srv.client() as c:
        for body in ({"kind": "apache"}, {"style": "sideways"}, {"domain": "https://example.com/x"},
                     {"upstream": "moon"}, {"lan": "not-a-range"}, {"apps": {"radarr": {"expose": "everyone"}}}):
            r = c.put("/api/proxy", json=body)
            assert r.status_code == 400 and r.json().get("error"), (body, r.text)


def test_links_follow_the_proxy_when_wranglarr_is_opened_through_it(srv):
    with srv.client() as c:
        c.delete("/api/links")
        r = c.put("/api/proxy", json={"kind": "caddy", "style": "subdomain", "domain": "example.com",
                                      "upstream": "host", "upstream_host": "",
                                      "apps": {"radarr": {"expose": "public", "sub": "movies"},
                                                                   "sabnzbd": {"expose": "off"}}})
        assert r.status_code == 200, r.text
        proxied = c.get("/api/links", headers={"Host": "wranglarr.example.com"}).json()["links"]
        assert proxied["radarr"]["url"] == "https://movies.example.com/" and proxied["radarr"]["how"] == "proxy"
        assert proxied["qbittorrent"]["url"] == "https://qbittorrent.example.com/", "not set: proxied, LAN only"
        assert proxied["sabnzbd"]["how"] == "none" and "local only" in proxied["sabnzbd"]["note"], \
            "not proxied, and no local address known: no link, rather than wranglarr's name and a port"
        # the Reverse proxy page's "this machine's address" is the local one
        assert c.put("/api/proxy", json={"upstream_host": "192.168.1.20"}).status_code == 200
        sab = c.get("/api/links", headers={"Host": "wranglarr.example.com"}).json()["links"]["sabnzbd"]
        assert sab["url"] == "http://192.168.1.20:8080/" and "local only" in sab["note"], sab
        # opened by IP: the apps open by IP and port, as before
        direct = c.get("/api/links", headers={"Host": "192.168.1.10:8787"}).json()["links"]
        assert direct["radarr"]["how"] != "proxy" and "192.168.1.10" in (direct["radarr"]["url"] or "192.168.1.10")
        # the Links panel's address: this machine's, for the apps without a name on the proxy
        r = c.put("/api/links", json={"mode": "port", "scheme": "http", "host": "10.0.0.5", "pattern": "", "apps": {}})
        assert r.status_code == 200, r.text
        fixed = c.get("/api/links", headers={"Host": "wranglarr.example.com"}).json()["links"]
        assert fixed["radarr"]["url"] == "https://movies.example.com/", "the proxy's names still come first"
        sab = fixed["sabnzbd"]
        assert sab["url"] == "http://10.0.0.5:8080/" and "local only" in sab["note"], sab
        # the Links panel: wranglarr first, then each app with its place, port and addresses
        panel = c.get("/api/links", headers={"Host": "wranglarr.example.com"}).json()["panel"]
        names = [a["name"] for a in panel["apps"]]
        assert names[0] == "wranglarr" and "sabnzbd" in names, names
        sab = next(a for a in panel["apps"] if a["name"] == "sabnzbd")
        assert (sab["at"], sab["port"], sab["direct"]) == ("direct", 8080, "http://10.0.0.5:8080/"), sab
        assert panel["domain"] == "example.com" and panel["this_machine"] == "10.0.0.5", panel
        # an app under wranglarr's name, local only
        r = c.put("/api/proxy", json={"apps": {"wranglarr": {"at": "subdomain", "local": False},
                                               "radarr": {"at": "subdomain", "local": False, "sub": "movies"},
                                               "sabnzbd": {"at": "path", "local": True}}})
        assert r.status_code == 200, r.text
        sab = c.get("/api/links", headers={"Host": "wranglarr.example.com"}).json()["links"]["sabnzbd"]
        assert sab["url"] == "https://wranglarr.example.com/sabnzbd/" and sab["how"] == "proxy", sab
        c.delete("/api/links")


def test_proxy_setup_command_prints_the_saved_config(srv):
    """`docker compose exec -T wranglarr proxy-setup > file`: the page's text on stdout,
    the notes on stderr, and --kind for another proxy without saving it."""
    import subprocess

    from harness import WRANGLARR_CMD
    cmd = list(WRANGLARR_CMD)[:-1] + ["proxy-setup"]
    run = lambda *a: subprocess.run(cmd + list(a), env=srv.env, capture_output=True, text=True, timeout=60)
    with srv.client() as c:
        r = c.put("/api/proxy", json={"kind": "caddy", "domain": "example.com", "upstream": "host",
                                      "upstream_host": "192.168.1.10", "apps": APPS})
        assert r.status_code == 200, r.text
        page = c.get("/api/proxy").json()["text"]
        p = run()
        assert p.returncode == 0, p.stderr
        assert p.stdout == page, "the same text as the Reverse proxy page"
        assert p.stderr.startswith("# "), "the notes, as comments, on stderr"
        n = run("--kind", "nginx")
        assert n.returncode == 0 and "server_name" in n.stdout and "192.168.1.10" in n.stdout, n.stderr
        assert c.get("/api/proxy").json()["settings"]["kind"] == "caddy", "--kind isn't saved"
        bad = run("--kind", "apache")
        assert bad.returncode == 2 and "one of caddy" in bad.stderr, bad.stderr
