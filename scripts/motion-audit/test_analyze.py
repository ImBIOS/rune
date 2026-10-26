#!/usr/bin/env python3
"""Checks analyze.py on small synthetic traces. Run: python3 scripts/motion-audit/test_analyze.py"""

import json
import os
import sys
import tempfile

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(__file__))
import analyze  # noqa: E402


def trace(states):
    """A 120 Hz trace of one 10×10 element; `states` holds (x, opacity, visible width) per frame."""
    lines = [json.dumps({"width": 200, "height": 100, "hz": 120})]
    for k, (x, op, vis_w) in enumerate(states):
        fr = {"e": [[1, x, 40, 10, 10, op, 0, 0, max(x, 20), 40, vis_w, 10]], "quality": {"animating": True}}
        if k == 0:
            fr["m"] = [{"id": 1, "nm": "label/sheen"}]
        lines.append(json.dumps(fr))
    f = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
    f.write("\n".join(lines) + "\n")
    f.close()
    return f.name


def jumps(states):
    path = trace(states)
    try:
        return [f for f in analyze.analyze(path)["findings"] if f["kind"] in ("jump", "step")]
    finally:
        os.unlink(path)


def sheen(wrap_from, wrap_to):
    """A loop sliding right 0.5 pt a frame, then wrapping back to its start."""
    before = [(86 - 0.5 * (9 - i), *wrap_from) for i in range(10)]
    after = [(15 + 0.5 * i, *wrap_to) for i in range(10)]
    return before + after


def main():
    assert not jumps(sheen((1.0, 0.0), (1.0, 5.0))), "a wrap from clipped away isn't a jump"
    assert not jumps(sheen((0.0, 10.0), (1.0, 5.0))), "a wrap from transparent isn't a jump"
    assert not jumps(sheen((1.0, 10.0), (0.0, 10.0))), "a wrap into transparent isn't a jump"
    found = jumps(sheen((1.0, 10.0), (1.0, 10.0)))
    assert len(found) == 1 and found[0]["at"] == "10", f"a wrap in sight is: {found}"
    print("ok")


if __name__ == "__main__":
    main()
