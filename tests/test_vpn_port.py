"""The VPN's forwarded port stays in qBittorrent: its image resets the listening port
(TORRENTING_PORT) whenever it restarts on its own, and gluetun only pushes the port
when it changes. wranglarr checks (WRANGLARR_VPN_PORT_POLL, shrunk here) and sets it back."""
import httpx

from harness import MockDownloads, Server, base_toml, suite_toml, wait_until


def test_qbittorrent_keeps_the_forwarded_port(tmp_path):
    dl = MockDownloads(tmp_path)
    toml = (base_toml(tmp_path)
            + f'[vpn]\ncontrol_url = "http://127.0.0.1:{dl.gluetun}"\napi_key = "gluekey"\n\n'
            + suite_toml("media", [dict(name="qbittorrent", kind="qbittorrent", url=f"http://127.0.0.1:{dl.qbit}",
                                        username="admin", password="qbpass")]))
    s = Server(tmp_path, toml, env=dict(WRANGLARR_VPN_PORT_POLL="0.2")).start()
    qb = httpx.Client(base_url=f"http://127.0.0.1:{dl.qbit}", headers={"Cookie": "SID=abc"})
    prefs = lambda: qb.get("/api/v2/app/preferences").json()
    try:
        # the mock gluetun forwards 51234; qBittorrent starts on 6881 with UPnP on
        wait_until(lambda: prefs()["listen_port"] == 51234, 15, 0.2, what="the forwarded port in qBittorrent")
        assert prefs()["upnp"] is False and prefs()["random_port"] is False
        # qBittorrent restarts on its own: the image puts 6881 back
        qb.post("/api/v2/app/setPreferences", data={"json": '{"listen_port": 6881}'}).raise_for_status()
        wait_until(lambda: prefs()["listen_port"] == 51234, 15, 0.2, what="the forwarded port set again")
        assert "set it" in s.log()
    finally:
        qb.close()
        s.stop()
        dl.stop()
