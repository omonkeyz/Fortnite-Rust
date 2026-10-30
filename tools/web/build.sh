#!/bin/bash
# Builds the BROWSER version of the Fortnite 1.8 Rust port (wasm32 + WebGPU + threads) and its local launcher.
#
#   tools/web/build.sh            -> dist/web/ (index.html, pkg/fortnite_web*.{js,wasm}, worker.js) + dist/fortnite-web.exe
#   tools/web/build.sh --smoke    -> the platform-layer smoke test instead of the game (dist/web-smoke/)
#   tools/web/build.sh --debug    -> unoptimised wasm (faster to build, much slower to run)
#
# The native exe (fortnite-rs.exe) is NOT touched: it keeps building with the normal stable toolchain (`cargo g` etc.).
# Requirements (installed on first run if missing): rustup toolchain nightly-2026-10-06 + rust-src, wasm-bindgen-cli 0.2.129 (cargo install).
#
# How it works (docs/spec/browser_port.md):
#  * std is rebuilt from a copy of the pinned nightly's rust-src with tools/web/std-patch/fnweb-std.patch applied (-Z build-std, located through
#    cargo's __CARGO_TESTS_ONLY_SRC_ROOT); the patch routes std's fs / time / threads / env / stdio for wasm32-unknown-unknown+atomics to the
#    __fnweb_* hooks of tools/web/fn-web-rt. The rustup install itself is never modified, and no rust-toolchain.toml is added to the workspace.
#  * the game crate is tools/web/fn-web (its own workspace; its library root is crates/fn-app/src/main.rs, so the module tree is shared with the exe).
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TOOLCHAIN=nightly-2026-10-06
WB_VERSION=0.2.129
TARGET_DIR="${FNWEB_TARGET_DIR:-D:/target-fn-web}"
NATIVE_TARGET_DIR="${FNWEB_NATIVE_TARGET_DIR:-D:/target-fn-web-native}"
MODE=game
PROFILE=release
for a in "$@"; do
  case "$a" in
    --smoke) MODE=smoke ;;
    --debug) PROFILE=dev ;;
    --no-launcher) NO_LAUNCHER=1 ;;
    --check) CHECK=1 ;;
    *) echo "unknown argument $a"; exit 2 ;;
  esac
done

say() { echo "[build-web] $*"; }

# 1. toolchain + wasm-bindgen
if ! rustup run "$TOOLCHAIN" rustc --version >/dev/null 2>&1; then
  say "installing $TOOLCHAIN (+ rust-src)"; rustup toolchain install "$TOOLCHAIN" --profile minimal --component rust-src
fi
SYSROOT="$(rustup run "$TOOLCHAIN" rustc --print sysroot)"
if [ ! -d "$SYSROOT/lib/rustlib/src/rust/library/std" ]; then rustup component add rust-src --toolchain "$TOOLCHAIN"; fi
if ! wasm-bindgen --version 2>/dev/null | grep -q "$WB_VERSION"; then
  say "installing wasm-bindgen-cli $WB_VERSION"; CARGO_TARGET_DIR="$TARGET_DIR/tools" cargo install wasm-bindgen-cli --version "$WB_VERSION" --locked
fi

# 2. patched std sources (re-made when the patch or the toolchain changes)
PATCH="$REPO/tools/web/std-patch/fnweb-std.patch"
STAMP="$TOOLCHAIN $(sha1sum "$PATCH" | cut -c1-16)"
STD_SRC="$TARGET_DIR/std-src/$TOOLCHAIN"
if [ "$(cat "$STD_SRC/.fnweb-stamp" 2>/dev/null || true)" != "$STAMP" ]; then
  say "preparing patched std sources in $STD_SRC"
  rm -rf "$STD_SRC"; mkdir -p "$STD_SRC"
  cp -r "$SYSROOT/lib/rustlib/src/rust/library" "$STD_SRC/"
  (cd "$STD_SRC/library" && git -c core.autocrlf=false apply --whitespace=nowarn "$PATCH")
  echo "$STAMP" > "$STD_SRC/.fnweb-stamp"
fi

# 3. the wasm build
if [ "$MODE" = smoke ]; then CRATE_DIR="$REPO/tools/web/smoke"; LIB=fn_web_smoke; OUT="$REPO/dist/web-smoke"; else CRATE_DIR="$REPO/tools/web/fn-web"; LIB=fortnite_web; OUT="${FNWEB_OUT_DIR:-$REPO/dist/web}"; fi

# 3a. wgpu with tools/web/std-patch/fnweb-wgpu.patch (its WebGPU backend hands WGSL to the browser verbatim; the patch turns off Tint's
#     derivative-uniformity errors, which naga never checks), selected with --config so no manifest carries a machine path
WGPU_VERSION=30.0.1
(cd "$CRATE_DIR" && cargo "+$TOOLCHAIN" fetch --target wasm32-unknown-unknown >/dev/null 2>&1 || true)
WGPU_REG="$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/wgpu-$WGPU_VERSION 2>/dev/null | head -1)"
[ -n "$WGPU_REG" ] || { echo "wgpu $WGPU_VERSION sources not found in the cargo registry"; exit 1; }
WPATCH="$REPO/tools/web/std-patch/fnweb-wgpu.patch"
WGPU_DIR="$TARGET_DIR/patched/wgpu-$WGPU_VERSION"
WSTAMP="$(sha1sum "$WPATCH" | cut -c1-16)"
if [ "$(cat "$WGPU_DIR/.fnweb-stamp" 2>/dev/null || true)" != "$WSTAMP" ]; then
  say "preparing patched wgpu in $WGPU_DIR"
  rm -rf "$WGPU_DIR"; mkdir -p "$(dirname "$WGPU_DIR")"
  cp -r "$WGPU_REG" "$WGPU_DIR"
  (cd "$WGPU_DIR" && git -c core.autocrlf=false apply --whitespace=nowarn "$WPATCH")
  echo "$WSTAMP" > "$WGPU_DIR/.fnweb-stamp"
