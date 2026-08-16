#!/usr/bin/env python3
"""Build every icon the desktop app, the Microsoft Store package and the landing
page need, from the two source images in `brand/`.

Written against the Python standard library only (zlib + struct), for the same
reason the rest of this repo has no dependencies: adding Pillow or ImageMagick
would make regenerating the icons impossible on a machine that cannot install
things, and these outputs are committed artifacts that anyone may need to
rebuild. Everything here — PNG decode, background removal, resampling, ICO and
ICNS containers — is a few dozen lines of format handling.

    python3 scripts/icons_from_source.py            # both sources
    python3 scripts/icons_from_source.py --app-only
    python3 scripts/icons_from_source.py --self-test   # codec + resampling, no I/O

Outputs (all overwritten in place):

    src-tauri/icons/            32x32, 128x128, 128x128@2x, icon.png, .ico, .icns
    src-tauri/msstore/assets/   Square44x44Logo, Square150x150Logo, StoreLogo
    ui/assets/                  icon.png (the in-app titlebar mark)
    ../web-lp/assets/           icon.png (header/OGP mark), favicon.ico
"""

import argparse
import os
import struct
import sys
import zlib
from collections import deque

HERE = os.path.dirname(os.path.abspath(__file__))
DESKTOP = os.path.join(HERE, "..")
REPO = os.path.join(DESKTOP, "..", "..")

BRAND = os.path.join(REPO, "brand")
ICONS_OUT = os.path.join(DESKTOP, "src-tauri", "icons")
MSSTORE_OUT = os.path.join(DESKTOP, "src-tauri", "msstore", "assets")
LP_OUT = os.path.join(REPO, "apps", "web-lp", "assets")
# The desktop frontend is served from `ui/`, so the in-app titlebar mark has to
# live there — the bundled `src-tauri/icons/` are for the OS, not the webview.
UI_OUT = os.path.join(DESKTOP, "ui", "assets")

# A pixel counts as background when every channel is at least this bright. Only
# such pixels *connected to the border* are cleared (see `strip_background`), so
# a white specular highlight inside the artwork survives.
WHITE = 236


# --------------------------------------------------------------------------- #
# PNG decoding
# --------------------------------------------------------------------------- #

class Image:
    """8-bit RGBA pixels in a flat bytearray, row-major."""

    def __init__(self, width, height, pixels):
        self.width = width
        self.height = height
        self.px = pixels  # bytearray, 4 bytes per pixel

    def at(self, x, y):
        i = (y * self.width + x) * 4
        return self.px[i : i + 4]


def _paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def decode_png(path):
    with open(path, "rb") as f:
        return decode_png_bytes(f.read(), path)


def decode_png_bytes(data, path="<memory>"):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path}: not a PNG")

    pos = 8
    ihdr = None
    palette = b""
    trns = b""
    idat = bytearray()
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        tag = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        pos += 12 + length  # length + tag + body + crc
        if tag == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", body)
        elif tag == b"PLTE":
            palette = body
        elif tag == b"tRNS":
            trns = body
        elif tag == b"IDAT":
            idat += body
        elif tag == b"IEND":
            break

    if ihdr is None:
        raise ValueError(f"{path}: missing IHDR")
    width, height, depth, color, _comp, _filt, interlace = ihdr
    if interlace:
        raise ValueError(f"{path}: interlaced PNGs are not supported — re-save without Adam7")
    if depth not in (8, 16):
        raise ValueError(f"{path}: {depth}-bit PNGs are not supported — re-save as 8-bit")

    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color]
    sample = depth // 8
    bpp = channels * sample
    stride = width * bpp

    raw = zlib.decompress(bytes(idat))
    out = bytearray(stride * height)
    prev = bytearray(stride)
    p = 0
    for y in range(height):
        ftype = raw[p]
        p += 1
        line = bytearray(raw[p : p + stride])
        p += stride
        if ftype == 1:
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 0xFF
        elif ftype == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ftype == 3:
            for i in range(stride):
                left = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif ftype == 4:
            for i in range(stride):
                left = line[i - bpp] if i >= bpp else 0
                upleft = prev[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + _paeth(left, prev[i], upleft)) & 0xFF
        elif ftype != 0:
            raise ValueError(f"{path}: bad filter type {ftype}")
        out[y * stride : (y + 1) * stride] = line
        prev = line

    # Normalise whatever colour type this was into straight RGBA8.
    px = bytearray(width * height * 4)
    for i in range(width * height):
        base = i * bpp
        if sample == 2:  # 16-bit: keep the high byte
            vals = [out[base + c * 2] for c in range(channels)]
        else:
            vals = [out[base + c] for c in range(channels)]
        if color == 0:
            r = g = b = vals[0]
            a = 255
        elif color == 2:
            r, g, b = vals
            a = 255
        elif color == 3:
            idx = vals[0]
            r, g, b = palette[idx * 3 : idx * 3 + 3]
            a = trns[idx] if idx < len(trns) else 255
        elif color == 4:
            r = g = b = vals[0]
            a = vals[1]
        else:
            r, g, b, a = vals
        px[i * 4 : i * 4 + 4] = bytes((r, g, b, a))
    return Image(width, height, px)


