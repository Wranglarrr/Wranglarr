#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Make the sized copies of the brand art that wranglarr, the wiki and the docs
site use, from logo.png (the icon) and wordmark.png (the name) in this folder.

Run after changing the art (needs Pillow):  python3 assets/brand/build.py
"""
from pathlib import Path

from PIL import Image, ImageChops, ImageFilter

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
APP = ROOT / "wranglarr/static/brand"
WIKI = ROOT / "wiki/images/brand"
SLATE = (58, 63, 81, 255)          # wranglarr's header colour
OUTLINE = (150, 100, 4, 255)       # the rope's dark outline


def cleaned(im: Image.Image, edge: int = 2) -> Image.Image:
    """Drop what the background removal left behind: faint red pixels, stray
    dots and haze away from the rope. Keep a soft edge `edge` px wide around
    the solid rope, tinted the colour of its outline so it can't fringe."""
    a = im.getchannel("A")
    body = a.point(lambda v: 255 if v >= 128 else 0)
    near = body.filter(ImageFilter.MaxFilter(2 * edge + 1))
    a = ImageChops.multiply(a, near)
    r, g, _, _ = im.split()
    # the rope is orange (green about 0.6-0.75 of red); redder pixels are debris
    red = ImageChops.subtract(r.point(lambda v: v * 0.55), g).point(lambda v: 255 if v > 0 else 0)
    not_red = ImageChops.lighter(ImageChops.invert(red), body)
    a = ImageChops.multiply(a, not_red)
    out = im.copy()
    outline = Image.new("RGBA", im.size, OUTLINE)
    soft = a.point(lambda v: 255 if 0 < v < 128 else 0)
    out.paste(outline, (0, 0), soft)
    out.putalpha(a)
    return out


def trimmed(name: str) -> Image.Image:
    im = cleaned(Image.open(HERE / name).convert("RGBA"))
    return im.crop(im.getchannel("A").getbbox())


def by_height(im: Image.Image, h: int) -> Image.Image:
    return im.resize((round(im.width * h / im.height), h), Image.LANCZOS)


def by_width(im: Image.Image, w: int) -> Image.Image:
    return im.resize((w, round(im.height * w / im.width)), Image.LANCZOS)


def square(im: Image.Image, size: int, pad: float = 0.0, bg=(0, 0, 0, 0)) -> Image.Image:
    inner = round(size * (1 - 2 * pad))
    fit = im.copy()
    fit.thumbnail((inner, inner), Image.LANCZOS)
    out = Image.new("RGBA", (size, size), bg)
    out.alpha_composite(fit, ((size - fit.width) // 2, (size - fit.height) // 2))
    return out


def save(im: Image.Image, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    im.save(path, optimize=True)
    print(f"{path.relative_to(ROOT)}  {im.width}x{im.height}  {path.stat().st_size // 1024} KB")


logo, word = trimmed("logo.png"), trimmed("wordmark.png")
# wranglarr: header (shown 36px tall) and sign-in card (56px), at 2x+
save(by_height(word, 120), APP / "wordmark.png")
save(square(logo, 32), APP / "favicon-32.png")
save(square(logo, 64), APP / "favicon-64.png")
save(square(logo, 180, pad=0.14, bg=SLATE), APP / "apple-touch-icon.png")
# wiki, README and the docs site
save(by_width(word, 900), WIKI / "wordmark.png")
save(by_height(word, 80), WIKI / "wordmark-header.png")
save(square(logo, 256), WIKI / "logo.png")
save(square(logo, 64), WIKI / "favicon-64.png")
save(square(logo, 180, pad=0.14, bg=SLATE), WIKI / "apple-touch-icon.png")
