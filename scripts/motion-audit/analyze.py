#!/usr/bin/env python3
"""Motion audit of Rune traces (`--trace out.jsonl --trace-all`).

Reads each element's presented rect, opacity and clipped rect per frame, the tracks the
motion store sampled (`tk`) and why the frame was animating (`quality.why`), and lists what
looks wrong:

  invisible-work   tracks sampled while their element can't be seen (opacity 0, clipped
                   away or off the window) for several frames in a row
  idle-frames      frames that asked for another frame but changed nothing traced
  never-settles    still animating at the end of the run, long after the last input
  pop-in/pop-out   mounted or unmounted at full opacity with no transition
  jump             a rect edge moves far in one frame between still frames (no interpolation)
  step             one frame of a smooth motion moves much more than its neighbours
  sibling-jump     a jump in a sibling of something that just mounted or unmounted
  overshoot        an edge passes its rest value and comes back
  jiggle           an edge reverses direction twice or more within one motion
  latency          input whose first visible response comes frames later
  hitch            frame CPU over budget during motion

Usage: scripts/motion-audit/analyze.py TRACE.jsonl... [--json out.json] [--quiet]
Exit code is 0; this is a report, not a gate.
"""

import json
import math
import statistics
import sys
from collections import defaultdict

VISIBLE_OPACITY = 0.01
POP_OPACITY = 0.5
JUMP_PT = 12.0
MIN_AREA = 1.0


def short(name, parts=3):
    segs = name.split("/")
    return "/".join(segs[-parts:]) if len(segs) > parts else name


class El:
    __slots__ = ("id", "name", "parent", "rect", "op", "flags", "vis", "born", "hist", "ever_drawn")

    def __init__(self, id, name, parent):
        self.id, self.name, self.parent = id, name, parent
        self.rect = None
        self.op = 1.0
        self.flags = 0
        self.vis = None
        self.born = None
        self.ever_drawn = False  # had area at some point (spacers never do)
        self.hist = []  # (frame, x, y, w, h, op, flags, visible-area-fraction, on-screen)


def area(r):
    return max(r[2], 0) * max(r[3], 0)


def on_screen(r, W, H):
    return r[0] < W and r[1] < H and r[0] + r[2] > 0 and r[1] + r[3] > 0 and area(r) > 0


def visible_part(e, W, H):
    r = e.vis if e.vis is not None else e.rect
    x0, y0 = max(r[0], 0), max(r[1], 0)
    x1, y1 = min(r[0] + r[2], W), min(r[1] + r[3], H)
    return [x0, y0, max(x1 - x0, 0), max(y1 - y0, 0)]


def seen(e, W, H):
    return e.op > VISIBLE_OPACITY and area(visible_part(e, W, H)) >= MIN_AREA


def load(path):
    frames = []
    with open(path) as f:
        header = json.loads(f.readline())
        for line in f:
            line = line.strip()
            if line:
                frames.append(json.loads(line))
    return header, frames


def runs(frames_list):
    """Consecutive frame indices → [(start, end)]."""
    out = []
    for k in sorted(frames_list):
        if out and k == out[-1][1] + 1:
            out[-1][1] = k
        else:
            out.append([k, k])
    return [tuple(r) for r in out]


def fmt_runs(rs, limit=4):
    s = ", ".join(f"{a}" if a == b else f"{a}–{b}" for a, b in rs[:limit])
    return s + (f" (+{len(rs) - limit} more)" if len(rs) > limit else "")


