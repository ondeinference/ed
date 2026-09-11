#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT_DIR"

DIST_DIR="$ROOT_DIR/dist/swift"
PACKAGE_DIR="$DIST_DIR/Package"
HEADERS_DIR="$DIST_DIR/Headers"
FRAMEWORK_DIR="$DIST_DIR/EdFramework.xcframework"
ZIP_PATH="$DIST_DIR/EdFramework.xcframework.zip"
CHECKSUM_PATH="$DIST_DIR/EdFramework.checksum.txt"
VERSION_PATH="$DIST_DIR/version.txt"
BINDGEN="$ROOT_DIR/target/release/uniffi-bindgen"
MANIFEST="$ROOT_DIR/crates/ed-agent-ffi/Cargo.toml"
APPLE_TARGET_DIR="$ROOT_DIR/target/apple"

IOS_DEPLOYMENT_TARGET="${IOS_DEPLOYMENT_TARGET:-16.0}"
MACOS_DEPLOYMENT_TARGET="${MACOS_DEPLOYMENT_TARGET:-14.0}"
TVOS_DEPLOYMENT_TARGET="${TVOS_DEPLOYMENT_TARGET:-16.0}"
VISIONOS_DEPLOYMENT_TARGET="${VISIONOS_DEPLOYMENT_TARGET:-1.0}"
WATCHOS_DEPLOYMENT_TARGET="${WATCHOS_DEPLOYMENT_TARGET:-9.0}"

rm -rf "$FRAMEWORK_DIR" "$ZIP_PATH" "$CHECKSUM_PATH" "$VERSION_PATH" "$PACKAGE_DIR" "$HEADERS_DIR"
mkdir -p "$PACKAGE_DIR/Sources/Ed" "$HEADERS_DIR"

cargo build --manifest-path uniffi-bindgen/Cargo.toml --release

# Each slice takes 15-20 minutes and cargo never prints the target it is
# building, so a bare `error: could not compile` an hour into the log cannot be
# attributed without counting `Finished` lines. Announce every target.
#
# The tvOS, visionOS and watchOS targets are tier 3: no prebuilt std ships for
# them, hence nightly and -Z build-std.
build_target() {
  local target="$1" deployment="$2"
  shift 2
  echo "::group::cargo rustc --target $target"
  env "$deployment" cargo "$@" rustc \
    --target-dir "$APPLE_TARGET_DIR" \
    --manifest-path "$MANIFEST" \
    --target "$target" \
    --release --lib --crate-type staticlib
  echo "::endgroup::"
}

build_target aarch64-apple-ios          "IPHONEOS_DEPLOYMENT_TARGET=$IOS_DEPLOYMENT_TARGET"
build_target aarch64-apple-ios-sim      "IPHONEOS_DEPLOYMENT_TARGET=$IOS_DEPLOYMENT_TARGET"
build_target aarch64-apple-darwin       "MACOSX_DEPLOYMENT_TARGET=$MACOS_DEPLOYMENT_TARGET"
build_target aarch64-apple-tvos         "TVOS_DEPLOYMENT_TARGET=$TVOS_DEPLOYMENT_TARGET" +nightly -Z build-std
build_target aarch64-apple-tvos-sim     "TVOS_DEPLOYMENT_TARGET=$TVOS_DEPLOYMENT_TARGET" +nightly -Z build-std
build_target aarch64-apple-visionos     "XROS_DEPLOYMENT_TARGET=$VISIONOS_DEPLOYMENT_TARGET" +nightly -Z build-std
build_target aarch64-apple-visionos-sim "XROS_DEPLOYMENT_TARGET=$VISIONOS_DEPLOYMENT_TARGET" +nightly -Z build-std
build_target aarch64-apple-watchos      "WATCHOS_DEPLOYMENT_TARGET=$WATCHOS_DEPLOYMENT_TARGET" +nightly -Z build-std
build_target aarch64-apple-watchos-sim  "WATCHOS_DEPLOYMENT_TARGET=$WATCHOS_DEPLOYMENT_TARGET" +nightly -Z build-std

"$BINDGEN" generate "$APPLE_TARGET_DIR/aarch64-apple-ios/release/libed_agent_ffi.a" --crate ed_agent_ffi --language swift --out-dir "$PACKAGE_DIR/Sources/Ed"
cp "$PACKAGE_DIR/Sources/Ed/ed_agent_ffiFFI.h" "$HEADERS_DIR/ed_agent_ffiFFI.h"
cp "$PACKAGE_DIR/Sources/Ed/ed_agent_ffiFFI.modulemap" "$HEADERS_DIR/module.modulemap"

xcodebuild -create-xcframework \
  -library "$APPLE_TARGET_DIR/aarch64-apple-ios/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -library "$APPLE_TARGET_DIR/aarch64-apple-ios-sim/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -library "$APPLE_TARGET_DIR/aarch64-apple-tvos/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -library "$APPLE_TARGET_DIR/aarch64-apple-tvos-sim/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -library "$APPLE_TARGET_DIR/aarch64-apple-visionos/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -library "$APPLE_TARGET_DIR/aarch64-apple-visionos-sim/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -library "$APPLE_TARGET_DIR/aarch64-apple-watchos/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -library "$APPLE_TARGET_DIR/aarch64-apple-watchos-sim/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -library "$APPLE_TARGET_DIR/aarch64-apple-darwin/release/libed_agent_ffi.a" -headers "$HEADERS_DIR" \
  -output "$FRAMEWORK_DIR"

export FRAMEWORK_DIR ZIP_PATH
python3 - <<'PY'
import os, stat, zipfile
from pathlib import Path

root = Path(os.environ["FRAMEWORK_DIR"])
output = Path(os.environ["ZIP_PATH"])
with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
    for path in sorted(item for item in root.rglob("*") if item.is_file()):
        info = zipfile.ZipInfo(str(path.relative_to(root.parent)), (1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = (stat.S_IFREG | 0o644) << 16
        archive.writestr(info, path.read_bytes())
PY

swift package compute-checksum "$ZIP_PATH" > "$CHECKSUM_PATH"
cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "ed-agent"))' \
  > "$VERSION_PATH"

printf 'XCFramework: %s\nChecksum: %s\nVersion: %s\n' \
  "$FRAMEWORK_DIR" "$(cat "$CHECKSUM_PATH")" "$(cat "$VERSION_PATH")"
