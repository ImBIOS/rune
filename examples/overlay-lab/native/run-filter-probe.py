"""One native capsule still on the build Mac, while holding its shared GUI lock. Run on a build worker."""
import fcntl
import json
import os
import pathlib
import re
import selectors
import subprocess
import sys
import time

root = pathlib.Path(sys.argv[1]).resolve()
root.mkdir(parents=True, exist_ok=True)
lock = open('/tmp/pro-gui.lock', 'a')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
driver = pathlib.Path.home() / '.local/bin/cua-driver'
session = 'rune-layer-filter-overlay'
binary = pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'debug/layer-filter-overlay'
probe = root / 'presentation-probe.dylib'
subprocess.run(['xcrun', 'clang', '-dynamiclib', '-fobjc-arc', '-framework', 'AppKit',
    '-framework', 'QuartzCore', '-framework', 'Metal',
    'examples/overlay-lab/native/presentation-probe.m', '-o', str(probe)], check=True)

def cua(tool, **args):
    run = subprocess.run([str(driver), tool, json.dumps(dict(args, session=session))],
                         capture_output=True, text=True, timeout=45)
    (root / (tool + '.json')).write_text(run.stdout)
    if run.returncode:
        raise RuntimeError(run.stdout + run.stderr)
    value = json.loads(run.stdout)
    if value.get('isError') or value.get('error'):
        raise RuntimeError(run.stdout)
    return value

app = None
try:
    cua('start_session')
    app = subprocess.Popen([str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, env=dict(os.environ, DYLD_INSERT_LIBRARIES=str(probe)))
    ready = selectors.DefaultSelector()
    ready.register(app.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + 60
    pending = b''
    lines = []
    window = None
    shown = set()
    presented = set()
    while time.monotonic() < deadline:
        if not ready.select(max(0, deadline - time.monotonic())):
            break
        chunk = os.read(app.stdout.fileno(), 65536)
        if not chunk:
            raise RuntimeError('overlay exited before presentation')
        pending += chunk
        while b'\n' in pending:
            raw, pending = pending.split(b'\n', 1)
            line = raw.decode(errors='replace')
            lines.append(line)
            if 'filter-overlay created' in line:
                window = int(re.search(r'window_number: Some\((\d+)\)', line)[1])
            match = re.search(r'probe alpha window=(\d+) alpha=1\.0 ', line)
            if match:
                shown.add(int(match[1]))
            match = re.search(r'probe presented window=(\d+) .*presented=([\d.]+)', line)
            if match and float(match[2]) > 0:
                presented.add(int(match[1]))
        if window in shown and window in presented:
            break
    (root / 'presentation.log').write_text('\n'.join(lines) + '\n')
    if window not in shown or window not in presented:
        raise TimeoutError('no visible, positively presented overlay drawable')
    cua('get_window_state', pid=app.pid, window_id=window, include_accessibility_tree=False,
        max_image_dimension=0, screenshot_out_file=str(root / 'native-overlay.png'))
    if not (root / 'native-overlay.png').is_file():
        raise RuntimeError('Cua did not capture the requested window')
    result = dict(pid=app.pid, window=window, positive_presentation=True,
                  screenshot='native-overlay.png', fixture='400x40pt; sigma 1.5pt; opacity .65')
    (root / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result), flush=True)
finally:
    if app is not None:
        app.stdin.close()
        try:
            app.wait(timeout=15)
        except subprocess.TimeoutExpired:
            app.terminate()
            app.wait(timeout=10)
    cua('end_session')
    probe.unlink(missing_ok=True)
