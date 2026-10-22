#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow>=10", "numpy>=1.26"]
# ///
"""Diffs rune-kit's gallery against @opencode/ui's Storybook, story by story.

usage:
  OPENCODE=<opencode checkout> scripts/kit-diff.py [--out perf/kit-diff] [--only button,switch]
                                                   [--schemes light,dark] [--no-build]

Needs: `bun install` and `bun run --cwd packages/storybook build` in the OpenCode checkout
(Playwright's Chromium comes from that checkout), and a GPU or lavapipe for Rune
(VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json on a headless Linux box).

For every story in STORIES and each scheme it renders the web story in headless Chromium at
2x (viewport fitted to the story), renders the gallery scene of the same id at the same size,
and writes web.png, rune.png, diff.png (amplified |web - rune|) and side.png (web | rune |
diff) per story, plus report.json and report.md with two numbers per story:

  similarity  100 * (1 - mean |web - rune| / 255) over RGB; 100 is identical
  off         % of pixels whose largest channel differs by more than 24/255 (visible)
"""

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent

# Every gallery scene is diffed against the Storybook story of the same id. Per-story options:
# width (viewport points, default 720) and actions run before the web shot (click/hover/focus a
# selector) to reach the state the Rune scene starts in.
MENU = {"actions": [{"click": "#storybook-root button"}]}
OPTIONS: dict[str, dict] = {
    "ui-dialog--basic": {"height": 480},
    "ui-menu--basic": MENU,
    "ui-menu--rich": MENU,
    "ui-menu--with-checkbox": MENU,
    "ui-menu--with-radio": MENU,
    "ui-menu--with-submenu": MENU,
    "ui-select--playground": {"actions": [{"click": "[data-component=select-v2]"}]},
    "ui-select--grouped": {"actions": [{"click": "[data-component=select-v2]"}]},
    "components-hover-card--basic": {"actions": [{"hover": "[data-slot=hover-card-trigger]", "wait": 1200}]},
    "ui-tooltip--simple": {"actions": [{"hover": "[data-component=tooltip-v2-trigger]", "wait": 900}]},
    "ui-tooltip--with-keybind": {"actions": [{"hover": "[data-component=tooltip-v2-trigger]", "wait": 900}]},
    "ui-tooltip--path": {"actions": [{"hover": "[data-component=tooltip-v2-trigger]", "wait": 900}]},
    "ui-tooltip--title-description": {"actions": [{"hover": "[data-component=tooltip-v2-trigger]", "wait": 900}]},
    "ui-toast--all-examples": {"actions": [{"click": "#storybook-root button", "wait": 900}], "height": 480},
}


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def load(p: Path) -> np.ndarray:
    return np.asarray(Image.open(p).convert("RGB")).astype(np.int16)


def compare(web: Path, rune: Path, out: Path) -> dict:
    a, b = load(web), load(rune)
    h, w = min(a.shape[0], b.shape[0]), min(a.shape[1], b.shape[1])
    a, b = a[:h, :w], b[:h, :w]
    d = np.abs(a - b)
    similarity = 100.0 * (1.0 - d.mean() / 255.0)
    off = 100.0 * float((d.max(axis=2) > 24).mean())
    heat = np.clip(d.max(axis=2) * 4, 0, 255).astype(np.uint8)
    diff = np.stack([heat, np.zeros_like(heat), np.zeros_like(heat)], axis=2)
    base = (0.25 * a.mean(axis=2, keepdims=True)).astype(np.uint8)
    diff = np.maximum(diff, np.repeat(base, 3, axis=2))
    Image.fromarray(diff).save(out / "diff.png")
    gap = 8
    side = np.full((h, w * 3 + gap * 2, 3), 128, np.uint8)
    side[:, :w] = a.astype(np.uint8)
    side[:, w + gap : 2 * w + gap] = b.astype(np.uint8)
    side[:, 2 * (w + gap) :] = diff
    Image.fromarray(side).save(out / "side.png")
    return {"similarity": round(similarity, 3), "off": round(off, 3)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(ROOT / "perf/kit-diff"))
    ap.add_argument("--only", default="")
    ap.add_argument("--schemes", default="light,dark")
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--scale", type=float, default=2.0)
    args = ap.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("OPENCODE", str(ROOT.parent / "opencode"))

    if not args.no_build:
        sh(["cargo", "build", "--release", "-q", "-p", "kit-gallery"], cwd=ROOT)
    gallery = ROOT / "target/release/kit-gallery"
    scenes = set(subprocess.run([gallery, "--list-scenes"], capture_output=True, text=True, check=True).stdout.split())

    only = [s for s in args.only.split(",") if s]
    ids = [i for i in sorted(scenes) if "--" in i and (not only or any(o in i for o in only))]
    schemes = args.schemes.split(",")

    jobs = []
    for i in ids:
        for sc in schemes:
            d = out / i / sc
            d.mkdir(parents=True, exist_ok=True)
            o = OPTIONS.get(i, {})
            jobs.append({"id": i, "scheme": sc, "width": o.get("width", 720), "out": str(d / "web.png"),
                         "actions": o.get("actions", []), "scale": args.scale, **({"height": o["height"]} if "height" in o else {})})
    shots = subprocess.run(["node", str(ROOT / "scripts/kit/shots.mjs")], input=json.dumps(jobs), capture_output=True,
                           text=True, cwd=ROOT)
    if shots.returncode != 0:
        sys.exit(shots.stderr)
    sizes = {(r["id"], r["scheme"]): (r["width"], r["height"]) for r in json.loads(shots.stdout)}

    env = dict(os.environ)
    if sys.platform.startswith("linux") and Path("/usr/share/vulkan/icd.d/lvp_icd.json").exists():
        env.setdefault("VK_ICD_FILENAMES", "/usr/share/vulkan/icd.d/lvp_icd.json")
    report = []
    for i in ids:
        for sc in schemes:
            d = out / i / sc
            w, h = sizes[(i, sc)]
            subprocess.run([gallery, "--scene", i, "--snap", d / "rune.png", "--theme", sc, "--size", f"{w}x{h}",
                            "--scale", str(args.scale)], check=True, env=env, capture_output=True)
            r = {"id": i, "scheme": sc, "size": [w, h], **compare(d / "web.png", d / "rune.png", d)}
            report.append(r)
            print(f"{i:52} {sc:5}  similarity {r['similarity']:7.3f}  off {r['off']:6.2f}%")

    (out / "report.json").write_text(json.dumps(report, indent=1))
    lines = ["| story | scheme | similarity | off (% px) |", "|---|---|---:|---:|"]
    lines += [f"| {r['id']} | {r['scheme']} | {r['similarity']:.2f} | {r['off']:.2f} |" for r in report]
    mean = sum(r["similarity"] for r in report) / max(len(report), 1)
    lines.append(f"\nmean similarity {mean:.2f} over {len(report)} renders")
    (out / "report.md").write_text("\n".join(lines) + "\n")
    print(f"mean similarity {mean:.3f} over {len(report)} renders -> {out}/report.md")


if __name__ == "__main__":
    main()
