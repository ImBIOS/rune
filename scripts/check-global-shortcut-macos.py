#!/usr/bin/env python3
"""Run on pro with the built shortcut_probe binary. Does not change TCC permissions."""
import fcntl
import json
import os
import platform
import selectors
import subprocess
import sys
import time

CUA = "/Applications/CuaDriver.app/Contents/MacOS/cua-driver"
SESSION = "rune-global-shortcut"


def call(tool, arguments):
    result = subprocess.run([CUA, tool, json.dumps(arguments)], capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(result.stderr or result.stdout)
    value = json.loads(result.stdout)
    if value.get("isError"):
        raise RuntimeError(result.stdout)
    return value


def line(process, expected):
    deadline = time.monotonic() + 30
    pending = b""
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        while time.monotonic() < deadline:
            if not selector.select(max(0, deadline - time.monotonic())):
                break
            output = os.read(process.stdout.fileno(), 4096)
            if not output:
                raise RuntimeError(f"probe exited {process.poll()} before {expected}")
            pending += output
            while b"\n" in pending:
                output, pending = pending.split(b"\n", 1)
                output = output.decode()
                print(output.rstrip(), flush=True)
                if output.rstrip() == expected:
                    return
    raise RuntimeError(f"timed out waiting for {expected}")


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: check-global-shortcut-macos.py /path/to/shortcut_probe")
    if not platform.node().lower().startswith("kits-shiny-computer"):
        raise SystemExit("this native check must run on pro, not a fallback worker")
    with open("/tmp/pro-gui.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        owner = subprocess.Popen([sys.argv[1]], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, text=True, bufsize=1)
        try:
            line(owner, "READY")
            # A second process must get Carbon's real conflict, not an in-process duplicate.
            conflict = subprocess.run([sys.argv[1], "--probe-conflict"], input="", capture_output=True, text=True, timeout=30)
            if conflict.returncode or "CONFLICT" not in conflict.stdout:
                raise RuntimeError(conflict.stdout + conflict.stderr)
            print("SECOND PROCESS: CONFLICT", flush=True)
            # Inspect the native session before input. The registered app stays windowless.
            call("start_session", {"session": SESSION})
            call("list_apps", {"session": SESSION})
            result = call("press_key", {"session": SESSION, "target": {"kind": "desktop", "display_id": "primary"},
                                        "key": "k", "modifiers": ["cmd", "ctrl", "alt", "shift"]})
            print(json.dumps(result), flush=True)
            # Driver success alone is not proof: the application must report the handler.
            line(owner, "FIRED 1")
            print("PASS: Carbon activation reached the app state on its creating thread", flush=True)
        finally:
            owner.stdin.close()
            try:
                owner.wait(timeout=10)
            except subprocess.TimeoutExpired:
                owner.terminate()
                owner.wait(timeout=10)
            subprocess.run([CUA, "end_session", json.dumps({"session": SESSION})], capture_output=True, timeout=15)


if __name__ == "__main__":
    main()
