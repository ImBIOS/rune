#!/usr/bin/env python3
"""Refresh text-affected stills, reusing the committed Storybook reference columns.

Run after building settings, opencode and kit-gallery in release mode. No browser.
Historical motion strips are separate captures; this updates only named stills.
"""
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'perf/autoresearch/text/goldens'
OUT.mkdir(parents=True, exist_ok=True)
spec = importlib.util.spec_from_file_location('kit_diff', ROOT / 'scripts/kit-diff.py')
kit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(kit)


def snap(binary, path, *args):
    subprocess.run([str(ROOT / 'target/release' / binary), *args, '--snap', str(path)],
                   cwd=ROOT, check=True, capture_output=True)


fixture = ['--fixture', 'examples/opencode/assets/fixture.json']
for name, args in {
    'home': ['--scene', 'home'],
    'session-bottom': ['--scene', 'session'],
    'session-light': ['--scene', 'session', '--theme', 'light'],
    'session-top': ['--scene', 'session', '--script', 'examples/opencode/scripts/scroll.json', '--at', '3'],
    'streaming': ['--scene', 'streaming', '--at', '4'],
}.items():
    snap('opencode', ROOT / f'demo/opencode/{name}.png', *fixture, *args)
snap('settings', ROOT / 'demo/settings-dark.png', '--theme', 'dark')
snap('settings', ROOT / 'demo/settings-light.png', '--scene', 'light')

report = []
for path in sorted((ROOT / 'demo/kit').glob('side-*.png')):
    story, scheme = path.stem.removeprefix('side-').rsplit('-', 1)
    directory = OUT / f'{story}-{scheme}'
    directory.mkdir(exist_ok=True)
    image = Image.open(path).convert('RGB')
    width, height = (image.width - 16) // 3, image.height
    image.crop((0, 0, width, height)).save(directory / 'web.png')
    snap('kit-gallery', directory / 'rune.png', '--scene', story, '--theme', scheme,
         '--size', f'{width // 2}x{height // 2}', '--scale', '2')
    result = {'story': story, 'scheme': scheme, **kit.compare(directory / 'web.png', directory / 'rune.png', directory)}
    report.append(result)
    shutil.copyfile(directory / 'side.png', path)
for scheme in ('light', 'dark'):
    snap('kit-gallery', ROOT / f'demo/kit/index-{scheme}.png',
         '--theme', scheme, '--size', '720x1400', '--scale', '1')

(ROOT / 'demo/text/kit-metrics.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
