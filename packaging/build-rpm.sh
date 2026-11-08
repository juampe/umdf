#!/usr/bin/env bash
# Build an RPM package from the release binaries.
#
# Usage: packaging/build-rpm.sh <bin-dir> <ort-lib-dir|-> <output-dir> <version>
set -euo pipefail

BIN_DIR=${1:?binary directory}
ORT_LIB_DIR=${2:--}
OUT_DIR=${3:?output directory}
VERSION=${4:?version without the leading v}

TOP=$(mktemp -d)
trap 'rm -rf "$TOP"' EXIT

packaging/stage-root.sh "$BIN_DIR" "$ORT_LIB_DIR" "$TOP/root" "$VERSION"
mkdir -p "$TOP/SOURCES" "$TOP/SPECS" "$TOP/BUILD" "$TOP/RPMS" "$TOP/SRPMS"
cp -a "$TOP/root" "$TOP/SOURCES/root"
sed "s/@VERSION@/$VERSION/g" packaging/umdf.spec.in > "$TOP/SPECS/umdf.spec"

rpmbuild -bb \
    --define "_topdir $TOP" \
    --define "_build_id_links none" \
    --define "__os_install_post %{nil}" \
    "$TOP/SPECS/umdf.spec"

mkdir -p "$OUT_DIR"
cp "$TOP"/RPMS/*/umdf-*.rpm "$OUT_DIR/"
echo "Created $OUT_DIR/$(basename "$TOP"/RPMS/*/umdf-*.rpm)"
