#!/usr/bin/env bash
# Build a Debian package from the release binaries.
#
# Usage: packaging/build-deb.sh <bin-dir> <ort-lib-dir|-> <output-dir> <version>
set -euo pipefail

BIN_DIR=${1:?binary directory}
ORT_LIB_DIR=${2:--}
OUT_DIR=${3:?output directory}
VERSION=${4:?version without the leading v}

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT

packaging/stage-root.sh "$BIN_DIR" "$ORT_LIB_DIR" "$ROOT" "$VERSION"
mkdir -p "$ROOT/DEBIAN"
INSTALLED_SIZE=$(du -sk "$ROOT" | cut -f1)

cat > "$ROOT/DEBIAN/control" <<EOF
Package: umdf
Version: $VERSION
Architecture: amd64
Maintainer: juampe <juampe@users.noreply.github.com>
Section: graphics
Priority: optional
Homepage: https://github.com/juampe/umdf
Installed-Size: $INSTALLED_SIZE
Depends: libc6 (>= 2.35), libfontconfig1, libfreetype6, libgl1, libegl1, libx11-6, libxcursor1, libxi6, libxrandr2, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0
Recommends: ffmpeg
Description: Find duplicate and visually similar media
 UMDF compares exact file contents and perceptual/semantic fingerprints to
 find duplicate photos, videos and audio. It includes a CLI (umdf) and a
 Slint desktop application (umdf-ui).
EOF

mkdir -p "$OUT_DIR"
dpkg-deb --build --root-owner-group "$ROOT" "$OUT_DIR/umdf_${VERSION}_amd64.deb"
echo "Created $OUT_DIR/umdf_${VERSION}_amd64.deb"
