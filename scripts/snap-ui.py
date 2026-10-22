#!/usr/bin/env python3
"""Pin headless regression images. Usage: snap-ui.py OUT [BIN_DIR]. No native windows."""
import pathlib
import subprocess
import sys

out = pathlib.Path(sys.argv[1])
bin_dir = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else pathlib.Path('target/release')
out.mkdir(parents=True, exist_ok=True)
f = ['--fixture', 'examples/opencode/assets/fixture.json']
cases = {
    'home': ('opencode', f + ['--scene', 'home']),
    'session-bottom': ('opencode', f + ['--scene', 'session']),
    'session-light': ('opencode', f + ['--scene', 'session', '--theme', 'light']),
    'session-top': ('opencode', f + ['--scene', 'session', '--script', 'examples/opencode/scripts/scroll.json', '--at', '3']),
    'streaming': ('opencode', f + ['--scene', 'streaming', '--at', '4']),
    'stream-end': ('opencode', f + ['--scene', 'streaming', '--at', '12']),
    'send': ('opencode', f + ['--scene', 'session', '--script', 'examples/opencode/scripts/send.json', '--at', '4']),
    'settings-dark': ('settings', ['--scene', 'dark']),
    'settings-light': ('settings', ['--scene', 'light']),
}
for name, (binary, args) in cases.items():
    subprocess.run([str(bin_dir / binary), *args, '--snap', str(out / f'{name}.png')], check=True)
