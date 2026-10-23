#!/usr/bin/env python3
"""Writes showcase.json: a pointer that glides between controls and flips them one after another.

Coordinates are window points (960x660). Run: python3 showcase.py > showcase.json
"""
import json
import math

events = []
pos = [600.0, 420.0]
clock = 0.0


def glide(to, dur=0.55, at=None):
    """An eased, slightly arced move sampled at 120 Hz."""
    global pos, clock
    if at is not None:
        clock = at
    x0, y0 = pos
    x1, y1 = to
    n = max(2, int(dur * 120))
    dx, dy = x1 - x0, y1 - y0
    bend = 0.08 * math.hypot(dx, dy)
    nx, ny = (-dy, dx) if (dx or dy) else (0, 0)
    ln = math.hypot(nx, ny) or 1.0
    for k in range(1, n + 1):
        f = k / n
        e = f * f * f * (f * (f * 6 - 15) + 10)
        arc = math.sin(math.pi * e) * bend
        events.append({
            "t": round(clock + dur * f, 4),
            "type": "move",
            "x": round(x0 + dx * e + nx / ln * arc, 2),
            "y": round(y0 + dy * e + ny / ln * arc, 2),
        })
    clock += dur
    pos = [x1, y1]


def tap(to, dur=0.55, at=None, settle=0.12):
    glide(to, dur, at)
    global clock
    clock += settle
    events.append({"t": round(clock, 4), "type": "tap", "x": to[0], "y": to[1], "hold": 0.09})
    clock += 0.1


def wait(s):
    global clock
    clock += s


NAV = {"general": (70, 78), "appearance": (82, 108), "network": (72, 138), "security": (70, 168),
       "accessibility": (84, 198), "developer": (76, 228)}
SW1, SW2 = (891, 166), (891, 232)

wait(0.6)
# General: lightning, then the spotlight.
tap((891, 232), 0.7)          # Lightning-fast startup
wait(1.3)
tap((891, 355), 0.6)          # Show in Spotlight
wait(1.9)
tap((891, 355), 0.25)         # ...and back off
wait(0.4)

# Network: firewall, static IP.
tap(NAV["network"], 0.55)
wait(0.5)
tap(SW1, 0.6)                # Firewall
wait(1.4)
tap((891, 481), 0.6)          # Static IP
wait(1.0)

# Security: electric fence, then get too close.
tap(NAV["security"], 0.55)
wait(0.5)
tap(SW1, 0.6)                # Electric fence
wait(1.0)
glide((640, 530), 0.55)
wait(0.6)
glide((700, 570), 0.3)

# Accessibility: haptics Strong, frosting, contrast.
tap(NAV["accessibility"], 0.55)
wait(0.5)
tap((869, 478), 0.6)         # Haptic feedback: Strong
wait(0.8)
tap((560, 300), 0.45)         # Just clicking around.
wait(0.7)
tap(SW2, 0.6)                 # Reduce transparency
wait(1.4)
tap(SW1, 0.45)                # Increase contrast
wait(1.8)
tap(SW1, 0.2)
wait(0.3)

# Developer: hot reload, flush.
tap(NAV["developer"], 0.55)
wait(0.5)
tap(SW1, 0.6)                # Hot reload
wait(1.6)
tap((873, 301), 0.5)          # Flush cache
wait(1.8)

# Appearance: Dark, then wander with the torch.
tap(NAV["appearance"], 0.55)
wait(0.5)
tap((801, 166), 0.6)         # Dark
wait(0.9)
glide((470, 330), 0.6)
glide((300, 470), 0.6)
glide((150, 590), 0.45)

# Restore Defaults.
tap((115, 630), 0.5)
wait(2.3)

json.dump({"events": events}, open(__file__.replace(".py", ".json"), "w"))
print(f"{clock:.2f} s, {len(events)} events")
