#!/bin/sh
# Invoke through HiveMind on a worker. GPU is headless but still needs an adapter.
set -eu
export RUSTC_WRAPPER=
export RUSTFLAGS="${RUSTFLAGS:-} -C llvm-args=-fp-contract=off"
cargo test --locked -p rune-ref --test reference --test alloc
cargo test --locked -p rune --test alloc
if [ "${1:-}" = "--gpu" ]; then
    cargo test --locked -p rune-ref --features gpu --test differential -- --test-threads=1 --nocapture
fi
