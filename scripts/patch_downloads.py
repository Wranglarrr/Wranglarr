#!/usr/bin/env python3
"""Point the download clients at the shared /data mount.

Usage: patch_downloads.py <dir containing qbittorrent/, sabnzbd/, nzbget/ config dirs>

The arrs import what a client finished, so the client has to save into a folder
the arrs see at the *same path*. With everything under /data (one mount, same
path in every container) no remote path mapping is needed, and imports into
/data/media can be hardlinks. The images' defaults put downloads in /downloads
(not mounted) or, for SABnzbd, inside its own /config.

Only values still at an image default are changed, so re-running never
overwrites a path you set yourself. Environment:

    DL_APPS            which clients to patch (comma/space separated)
    TORRENTS_DIR       default /data/torrents   (qBittorrent, per-category subfolders)
    USENET_DIR         default /data/usenet     (SABnzbd / NZBGet: incomplete/, complete/)
    SABNZBD_NZB_BACKUP default nzb-backup       (inside /config: a copy of every added .nzb,
                                                 so backups hold the NZBs; "" to turn off)
    APPS_AUTH          apps | tunnel: also write qBittorrent's web login from
                       QBITTORRENT_USER / QBITTORRENT_PASSWORD (so it never needs its
                       first-start temporary password); tunnel: let the Docker networks
                       in without it (the gate signs people in; the port isn't published)
"""
import base64
import hashlib
import hmac
import os
import re
import secrets
import sys
from pathlib import Path

TORRENTS = os.environ.get("TORRENTS_DIR", "/data/torrents").rstrip("/")
USENET = os.environ.get("USENET_DIR", "/data/usenet").rstrip("/")
NZB_BACKUP = os.environ.get("SABNZBD_NZB_BACKUP", "nzb-backup")
APPS_AUTH = os.environ.get("APPS_AUTH", "manual").strip().lower()
QB_USER = os.environ.get("QBITTORRENT_USER", "admin")
QB_PASS = os.environ.get("QBITTORRENT_PASSWORD", "")
# AuthTunnel: requests from these reach qBittorrent without its login. Its web port is
# then published only by the gate, so that means the gate (and the other containers).
PRIVATE = "10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16"


def qb_hash(password: str, salt: bytes | None = None) -> str:
    """qBittorrent's WebUI\\Password_PBKDF2: PBKDF2-HMAC-SHA512, 100000 rounds, 16-byte salt."""
    salt = salt or secrets.token_bytes(16)
    key = hashlib.pbkdf2_hmac("sha512", password.encode(), salt, 100_000, 64)
    return f'"@ByteArray({base64.b64encode(salt).decode()}:{base64.b64encode(key).decode()})"'


def qb_hash_ok(stored: str, password: str) -> bool:
    m = re.match(r'^"?@ByteArray\(([^:]+):([^)]+)\)"?$', (stored or "").strip())
    if not m:
        return False
    try:
        salt = base64.b64decode(m.group(1))
        return hmac.compare_digest(qb_hash(password, salt), f'"@ByteArray({m.group(1)}:{m.group(2)})"')
    except ValueError:
        return False


class Ini:
    """Line-preserving key/value editor (qBittorrent.conf, sabnzbd.ini, nzbget.conf)."""

    def __init__(self, path: Path, sep: str):
        self.path, self.sep = path, sep
        self.lines = path.read_text().splitlines()
        self.changes: list[str] = []

    def _find(self, key: str, section: str | None):
        cur = None
        for i, line in enumerate(self.lines):
            s = line.strip()
            if s.startswith("[") and s.endswith("]"):
                cur = s[1:-1]
                continue
            if section is not None and cur != section:
                continue
            k, sep, v = line.partition(self.sep)
            if sep and k.strip() == key and not s.startswith(("#", ";")):
                return i, v.strip()
        return None, None

    def get(self, key, section=None):
        return self._find(key, section)[1]

    def set(self, key, value, section=None, only_if=None):
        """Set key=value. only_if: current values (normalised) that may be replaced."""
        i, cur = self._find(key, section)
        norm = (cur or "").strip('"').strip("'").rstrip("/")
        if only_if is not None and norm not in {x.rstrip("/") for x in only_if}:
            return False
        if cur is not None and norm == str(value).rstrip("/"):
            return False
        pad = " " if self.sep == "=" and " = " in "\n".join(self.lines[:50]) else ""
        new = f"{key}{pad}{self.sep}{pad}{value}"
        if i is not None:
            self.lines[i] = new
        else:
            at = len(self.lines)
            if section is not None:
                hdr = next((n for n, l in enumerate(self.lines) if l.strip() == f"[{section}]"), None)
                if hdr is None:
                    self.lines += ["", f"[{section}]"]
                    at = len(self.lines)
                else:
                    at = hdr + 1
            self.lines.insert(at, new)
        self.changes.append(f"{key}: {cur or '(unset)'} -> {value}")
        return True

    def save(self):
        if self.changes:
            self.path.write_text("\n".join(self.lines) + "\n")


