#!/usr/bin/env python3
"""Draws mNote's app icon: the design's logo (a cursor block in `cursor`)
on a rounded square of `paper`, as a 1024px PNG. Standard library only.

Usage: python3 tools/make_icon.py OUT.png
"""

import struct
import sys
import zlib

SIZE = 1024
# The dark theme's `paper`, `chrome` and `cursor` tokens.
PAPER = (0x28, 0x2C, 0x34)
EDGE = (0x21, 0x25, 0x2B)
CURSOR = (0x56, 0xB6, 0xC2)

# macOS icon grid: the shape sits inside a 824px square, corners ~185px.
INSET = 100
RADIUS = 185
# The cursor block, in the design's 18:34 proportions.
BAR_W, BAR_H = 230, 434
BAR_RADIUS = 51


def inside_rounded(x, y, left, top, right, bottom, radius):
    """Coverage (0..1) of pixel (x, y) by a rounded rectangle, 4x4 samples."""
    hits = 0
    for sy in range(4):
        for sx in range(4):
            px = x + (sx + 0.5) / 4
            py = y + (sy + 0.5) / 4
            if px < left or px > right or py < top or py > bottom:
                continue
            cx = min(max(px, left + radius), right - radius)
            cy = min(max(py, top + radius), bottom - radius)
            if (px - cx) ** 2 + (py - cy) ** 2 <= radius**2:
                hits += 1
    return hits / 16


def blend(base, over, alpha):
    return tuple(round(b + (o - b) * alpha) for b, o in zip(base, over))


def pixel(x, y):
    shape = (INSET, INSET, SIZE - INSET, SIZE - INSET)
    # Cheap tests first: most pixels are fully in or out.
    near_shape_edge = not (
        shape[0] + RADIUS <= x <= shape[2] - RADIUS - 1
        or shape[1] + RADIUS <= y <= shape[3] - RADIUS - 1
    ) or x < shape[0] + 1 or y < shape[1] + 1 or x > shape[2] - 2 or y > shape[3] - 2
    cover = inside_rounded(x, y, *shape, RADIUS) if near_shape_edge else 1.0
    if cover == 0:
        return (0, 0, 0, 0)
    colour = PAPER
    # A 1px-ish edge in `chrome`, like the window's own outline.
    ring = inside_rounded(x, y, shape[0] + 6, shape[1] + 6, shape[2] - 6, shape[3] - 6, RADIUS - 6)
    colour = blend(EDGE, PAPER, ring)
    left = (SIZE - BAR_W) / 2
    top = (SIZE - BAR_H) / 2
    if left - 1 <= x <= left + BAR_W + 1 and top - 1 <= y <= top + BAR_H + 1:
        bar = inside_rounded(x, y, left, top, left + BAR_W, top + BAR_H, BAR_RADIUS)
        colour = blend(colour, CURSOR, bar)
    return (*colour, round(255 * cover))


def png(path):
    rows = bytearray()
    for y in range(SIZE):
        rows.append(0)
        for x in range(SIZE):
            rows.extend(pixel(x, y))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    with open(path, "wb") as out:
        out.write(b"\x89PNG\r\n\x1a\n")
        out.write(chunk(b"IHDR", header))
        out.write(chunk(b"IDAT", zlib.compress(bytes(rows), 9)))
        out.write(chunk(b"IEND", b""))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    png(sys.argv[1])
