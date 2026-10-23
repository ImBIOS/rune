#!/bin/sh
# Render the showcase clip headless and encode it (H.264, yuv420p, 60 fps) plus a poster and
# review stills into captures/cursed-show. Raw frames are deleted.
# Run from the repo root: sh examples/cursed-settings/showcase.sh
set -e
cargo build --release -p cursed-settings 2>&1 | grep -E '^error|-->' | head -40 || true
cargo build --release -p cursed-settings -q
bin="${CARGO_TARGET_DIR:-target}/release/cursed-settings"
out=captures/cursed-show
rm -rf "$out" && mkdir -p "$out"
script=examples/cursed-settings/scripts/showcase.json
secs=${SECS:-39.8}
frames=$(python3 -c "print(int($secs * 60))")
for theme in ${THEMES:-light dark}; do
  dir=$(mktemp -d)
  "$bin" --cursor --theme "$theme" --scale 2 --script "$script" --frames "$frames" --fps 60 --out "$dir" > "$out/$theme.log" 2>&1
  ffmpeg -loglevel error -y -framerate 60 -i "$dir/frame_%04d.png" -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -r 60 -movflags +faststart "$out/showcase-$theme.mp4"
  # Review stills: a moment after each punchline lands.
  for t in ${REVIEW:-1.6 1.8 1.9 2.2 3.9 4.6 5.3 8.6 9.0 9.8 11.0 13.9 14.5 15.6 16.0 18.5 21.9 24.3 28.5 30.0 30.6 31.0 31.6 32.4 34.3 34.5 35.4 36.6 37.7 39.0}; do
    n=$(python3 -c "print(f'{int(round($t * 60)):04d}')")
    [ -f "$dir/frame_$n.png" ] && sips -s format jpeg -s formatOptions 80 -Z 1280 "$dir/frame_$n.png" --out "$out/$theme-$t.jpg" > /dev/null
  done
  cp "$dir/frame_$(python3 -c "print(f'{int(9.8 * 60):04d}')").png" "$out/poster-$theme.png"
  rm -rf "$dir"
done
ls -la "$out" | head -80
