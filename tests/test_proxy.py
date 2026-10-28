"""Reverse proxy config: every proxy kind x style renders, with and without AuthTunnel
(APPS_AUTH=tunnel sends the gated apps through wranglarr-gate), plus a few spot checks."""
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
    with srv.client() as c:
        r = c.put("/api/proxy", json={"kind": kind, "style": style, "domain": "example.com",
                                      "upstream": "docker" if docker else "host", "upstream_host": "192.168.1.10",
                                      "apps": APPS})
        assert r.status_code == 200, r.text
        j = r.json()
        assert j["settings"]["kind"] == kind and j["settings"]["apps"]["qbittorrent"] == {"expose": "lan", "sub": "qb"}
        text = j["text"]
        assert text.strip() and isinstance(j["notes"], list)
        assert "wranglarr.example.com" in text, "wranglarr always gets a name of its own"
        assert "radarr" in text
        if kind == "cloudflared":               # a tunnel is public: the LAN-only apps stay out of it
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
