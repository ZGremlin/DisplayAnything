#!/usr/bin/env bash
# Builds the DisplayAnything client core for iPhone and iPad and generates
# the Swift bindings the app uses.
#
# Output:
#   target/ios/DAMobile.xcframework           (device + simulator)
#   apps/ios/DisplayAnything/Core/Generated/  (DAMobile.swift + the C header)
#
# Needs macOS with Xcode (for xcrun and the iOS SDK) and the Rust targets:
#   rustup target add aarch64-apple-ios aarch64-apple-ios-sim
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CRATE=da-mobile
LIB=libda_mobile.a
OUT=target/ios
GEN=apps/ios/DisplayAnything/Core/Generated

for t in aarch64-apple-ios aarch64-apple-ios-sim; do
  cargo build --release -p "$CRATE" --target "$t"
done

rm -rf "$OUT" "$GEN"
mkdir -p "$OUT/headers" "$GEN"

# Swift bindings and the C header, generated from the device build.
cargo run -q -p "$CRATE" --bin uniffi-bindgen -- generate \
  --library "target/aarch64-apple-ios/release/$LIB" \
  --language swift \
  --config "crates/$CRATE/uniffi.toml" \
  --out-dir "$OUT/gen"

cp "$OUT/gen/DAMobile.swift" "$GEN/"
cp "$OUT/gen/DAMobileFFI.h" "$OUT/headers/"
cp "$OUT/gen/DAMobileFFI.modulemap" "$OUT/headers/module.modulemap"

xcodebuild -create-xcframework \
  -library "target/aarch64-apple-ios/release/$LIB" -headers "$OUT/headers" \
  -library "target/aarch64-apple-ios-sim/release/$LIB" -headers "$OUT/headers" \
  -output "$OUT/DAMobile.xcframework"

echo "Built $OUT/DAMobile.xcframework and $GEN"
