#!/usr/bin/env python3
"""Prints one element's per-frame state from a Rune trace: frame, t, rect, opacity, flags,
visible rect, and the tracks sampled for it.

Usage: scripts/motion-audit/track.py TRACE.jsonl NAME-SUBSTRING [FROM [TO]] [--all-names]
"""

import json
import sys


def main():
    path, needle = sys.argv[1], sys.argv[2]
    rest = [a for a in sys.argv[3:] if not a.startswith("--")]
    lo = int(rest[0]) if rest else 0
    hi = int(rest[1]) if len(rest) > 1 else 10**9
    names, state, alive = {}, {}, set()
    with open(path) as f:
        f.readline()
        frames = [json.loads(l) for l in f if l.strip()]
    if "--all-names" in sys.argv:
        for fr in frames:
            for m in fr.get("m", []):
                if needle in m["nm"]:
                    names[m["id"]] = m["nm"]
        for id, n in names.items():
            print(id, n)
        return
    for k, fr in enumerate(frames):
        for m in fr.get("m", []):
            names[m["id"]] = m["nm"]
        rows = {r[0]: r for r in fr.get("e", [])}
        for id, r in rows.items():
            state[id] = r
            alive.add(id)
        gone = set(fr.get("g", []))
        tk = {}
        for who, prop, active in fr.get("tk", []):
            tk.setdefault(who, []).append(prop + ("" if active else "·"))
        if not (lo <= k <= hi):
            alive -= gone
            continue
        for id in sorted(set(rows) | (gone & alive)):
            if needle not in names.get(id, ""):
                continue
            if id in gone:
                print(f"{k:4} {fr['t']:8.1f} {names[id][-50:]:<50} GONE")
                continue
            r = state[id]
            vis = f" vis={r[8:12]}" if len(r) >= 12 else ""
            q = fr.get("quality", {})
            print(f"{k:4} {fr['t']:8.1f} {names[id][-50:]:<50} x={r[1]:<8} y={r[2]:<8} w={r[3]:<8} h={r[4]:<8} op={r[5]:<6} fl={r[7]}{vis} tk={tk.get(id, '')} {'+'.join(q.get('why', []))}")
        alive -= gone


if __name__ == "__main__":
    main()
