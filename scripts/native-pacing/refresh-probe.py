"""Pro-only, no capture. Runs refresh-probe.m variants under /tmp/pro-gui.lock.

python3 scripts/native-pacing/refresh-probe.py <out> [mode:host:drawables ...]
"""
import fcntl
import socket
import subprocess
import sys
from pathlib import Path

assert socket.gethostname().startswith("Kits-Shiny-Computer"), "Pro only"
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
variants = sys.argv[2:] or ["present:sub:3", "transaction:backing:3", "minduration:sub:3", "present:sub:2", "unsynced:sub:3"]
binary = out / "refresh-probe"
subprocess.run(["clang", "-fobjc-arc", "-O2", str(Path(__file__).with_name("refresh-probe.m")),
                "-framework", "AppKit", "-framework", "QuartzCore", "-framework", "Metal",
                "-framework", "CoreVideo", "-Wno-deprecated-declarations", "-o", str(binary)], check=True)
with open("/tmp/pro-gui.lock", "a") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    for variant in variants:
        mode, host, drawables = variant.split(":")
        result = subprocess.run([str(binary), mode, host, drawables, "5"], capture_output=True, text=True, timeout=30)
        text = result.stdout + result.stderr
        (out / f"{mode}-{host}-{drawables}.log").write_text(text)
        print(text, flush=True)
binary.unlink(missing_ok=True)