def analyze(path):
    header, frames = load(path)
    W, H, hz = header["width"], header["height"], header["hz"]
    dt_ms = 1000.0 / hz
    els = {}
    alive = set()
    findings = []
    touched_invisible = defaultdict(list)  # (name, prop) -> frames
    touched_untraced = defaultdict(int)
    idle = []
    animating = []
    inputs = []  # (frame, event)
    births = defaultdict(list)  # frame -> ids
    deaths = defaultdict(list)  # frame -> (id, last state)
    cpu = []
    changed_by_frame = []

    for k, fr in enumerate(frames):
        for m in fr.get("m", []):
            e = els.get(m["id"])
            if e is None:
                e = els[m["id"]] = El(m["id"], m["nm"], m.get("pv"))
            else:
                e.name, e.parent = m["nm"], m.get("pv", e.parent)
        changed = set()
        for row in fr.get("e", []):
            id = row[0]
            e = els[id]
            e.rect = row[1:5]
            e.op = row[5]
            e.flags = row[7]
            e.vis = row[8:12] if len(row) >= 12 else None
            e.ever_drawn |= area(e.rect) >= MIN_AREA
            if id not in alive:
                alive.add(id)
                e.born = k
                if k > 0:
                    births[k].append(id)
            changed.add(id)
        for id in fr.get("g", []):
            if id in alive:
                alive.discard(id)
                e = els[id]
                deaths[k].append((id, e.rect, e.op, seen(e, W, H)))
        for id in alive:
            e = els[id]
            vp = visible_part(e, W, H)
            frac = area(vp) / area(e.rect) if area(e.rect) > 0 else 0.0
            e.hist.append((k, *e.rect, e.op, e.flags, frac, on_screen(e.rect, W, H)))
        q = fr.get("quality", {})
        cpu.append(q.get("cpu_ms", 0.0))
        anim = bool(q.get("animating"))
        animating.append(anim)
        visible_change = any(seen(els[i], W, H) or i in births.get(k, []) for i in changed)
        changed_by_frame.append(visible_change)
        for ev in fr.get("ev", []):
            if not ev.startswith(("settled", "completed")):
                inputs.append((k, ev))
        tracks = fr.get("tk", [])
        if anim and not changed and k > 0 and not any(a for _, _, a in tracks):
            idle.append((k, tuple(q.get("why", []))))
        for who, prop, active in tracks:
            if not active:
                continue  # set without animating: no motion this frame
            if isinstance(who, str):
                touched_untraced[(short(who), prop)] += 1
                continue
            e = els.get(who)
            if e is None or who not in alive:
                touched_untraced[(f"#{who}", prop)] += 1
                continue
            if not seen(e, W, H) and e.ever_drawn:
                touched_invisible[(e.name, prop, who)].append(k)

    n = len(frames)

    # invisible work: report the outermost element of a subtree that went unseen together
    keys = {(id, prop): ks for (name, prop, id), ks in touched_invisible.items()}
    def covered(id, prop, ks):
        p = els[id].parent
        while p is not None:
            for prop2 in {prop, "frame", "presence", "opacity"}:
                if (p, prop2) in keys and set(ks) <= set(keys[(p, prop2)]):
                    return True
            p = els[p].parent if p in els else None
        return False
    for (name, prop, id), ks in touched_invisible.items():
        if covered(id, prop, ks):
            continue
        rs = runs(ks)
        longest = max(b - a + 1 for a, b in rs)
        if longest < 6:
            continue
        e = els[id]
        why = []
        last = e.hist[-1] if e.hist else None
        if e.op <= VISIBLE_OPACITY:
            why.append(f"opacity {e.op}")
        if last and last[8] == 0:
            why.append("clipped away")
        if e.rect and not on_screen(e.rect, W, H):
            why.append("off-window")
        findings.append(dict(kind="invisible-work", element=name, prop=prop, frames=len(ks), longest=longest,
                             at=fmt_runs(rs), detail=f"{prop} sampled on {len(ks)} frames while unseen ({', '.join(why) or 'unseen'})"))
    for (who, prop), count in touched_untraced.items():
        if count >= 6:
            findings.append(dict(kind="invisible-work", element=who, prop=prop, frames=count, longest=count, at="",
                                 detail=f"{prop} sampled on {count} frames for a node not in the element tree"))

    # idle frames and settling
    if idle:
        by_why = defaultdict(list)
        for k, why in idle:
            by_why[why].append(k)
        for why, ks in by_why.items():
            rs = runs(ks)
            findings.append(dict(kind="idle-frames", element="(frame)", frames=len(ks), longest=max(b - a + 1 for a, b in rs),
                                 at=fmt_runs(rs), detail=f"{len(ks)} frames animating ({'+'.join(why) or '?'}) with no traced change"))
    last_input = max((k for k, _ in inputs), default=0)
    tail = [k for k in range(n) if animating[k]]
    if animating and animating[-1] and (n - 1 - last_input) * dt_ms > 1000:
        q = frames[-1].get("quality", {})
        start = n - 1
        while start > 0 and animating[start - 1]:
            start -= 1
        busy = defaultdict(int)
        for fr in frames[start:]:
            for who, prop, _ in fr.get("tk", []):
                busy[(els[who].name if isinstance(who, int) and who in els else str(who), prop)] += 1
        top = sorted(busy.items(), key=lambda kv: -kv[1])[:4]
        findings.append(dict(kind="never-settles", element="(frame)", frames=n - start, longest=n - start, at=f"{start}–{n - 1}",
                             detail=f"still animating ({'+'.join(q.get('why', []))}) {((n - 1 - last_input) * dt_ms / 1000):.1f}s after the last input; "
                                    + ("busiest tracks: " + "; ".join(f"{short(nm)} {p} ×{c}" for (nm, p), c in top) if top else "no store tracks")))

    # pops
    def top_level(ids, k, table):
        s = set(ids)
        return [i for i in ids if els[i].parent not in s]

    for k, ids in births.items():
        for id in top_level(ids, k, births):
            e = els[id]
            h = [x for x in e.hist if x[0] >= k][:3]
            if not h:
                continue
            k0, x, y, w, hh, op, flags, frac, ons = h[0]
            if op < POP_OPACITY or frac == 0 or not ons or w * hh < 4:
                continue
            grows = len(h) > 1 and (abs(h[1][3] - w) > 0.5 or abs(h[1][4] - hh) > 0.5 or abs(h[1][5] - op) > 0.01)
            intent = flags >> 8
            if grows or (intent & (16 | 4 | 32)):
                continue
            moved = len(h) > 1 and (abs(h[1][1] - x) > JUMP_PT or abs(h[1][2] - y) > JUMP_PT)
            findings.append(dict(kind="pop-in" if not moved else "appear-then-jump", element=e.name, frames=1, longest=1, at=str(k),
                                 detail=f"mounted at opacity {op} size {w:.0f}×{hh:.0f} at ({x:.0f},{y:.0f})"
                                        + (f", then jumped to ({h[1][1]:.0f},{h[1][2]:.0f})" if moved else " with no transition")))
    for k, items in deaths.items():
        ids = [i for i, *_ in items]
        s = set(ids)
        for id, rect, op, was_seen in items:
            if els[id].parent in s or not was_seen or op < POP_OPACITY:
                continue
            findings.append(dict(kind="pop-out", element=els[id].name, frames=1, longest=1, at=str(k),
                                 detail=f"unmounted at opacity {op} size {rect[2]:.0f}×{rect[3]:.0f} at ({rect[0]:.0f},{rect[1]:.0f})"))

    # geometry: jumps, steps, overshoot, jiggle
    changed_parents = defaultdict(set)  # frame -> parents with births/deaths
    for k, ids in births.items():
        for i in ids:
            changed_parents[k].add(els[i].parent)
    for k, items in deaths.items():
        for i, *_ in items:
            changed_parents[k].add(els[i].parent)
    pressing = [bool(fr.get("k")) for fr in frames]
    for e in els.values():
        h = e.hist
        if len(h) < 3:
            continue
        # Only report the outermost element of a group moving together.
        for edge, label in ((1, "x"), (2, "y"), (3, "w"), (4, "h")):
            d = [h[i][edge] - h[i - 1][edge] if h[i][0] == h[i - 1][0] + 1 else 0.0 for i in range(1, len(h))]
            for i in range(1, len(h)):
                k = h[i][0]
                step = d[i - 1]
                if abs(step) < JUMP_PT:
                    continue
                st = h[i]
                if st[5] <= 0.05 or st[7] == 0 or not st[8] or pressing[k] or st[6] & 4:
                    continue
                intent = st[6] >> 8
                if intent & (1 if label in "xy" else 2):
                    continue
                before = d[i - 2] if i >= 2 else 0.0
                after = d[i] if i < len(d) else 0.0
                if abs(before) < 1.0 and abs(after) < 1.0:
                    kind = "sibling-jump" if any(els.get(e.parent) and p == e.parent for p in changed_parents.get(k, ())) else "jump"
                elif abs(step) > 3 * max(abs(before), abs(after), 1.0):
                    kind = "step"
                else:
                    continue
                parent = els.get(e.parent)
                if parent and any(x[0] == k for x in parent.hist):
                    ph = {x[0]: x for x in parent.hist}
                    if k in ph and k - 1 in ph and abs((ph[k][edge] - ph[k - 1][edge]) - step) < 0.5 and label in "xy":
                        continue  # rides its parent; the parent reports it
                findings.append(dict(kind=kind, element=e.name, frames=1, longest=1, at=str(k),
                                     detail=f"{label} {h[i - 1][edge]:.1f} → {st[edge]:.1f} ({step:+.1f}pt) in one frame; neighbours {before:+.1f}/{after:+.1f}"))
            # motion segments
            seg = []
            for i in range(1, len(h)):
                moving = abs(d[i - 1]) > 0.01
                if moving:
                    seg.append(i)
                if (not moving or i == len(h) - 1) and len(seg) >= 4:
                    vals = [h[seg[0] - 1][edge]] + [h[j][edge] for j in seg]
                    if all(h[j][5] <= 0.05 or not h[j][8] for j in seg):
                        seg = []
                        continue
                    if any(pressing[h[j][0]] or h[j][6] & 4 for j in seg):
                        seg = []
                        continue
                    rest = vals[-1]
                    start = vals[0]
                    travel = rest - start
                    signs = [math.copysign(1, b - a) for a, b in zip(vals, vals[1:]) if abs(b - a) > 0.05]
                    reversals = sum(1 for a, b in zip(signs, signs[1:]) if a != b)
                    past = max(((v - rest) * math.copysign(1, travel) for v in vals), default=0) if abs(travel) > 0.5 else 0
                    k0, k1 = h[seg[0]][0], h[seg[-1]][0]
                    if past > 1.0:
                        findings.append(dict(kind="overshoot", element=e.name, frames=k1 - k0 + 1, longest=k1 - k0 + 1, at=f"{k0}–{k1}",
                                             detail=f"{label} {start:.1f} → {rest:.1f}, overshoots by {past:.1f}pt ({100 * past / abs(travel):.0f}%)"))
                    elif reversals >= 2:
                        amp = max(vals) - min(vals)
                        if amp > 0.5:
                            findings.append(dict(kind="jiggle", element=e.name, frames=k1 - k0 + 1, longest=k1 - k0 + 1, at=f"{k0}–{k1}",
                                                 detail=f"{label} reverses {reversals}× over {amp:.1f}pt ({start:.1f} → {rest:.1f})"))
                if not moving:
                    seg = []

    # latency: input to first visible change
    for k, ev in inputs:
        if not ev.startswith(("press", "key", "submit", "double press", "secondary press")):
            continue
        j = next((j for j in range(k, min(n, k + 30)) if changed_by_frame[j]), None)
        if j is None:
            findings.append(dict(kind="latency", element="(input)", frames=30, longest=30, at=str(k), detail=f"{ev}: no visible change within 30 frames"))
        elif j - k >= 2:
            findings.append(dict(kind="latency", element="(input)", frames=j - k, longest=j - k, at=f"{k}→{j}",
                                 detail=f"{ev}: first visible change {j - k} frames ({(j - k) * dt_ms:.0f} ms) later"))

    # hitches during motion
    moving_frames = [k for k in range(1, n) if animating[k - 1]]
    if moving_frames:
        vals = [cpu[k] for k in moving_frames]
        med = statistics.median(vals)
        budget = dt_ms
        bad = [k for k in moving_frames if cpu[k] > max(budget, 3 * med)]
        if bad:
            worst = max(bad, key=lambda k: cpu[k])
            findings.append(dict(kind="hitch", element="(frame)", frames=len(bad), longest=len(bad), at=fmt_runs(runs(bad)),
                                 detail=f"{len(bad)} animating frames over {budget:.1f} ms CPU (median {med:.2f}, worst {cpu[worst]:.1f} at frame {worst})"))

    return dict(trace=path, frames=n, hz=hz, inputs=[(k, ev) for k, ev in inputs], findings=findings,
                animating_frames=sum(animating), idle_frames=len(idle))


def main():
    args = sys.argv[1:]
    out = None
    quiet = "--quiet" in args
    args = [a for a in args if a != "--quiet"]
    if "--json" in args:
        i = args.index("--json")
        out = args[i + 1]
        del args[i:i + 2]
    reports = [analyze(p) for p in args]
    for r in reports:
        name = r["trace"].rsplit("/", 1)[-1]
        print(f"== {name}: {r['frames']} frames, {r['animating_frames']} animating, {r['idle_frames']} idle, {len(r['findings'])} findings")
        if quiet:
            continue
        for f in sorted(r["findings"], key=lambda f: (f["kind"], -f["frames"])):
            print(f"  {f['kind']:<16} {short(f['element']):<60} @{f['at']:<14} {f['detail']}")
    if out:
        with open(out, "w") as f:
            json.dump(reports, f, indent=1)


if __name__ == "__main__":
    main()
