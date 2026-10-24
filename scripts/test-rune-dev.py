#!/usr/bin/env python3
"""Source-watch, external-crate and desk-relay regressions; no compiler, SSH, renderer or wall-clock waits."""
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

loader = importlib.machinery.SourceFileLoader("rune_dev", str(Path(__file__).with_name("rune-dev")))
spec = importlib.util.spec_from_loader(loader.name, loader)
dev = importlib.util.module_from_spec(spec)
loader.exec_module(dev)


class WatchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.names = ["Cargo.toml", "examples/settings/src/main.rs", "examples/settings/assets/font.ttf"]
        for name in self.names:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"original")
        for mock in [patch.object(dev, "ROOT", self.root), patch.object(dev, "files", lambda: self.names)]:
            mock.start()
            self.addCleanup(mock.stop)

    def fresh(self):
        return {p: (self.root / p).read_bytes() for p in self.names if (self.root / p).is_file()}

    def test_cache_matches_full_reads_through_saves_replacements_additions_and_deletions(self):
        cache = {}
        reads = []
        read = Path.read_bytes

        def counted(path):
            reads.append(path)
            return read(path)

        with patch.object(Path, "read_bytes", counted):
            first = dev.inputs(cache)
            self.assertEqual(len(reads), 3)
            self.assertEqual(dev.inputs(cache), first)
            self.assertEqual(len(reads), 3, "unchanged scans must not re-read assets or source")
            source = self.root / self.names[1]
            saved = source.stat()
            source.write_bytes(b"modified")
            os.utime(source, ns=(saved.st_atime_ns, saved.st_mtime_ns))
            changed = dev.inputs(cache)
            self.assertEqual(changed[self.names[1]], b"modified")
            self.assertEqual(reads[3:], [source], "same-size edits with restored mtime must be read")
            replacement = self.root / "replacement"
            replacement.write_bytes(b"replaced")
            os.utime(replacement, ns=(saved.st_atime_ns, saved.st_mtime_ns))
            replacement.replace(source)
            self.assertEqual(dev.inputs(cache)[self.names[1]], b"replaced")
        (self.root / self.names[0]).unlink()
        self.names.append("examples/settings/src/added.rs")
        (self.root / self.names[-1]).write_bytes(b"new")
        self.assertEqual(dev.inputs(cache), self.fresh())
        self.assertNotIn("Cargo.toml", cache)

    def test_partial_read_does_not_publish_a_new_cache(self):
        cache = {}
        original = dev.inputs(cache)
        source = self.root / self.names[1]
        source.write_bytes(b"edit")
        read = Path.read_bytes

        def racing(path):
            data = read(path)
            if path == source:
                path.write_bytes(b"finished edit")
            return data

        with patch.object(Path, "read_bytes", racing):
            with self.assertRaises(OSError):
                dev.inputs(cache)
        self.assertEqual({p: data for p, (_, data) in cache.items()}, original)
        self.assertEqual(dev.inputs(cache), self.fresh())

    def test_shared_watch_retries_read_races_and_reports_each_change_once(self):
        initial = {"view.rs": b"old"}
        edited = {"view.rs": b"new"}
        process = Mock()
        process.poll.side_effect = [None, None, None, None, None, 0]
        on_change, on_tick = Mock(), Mock()
        with patch.object(dev.select, "select"), patch.object(dev, "inputs", side_effect=[initial, OSError("save in progress"), edited, edited, {}]):
            dev.watch(on_change, process, initial, {}, on_tick)
        self.assertEqual([call.args[0] for call in on_change.call_args_list], [edited, {}])
        self.assertEqual(on_tick.call_count, 5)


