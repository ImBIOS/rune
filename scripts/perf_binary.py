"""Benchmarks time `--profile perf` binaries only. Release builds are incremental, which can move timings."""
import os
from pathlib import Path
import sys


def _is_target(path):
    target = os.environ.get('CARGO_TARGET_DIR')
    return path.name == 'target' or (target is not None and path == Path(target).resolve())


def perf_binary(path):
    path = Path(path).resolve()
    profile = path.parent.parent if path.parent.name == 'examples' else path.parent
    # <target>/perf/<bin>, <target>/perf/examples/<bin> or <target>/<triple>/perf/...
    if profile.name != 'perf' or not (_is_target(profile.parent) or _is_target(profile.parent.parent)):
        sys.exit(f'refusing to benchmark {path}: not a --profile perf build.\n'
                 'Build with `cargo build --profile perf -p <package>` and pass target/perf/<binary> '
                 '(target/perf/examples/<example> for examples; a saved copy must keep the '
                 'target/perf/ layout). Release builds are incremental, so their timings are not comparable.')
    if not path.is_file():
        sys.exit(f'missing perf binary {path}: build it with `cargo build --profile perf`.')
    return path
