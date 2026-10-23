#!/bin/sh
# Build dist/xflow-<version>.pkg: /usr/local/bin/{xflow,xflowd}, signed with stable
# identifiers, plus the guided-setup launcher the postinstall opens.
# Usage: scripts/package-macos-pkg.sh [bin-dir]   (default: build target/release)
# OUT=<dir> changes the output directory; SIGN=<identity> the signing identity
# (default xflow-dev, see README; - signs ad-hoc).
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
[ -n "$version" ] || { echo "error: workspace version not found in Cargo.toml" >&2; exit 1; }
bin=${1:-}
if [ -z "$bin" ]; then
    cargo build --release --locked -p xflow-app --bins
    bin=target/release
fi
out=${OUT:-dist}
work=$out/pkg-work
rm -rf "$work"
mkdir -p "$work/root/usr/local/bin" "$work/root/usr/local/share/xflow" "$work/component"
cp -R packaging/macos/scripts "$work/scripts"
install -m755 "$bin/xflow" "$bin/xflowd" "$work/root/usr/local/bin/"
install -m755 "packaging/macos/Set up xflow.command" "$work/root/usr/local/share/xflow/"
# Grants follow the designated requirement: with a certificate it is identifier +
# certificate, stable across builds; ad-hoc it is the build's hash, so every new
# build leaves a stale, switched-on row behind. An untrusted self-signed cert is fine.
sign=${SIGN:-xflow-dev}
if [ "$sign" != - ] && ! security find-identity -p codesigning | grep -qF "\"$sign\""; then
    echo "warning: no \"$sign\" code-signing identity; signing ad-hoc, so macOS forgets permissions on every new build (see README)" >&2
    sign=-
fi
codesign --force --sign "$sign" --identifier com.xflow.xflowd "$work/root/usr/local/bin/xflowd"
codesign --force --sign "$sign" --identifier com.xflow.xflow "$work/root/usr/local/bin/xflow"
archs=$(lipo -archs "$bin/xflowd" | tr ' ' ',')
sed -e "s/@VERSION@/$version/g" -e "s/@ARCHS@/$archs/" packaging/macos/distribution.xml >"$work/distribution.xml"

# No AppleDouble (._*) files: pkgbuild archives extended attributes (e.g. provenance).
export COPYFILE_DISABLE=1
xattr -rc "$work/root" "$work/scripts"
pkgbuild --quiet --root "$work/root" --scripts "$work/scripts" \
    --identifier com.xflow.pkg --version "$version" --install-location / \
    "$work/component/xflow-core.pkg"
productbuild --quiet --distribution "$work/distribution.xml" \
    --resources packaging/macos/resources --package-path "$work/component" \
    "$out/xflow-$version.pkg"
rm -rf "$work"
echo "$out/xflow-$version.pkg"
