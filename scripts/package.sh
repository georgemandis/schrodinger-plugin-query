#!/bin/sh
# Build the plugin and stage it for install: dist/query/{plugin.json,lib/}.
# CARGO_FLAGS passes extra arguments to cargo (a --config patch, say).
set -eu
cd "$(dirname "$0")/.."

# shellcheck disable=SC2086
cargo build --release ${CARGO_FLAGS:-}

OUT=dist/query
rm -rf "$OUT"
mkdir -p "$OUT/lib"
cp plugin.json "$OUT/"
case "$(uname -s)" in
  Darwin)
    cp target/release/libschrodinger_plugin_query.dylib "$OUT/lib/"
    strip -x "$OUT/lib/libschrodinger_plugin_query.dylib"
    codesign --force --sign "${CODESIGN_IDENTITY:--}" "$OUT/lib/libschrodinger_plugin_query.dylib"
    ;;
  *)
    cp target/release/libschrodinger_plugin_query.so "$OUT/lib/"
    ;;
esac
echo "Staged in $OUT"
