#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Build the GitHub wiki from wiki/:

    scripts/build-wiki.py OUT_DIR

wiki/ in the repo is the source; the GitHub wiki is a copy, rebuilt by
.github/workflows/wiki.yml. Each page's links are rewritten for the wiki:
another page becomes its wiki URL, an image loads from the repo (so the
images aren't copied), and anything else in the repo opens on GitHub.
_Sidebar.md follows the docs site's sidebar (NAV in site/index.html), and
_Footer.md says where to edit.
"""
import posixpath
import re
import sys
from pathlib import Path
from urllib.parse import unquote

OWNER_REPO = "Wranglarrr/Wranglarr"
REPO = f"https://github.com/{OWNER_REPO}"
WIKI = f"{REPO}/wiki"
BRANCH = "main"
RAW = f"https://raw.githubusercontent.com/{OWNER_REPO}/{BRANCH}"

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "wiki"

# ](target) or ](target "title"); src/href/srcset="target"
MD_LINK = re.compile(r'(\]\()([^)\s]+)((?:\s+"[^"]*")?\))')
HTML_ATTR = re.compile(r'(\b(?:src|href|srcset)=")([^"]+)(")')
FENCE = re.compile(r"^\s*(```|~~~)")


def wiki_url(path, frag=""):
    """The URL for `path` (from the repo root) once it's in the wiki."""
    hash_ = f"#{frag}" if frag else ""
    if path == "wiki/Home.md":
        return WIKI + hash_
    if re.fullmatch(r"wiki/[^/]+\.md", path):
        return f"{WIKI}/{path[5:-3]}{hash_}"
    if path.startswith("wiki/images/"):
        return f"{RAW}/{path}"
    if path in ("", "README.md"):
        return REPO + (hash_ or "#readme")
    kind = "tree" if (ROOT / path).is_dir() else "blob"
    return f"{REPO}/{kind}/{BRANCH}/{path}{hash_}"


def rewrite(target, page_dir):
    """A link or image path as written in a page in `page_dir`, for the wiki."""
    if re.match(r"^[a-z][a-z0-9+.-]*:", target, re.I) or target.startswith(("//", "#")):
        return target
    path, _, frag = target.partition("#")
    full = posixpath.normpath(posixpath.join(page_dir, unquote(path)))
    if full.startswith("../"):
        sys.exit(f"build-wiki: {target} in {page_dir} points outside the repo")
    return wiki_url("" if full == "." else full, frag)


def convert(text, page_dir):
    out, fenced = [], False
    for line in text.splitlines(keepends=True):
        if FENCE.match(line):
            fenced = not fenced
        if not fenced:
            sub = lambda m: m.group(1) + rewrite(m.group(2), page_dir) + m.group(3)
            line = HTML_ATTR.sub(sub, MD_LINK.sub(sub, line))
        out.append(line)
    return "".join(out)


def sidebar(pages):
    """The docs site's sidebar as wiki Markdown, and any page it leaves out."""
    site = (ROOT / "site" / "index.html").read_text()
    nav = site[site.index("const NAV = [") : site.index("const FLAT")]
    lines, listed = [f"**[wranglarr]({WIKI})**", ""], set()
    for group, body in re.findall(r"\['([^']+)', \[(.*?)\]\],", nav, re.S):
        lines.append(f"**{group}**")
        lines.append("")
        for _, path, title in re.findall(r"\['([^']*)', '([^']+)', '([^']+)'\]", body):
            listed.add(path)
            lines.append(f"- [{title}]({wiki_url(path)})")
        lines.append("")
    rest = sorted(p for p in pages if f"wiki/{p}" not in listed)
    if rest:
        lines += ["**More**", ""]
        lines += [f"- [{p[:-3].replace('-', ' ')}]({wiki_url('wiki/' + p)})" for p in rest]
        lines.append("")
    return "\n".join(lines)


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: build-wiki.py OUT_DIR")
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    pages = sorted(p.name for p in SRC.glob("*.md"))
    for name in pages:
        (out / name).write_text(convert((SRC / name).read_text(), "wiki"))
    (out / "_Sidebar.md").write_text(sidebar(pages))
    (out / "_Footer.md").write_text(
        f"This wiki is built from [wiki/]({REPO}/tree/{BRANCH}/wiki) in the repo, "
        "and edits made here are replaced on the next build: "
        f"change the page there instead. Also at <https://wranglarrr.github.io/Wranglarr/>.\n"
    )
    print(f"build-wiki: {len(pages)} pages -> {out}")


if __name__ == "__main__":
    main()
