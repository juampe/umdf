#!/usr/bin/env bash
# Build a macOS .app bundle.
#
# Usage: packaging/build-macos-app.sh <bin-dir> <app-dir> <version> [ort-lib-dir]
set -euo pipefail

BIN_DIR=${1:?binary directory}
APP_DIR=${2:?app directory}
VERSION=${3:?version without the leading v}
ORT_LIB_DIR=${4:-}

mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

install -m 0755 "$BIN_DIR/umdf-ui" "$APP_DIR/Contents/MacOS/umdf-ui"
install -m 0644 assets/icon.icns "$APP_DIR/Contents/Resources/icon.icns"
install -m 0644 LICENSE THIRD_PARTY.md THIRD_PARTY_LICENSES.md "$APP_DIR/Contents/Resources/"
sed "s/@VERSION@/$VERSION/g" packaging/Info.plist > "$APP_DIR/Contents/Info.plist"

if [ -n "$ORT_LIB_DIR" ] && [ -d "$ORT_LIB_DIR" ]; then
    mkdir -p "$APP_DIR/Contents/MacOS/lib"
    cp -a "$ORT_LIB_DIR/." "$APP_DIR/Contents/MacOS/lib/"
    ORT_ROOT=$(dirname "$ORT_LIB_DIR")
    [ -f "$ORT_ROOT/LICENSE" ] && cp "$ORT_ROOT/LICENSE" "$APP_DIR/Contents/Resources/ONNXRUNTIME-LICENSE"
    [ -f "$ORT_ROOT/ThirdPartyNotices.txt" ] && cp "$ORT_ROOT/ThirdPartyNotices.txt" "$APP_DIR/Contents/Resources/ONNXRUNTIME-THIRD-PARTY-NOTICES.txt"
fi

if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 || true
fi

echo "Created $APP_DIR"
