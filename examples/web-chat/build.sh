#!/usr/bin/env bash
# Build the wasm package into www/pkg. Serve www/ with any static server:
#   python3 -m http.server -d www 8080
# Extra arguments go to `cargo`, e.g. a local onde while it is unreleased:
#   ./build.sh --config 'patch.crates-io.onde.path="../../../onde"'
set -euo pipefail
cd "$(dirname "$0")"

# simd128 is what makes CPU inference usable in a browser. Every current
# browser supports it, and main.js checks for it before loading anything.
export RUSTFLAGS="${RUSTFLAGS:--C target-feature=+simd128}"

wasm-pack build --target web --release --out-dir www/pkg --no-typescript -- "$@"