def qbittorrent(d: Path) -> list[str]:
    conf = next((p for p in (d / "qBittorrent" / "qBittorrent.conf", d / "config" / "qBittorrent.conf") if p.exists()), None)
    if not conf:
        return ["no qBittorrent.conf yet (start it once)"]
    c = Ini(conf, "=")
    sec = "BitTorrent"
    if c.set("Session\\DefaultSavePath", f"{TORRENTS}/", sec, only_if={"", "/downloads"}):
        # fresh install: categories -> /data/torrents/<category> (the arrs set radarr/sonarr/lidarr)
        c.set("Session\\DisableAutoTMMByDefault", "false", sec)
    if c.set("Session\\TempPath", f"{TORRENTS}/incomplete/", sec,
             only_if={"", "/downloads/incomplete", "/downloads/temp", "/incomplete"}):
        c.set("Session\\TempPathEnabled", "true", sec)
    if APPS_AUTH in ("apps", "tunnel"):
        # qBittorrent 5 keeps these in [WebUI] (Password_PBKDF2=...); 4.x in [Preferences] (WebUI\\Password_PBKDF2=...)
        old = any(l.startswith("WebUI\\") for l in c.lines) and not any(l.strip() == "[WebUI]" for l in c.lines)
        sec, pre = ("Preferences", "WebUI\\") if old else ("WebUI", "")
        if QB_PASS:
            c.set(pre + "Username", QB_USER, sec)
            if not qb_hash_ok(c.get(pre + "Password_PBKDF2", sec) or "", QB_PASS):
                c.set(pre + "Password_PBKDF2", qb_hash(QB_PASS), sec)
                c.changes[-1] = "web password: set from QBITTORRENT_PASSWORD"
        else:
            c.changes.append("QBITTORRENT_PASSWORD is empty: web login left as it is")
        tunnel = APPS_AUTH == "tunnel"
        c.set(pre + "AuthSubnetWhitelistEnabled", "true" if tunnel else "false", sec)
        if tunnel:
            c.set(pre + "AuthSubnetWhitelist", PRIVATE, sec)
    c.save()
    return c.changes


def sabnzbd(d: Path) -> list[str]:
    ini = d / "sabnzbd.ini"
    if not ini.exists():
        return ["no sabnzbd.ini yet (start it once)"]
    c = Ini(ini, "=")
    c.set("download_dir", f"{USENET}/incomplete", "misc", only_if={"", '""', "Downloads/incomplete"})
    c.set("complete_dir", f"{USENET}/complete", "misc", only_if={"", '""', "Downloads/complete"})
    if NZB_BACKUP:
        c.set("nzb_backup_dir", NZB_BACKUP, "misc", only_if={"", '""'})
    # let the arrs reach it by container name (SABnzbd rejects unknown Host headers)
    wl = (c.get("host_whitelist", "misc") or "").strip('"')
    names = [x.strip() for x in wl.split(",") if x.strip()]
    if "sabnzbd" not in names:
        c.set("host_whitelist", ", ".join(names + ["sabnzbd"]), "misc")
    c.save()
    return c.changes


def nzbget(d: Path) -> list[str]:
    conf = d / "nzbget.conf"
    if not conf.exists():
        return ["no nzbget.conf yet (start it once)"]
    c = Ini(conf, "=")
    c.set("DestDir", f"{USENET}/complete", only_if={"", "/downloads/completed", "${MainDir}/completed", "${MainDir}/dst"})
    c.set("InterDir", f"{USENET}/incomplete", only_if={"", "/downloads/intermediate", "${MainDir}/intermediate",
                                                        "${MainDir}/inter"})
    c.save()
    return c.changes


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    root = Path(sys.argv[1])
    apps = [a for a in re.split(r"[,\s]+", os.environ.get("DL_APPS", "qbittorrent sabnzbd nzbget")) if a]
    for app in apps:
        fn = {"qbittorrent": qbittorrent, "sabnzbd": sabnzbd, "nzbget": nzbget}.get(app)
        if fn is None:
            continue
        d = root / app
        if not d.is_dir():
            print(f"patch_downloads: {app}: no config dir, skipped")
            continue
        changes = fn(d)
        print(f"patch_downloads: {app}: " + ("; ".join(changes) if changes else "already set, nothing to change"))


if __name__ == "__main__":
    main()
