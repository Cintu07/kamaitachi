"""Draws `hayate shot` json dumps as png, with the fonts windows terminal uses."""
import json
import os
import sys

from PIL import Image, ImageDraw, ImageFont

FONTS = r"C:\Windows\Fonts"
SIZE = 16
CW, CH = 9, 20


def font(name, size=SIZE):
    p = os.path.join(FONTS, name)
    return ImageFont.truetype(p, size) if os.path.exists(p) else None


mono = font("CascadiaMono.ttf")
bold = font("CascadiaMono.ttf")
try:
    bold.set_variation_by_name("Bold")
except Exception:
    pass
cjk = font("YuGothM.ttc") or font("msgothic.ttc") or mono
emoji = font("seguiemj.ttf") or mono
sym = font("seguisym.ttf") or mono
indic = font("Nirmala.ttc") or font("Nirmala.ttf") or cjk


from fontTools.ttLib import TTFont

CMAPS = {}


def cmap(name):
    """The code points a font file has glyphs for."""
    if name not in CMAPS:
        p = os.path.join(FONTS, name)
        try:
            CMAPS[name] = set(TTFont(p, fontNumber=0).getBestCmap()) if os.path.exists(p) else set()
        except Exception:
            CMAPS[name] = set()
    return CMAPS[name]


def pick(text, b):
    """The first font with the glyph, in roughly windows terminal's fallback order."""
    c = ord(text[0])
    if c >= 0x1F000 or "\ufe0f" in text or "\u200d" in text:
        return emoji
    if c in cmap("CascadiaMono.ttf"):
        return bold if b else mono
    if 0x0C00 <= c <= 0x0C7F:
        return indic
    if c in cmap("YuGothM.ttc") or c in cmap("msgothic.ttc"):
        return cjk
    if c in cmap("seguisym.ttf"):
        return sym
    return emoji


def draw(path, out):
    d = json.load(open(path, encoding="utf-8"))
    img = Image.new("RGB", (d["w"] * CW, d["h"] * CH))
    g = ImageDraw.Draw(img)
    for y, row in enumerate(d["rows"]):
        x = 0
        for text, fg, bg, attr, width in row:
            g.rectangle([x * CW, y * CH, (x + width) * CW - 1, (y + 1) * CH - 1], fill=bg)
            if text.strip():
                f = pick(text, attr & 1)
                g.text((x * CW, y * CH + 1), text, font=f, fill=fg, embedded_color=(f is emoji))
                if attr & 8:
                    g.line([x * CW, (y + 1) * CH - 3, (x + width) * CW - 1, (y + 1) * CH - 3], fill=fg)
            x += width
    img.save(out)


for p in sys.argv[1:]:
    draw(p, p[:-5] + ".png")
    print(p[:-5] + ".png")