fi
WGPU_PATCH_CFG="patch.crates-io.wgpu.path=\"$WGPU_DIR\""
# Shared memory with a 4 GiB maximum (the whole wasm32 address space), TLS exports for wasm-bindgen's thread support, an 8 MiB main stack
# (Windows threads get 1-2 MiB natively, but wasm has no guard page: an overflow would silently corrupt the heap).
export RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128 -C link-arg=--shared-memory -C link-arg=--max-memory=4294967296 -C link-arg=--import-memory -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base -C link-arg=-zstack-size=8388608 ${FNWEB_EXTRA_RUSTFLAGS:-}"
export __CARGO_TESTS_ONLY_SRC_ROOT="$STD_SRC/library"
if [ -n "${CHECK:-}" ]; then export CARGO_TARGET_DIR="$TARGET_DIR/wasm-check"; (cd "$CRATE_DIR" && cargo "+$TOOLCHAIN" check --lib --target wasm32-unknown-unknown -Z build-std=std,panic_abort --config "$WGPU_PATCH_CFG"); say "check ok"; exit 0; fi
export CARGO_TARGET_DIR="$TARGET_DIR/wasm"
say "cargo +$TOOLCHAIN build ($MODE, profile $PROFILE)"
(cd "$CRATE_DIR" && cargo "+$TOOLCHAIN" build --lib --target wasm32-unknown-unknown --profile "$PROFILE" -Z build-std=std,panic_abort --config "$WGPU_PATCH_CFG")
PDIR=$([ "$PROFILE" = dev ] && echo debug || echo "$PROFILE")
WASM="$CARGO_TARGET_DIR/wasm32-unknown-unknown/$PDIR/$LIB.wasm"
say "wasm-bindgen $(du -h "$WASM" | cut -f1) -> $OUT/pkg"
rm -rf "$OUT/pkg"; mkdir -p "$OUT/pkg"
wasm-bindgen --target web --out-dir "$OUT/pkg" --out-name "$LIB" "$WASM"
# wasm-bindgen 0.2.129's glue writes an import's String / Vec return through the caller's return pointer as `argN + 4 * k`, a SIGNED i32: on a
# thread whose stack lies above 2 GiB of the shared memory (a pool worker started once the heap is that big) the offset is negative and the DataView
# throws (seen: JsValue::as_string -> __wbindgen_string_get, the heap tracker's fnweb_stack). Read the pointer as unsigned, as the rest of the glue does.
sed -i -E 's/getDataViewMemory0\(\)\.set(Int32|Uint32|Float64|Float32|BigInt64)\(arg([0-9]+) \+ /getDataViewMemory0().set\1((arg\2 >>> 0) + /g' "$OUT/pkg/$LIB.js"
if grep -qE 'getDataViewMemory0\(\)\.set[A-Za-z0-9]+\(arg[0-9]+ \+' "$OUT/pkg/$LIB.js"; then echo "glue still has signed return pointers"; exit 1; fi

# 4. the page
cp "$REPO/tools/web/www/"*.js "$OUT/"
# remote mode's start-up prefetch list (file paths + byte ranges of a recorded load, no file contents; see www/fnweb-prefetch.js)
[ -f "$REPO/tools/web/www/fnweb-psowarm.json" ] && [ "$MODE" != smoke ] && cp "$REPO/tools/web/www/fnweb-psowarm.json" "$OUT/"
[ -f "$REPO/tools/web/www/fnweb-boot-manifest.json" ] && [ "$MODE" != smoke ] && cp "$REPO/tools/web/www/fnweb-boot-manifest.json" "$OUT/"
if [ "$MODE" = smoke ]; then cp "$REPO/tools/web/www/smoke.html" "$OUT/index.html"; else cp "$REPO/tools/web/www/index.html" "$OUT/index.html"; fi
sed -i "s/__FNWEB_LIB__/$LIB/g" "$OUT/"*.js "$OUT/index.html"
echo "built $(date '+%Y-%m-%d %H:%M:%S') mode=$MODE profile=$PROFILE toolchain=$TOOLCHAIN" > "$OUT/build-info.txt"

# 5. the launcher (native, std only)
if [ -z "${NO_LAUNCHER:-}" ]; then
  say "building fortnite-web.exe"
  (unset RUSTFLAGS __CARGO_TESTS_ONLY_SRC_ROOT; cd "$REPO" && CARGO_TARGET_DIR="$NATIVE_TARGET_DIR" cargo build --release -p fn-web-serve --bin fortnite-web)
  cp "$NATIVE_TARGET_DIR/release/fortnite-web.exe" "$REPO/dist/fortnite-web.exe"
fi
say "done: $OUT  (run dist/fortnite-web.exe$([ "$MODE" = smoke ] && echo " --www dist/web-smoke"))"
