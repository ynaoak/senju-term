#!/usr/bin/env python3
"""Generate the app icon set (PNG / ICO / ICNS) with the Python standard
library only, so the repo has no image-tool build dependency.

The icon is a dark rounded square with a teal terminal prompt (chevron and
cursor underscore) — drawn analytically per pixel.

Usage: python3 scripts/gen_icons.py
"""
import math
import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(__file__), "..", "src-tauri", "icons")

BG = (13, 17, 23)          # near-black slate
BG_EDGE = (22, 27, 34)     # subtle vignette edge
ACCENT = (0, 229, 190)     # teal prompt
ACCENT2 = (137, 87, 229)   # violet underscore


def dist_to_segment(px, py, ax, ay, bx, by):
    vx, vy = bx - ax, by - ay
    wx, wy = px - ax, py - ay
    seg_len2 = vx * vx + vy * vy
    t = 0.0 if seg_len2 == 0 else max(0.0, min(1.0, (wx * vx + wy * vy) / seg_len2))
    dx, dy = px - (ax + t * vx), py - (ay + t * vy)
    return math.hypot(dx, dy)


def rounded_rect_alpha(u, v, radius):
    """1 inside a rounded unit square, softly 0 outside."""
    cx = min(max(u, radius), 1 - radius)
    cy = min(max(v, radius), 1 - radius)
    d = math.hypot(u - cx, v - cy)
    return max(0.0, min(1.0, (radius - d) * 40 + 1))


def pixel(u, v):
    alpha = rounded_rect_alpha(u, v, 0.18)
    if alpha <= 0:
        return (0, 0, 0, 0)
    # background with a slight radial lift toward the top-left
    g = max(0.0, 1 - math.hypot(u - 0.35, v - 0.3))
    base = tuple(int(BG[i] + (BG_EDGE[i] - BG[i]) * (1 - g)) for i in range(3))

    # prompt chevron ">" and cursor underscore
    strokes = [
        (ACCENT, dist_to_segment(u, v, 0.30, 0.34, 0.50, 0.50)),
        (ACCENT, dist_to_segment(u, v, 0.50, 0.50, 0.30, 0.66)),
        (ACCENT2, dist_to_segment(u, v, 0.56, 0.64, 0.74, 0.64)),
    ]
    color = base
    for stroke_color, d in strokes:
        s = max(0.0, min(1.0, (0.045 - d) * 60 + 1))
        if s > 0:
            color = tuple(int(color[i] + (stroke_color[i] - color[i]) * s) for i in range(3))
    return (*color, int(255 * alpha))


def render(size):
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            # 2x2 supersampling
            acc = [0, 0, 0, 0]
            for sx, sy in ((0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)):
                p = pixel((x + sx) / size, (y + sy) / size)
                for i in range(4):
                    acc[i] += p[i]
            row.extend(c // 4 for c in acc)
        rows.append(bytes(row))
    return rows


def png_bytes(size):
    rows = render(size)
    raw = b"".join(b"\x00" + r for r in rows)

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico_bytes(png):
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 22)
    return header + entry + png


def icns_bytes(pngs):
    # (type, png) pairs; PNG payloads are valid for these OSTypes
    body = b""
    for tag, png in pngs:
        body += tag + struct.pack(">I", len(png) + 8) + png
    return b"icns" + struct.pack(">I", len(body) + 8) + body


def main():
    os.makedirs(OUT, exist_ok=True)
    sizes = {32: None, 128: None, 256: None, 512: None}
    for s in sizes:
        sizes[s] = png_bytes(s)

    out = {
        "32x32.png": sizes[32],
        "128x128.png": sizes[128],
        "128x128@2x.png": sizes[256],
        "icon.png": sizes[512],
        "icon.ico": ico_bytes(sizes[256]),
        "icon.icns": icns_bytes([(b"ic07", sizes[128]), (b"ic08", sizes[256]), (b"ic09", sizes[512])]),
    }
    for name, data in out.items():
        path = os.path.join(OUT, name)
        with open(path, "wb") as f:
            f.write(data)
        print(f"wrote {path} ({len(data)} bytes)")


if __name__ == "__main__":
    main()
