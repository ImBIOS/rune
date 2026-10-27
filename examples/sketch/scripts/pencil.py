#!/usr/bin/env python3
"""Draws assets/pencil.png: a 64x64 px (32x32 pt at 2x) pencil cursor, tip at (4, 60) px."""
import math, os, struct, zlib

SIZE, SS = 64, 4
TIP, W = (4.0, 60.0), 6.5
D = (1 / math.sqrt(2), -1 / math.sqrt(2))
N = (-D[1], D[0])
# The silhouette in pencil space (u along the pencil from the tip, v across it): convex.
HULL = [(0.0, 0.0), (16.0, -W), (74.0, -W), (74.0, W), (16.0, W)]

def hexc(h): return tuple(int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))
INK, HALO = hexc("1d1d1f"), (1.0, 1.0, 1.0)

def inside(u, v): return 0 <= u <= 74 and abs(v) <= min(W, W * u / 16)

def edge_dist(u, v):
    best = 1e9
    for (au, av), (bu, bv) in zip(HULL, HULL[1:] + HULL[:1]):
        eu, ev = bu - au, bv - av
        t = max(0.0, min(1.0, ((u - au) * eu + (v - av) * ev) / (eu * eu + ev * ev)))
        best = min(best, math.hypot(u - au - t * eu, v - av - t * ev))
    return best

def fill(u, v):
    if u < 5: return INK
    if u < 16: return hexc("e8c39e")
    if u < 62: return hexc("d9a800") if v < -W / 3 else hexc("ffd84d") if v > W / 3 else hexc("f5c518")
    if u < 66: return hexc("a9a9b2")
    return hexc("f08a9a")

def sample(x, y):
    px, py = x - TIP[0], y - TIP[1]
    u, v = px * D[0] + py * D[1], px * N[0] + py * N[1]
    d = edge_dist(u, v)
    if inside(u, v):
        return (INK if d < 0.8 or 61.6 < u < 62.4 or 65.6 < u < 66.4 else fill(u, v)) + (1.0,)
    if d < 0.8: return INK + (1.0,)
    if d < 2.0: return HALO + (1.0,)
    return (0.0, 0.0, 0.0, 0.0)

rows = []
for y in range(SIZE):
    row = bytearray([0])
    for x in range(SIZE):
        acc = [0.0] * 4
        for j in range(SS):
            for i in range(SS):
                r, g, b, a = sample(x + (i + 0.5) / SS, y + (j + 0.5) / SS)
                acc[0] += r * a; acc[1] += g * a; acc[2] += b * a; acc[3] += a
        a = acc[3] / (SS * SS)
        rgb = [c / acc[3] if acc[3] else 0.0 for c in acc[:3]]
        row += bytes(round(c * 255) for c in rgb + [a])
    rows.append(bytes(row))

def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
out = os.path.join(os.path.dirname(__file__), "..", "assets", "pencil.png")
open(out, "wb").write(png)
print(f"{out}: {len(png)} bytes")
