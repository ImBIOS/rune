#!/usr/bin/env python3
"""Source-watch regressions; no compiler, SSH, renderer or wall-clock waits."""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import tempfile
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


if __name__ == "__main__":
    unittest.main()
