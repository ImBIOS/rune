#!/bin/sh
# Records a motion trace of every element (--trace-all) for each scenario below, at 120 fps on
# the virtual clock, plus the final element tree. Then scripts/motion-audit/analyze.py reads them.
# Run from the repo root: scripts/motion-audit/run.sh [out-dir] (default target/motion-audit).
set -e
O=${1:-target/motion-audit}
mkdir -p "$O"
cargo build --release -p opencode -p todo -p settings -p kit-gallery
B=${CARGO_TARGET_DIR:-target}/release
S=scripts/motion-audit/scenarios
OC="$B/opencode --fixture examples/opencode/assets/fixture.json"
FPS=120

# name | seconds | command (trace args appended)
run() {
  name=$1; secs=$2; shift 2
  frames=$(python3 -c "print(int($secs * $FPS))")
  echo "== $name ($frames frames)"
  "$@" --fps $FPS --frames "$frames" --trace-all --trace "$O/$name.jsonl" --tree "$O/$name-end.json" 2>&1 | grep -E "error|panic" || true
}

run oc-idle-session   3.0 $OC --scene session
run oc-idle-streaming 4.0 $OC --scene streaming
run oc-idle-status    3.5 $OC --scene status
run oc-idle-home      2.0 $OC
run oc-toggle         3.2 $OC --scene session --script examples/opencode/scripts/toggle.json
run oc-toggle-interrupt 2.6 $OC --scene session --script $S/opencode-toggle-interrupt.json
run oc-switch         8.5 $OC --script examples/opencode/scripts/switch.json
run oc-tab-hover      2.6 $OC --scene session --script examples/opencode/scripts/tab-hover.json
run oc-close-tab      3.6 $OC --scene session --script $S/opencode-close-tab.json
run oc-new-session    5.2 $OC --scene session --script $S/opencode-new-session.json
run oc-resize         3.4 $OC --scene session --script examples/opencode/scripts/resize.json
run oc-scroll         3.6 $OC --scene session --script examples/opencode/scripts/scroll.json
run oc-send           4.0 $OC --scene session --script examples/opencode/scripts/send.json
run oc-send-overlap   3.0 $OC --scene session --script examples/opencode/scripts/send-overlap.json
run oc-undo-redo      5.0 $OC --scene session --script examples/opencode/scripts/undo-redo.json
run oc-pickers        8.0 $OC --scene session --script examples/opencode/scripts/pickers.json
run todo-add-remove   4.4 $B/todo --script $S/todo-add-remove.json
run settings-toggle-retarget 1.5 $B/settings --script examples/settings/scripts/toggle-retarget.json
run settings-segmented-retarget 1.5 $B/settings --script examples/settings/scripts/segmented-retarget.json
run settings-slider   3.0 $B/settings --script examples/settings/scripts/slider.json
run settings-hover    3.0 $B/settings --script examples/settings/scripts/hover.json
run gallery-combobox  3.0 $B/kit-gallery --scene rune-combobox--searchable --script examples/kit-gallery/scripts/combobox.json
run gallery-modal     5.5 $B/kit-gallery --scene motion-focus-modal --script examples/kit-gallery/scripts/motion-focus-modal.json
ls "$O"