class ArgumentTests(unittest.TestCase):
    def test_examples_keep_their_old_command_lines(self):
        opts = dev.parse(["opencode", "--scene", "session", "--snap-on-patch", "a.png"])
        self.assertEqual((opts.example, opts.app_args), ("opencode", ["--scene", "session", "--snap-on-patch", "a.png"]))
        self.assertEqual(dev.parse(["settings", "--", "--port", "1"]).port, 18744, "after -- every flag is the app's")

    def test_external_crates_name_their_patchable_files(self):
        opts = dev.parse(["--manifest-path", "../app/Cargo.toml", "--bin", "app", "--features", "fixture", "--patch", "src/ui/*.rs", "--desk", "--", "--hot-control", "c.json"])
        self.assertEqual((opts.manifest_path, opts.bin, opts.features, opts.patch, opts.desk), (Path("../app/Cargo.toml"), "app", "fixture", ["src/ui/*.rs"], True))
        self.assertEqual(opts.app_args, ["--hot-control", "c.json"])
        for bad in [[], ["settings", "--manifest-path", "x/Cargo.toml"], ["--manifest-path", "x/Cargo.toml"], ["settings", "--patch", "*.rs"]]:
            with self.subTest(bad=bad), self.assertRaises(SystemExit), patch("sys.stderr"):
                dev.parse(bad)

    def test_remote_arguments_name_the_staged_crate_and_park_the_build_copy(self):
        opts = dev.parse(["--manifest-path", "/w/app/Cargo.toml", "--patch", "src/ui/**/*.rs", "--port", "9", "--", "--fixture", "f.json"])
        with patch.object(dev, "EXTERNAL", dev.External(Path("/w"), "external/app", ["app"], [])):
            self.assertEqual(dev.remote_argv(opts, False), ["--manifest-path", "external/app/Cargo.toml", "--patch", "src/ui/**/*.rs", "--port", "9", "--", "--fixture", "f.json"])
            self.assertEqual(dev.remote_argv(opts, True)[-1], "--desk")
            self.assertNotIn("f.json", dev.remote_argv(opts, True))

    def test_dx_gets_the_app_target_and_an_adhoc_profile(self):
        command = dev.dx_command("dx", 7, "app", "fixture,", "app", dev.PROFILE, [])
        self.assertEqual(command[command.index("--features") + 1], "hot,fixture")
        self.assertEqual(command[command.index("--bin") + 1], "app")
        cargo_args = next(a for a in command if a.startswith("--cargo-args="))
        self.assertIn("--config 'profile.rune-hot.inherits=\"dev\"'", cargo_args)
        self.assertFalse(any(a.startswith("--args") for a in command), "no app arguments: RUNE_HOT enables Rune")
        self.assertFalse(any(a.startswith("--cargo-args") for a in dev.dx_command("dx", 7, "app", "", None, [], ["--hot"])))


class ExternalTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def write(self, name, text=""):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def test_layout_mirrors_path_dependencies_and_their_workspace_files(self):
        self.write("work/Cargo.toml", '[workspace]\nmembers = ["app", "ui"]\n[workspace.dependencies]\nshared = { path = "../shared" }\n')
        self.write("work/Cargo.lock")
        self.write("work/.cargo/config.toml")
        self.write("work/app/Cargo.toml", '[package]\nname = "app"\n[features]\nhot = ["rune/hot"]\n[dependencies]\nui = { path = "../ui" }\nshared.workspace = true\nrune = { git = "https://example.com/rune" }\n[lib]\npath = "src/lib.rs"\n')
        self.write("work/ui/Cargo.toml", '[package]\nname = "ui"\n[target.\'cfg(unix)\'.dev-dependencies]\nleaf = { path = "../../leaf" }\n')
        self.write("shared/Cargo.toml", '[package]\nname = "shared"\n')
        self.write("leaf/Cargo.toml", '[package]\nname = "leaf"\n')
        layout = dev.external_layout(self.root / "work/app/Cargo.toml")
        self.assertEqual(layout.base, self.root)
        self.assertEqual(layout.crate, "external/work/app")
        self.assertEqual(layout.dirs, ["leaf", "shared", "work/app", "work/ui"])
        self.assertEqual(sorted(layout.tops), ["work/.cargo/config.toml", "work/Cargo.lock", "work/Cargo.toml"])
        self.assertEqual(dev.crate_settings(self.root / "work/app", self.root), ("app", False))
        self.write("work/Cargo.toml", '[workspace]\n[profile.rune-hot]\ninherits = "dev"\n')
        self.assertEqual(dev.crate_settings(self.root / "work/app", self.root), ("app", True))
        self.write("work/ui/Cargo.toml", '[package]\nname = "ui"\n')
        with self.assertRaisesRegex(SystemExit, "hot"):
            dev.crate_settings(self.root / "work/ui", self.root)

    def test_external_keys_read_the_original_files(self):
        self.write("app/Cargo.toml", '[package]\nname = "app"\n')
        self.write("app/src/ui/list.rs", "fn view() {}")
        self.write("app/target/debug/app", "binary")
        layout = dev.external_layout(self.root / "app/Cargo.toml")
        self.assertEqual(layout.crate, "external")
        with patch.object(dev, "EXTERNAL", layout):
            keys = dev.files()
            self.assertIn("external/src/ui/list.rs", keys)
            self.assertIn("scripts/rune-dev", keys)
            self.assertFalse(any("target/" in k for k in keys))
            self.assertEqual(dev.source("external/src/ui/list.rs").read_text(), "fn view() {}")
            self.assertEqual(dev.source("scripts/rune-dev"), dev.ROOT / "scripts/rune-dev")
            self.assertEqual(set(dev.inputs({})), {"external/Cargo.toml", "external/src/ui/list.rs"})

    def test_only_globbed_rust_files_in_the_app_crate_patch(self):
        allowed = dev.patchable("external/work/app", ["src/ui/*.rs", "src/views/**/*.rs"])
        self.assertTrue(allowed("external/work/app/src/ui/list.rs"))
        self.assertTrue(allowed("external/work/app/src/views/a/b/c.rs"))
        for key in ["external/work/app/src/ui/nested/list.rs", "external/work/app/src/ui/icon.svg", "external/work/app/src/main.rs",
                    "external/work/app/Cargo.toml", "external/work/ui/src/ui/list.rs", "external/work/application/src/ui/list.rs",
                    "external/rune/crates/rune/src/runtime.rs"]:
            self.assertFalse(allowed(key), key)


def ws_frame(opcode, payload, fin=True):
    return dev.frame(opcode, payload, fin)


