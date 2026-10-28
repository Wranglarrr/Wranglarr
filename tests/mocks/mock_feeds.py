"""Mock Sonarr, Radarr, Lidarr, Bazarr and Prowlarr for the home page lists (feeds).

    mock_feeds.py SONARR_PORT RADARR_PORT LIDARR_PORT BAZARR_PORT PROWLARR_PORT

Every app takes the API key "feedkey". Dates are relative to the time of the request,
a little away from the edges of wranglarr's windows so the order of the two clocks
doesn't matter.
"""
import json
import sys
import threading
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

KEY = "feedkey"
H1 = timedelta(hours=1)


def iso(d):
    return d.strftime("%Y-%m-%dT%H:%M:%SZ")


def sonarr(p, now):
    if p == "/api/v3/wanted/missing":
        # episode n aired n*10 days (+1 h) ago: with missing_days=30 only S01E01 and S01E02 are recent
        recs = [{"series": {"title": "S", "titleSlug": "s"}, "seasonNumber": 1, "episodeNumber": n,
                 "title": f"e{n}", "airDateUtc": iso(now - timedelta(days=n * 10) - H1)} for n in range(1, 8)]
        return {"records": recs, "totalRecords": 99}
    if p == "/api/v3/calendar":
        return [{"series": {"title": "X", "titleSlug": "x"}, "seasonNumber": 2, "episodeNumber": 3, "title": "T",
                 "airDateUtc": "2026-01-01T01:00:00Z", "hasFile": True}]


def radarr(p, now):
    if p == "/api/v3/wanted/missing":
        return {"records": [
            {"title": "Old", "year": 2001, "isAvailable": True, "releaseDate": "2001-01-01T00:00:00Z", "titleSlug": "old"},
            {"title": "New", "year": 2026, "isAvailable": True, "releaseDate": iso(now - timedelta(days=3)), "titleSlug": "new"},
            {"title": "Soon", "year": 2027, "isAvailable": False, "releaseDate": iso(now + timedelta(days=90))}]}
    if p == "/api/v3/calendar":
        return [{"title": "Film", "year": 2026, "inCinemas": iso(now + timedelta(days=2)),
                 "digitalRelease": iso(now + timedelta(days=60)), "physicalRelease": iso(now + timedelta(days=3)),
                 "hasFile": False, "titleSlug": "film"}]


def lidarr(p, now):
    if p == "/api/v1/calendar":
        return [{"artist": {"artistName": "Band"}, "title": "LP", "albumType": "EP",
                 "releaseDate": "2026-01-01T00:00:00Z", "foreignAlbumId": "abc"}]
    if p == "/api/v1/wanted/missing":
        return {"records": [], "totalRecords": 0}


def bazarr(p, now):
    if p == "/api/episodes/wanted":
        return {"data": [{"seriesTitle": "Show", "episode_number": "1x02", "episodeTitle": "Two", "sonarrSeriesId": 4,
                          "missing_subtitles": [{"name": "English", "code2": "en"}, {"name": "French"}]}], "total": 3}
    if p == "/api/movies/wanted":
        return {"data": [{"title": "Film", "radarrId": 9, "missing_subtitles": [{"code2": "de"}]}], "total": 1}
    if p == "/api/system/status":
        return {"data": {"bazarr_version": "1.5.2"}}


def prowlarr(p, now):
    if p == "/api/v1/indexer":
        return [{"id": 1, "name": "B ok", "enable": True, "protocol": "torrent"}, {"id": 2, "name": "A off", "enable": False},
                {"id": 3, "name": "Z bad", "enable": True, "protocol": "usenet", "privacy": "private"}]
    if p == "/api/v1/indexerstatus":
        return [{"indexerId": 3, "disabledTill": iso(now + H1), "mostRecentFailure": iso(now)}]


def handler(fn):
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def send(self, code, obj):
            b = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)

        def do_GET(self):
            p = urlparse(self.path).path
            if p in ("/ping", "/api/system/ping"):
                return self.send(200, {"status": "OK"})
            if (self.headers.get("X-Api-Key") or self.headers.get("X-API-KEY")) != KEY:
                return self.send(401, {"error": "Unauthorized"})
            out = fn(p, datetime.now(timezone.utc))
            self.send(200, out) if out is not None else self.send(404, {"error": "nope " + p})
    return H


for fn, port in zip((sonarr, radarr, lidarr, bazarr, prowlarr), map(int, sys.argv[1:6])):
    threading.Thread(target=ThreadingHTTPServer(("127.0.0.1", port), handler(fn)).serve_forever, daemon=True).start()
threading.Event().wait()
