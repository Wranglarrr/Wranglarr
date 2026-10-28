"""Preview the docs site: python3 site/serve.py, then open http://127.0.0.1:8000

Serves the same files the Pages workflow publishes (this page, README.md,
ROADMAP.md, wiki/ and wranglarr/README.md) and nothing else from the repository.
"""
import functools, http.server, pathlib, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PUBLISHED = ("README.md", "ROADMAP.md", "wiki/", "wranglarr/README.md")


class Docs(http.server.SimpleHTTPRequestHandler):
    def translate_path(self, path):
        full = pathlib.Path(super().translate_path(path)).resolve()     # decoded and normalised
        rel = full.relative_to(ROOT).as_posix() if full.is_relative_to(ROOT) else None
        if rel in (".", "index.html"):
            return str(ROOT / "site" / "index.html")
        if rel is None or not any(rel == p or (p.endswith("/") and rel.startswith(p)) for p in PUBLISHED):
            return str(ROOT / "site" / "missing")
        return str(full)


port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
print(f"docs: http://127.0.0.1:{port}")
http.server.ThreadingHTTPServer(("127.0.0.1", port), functools.partial(Docs, directory=str(ROOT))).serve_forever()
