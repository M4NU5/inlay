"""Writes Windows/inlay.ico: the Mac app icon from scripts/make-icon.swift.

Python standard library only, so it runs anywhere:

    python3 scripts/make-windows-icon.py

Keep the geometry and colours in sync with make-icon.swift.
"""

import math
import struct
import zlib
from pathlib import Path

SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
SAMPLES = 4  # per axis, per pixel


def rgb(value, alpha=1.0):
    return ((value >> 16) & 0xFF) / 255, ((value >> 8) & 0xFF) / 255, (value & 0xFF) / 255, alpha


def rounded_rect(x, y, left, top, width, height, radius):
    """Signed distance to a rounded rectangle; negative inside."""
    cx, cy = left + width / 2, top + height / 2
    qx = abs(x - cx) - width / 2 + radius
    qy = abs(y - cy) - height / 2 + radius
    outside = math.hypot(max(qx, 0), max(qy, 0))
    return outside + min(max(qx, qy), 0) - radius


def gradient(x, y, start, end, first, last):
    dx, dy = end[0] - start[0], end[1] - start[1]
    t = ((x - start[0]) * dx + (y - start[1]) * dy) / (dx * dx + dy * dy)
    t = min(max(t, 0), 1)
    return tuple(a + (b - a) * t for a, b in zip(first, last))


def over(under, colour):
    """Source-over for straight-alpha RGBA tuples."""
    r, g, b, a = colour
    ur, ug, ub, ua = under
    out = a + ua * (1 - a)
    if out == 0:
        return 0, 0, 0, 0
    mix = lambda top, bottom: (top * a + bottom * ua * (1 - a)) / out
    return mix(r, ur), mix(g, ug), mix(b, ub), out


# Logo strokes in the 128-unit space of Resources/Inlay.svg.
STROKES = [(20, 20, 88, 16), (56, 44, 16, 40), (20, 92, 88, 16)]
INK = rgb(0x352D3A)


def sample(x, y):
    """Colour at a point in the 512-unit icon space, y pointing down."""
    colour = (0, 0, 0, 0)
    if rounded_rect(x, y, 12, 12, 488, 488, 112) <= 0:
        colour = gradient(x, y, (80, 20), (420, 500), rgb(0xFCF9F2), rgb(0xE8D9D4))
    if abs(rounded_rect(x, y, 13, 13, 486, 486, 111)) <= 1:
        colour = over(colour, gradient(x, y, (180, 0), (300, 512),
                                       rgb(0xFFFFFF, 0.9), rgb(0x352D3A, 0.12)))
    lx, ly = (x - 83) / 2.7, (y - 83) / 2.7
    if any(rounded_rect(lx, ly, *stroke, 8) <= 0 for stroke in STROKES):
        colour = INK
    return colour


def render(pixels):
    scale = 512 / pixels
    rows = []
    for row in range(pixels):
        line = bytearray([0])  # PNG filter: none
        for column in range(pixels):
            # Average premultiplied colour so transparent samples don't darken edges.
            r = g = b = a = 0.0
            for sy in range(SAMPLES):
                for sx in range(SAMPLES):
                    x = (column + (sx + 0.5) / SAMPLES) * scale
                    y = (row + (sy + 0.5) / SAMPLES) * scale
                    cr, cg, cb, ca = sample(x, y)
                    r, g, b, a = r + cr * ca, g + cg * ca, b + cb * ca, a + ca
            count = SAMPLES * SAMPLES
            a /= count
            if a > 0:
                r, g, b = r / count / a, g / count / a, b / count / a
            line += bytes(round(v * 255) for v in (r, g, b, a))
        rows.append(bytes(line))
    return png(pixels, b"".join(rows))


def png(pixels, data):
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    header = struct.pack(">IIBBBBB", pixels, pixels, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(data, 9)) + chunk(b"IEND", b""))


def ico(images):
    offset = 6 + 16 * len(images)
    directory, bodies = b"", b""
    for pixels, body in images:
        side = 0 if pixels >= 256 else pixels
        directory += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(body), offset + len(bodies))
        bodies += body
    return struct.pack("<HHH", 0, 1, len(images)) + directory + bodies


if __name__ == "__main__":
    output = Path(__file__).resolve().parent.parent / "Windows" / "inlay.ico"
    output.write_bytes(ico([(size, render(size)) for size in SIZES]))
    print(output)
