#!/bin/sh
# Build, then render headless stills of every scene into captures/cursed (both themes).
# Run from the repo root: sh examples/cursed-settings/snap.sh
set -e
cargo build --release -p cursed-settings 2>&1 | grep -E '^(error|warning: unused)|-->|^\s+\|' | head -80 || true
cargo build --release -p cursed-settings -q
bin="${CARGO_TARGET_DIR:-target}/release/cursed-settings"
out=captures/cursed
rm -rf "$out" && mkdir -p "$out"
for theme in light dark; do
  for scene in ${SCENES:-general network security fire fence dark spotlight contrast frosted hot flushed}; do
    "$bin" --scene "$scene" --theme "$theme" --scale 1 --at "${AT:-2.0}" --snap "$out/$scene-$theme.png" > "$out/$scene-$theme.log" 2>&1 || echo "FAIL $scene $theme"
  done
done
ls "$out"
