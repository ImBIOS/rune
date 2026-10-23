#!/usr/bin/env python3
"""Pro-only accessory -> Regular proof; uses existing TCC and /tmp/pro-gui.lock."""
import fcntl
import importlib
import json
import os
import platform
import subprocess
import sys

native = importlib.import_module("check-native-menu-macos")
native.SESSION = "rune-dock-menu"


def check(binary, order):
    process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, env=dict(os.environ, RUNE_MENU_ORDER=order))
    reader = native.Reader(process)
    def command(text):
        process.stdin.write((text + "\n").encode())
        process.stdin.flush()
    def status(custom, regular=True):
        command("status")
        lines = reader.until("STATUS")
        assert f"CUSTOM_PRESENT {str(custom).lower()}" in lines
        assert f"POLICY NSApplicationActivationPolicy({0 if regular else 1})" in lines
        return next(line.removeprefix("QUIT_KEY ") for line in lines if line.startswith("QUIT_KEY "))
    try:
        reader.until("ACCESSORY_CUSTOM" if order == "before" else "ACCESSORY_MISSING")
        command("show")
        reader.until("WINDOW_READY")
        native.call("start_session")
        windows = native.call("list_windows", pid=process.pid)
        window = next(w for w in windows["windows"] if w["pid"] == process.pid
                      and w.get("title") == "Rune Dock menu probe" and w.get("is_on_screen"))
        target = dict(pid=process.pid, window_id=window["window_id"])
        status(order == "before")
        if order == "after":
            command("custom")
            status(True)
        # A real AX menu action verifies the application menu is available after promotion.
        native.call("invoke_menu", **target, path=[os.path.basename(binary), "Custom Action"])
        reader.until("CUSTOM")
        command("clear")
        status(False)
        command("dock-off")
        status(False, regular=False)  # The same root/defaults survive hiding the Dock icon.
        command("dock-on")
        quit_key = status(False)
        native.call("press_key", **target, key=quit_key, modifiers=["cmd"], delivery_mode="foreground")
        process.wait(timeout=15)
        assert process.returncode == 0, process.returncode
        print(f"PASS {order}: accessory promotion, visible app menu, custom composition/removal, Dock toggles and Cmd-Q", flush=True)
    finally:
        if process.poll() is None:
            command("quit")
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
        subprocess.run([native.CUA, "end_session", json.dumps({"session": native.SESSION})],
                       capture_output=True, timeout=15)


def main():
    if len(sys.argv) != 2 or not platform.node().lower().startswith("kits-shiny-computer"):
        raise SystemExit("run on Pro: check-dock-menu-macos.py /path/to/dock_menu_probe")
    with open("/tmp/pro-gui.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        for order in ("after", "before"):
            check(sys.argv[1], order)


if __name__ == "__main__":
    main()
