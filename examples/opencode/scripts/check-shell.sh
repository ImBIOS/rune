#!/bin/sh
# Headless shell checks (fixture only) for the OpenCode example: snapshots with the traffic-light overlay,
# strips and traces for resize, toggle, hover and status, in perf/autoresearch/shell/out.
# Run from the repo root after a release build.
set -e
O=perf/autoresearch/shell/out
rm -rf "$O" && mkdir -p "$O" && for t in resize toggle hover status switch; do mkdir -p "$O/lint/$t"; done
B=${CARGO_TARGET_DIR:-target}/release/opencode
F="--fixture examples/opencode/assets/fixture.json"
for sc in home session streaming draft sidebar-closed status; do
  $B $F --scene $sc --chrome --snap "$O/$sc.png" --tree "$O/$sc-tree.json" 2>&1 | grep -E "chrome|error|panic" | sed "s/^/$sc: /" || true
done
$B $F --scene status --at 3.2 --snap "$O/status-end.png" 2>&1 | grep -E "error|panic" || true
$B $F --scene session --chrome --theme light --snap "$O/session-light.png" 2>&1 | grep -E "chrome|panic" || true
$B $F --scene session --script examples/opencode/scripts/resize.json --start 0.8 --frames 160 --every 8 --cols 5 \
  --strip "$O/resize-strip.png" --crop 0,0,720,300 --trace "$O/lint/resize/trace.jsonl" 2>&1 | grep -E "error|panic" || true
$B $F --scene session --script examples/opencode/scripts/toggle.json --start 0.35 --frames 156 --every 6 --cols 6 \
  --strip "$O/toggle-strip.png" --crop 0,0,520,140 --trace "$O/lint/toggle/trace.jsonl" 2>&1 | grep -E "error|panic" || true
$B $F --scene session --script examples/opencode/scripts/tab-hover.json --start 0.2 --frames 120 --every 10 --cols 4 \
  --strip "$O/hover-strip.png" --crop 0,160,300,280 --trace "$O/lint/hover/trace.jsonl" 2>&1 | grep -E "error|panic" || true
$B $F --scene status --frames 200 --every 10 --cols 5 \
  --strip "$O/status-strip.png" --crop 0,230,300,240 --trace "$O/lint/status/trace.jsonl" 2>&1 | grep -E "error|panic" || true
$B $F --script examples/opencode/scripts/switch.json --frames 540 --every 60 --cols 3 \
  --strip "$O/switch-strip.png" --crop 0,0,700,420 --trace "$O/lint/switch/trace.jsonl" 2>&1 | grep -E "error|panic" || true
$B $F --scene session --scale 6 --frames 108 --every 6 --cols 9 --crop 230,236,28,28 --strip "$O/spinner-zoom.png" 2>&1 | grep -E "panic" || true
$B $F --scene session --scale 4 --script examples/opencode/scripts/resize.json --start 0.3 --frames 60 --every 6 --cols 5 --crop 230,380,60,80 --strip "$O/handle-zoom.png" 2>&1 | grep -E "panic" || true
ls "$O"
# motion-lint (on a machine with bun): for d in $O/lint/*; do bun ~/code/open-source/motion-lint/cli/motion-lint.ts analyze $d --check; done
