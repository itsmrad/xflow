#!/bin/sh
# Build dist/xflow-<version>.pkg: /usr/local/bin/{xflow,xflowd}, signed with stable
# identifiers.
# Usage: scripts/package-macos-pkg.sh [bin-dir]   (default: build apps/cli/target/release)
# OUT=<dir> changes the output directory; SIGN=<identity> the signing identity
# (default xflow-dev, see README; - signs ad-hoc); KEYCHAIN=<path> looks it up in that
# keychain file only. A bin-dir holding universal binaries gives a universal pkg.
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' apps/cli/Cargo.toml)
[ -n "$version" ] || { echo "error: workspace version not found in apps/cli/Cargo.toml" >&2; exit 1; }
bin=${1:-}
if [ -z "$bin" ]; then
    cargo build --manifest-path apps/cli/Cargo.toml --release --locked -p xflow-app --bins
    bin=apps/cli/target/release
fi
out=${OUT:-dist}
work=$out/pkg-work
rm -rf "$work"
mkdir -p "$work/root/usr/local/bin" "$work/root/usr/local/share/doc/xflow" "$work/component"
cp -R packaging/macos/scripts "$work/scripts"
install -m755 "$bin/xflow" "$bin/xflowd" "$work/root/usr/local/bin/"
# Third-party notices for embedded data: SCOWL word lists (xflow-core/src/words.rs)
# and OpenAI's BPE merges (xflow-providers/src/stt.rs).
install -m644 apps/cli/crates/xflow-core/src/SCOWL-LICENSE.txt "$work/root/usr/local/share/doc/xflow/"
install -m644 apps/cli/crates/xflow-providers/src/bpe/LICENSE "$work/root/usr/local/share/doc/xflow/BPE-LICENSE.txt"
# Grants follow the designated requirement: with a certificate it is identifier +
# certificate, stable across builds; ad-hoc it is the build's hash, so every new
# build leaves a stale, switched-on row behind. An untrusted self-signed cert is fine.
sign=${SIGN:-xflow-dev}
# Matches the identity's quoted name or its SHA-1 hash. Only the default falls back.
if [ "$sign" != - ] && ! security find-identity -p codesigning ${KEYCHAIN:+"$KEYCHAIN"} | grep -qiF -e "\"$sign\"" -e " $sign "; then
    [ -z "${SIGN:-}" ] || { echo "error: no \"$sign\" code-signing identity" >&2; exit 1; }
    echo "warning: no \"$sign\" code-signing identity; signing ad-hoc, so macOS forgets permissions on every new build (see README)" >&2
    sign=-
fi
set -- --force --sign "$sign" ${KEYCHAIN:+--keychain "$KEYCHAIN"} --timestamp=none --options runtime
# The hardened runtime (required for notarisation) needs the microphone entitlement.
codesign "$@" --entitlements packaging/macos/xflowd.entitlements --identifier com.xflow.xflowd "$work/root/usr/local/bin/xflowd"
codesign "$@" --identifier com.xflow.xflow "$work/root/usr/local/bin/xflow"
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
