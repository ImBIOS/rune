#!/usr/bin/env python3
"""Regenerates assets/theme.json from @opencode/ui's v2 tokens.

usage: tools/theme.py <opencode>/packages/ui/src/styles/tokens > assets/theme.json

Palette entries come from colors.css; light and dark tokens from the explicit
[data-color-scheme] blocks in theme.css. Tokens that are not plain colours
(shadows, rgba(), color-mix()) are skipped; the app spells those out.
"""
import json
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
colors = (root / "colors.css").read_text()
theme = (root / "theme.css").read_text()

decl = re.compile(r"--v2-([a-z0-9-]+):\s*([^;]+);")
palette = {k: v.strip() for k, v in decl.findall(colors)}


def block(selector):
    start = theme.index(selector)
    depth, i = 0, theme.index("{", start)
    for j in range(i, len(theme)):
        depth += {"{": 1, "}": -1}.get(theme[j], 0)
        if depth == 0:
            return theme[i + 1 : j]
    raise SystemExit(f"unclosed {selector}")


def tokens(src):
    raw = {k: " ".join(v.split()) for k, v in decl.findall(src)}
    out = {}
    for k in raw:
        v, seen = raw[k], set()
        while (m := re.fullmatch(r"var\(--v2-([a-z0-9-]+)\)", v)) and m.group(1) not in seen:
            seen.add(m.group(1))
            name = m.group(1)
            if name in palette:
                v = name
                break
            v = raw.get(name, v)
        if v in palette or re.fullmatch(r"#[0-9a-fA-F]{3,8}", v):
            out[k] = v
    return out


light = tokens(block('[data-color-scheme="light"]'))
dark = tokens(block('[data-color-scheme="dark"]'))
# Fixed tokens (avatars) live only in :root.
fixed = tokens(theme[: theme.index("/* OS preference fallback")])
for k, v in fixed.items():
    if k.startswith("avatar-"):
        light.setdefault(k, v)
        dark.setdefault(k, v)

json.dump(
    {
        "name": "OpenCode",
        "blend": {"mode": "srgb", "over": "background-bg-base"},
        "palette": palette,
        "light": dict(sorted(light.items())),
        "dark": dict(sorted(dark.items())),
        "numbers": {"radius-panel": 10, "radius-row": 6, "inset-shell": 8, "container-session": 744},
    },
    sys.stdout,
    indent=1,
)
print()
