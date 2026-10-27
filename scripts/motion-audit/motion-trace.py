#!/usr/bin/env python3
"""Reads an app's own motion trace, recorded in the installed app on real data, and prints, for
each key press, how many frames until a watched element first moved, and every frame of the move
that went over budget.

  scripts/motion-audit/motion-trace.py TRACE.jsonl [--key Enter] [--frames 40] [--budget 16.7]

The trace is JSON lines the app writes (README.md, "Tracing the real app"):
  {"k":"key","ms":…,"key":"Enter"}                    a key, as the app's handler got it
  {"k":"span","ms":…,"name":"load","dur":…}           time spent in one step on the main thread
  {"k":"frame","ms":…,"update":…,"render":…,"rows":{"row-1":[y,h,opacity],…}}
`ms` is wall time since launch at the start of the step. A frame's cost is Rune's update
(handlers, poll, view, layout) plus the compositor's render; over the budget, the display shows
the previous frame again. `rows` holds whatever the app watches, by id, as numbers that change
when it moves. Spans over 1 ms in the frame are listed beside it.
"""
import argparse
import json

ap = argparse.ArgumentParser()
ap.add_argument("trace")
ap.add_argument("--key", default="Enter")
ap.add_argument("--frames", type=int, default=40)
ap.add_argument("--budget", type=float, default=1000 / 60)
a = ap.parse_args()
lines = [json.loads(l) for l in open(a.trace) if l.strip()]
frames = [l for l in lines if l["k"] == "frame"]
spans = [l for l in lines if l["k"] == "span"]
keys = [l for l in lines if l["k"] == "key" and a.key in l["key"]]


def start(f):
    return f["ms"] - f["update"]


def moved(a_, b):
    rows_a, rows_b = a_.get("rows", {}), b.get("rows", {})
    for id_, r in rows_a.items():
        o = rows_b.get(id_)
        if o is None or any(abs(x - y) > 0.01 for x, y in zip(o, r)):
            return True
    return any(id_ not in rows_a for id_ in rows_b)


print(a.trace)
for k in keys:
    i = next((i for i, f in enumerate(frames) if f["ms"] >= k["ms"]), None)
    if i is None:
        continue
    key_frame = frames[i]
    first = next((j for j in range(i + 1, len(frames)) if moved(frames[j - 1], frames[j])), None)
    # The key frame itself already moved rows: the move started on it.
    if moved(frames[i - 1], key_frame) if i else False:
        first = i
    sel = f" (sel {key_frame['sel']})" if "sel" in key_frame else ""
    print(f"\n{k['key']} at {k['ms']:.0f} ms, handled in the frame starting {start(key_frame):.0f} ms{sel}")
    if first is None:
        print("  nothing watched moved")
        continue
    print(f"  first moved frame: +{first - i} frames, {frames[first]['ms'] + frames[first]['render'] - k['ms']:.0f} ms from the key to its render's end")
    over = 0
    for j in range(i, min(len(frames), i + a.frames)):
        f = frames[j]
        cost = f["update"] + f["render"]
        gap = start(f) - (frames[j - 1]["ms"] + frames[j - 1]["render"]) if j else 0
        if cost > a.budget:
            over += 1
        mark = "OVER" if cost > a.budget else "    "
        big = [s for s in spans if start(f) <= s["ms"] < f["ms"] + f["render"] and s["dur"] > 1.0]
        moving = "move" if j and moved(frames[j - 1], f) else "    "
        names = ", ".join(f"{s['name']} {s['dur']:.1f}" for s in big)
        print(f"  {mark} +{j - i:2d} {moving} update {f['update']:6.1f} render {f['render']:5.1f} idle-before {gap:6.1f}  {names}")
    print(f"  frames over {a.budget:.1f} ms in the first {a.frames}: {over}")