class DeskTests(unittest.TestCase):
    def test_only_this_process_patch_is_fetched_and_rewritten(self):
        message = {"HotReload": {"jump_table": {"lib": "/build/libserver-patch-1.dylib", "map": {"4294983856": 4295000000}, "aslr_reference": 1, "new_base_address": 2},
                                 "templates": [], "assets": [], "ms_elapsed": 1, "for_pid": 42, "for_build_id": 0}}
        fetched = []
        def fetch(lib):
            fetched.append(lib)
            return Path("/desk/patches/libserver-patch-1.dylib")
        rewritten = json.loads(dev.rewrite_patch(json.dumps(message), 42, fetch))
        self.assertEqual(rewritten["HotReload"]["jump_table"]["lib"], "/desk/patches/libserver-patch-1.dylib")
        self.assertEqual(rewritten["HotReload"]["jump_table"]["map"], message["HotReload"]["jump_table"]["map"])
        self.assertEqual(fetched, ["/build/libserver-patch-1.dylib"])
        for other in [json.dumps(message).replace('"for_pid": 42', '"for_pid": 7'), '"HotPatchStart"', "not json", '{"HotReload": {"jump_table": null, "for_pid": 42}}']:
            self.assertEqual(dev.rewrite_patch(other, 42, fetch), other)
        self.assertEqual(len(fetched), 1)

    def test_a_patch_for_another_base_binary_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            desk = dev.Desk("build", Path(tmp), [], tmp, {})
            desk.remote_exe, desk.sha = "/build/server", "a" * 64
            result = Mock(returncode=0, stdout=b"b" * 64 + b"\nPATCH", stderr=b"")
            with patch.object(dev.subprocess, "run", return_value=result), self.assertRaisesRegex(RuntimeError, "not applying"):
                desk.fetch_patch("/build/libserver-patch-2.dylib")
            result.stdout = b"a" * 64 + b"\nPATCH"
            with patch.object(dev.subprocess, "run", return_value=result) as run:
                local = desk.fetch_patch("/build/libserver-patch-2.dylib")
            self.assertEqual(local.read_bytes(), b"PATCH")
            self.assertEqual(local.name, "libserver-patch-2.dylib")
            self.assertIn("shasum -a 256 /build/server", run.call_args.args[0][-1])

    def test_a_base_for_another_platform_fails_before_copying(self):
        local = {"system": "Darwin", "machine": "arm64"}
        self.assertIsNone(dev.check_base({"system": "Darwin", "machine": "arm64"}, local))
        self.assertIn("x86_64", dev.check_base({"system": "Darwin", "machine": "x86_64"}, local))
        with tempfile.TemporaryDirectory() as tmp, patch.object(dev, "platform_info", return_value=local), patch.object(dev.subprocess, "run") as run, patch("sys.stderr"):
            desk = dev.Desk("build", Path(tmp), [], tmp, {})
            desk.base({"exe": "/build/server", "sha256": "a" * 64, "system": "Linux", "machine": "arm64"})
        self.assertIn("Linux", desk.failed)
        run.assert_not_called()

    def test_relay_rewrites_fragmented_patches_and_passes_other_frames(self):
        upstream = socket.create_server(("127.0.0.1", 0))
        self.addCleanup(upstream.close)
        patch_message = json.dumps({"HotReload": {"jump_table": {"lib": "/build/p.dylib", "map": {}}, "for_pid": 42}}).encode()
        long = json.dumps({"FullReloadStart": "x" * 70000}).encode()
        seen = {}
        def dx():
            conn, _ = upstream.accept()
            head, _ = dev.read_head(conn)
            seen["head"] = head
            conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
            conn.sendall(ws_frame(9, b"ping") + ws_frame(1, patch_message[:10], False) + ws_frame(0, patch_message[10:]) + ws_frame(1, long))
            fin, op, payload = dev.read_frame(conn.makefile("rb"))
            seen["client"] = (fin, op, payload)
            conn.close()
        server = threading.Thread(target=dx, daemon=True)
        server.start()
        relay = dev.PatchRelay(upstream.getsockname(), lambda lib: Path("/desk/p.dylib"))
        self.addCleanup(relay.close)
        with socket.create_connection(("127.0.0.1", relay.port)) as app:
            app.sendall(b"GET /_dioxus?aslr_reference=1&build_id=0&pid=42 HTTP/1.1\r\nHost: x\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n")
            response, rest = dev.read_head(app)
            self.assertIn(b"101", response)
            data = rest
            def next_frame():
                nonlocal data
                while True:
                    try:
                        frame = dev.read_frame(io.BytesIO(data))
                        size = len(dev.frame(frame[1], frame[2], frame[0]))
                        data = data[size:]
                        return frame
                    except EOFError:
                        chunk = app.recv(65536)
                        if not chunk: raise
                        data += chunk
            frames = [next_frame() for _ in range(3)]
            mask = b"\x01\x02\x03\x04"
            payload = b"pong"
            app.sendall(bytes([0x8a, 0x80 | len(payload)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))
            server.join(5)
        self.assertNotIn(b"permessage-deflate", seen["head"].lower())
        self.assertEqual(frames[0], (True, 9, b"ping"))
        self.assertEqual(json.loads(frames[1][2])["HotReload"]["jump_table"]["lib"], "/desk/p.dylib")
        self.assertEqual(frames[2], (True, 1, long))
        self.assertEqual(seen["client"], (True, 10, b"pong"), "the app's frames reach dx unchanged")


if __name__ == "__main__":
    unittest.main()
