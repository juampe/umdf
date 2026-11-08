#!/usr/bin/env bash
# Build an AppImage from the release binaries.
#
# Usage: packaging/build-appimage.sh <bin-dir> <ort-lib-dir|-> <output-dir> <version>
set -euo pipefail

BIN_DIR=${1:?binary directory}
ORT_LIB_DIR=${2:--}
OUT_DIR=${3:?output directory}
VERSION=${4:?version without the leading v}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
APPDIR="$WORK/AppDir"

packaging/stage-root.sh "$BIN_DIR" "$ORT_LIB_DIR" "$APPDIR" "$VERSION"

LINUXDEPLOY=${LINUXDEPLOY:-"$WORK/linuxdeploy-x86_64.AppImage"}
if [ ! -x "$LINUXDEPLOY" ]; then
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 \
        "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage" \
        -o "$LINUXDEPLOY"
    chmod +x "$LINUXDEPLOY"
fi

CLEAN_PATH=$(printf '%s' "$PATH" | awk -v RS=: -v ORS=: '$0 !~ /^\/mnt\//')
CLEAN_PATH=${CLEAN_PATH%:}

PATH="$CLEAN_PATH" "$LINUXDEPLOY" --appimage-extract-and-run \
    --appdir "$APPDIR" \
    --executable "$APPDIR/usr/lib/umdf/umdf-ui" \
    --desktop-file "$APPDIR/usr/share/applications/umdf.desktop" \
    --icon-file "$APPDIR/usr/share/icons/hicolor/256x256/apps/umdf.png"

APPIMAGETOOL=${APPIMAGETOOL:-"$WORK/appimagetool-x86_64.AppImage"}
if [ ! -x "$APPIMAGETOOL" ]; then
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage" \
        -o "$APPIMAGETOOL"
    chmod +x "$APPIMAGETOOL"
fi

mkdir -p "$OUT_DIR"
chmod -R a+rX "$APPDIR"

PATH="$CLEAN_PATH" ARCH=x86_64 "$APPIMAGETOOL" --appimage-extract-and-run --no-appstream \
    "$APPDIR" "$OUT_DIR/umdf-${VERSION}-x86_64.AppImage"
echo "Created $OUT_DIR/umdf-${VERSION}-x86_64.AppImage"
