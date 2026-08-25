#!/usr/bin/env bash
# Build a fully synthetic demo fixture for the diskpulse GIF recording.
#
# Everything is created inside ONE throwaway directory (default:
# /tmp/diskpulse-demo). The script rm -rf's and recreates that exact
# directory on every run — never touches anything outside it. All file
# contents are random bytes from /dev/urandom; no real user data involved.
#
# Usage: bash demo/build_fixture.sh [target-dir]   (default /tmp/diskpulse-demo)
set -euo pipefail

BASE="${1:-/tmp/diskpulse-demo}"
PROJ="$BASE/demo-project"
CACHE="$BASE/demo-cache"

# Idempotent: wipe and recreate the fixture root.
rm -rf "$BASE"
mkdir -p "$PROJ/src" "$PROJ/assets" "$PROJ/logs" "$PROJ/.git/objects/pack" \
         "$PROJ/node_modules/pkg-a/dist" "$PROJ/node_modules/pkg-b/lib" \
         "$PROJ/node_modules/pkg-c/esm" \
         "$PROJ/target/debug/deps" "$PROJ/target/debug/incremental" \
         "$CACHE/_cacache/sha512"

gen() { # gen <path> <bytes>
  head -c "$2" /dev/urandom > "$1"
}

# --- src/: varied, round-ish sizes so viz bars look meaningful -------------
gen "$PROJ/src/main.rs"          51200      # 50 KiB
gen "$PROJ/src/lib.rs"           25600      # 25 KiB
gen "$PROJ/src/router.js"        131072     # 128 KiB
gen "$PROJ/src/state.js"         65536      # 64 KiB
gen "$PROJ/src/big_module.rs"    2097152    # 2 MiB
gen "$PROJ/src/vendor_chunk.js"  15728640   # 15 MiB

# --- assets/ ----------------------------------------------------------------
gen "$PROJ/assets/logo.svg"      12288      # 12 KiB
gen "$PROJ/assets/sprites.png"   2097152    # 2 MiB
gen "$PROJ/assets/hero.png"      4194304    # 4 MiB
gen "$PROJ/assets/banner.jpg"    8388608    # 8 MiB

# --- logs/: targets for the --exclude "*.log" demo step ---------------------
gen "$PROJ/logs/app.log"         1048576    # 1 MiB
gen "$PROJ/logs/error.log"       524288     # 512 KiB
gen "$PROJ/logs/debug.log"       262144     # 256 KiB

# --- .git/: small; hidden-dir handling ---------------------------------------
gen "$PROJ/.git/index"                 16384   # 16 KiB
gen "$PROJ/.git/objects/pack/pack.idx" 65536   # 64 KiB
printf 'ref: refs/heads/main\n' > "$PROJ/.git/HEAD"

# --- node_modules/: clearly the largest thing (default-ignored) --------------
gen "$PROJ/node_modules/pkg-a/dist/bundle.js" 134217728  # 128 MiB
gen "$PROJ/node_modules/pkg-b/lib/index.js"    67108864  # 64 MiB
gen "$PROJ/node_modules/pkg-c/esm/chunk.mjs"   33554432  # 32 MiB
printf '{"name":"pkg-a","version":"1.0.0"}' > "$PROJ/node_modules/pkg-a/package.json"
printf '{"name":"pkg-b","version":"2.3.1"}' > "$PROJ/node_modules/pkg-b/package.json"

# --- target/: large Rust build artifacts (default-ignored) -------------------
gen "$PROJ/target/debug/deps/libcore.rlib"      104857600  # 100 MiB
gen "$PROJ/target/debug/incremental/cache.bin"   52428800  # 50 MiB
gen "$PROJ/target/debug/build_output.o"          26214400  # 25 MiB

# --- demo-cache/: stale-cache clutter for the clean DRY-RUN step -------------
for i in $(seq 0 29); do
  dir="$CACHE/_cacache/sha512/$(printf 'ab%02x' $i)"
  mkdir -p "$dir"
  gen "$dir/content-v1-sha512-$(printf '%02x' $i)deadbeef" $((4096 + i * 2048))
done
gen "$CACHE/metadata.json" 8192

echo "fixture built at $BASE"
