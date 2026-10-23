#!/usr/bin/env python3
"""Pro-only native menu proof. Uses existing CuaDriver permissions; no TCC changes."""
import fcntl
import json
import os
import platform
import selectors
import subprocess
import sys
import time

CUA = "/Applications/CuaDriver.app/Contents/MacOS/cua-driver"
SESSION = "rune-native-menu"


def call(tool, **arguments):
    result = subprocess.run([CUA, tool, json.dumps(dict(session=SESSION, **arguments))],
                            capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(result.stderr or result.stdout)
    value = json.loads(result.stdout)
    if value.get("isError"):
        raise RuntimeError(result.stdout)
    print(tool, json.dumps(value), flush=True)
    if "structuredContent" in value:
        return value["structuredContent"]
    for part in value.get("content", []):
        if part.get("type") == "text":
            try:
                return json.loads(part["text"])
            except json.JSONDecodeError:
                pass
    return value


class Reader:
    def __init__(self, process):
        self.process = process
        self.pending = b""

    def until(self, expected):
        deadline = time.monotonic() + 30
        lines = []
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while time.monotonic() < deadline:
                while b"\n" in self.pending:
                    line, self.pending = self.pending.split(b"\n", 1)
                    line = line.decode().rstrip()
                    print(line, flush=True)
                    lines.append(line)
                    if line == expected:
                        return lines
                if not selector.select(max(0, deadline - time.monotonic())):
                    break
                chunk = os.read(self.process.stdout.fileno(), 4096)
                if not chunk:
                    raise RuntimeError(f"probe exited {self.process.poll()} before {expected}")
                self.pending += chunk
        raise RuntimeError(f"timed out waiting for {expected}")


def main():
    if len(sys.argv) != 2 or not platform.node().lower().startswith("kits-shiny-computer"):
        raise SystemExit("run on Pro: check-native-menu-macos.py /path/to/menu_probe")
    with open("/tmp/pro-gui.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        process = subprocess.Popen([sys.argv[1]], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT)
        reader = Reader(process)
        def command(text):
            process.stdin.write((text + "\n").encode())
            process.stdin.flush()
        try:
            reader.until("READY")
            call("start_session")
            windows = call("list_windows", pid=process.pid)
            records = windows.get("windows", windows) if isinstance(windows, dict) else windows
            window = next(w for w in records if w["pid"] == process.pid and w.get("title") == "Rune menu probe" and w.get("is_on_screen"))
            target = dict(pid=process.pid, window_id=window["window_id"])
            command("status")
            status = reader.until("STATUS")
            assert "NATIVE enabled=true checked=false" in status and "DOCK items=1" in status
            # winit labels the default app menu with NSProcessInfo.processName.
            call("invoke_menu", **target, path=[os.path.basename(sys.argv[1]), "Open Quick Panel"])
            reader.until("FIRED 1")
            call("press_key", **target, key="k", modifiers=["cmd", "alt", "shift"], delivery_mode="foreground")
            reader.until("FIRED 2")
            call("invoke_menu", **target, path=["Rune Tools", "Named Quick Panel"])
            reader.until("FIRED 3")
            command("disable"); command("status")
            status = reader.until("STATUS")
            assert "NATIVE enabled=false checked=true" in status and "COUNT 3" in status
            command("clear"); command("status")
            status = reader.until("STATUS")
            assert "NATIVE cleared" in status and "DOCK items=0" in status and "DEFAULTS preserved" in status
            print("PASS: app-menu click, key equivalent and named-menu click ran on app thread; runtime state, Dock hook and defaults verified", flush=True)
        finally:
            if process.poll() is None:
                command("quit")
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.terminate(); process.wait(timeout=10)
            subprocess.run([CUA, "end_session", json.dumps({"session": SESSION})], capture_output=True, timeout=15)
        assert process.returncode == 0, process.returncode


if __name__ == "__main__":
    main()