# --------------------------------------------------------------------------- #
# PNG encoding
# --------------------------------------------------------------------------- #

def encode_png(img):
    raw = bytearray()
    stride = img.width * 4
    for y in range(img.height):
        raw.append(0)  # filter: none
        raw += img.px[y * stride : (y + 1) * stride]

    def chunk(tag, body):
        return (
            struct.pack(">I", len(body))
            + tag
            + body
            + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", img.width, img.height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


# --------------------------------------------------------------------------- #
# Image operations
# --------------------------------------------------------------------------- #

def strip_background(img):
    """Clears the flat backdrop the artwork was exported on.

    A flood fill inward from the border, rather than "every white pixel becomes
    transparent": the app icon's gloss highlight and the terminal window's light
    strokes are near-white too, and blanket keying would punch holes straight
    through them.
    """
    w, h, px = img.width, img.height, img.px
    seen = bytearray(w * h)
    queue = deque()

    def is_bg(i):
        o = i * 4
        return px[o] >= WHITE and px[o + 1] >= WHITE and px[o + 2] >= WHITE and px[o + 3] > 0

    for x in range(w):
        for i in (x, (h - 1) * w + x):
            if not seen[i] and is_bg(i):
                seen[i] = 1
                queue.append(i)
    for y in range(h):
        for i in (y * w, y * w + w - 1):
            if not seen[i] and is_bg(i):
                seen[i] = 1
                queue.append(i)

    cleared = 0
    while queue:
        i = queue.popleft()
        px[i * 4 + 3] = 0
        cleared += 1
        x, y = i % w, i // w
        for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
            if 0 <= nx < w and 0 <= ny < h:
                j = ny * w + nx
                if not seen[j] and is_bg(j):
                    seen[j] = 1
                    queue.append(j)
    return cleared


def crop_to_content(img, alpha_min=8):
    """Trims fully transparent margins so the mark fills its canvas."""
    w, h, px = img.width, img.height, img.px
    x0, y0, x1, y1 = w, h, -1, -1
    for y in range(h):
        row = y * w
        for x in range(w):
            if px[(row + x) * 4 + 3] >= alpha_min:
                if x < x0:
                    x0 = x
                if x > x1:
                    x1 = x
                if y < y0:
                    y0 = y
                if y > y1:
                    y1 = y
    if x1 < 0:
        return img  # nothing opaque; leave it alone
    nw, nh = x1 - x0 + 1, y1 - y0 + 1
    out = bytearray(nw * nh * 4)
    for y in range(nh):
        src = ((y + y0) * w + x0) * 4
        out[y * nw * 4 : (y + 1) * nw * 4] = px[src : src + nw * 4]
    return Image(nw, nh, out)


# How far off-square artwork may be before squaring it counts as distortion
# rather than correction. The app icon is drawn as a rounded square, so a few
# percent of drift is a render artefact; a wordmark that is twice as wide as it
# is tall is not, and must never be stretched into a tile.
SQUARE_TOLERANCE = 0.12


def square_fill(img):
    """Make the artwork exactly square so it sits flush in the icon canvas.

    Padding to square instead would centre the mark and leave transparent bands
    on the short axis — which is what macOS renders as a margin around the app
    icon in the dock and in Finder.

    Only mildly off-square art is stretched: the alternative, cropping the long
    axis, cuts into a rounded square's corners and visibly flattens its sides.
    Anything beyond the tolerance keeps its aspect and is padded, because at
    that point squaring would be mangling the design rather than correcting it.
    """
    long_side, short_side = max(img.width, img.height), min(img.width, img.height)
    drift = (long_side - short_side) / long_side
    if drift == 0:
        return img
    if drift > SQUARE_TOLERANCE:
        print(f"  aspect is {drift:.1%} off square — padding rather than stretching")
        return pad_to_square(img)
    print(f"  squaring {img.width}x{img.height} → {long_side}x{long_side} "
          f"({drift:.1%} drift) so the icon sits flush with no margin")
    return resize(img, long_side, long_side)


def pad_to_square(img):
    """Centres the mark on a transparent square — ICO/ICNS/tiles must be square."""
    side = max(img.width, img.height)
    if side == img.width == img.height:
        return img
    out = bytearray(side * side * 4)
    ox, oy = (side - img.width) // 2, (side - img.height) // 2
    for y in range(img.height):
        dst = ((y + oy) * side + ox) * 4
        src = y * img.width * 4
        out[dst : dst + img.width * 4] = img.px[src : src + img.width * 4]
    return Image(side, side, out)


def resize(img, tw, th):
    """Area-average resampling, done on premultiplied alpha.

    Premultiplying matters: averaging straight RGBA lets the colour of fully
    transparent pixels bleed into the edge, which shows up as a light halo
    around the mark at small sizes.
    """
    sw, sh, src = img.width, img.height, img.px
    out = bytearray(tw * th * 4)
    for ty in range(th):
        sy0, sy1 = ty * sh // th, max(ty * sh // th + 1, (ty + 1) * sh // th)
        for tx in range(tw):
            sx0, sx1 = tx * sw // tw, max(tx * sw // tw + 1, (tx + 1) * sw // tw)
            r = g = b = a = 0
            n = 0
            for sy in range(sy0, sy1):
                row = sy * sw
                for sx in range(sx0, sx1):
                    o = (row + sx) * 4
                    al = src[o + 3]
                    r += src[o] * al
                    g += src[o + 1] * al
                    b += src[o + 2] * al
                    a += al
                    n += 1
            o = (ty * tw + tx) * 4
            if a:
                out[o] = min(255, r // a)
                out[o + 1] = min(255, g // a)
                out[o + 2] = min(255, b // a)
                out[o + 3] = a // n
            # else: leave the pixel fully transparent black
    return Image(tw, th, out)


# --------------------------------------------------------------------------- #
# Icon containers
# --------------------------------------------------------------------------- #

def ico_bytes(pngs):
    """Multi-resolution ICO. `pngs` is [(size, png_bytes)]; Vista+ reads PNG
    payloads directly, which every target of this app supports."""
    count = len(pngs)
    header = struct.pack("<HHH", 0, 1, count)
    offset = 6 + 16 * count
    entries, blobs = b"", b""
    for size, png in pngs:
        entries += struct.pack(
            "<BBBBHHII", 0 if size >= 256 else size, 0 if size >= 256 else size,
            0, 0, 1, 32, len(png), offset
        )
        blobs += png
        offset += len(png)
    return header + entries + blobs


def icns_bytes(entries):
    """`entries` is [(ostype, png_bytes)] — these OSTypes take PNG payloads."""
    body = b""
    for tag, png in entries:
        body += tag + struct.pack(">I", len(png) + 8) + png
    return b"icns" + struct.pack(">I", len(body) + 8) + body


# --------------------------------------------------------------------------- #
# Pipeline
# --------------------------------------------------------------------------- #

def prepare(path, square):
    img = decode_png(path)
    cleared = strip_background(img)
    img = crop_to_content(img)
    if square:
        img = square_fill(img)
    print(f"  {os.path.basename(path)}: background cleared {cleared}px → {img.width}x{img.height}")
    return img


def write(path, blob):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(blob)
    print(f"  wrote {os.path.relpath(path, REPO)} ({len(blob)} bytes)")


def build_app(source):
    print(f"desktop app icons ← {os.path.relpath(source, REPO)}")
    img = prepare(source, square=True)
    png = {s: encode_png(resize(img, s, s)) for s in (32, 44, 50, 128, 150, 256, 512)}

    write(os.path.join(ICONS_OUT, "32x32.png"), png[32])
    write(os.path.join(ICONS_OUT, "128x128.png"), png[128])
    write(os.path.join(ICONS_OUT, "128x128@2x.png"), png[256])
    write(os.path.join(ICONS_OUT, "icon.png"), png[512])
    write(os.path.join(ICONS_OUT, "icon.ico"), ico_bytes([(32, png[32]), (128, png[128]), (256, png[256])]))
    write(
        os.path.join(ICONS_OUT, "icon.icns"),
        icns_bytes([(b"ic07", png[128]), (b"ic08", png[256]), (b"ic09", png[512])]),
    )
    write(os.path.join(MSSTORE_OUT, "Square44x44Logo.png"), png[44])
    write(os.path.join(MSSTORE_OUT, "Square150x150Logo.png"), png[150])
    write(os.path.join(MSSTORE_OUT, "StoreLogo.png"), png[50])
    # Shown ~20px tall in the titlebar; 128 keeps it crisp on any DPI.
    write(os.path.join(UI_OUT, "icon.png"), png[128])


def build_lp(source):
    print(f"landing page icons ← {os.path.relpath(source, REPO)}")
    # The header mark keeps the artwork's own aspect ratio; the favicon has to
    # be square, so it gets a centred, transparent-padded copy.
    mark = prepare(source, square=False)
    scale = 512 / max(mark.width, mark.height)
    write(
        os.path.join(LP_OUT, "icon.png"),
        encode_png(resize(mark, round(mark.width * scale), round(mark.height * scale))),
    )
    sq = pad_to_square(mark)
    write(
        os.path.join(LP_OUT, "favicon.ico"),
        ico_bytes([(s, encode_png(resize(sq, s, s))) for s in (16, 32, 48)]),
    )


# --------------------------------------------------------------------------- #
# Self test
# --------------------------------------------------------------------------- #

def _self_test():
    """Exercises the format handling without touching brand/ or the outputs.

    This is what CI runs: the icons themselves are committed artifacts, so the
    thing worth guarding is the codec underneath them.
    """
    # PNG round-trip: encode → decode must be pixel-identical.
    w, h = 9, 6
    px = bytearray()
    for y in range(h):
        for x in range(w):
            px += bytes((x * 20 % 256, y * 30 % 256, (x + y) * 10 % 256, 255))
    original = Image(w, h, px)
    back = decode_png_bytes(encode_png(original))
    assert (back.width, back.height) == (w, h), (back.width, back.height)
    assert back.px == original.px, "PNG round-trip changed pixels"

    # A flat image survives downscaling unchanged.
    flat = Image(4, 4, bytearray([10, 20, 30, 255] * 16))
    assert resize(flat, 2, 2).px == bytearray([10, 20, 30, 255] * 4)

    # A 2x2 block down to one pixel is an exact box average.
    quad = Image(2, 2, bytearray([0, 0, 0, 255, 100, 100, 100, 255,
                                  200, 200, 200, 255, 100, 100, 100, 255]))
    assert resize(quad, 1, 1).px == bytearray([100, 100, 100, 255])

    # Premultiplied resampling: a transparent neighbour must not tint the colour.
    masked = resize(Image(2, 1, bytearray([255, 0, 0, 255, 0, 0, 0, 0])), 1, 1)
    assert masked.px[:3] == bytearray([255, 0, 0]), masked.px[:3]
    assert masked.px[3] == 127, masked.px[3]   # 255 and 0 averaged, truncated

    # strip_background clears only the backdrop reachable from the border — a
    # white pixel walled off inside the artwork (a gloss highlight) stays put.
    dark = (10, 10, 10, 255)
    white = (255, 255, 255, 255)
    grid = [[white] * 5 for _ in range(5)]
    for y in range(1, 4):
        for x in range(1, 4):
            grid[y][x] = dark
    grid[2][2] = white  # the highlight, enclosed by dark pixels
    art = Image(5, 5, bytearray(b"".join(bytes(c) for row in grid for c in row)))
    cleared = strip_background(art)
    assert cleared == 16, cleared                      # the border ring only
    assert art.at(0, 0)[3] == 0, "border was not cleared"
    assert art.at(2, 2)[3] == 255, "enclosed highlight was cleared"

    # crop_to_content trims transparent margin; pad_to_square re-centres.
    cropped = crop_to_content(art)
    assert (cropped.width, cropped.height) == (3, 3), (cropped.width, cropped.height)
    wide = Image(4, 2, bytearray([1, 2, 3, 255] * 8))
    square = pad_to_square(wide)
    assert (square.width, square.height) == (4, 4), (square.width, square.height)
    assert square.at(0, 0)[3] == 0 and square.at(0, 1)[3] == 255, "padding is off-centre"

    # Containers: ICO directory count/offsets and the ICNS length prefix.
    png32 = encode_png(resize(flat, 32, 32))
    png256 = encode_png(resize(flat, 256, 256))
    ico = ico_bytes([(32, png32), (256, png256)])
    assert struct.unpack("<HHH", ico[:6]) == (0, 1, 2), ico[:6]
    assert ico[6] == 32 and ico[6 + 16] == 0, "256px entry must encode its size as 0"
    size, offset = struct.unpack("<II", ico[6 + 8 : 6 + 16])
    assert (size, offset) == (len(png32), 6 + 32), (size, offset)
    assert len(ico) == 6 + 32 + len(png32) + len(png256), len(ico)
    icns = icns_bytes([(b"ic07", png32)])
    assert icns[:4] == b"icns", icns[:4]
    assert struct.unpack(">I", icns[4:8])[0] == len(icns), "bad ICNS length"

    print("icons_from_source self-test: ok")


# --------------------------------------------------------------------------- #
# Entry point
# --------------------------------------------------------------------------- #

def main():
    if "--self-test" in sys.argv[1:]:
        _self_test()
        return 0

    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--app", default=os.path.join(BRAND, "app-icon.png"),
                    help="square app icon artwork (desktop + Microsoft Store)")
    ap.add_argument("--lp", default=os.path.join(BRAND, "lp-icon.png"),
                    help="landing page mark (header logo + favicon)")
    ap.add_argument("--app-only", action="store_true")
    ap.add_argument("--lp-only", action="store_true")
    args = ap.parse_args()

    missing = [p for p in ((args.app, not args.lp_only), (args.lp, not args.app_only))
               if p[1] and not os.path.exists(p[0])]
    if missing:
        for path, _ in missing:
            print(f"error: source image not found: {os.path.relpath(path, REPO)}", file=sys.stderr)
        print("\nPut the artwork in brand/ (see brand/README.md).", file=sys.stderr)
        return 1

    if not args.lp_only:
        build_app(args.app)
    if not args.app_only:
        build_lp(args.lp)
    return 0


if __name__ == "__main__":
    sys.exit(main())
