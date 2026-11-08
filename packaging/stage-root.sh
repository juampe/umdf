#!/usr/bin/env bash
# Populate a filesystem root that can be used by the .deb/.rpm/AppImage
# packagers.
#
# Usage: packaging/stage-root.sh <bin-dir> <ort-lib-dir|-> <root-dir> [version]
set -euo pipefail

BIN_DIR=${1:?binary directory}
ORT_LIB_DIR=${2:--}
ROOT=${3:?staging root}
VERSION=${4:-}

mkdir -p \
    "$ROOT/usr/bin" \
    "$ROOT/usr/lib/umdf/lib" \
    "$ROOT/usr/share/applications" \
    "$ROOT/usr/share/icons/hicolor/256x256/apps" \
    "$ROOT/usr/share/icons/hicolor/512x512/apps" \
    "$ROOT/usr/share/doc/umdf"
chmod 0755 "$ROOT"

install -m 0755 "$BIN_DIR/umdf" "$ROOT/usr/lib/umdf/umdf"
install -m 0755 "$BIN_DIR/umdf-ui" "$ROOT/usr/lib/umdf/umdf-ui"
ln -s ../lib/umdf/umdf "$ROOT/usr/bin/umdf"
ln -s ../lib/umdf/umdf-ui "$ROOT/usr/bin/umdf-ui"

if [ "$ORT_LIB_DIR" != "-" ] && [ -d "$ORT_LIB_DIR" ]; then
    find "$ORT_LIB_DIR" -maxdepth 1 \
        \( -name 'libonnxruntime*.so*' -o -name 'libonnxruntime*.dylib' -o -name 'onnxruntime.dll' \) \
        -exec cp -a {} "$ROOT/usr/lib/umdf/lib/" \;
    ORT_ROOT=$(dirname "$ORT_LIB_DIR")
    [ -f "$ORT_ROOT/LICENSE" ] && cp "$ORT_ROOT/LICENSE" "$ROOT/usr/share/doc/umdf/ONNXRUNTIME-LICENSE"
    [ -f "$ORT_ROOT/ThirdPartyNotices.txt" ] && cp "$ORT_ROOT/ThirdPartyNotices.txt" "$ROOT/usr/share/doc/umdf/ONNXRUNTIME-THIRD-PARTY-NOTICES.txt"
    [ -f "$ORT_ROOT/Privacy.md" ] && cp "$ORT_ROOT/Privacy.md" "$ROOT/usr/share/doc/umdf/ONNXRUNTIME-PRIVACY.md"
fi

install -m 0644 packaging/umdf.desktop "$ROOT/usr/share/applications/umdf.desktop"
install -m 0644 assets/icon-256.png "$ROOT/usr/share/icons/hicolor/256x256/apps/umdf.png"
install -m 0644 assets/icon-512.png "$ROOT/usr/share/icons/hicolor/512x512/apps/umdf.png"
install -m 0644 README.md CHANGELOG.md THIRD_PARTY.md THIRD_PARTY_LICENSES.md LICENSE "$ROOT/usr/share/doc/umdf/"
install -m 0644 assets/fonts/NOTO-COPYRIGHT.txt "$ROOT/usr/share/doc/umdf/NOTO-COPYRIGHT.txt"
install -m 0644 packaging/debian-copyright "$ROOT/usr/share/doc/umdf/copyright"

if [ -n "$VERSION" ]; then
    cat > "$ROOT/usr/share/doc/umdf/SOURCE.txt" <<EOF
UMDF $VERSION
Corresponding source code for this exact version:
https://github.com/juampe/umdf/tree/v$VERSION
EOF
    chmod 0644 "$ROOT/usr/share/doc/umdf/SOURCE.txt"
fi
