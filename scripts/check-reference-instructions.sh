#!/bin/sh
# Run BOTH revisions on the same Linux worker/toolchain with a shared target dir.
# First: ... save; on the candidate: ... check. Never silently bless a missing baseline.
set -eu
[ "$(uname -s)" = Linux ] || { echo 'Linux + Valgrind required' >&2; exit 1; }
command -v valgrind >/dev/null
command -v gungraun-runner >/dev/null
export RUSTC_WRAPPER=
export RUSTFLAGS="${RUSTFLAGS:-} -C llvm-args=-fp-contract=off"
baseline="${CARGO_TARGET_DIR:-target}/rune-ref-instruction-baseline"
case "${1:-check}" in
  save)
    cargo bench --locked -p rune-ref --bench instructions -- --save-baseline=main
    mkdir -p "$(dirname "$baseline")"
    { rustc -Vv; valgrind --version; gungraun-runner --version; } > "$baseline"
    ;;
  check)
    [ -f "$baseline" ] || { echo 'Missing baseline: measure base revision with save first' >&2; exit 1; }
    current=$(mktemp)
    trap 'rm -f "$current"' EXIT
    { rustc -Vv; valgrind --version; gungraun-runner --version; } > "$current"
    cmp "$baseline" "$current"
    cargo bench --locked -p rune-ref --bench instructions -- --baseline=main
    ;;
  *) echo 'usage: check-reference-instructions.sh save|check' >&2; exit 1 ;;
esac
