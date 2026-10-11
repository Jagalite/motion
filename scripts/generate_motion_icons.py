#!/usr/bin/env python3
"""Regenerate Motion's home-screen icons (ui-assets/icons) without image libraries:
the brand play glyph on the accent color, 4x supersampled. Usage:
    python3 scripts/generate_motion_icons.py ui-assets/icons
"""
import struct, sys, zlib
GOLD, INK = (0xe5, 0xa0, 0x0d), (0x1f, 0x23, 0x26)

def inside(x, y, tri):
    (x1, y1), (x2, y2), (x3, y3) = tri
    d = lambda ax, ay, bx, by, cx, cy: (ax - cx) * (by - cy) - (bx - cx) * (ay - cy)
    a, b, c = d(x, y, x1, y1, x2, y2), d(x, y, x2, y2, x3, y3), d(x, y, x3, y3, x1, y1)
    return not ((a < 0 or b < 0 or c < 0) and (a > 0 or b > 0 or c > 0))

def icon(size, scale):
    # Brand play glyph (M9 6.5v11l8.5-5.5z in a 24 box), centred on its
    # bounding box and scaled so it stays inside the maskable safe zone.
    pts = [(9, 6.5), (9, 17.5), (17.5, 12)]
    cx, cy = (9 + 17.5) / 2, 12
    tri = [((x - cx) / 11 * scale * size + size / 2, (y - cy) / 11 * scale * size + size / 2) for x, y in pts]
    ss, rows = 4, []
    for py in range(size):
        row = bytearray([0])
        for px in range(size):
            hit = sum(inside(px + (i + .5) / ss, py + (j + .5) / ss, tri) for i in range(ss) for j in range(ss)) / ss**2
            row += bytes(round(g * (1 - hit) + k * hit) for g, k in zip(GOLD, INK))
        rows.append(bytes(row))
    chunk = lambda t, d: struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 2, 0, 0, 0)) \
        + chunk(b'IDAT', zlib.compress(b''.join(rows), 9)) + chunk(b'IEND', b'')

out = sys.argv[1]
for size in (180, 192, 512):
    open(f'{out}/icon-{size}.png', 'wb').write(icon(size, .42))
