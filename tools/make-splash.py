#!/usr/bin/env python3
"""Render the boot splash images from the brand wordmark.

  lcd_320x170.rgb565  ST7789 frame, RGB565 big-endian (the LCD framebuffer's byte order)
  oled_128x32.bits    SSD1306 frame, 1 bit per pixel, row-major, MSB first

Run from the repo root: python3 tools/make-splash.py [--preview DIR]
"""
import math, sys
from pathlib import Path
from PIL import Image, ImageEnhance, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "docs/brand/dvb-WarpAxe_wordmark.png"
OUT = ROOT / "firmware/warpaxe/src/splash"

VOID = (5, 6, 11)
DEEP = (15, 18, 38)
WARP = (123, 92, 255)


def lcd():
    w, h = 320, 170
    bg = Image.new("RGB", (w, h))
    px = bg.load()
    # Radial glow behind the mark: deep violet centre fading to void.
    cx, cy = w / 2, 62
    for y in range(h):
        for x in range(w):
            d = min(1.0, math.hypot((x - cx) / 190, (y - cy) / 95))
            k = (1 - d) ** 2
            base = [VOID[i] + (DEEP[i] - VOID[i]) * (1 - d) for i in range(3)]
            px[x, y] = tuple(int(base[i] + (WARP[i] - base[i]) * 0.22 * k) for i in range(3))
    mark = Image.open(SRC).convert("RGBA")
    mw = 300
    mark = mark.resize((mw, round(mark.height * mw / mark.width)), Image.LANCZOS)
    bg.paste(mark, ((w - mw) // 2, 14), mark)
    return bg


def rgb565(im):
    out = bytearray()
    for r, g, b in im.get_flattened_data():
        v = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)
        out += v.to_bytes(2, "big")
    return bytes(out)


def oled():
    w, h = 128, 32
    mark = Image.open(SRC).convert("RGBA")
    # Crop to the lettering (the swoosh below it turns to noise at 32 px).
    bbox = mark.getbbox()
    mark = mark.crop((bbox[0], bbox[1], bbox[2], bbox[1] + int((bbox[3] - bbox[1]) * 0.56)))
    mw = w
    mh = round(mark.height * mw / mark.width)
    mark = mark.resize((mw, mh), Image.LANCZOS)
    lum = Image.new("L", (w, h), 0)
    a = mark.getchannel("A")
    grey = ImageEnhance.Contrast(mark.convert("L")).enhance(1.6)
    # Letters: lit where opaque; their dark outline stays dark.
    lit = Image.eval(grey, lambda v: 255 if v > 115 else 0)
    lit = Image.composite(lit, Image.new("L", lit.size, 0), a.point(lambda v: 255 if v > 128 else 0))
    # Top of the frame; the firmware writes model and version below.
    top = lit.getbbox()[1] if lit.getbbox() else 0
    lum.paste(lit, (0, 1 - top))
    return lum.point(lambda v: 255 if v > 127 else 0, "1")


def bits(im):
    w, h = im.size
    out = bytearray()
    px = im.load()
    for y in range(h):
        for x0 in range(0, w, 8):
            b = 0
            for i in range(8):
                if px[x0 + i, y]:
                    b |= 0x80 >> i
            out.append(b)
    return bytes(out)


def badge(width=66):
    """The wordmark small for the LCD header: RGB565 big-endian plus an
    8-bit alpha per pixel, so it sits on any background."""
    mark = Image.open(SRC).convert("RGBA")
    mark = mark.crop(mark.getbbox())
    h = round(mark.height * width / mark.width)
    mark = mark.resize((width, h), Image.LANCZOS)
    out = bytearray()
    for r, g, b, a in mark.get_flattened_data():
        v = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)
        out += v.to_bytes(2, "big") + bytes([a])
    return width, h, bytes(out), mark


# The makers' wordmarks for the Nerd boards' boot screen (docs/brand/nerd,
# used with their authors' and the trade mark holder's consent): black
# background → alpha.
MARKS = [
    ("nerdqaxe-plusplus", 30),
    ("nerdoctaxe", 34),
    ("nerdaxe", 34),
    ("nerdqx", 38),
    ("nerdaxe-gaia", 40),
]


def maker_mark(name, height):
    src = Image.open(ROOT / f"docs/brand/nerd/{name}-wordmark.png").convert("RGB")
    w = round(src.width * height / src.height)
    src = src.resize((w, height), Image.LANCZOS)
    out, img = bytearray(), Image.new("RGBA", (w, height))
    px = img.load()
    for i, (r, g, b) in enumerate(src.get_flattened_data()):
        a = max(r, g, b)
        # The source's black is not quite black: keep its noise out.
        a = 0 if a < 28 else a
        # Un-premultiply so the edges keep their colour over any background.
        rr, gg, bb = ((c * 255 // a) if a else 0 for c in (r, g, b))
        v = ((rr >> 3) << 11) | ((gg >> 2) << 5) | (bb >> 3)
        out += v.to_bytes(2, "big") + bytes([a])
        px[i % w, i // w] = (rr, gg, bb, a)
    return w, height, bytes(out), img


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    l, o = lcd(), oled()
    (OUT / "lcd_320x170.rgb565").write_bytes(rgb565(l))
    (OUT / "oled_128x32.bits").write_bytes(bits(o))
    bw, bh, bdata, bimg = badge()
    (OUT / f"badge_{bw}x{bh}.rgb565a").write_bytes(bdata)
    print(f"badge {bw}x{bh}")
    for name, height in MARKS:
        mw, mh, mdata, _ = maker_mark(name, height)
        for old in OUT.glob(f"mark_{name}_*.rgb565a"):
            old.unlink()
        (OUT / f"mark_{name}_{mw}x{mh}.rgb565a").write_bytes(mdata)
        print(f"mark {name} {mw}x{mh}")
    if "--preview" in sys.argv:
        d = Path(sys.argv[sys.argv.index("--preview") + 1])
        l.resize((960, 510), Image.NEAREST).save(d / "splash-lcd.png")
        o.convert("L").resize((512, 128), Image.NEAREST).save(d / "splash-oled.png")
        bimg.resize((bw * 4, bh * 4), Image.NEAREST).save(d / "badge.png")


if __name__ == "__main__":
    main()
