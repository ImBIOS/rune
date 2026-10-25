#!/usr/bin/env bash
# Run from the app's Cargo workspace. Produces a static, HTTPS-hostable folder.
set -euo pipefail
package=${1:?usage: web-build.sh PACKAGE [--example NAME] [--out DIR] [--opt]}
shift
example=
out=target/web
opt=false
while (($#)); do
    case "$1" in
        --example) example=${2:?--example needs a name}; shift 2 ;;
        --out) out=${2:?--out needs a directory}; shift 2 ;;
        --opt) opt=true; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
meta=$(cargo metadata --format-version 1 --no-deps)
selection=$(python3 -c '
import json,sys
m=json.load(sys.stdin); p=next((p for p in m["packages"] if p["name"]==sys.argv[1]),None)
if p is None: sys.exit("package is not a workspace member")
targets=[t for t in p["targets"] if "example" in t["kind"] and t["name"]==sys.argv[2]] if sys.argv[2] else [t for t in p["targets"] if "cdylib" in t["crate_types"]]
if not targets and not sys.argv[2]: targets=[t for t in p["targets"] if "bin" in t["kind"]]
if len(targets)!=1: sys.exit("select one cdylib, binary, or --example target")
t=targets[0]
print(m["workspace_root"]); print(m["target_directory"]); print(t["name"])
print("example" if sys.argv[2] else "lib" if "cdylib" in t["crate_types"] else "bin")
' "$package" "$example" <<< "$meta")
root=$(sed -n '1p' <<< "$selection")
target=$(sed -n '2p' <<< "$selection")
name=$(sed -n '3p' <<< "$selection")
kind=$(sed -n '4p' <<< "$selection")
args=(--lib)
artifact="$target/wasm32-unknown-unknown/release/${name//-/_}.wasm"
if [[ $kind == example ]]; then args=(--example "$name"); artifact="$target/wasm32-unknown-unknown/release/examples/$name.wasm"; fi
if [[ $kind == bin ]]; then args=(--bin "$name"); artifact="$target/wasm32-unknown-unknown/release/$name.wasm"; fi
CARGO_PROFILE_RELEASE_OPT_LEVEL=s CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=debuginfo \
    cargo build -p "$package" --target wasm32-unknown-unknown --release "${args[@]}"
version=$(python3 -c 'import sys,tomllib; print(next(p["version"] for p in tomllib.load(open(sys.argv[1],"rb"))["package"] if p["name"]=="wasm-bindgen"))' "$root/Cargo.lock")
if ! command -v wasm-bindgen >/dev/null || [[ $(wasm-bindgen --version) != "wasm-bindgen $version" ]]; then
    echo "Install the matching CLI: cargo install wasm-bindgen-cli --version $version --locked" >&2
    exit 2
fi
mkdir -p "$out"
wasm-bindgen --target web --out-dir "$out" --out-name app "$artifact"
if $opt; then
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-reference-types \
        "$out/app_bg.wasm" -o "$out/app_bg.opt.wasm"
    mv "$out/app_bg.opt.wasm" "$out/app_bg.wasm"
fi
cat > "$out/index.html" <<'HTML'
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no,viewport-fit=cover">
<meta name="color-scheme" content="light dark">
<title>Rune</title>
<style>html,body{margin:0;width:100%;height:100%;overflow:hidden;overscroll-behavior:none;touch-action:none}#status{font:16px system-ui;padding:24px}</style>
<div id="status">Loading…</div>
<script type="module">
import init from './app.js';
try {
    if (!navigator.gpu) throw new Error('WebGPU requires a supported browser and HTTPS.');
    await init();
    document.querySelector('#status').remove();
} catch (error) {
    document.querySelector('#status').textContent = String(error);
    console.error(error);
}
</script>
</html>
HTML
python3 -c 'import gzip,pathlib,sys; p=pathlib.Path(sys.argv[1]); b=p.read_bytes(); print(f"{p}: {len(b):,} bytes raw, {len(gzip.compress(b,mtime=0)):,} bytes gzip")' "$out/app_bg.wasm"
echo "Static folder: $out"
