#!/bin/sh
# Build release binaries and package dist/xflow-<version>-<target>.tar.gz.
# Usage: scripts/package-release.sh <target-triple> [expected-tag]
# The archive mirrors the repository layout so docs/platform-setup.md commands
# work unchanged from the extracted directory.
set -eu
cd "$(dirname "$0")/.."

target=${1:?usage: $0 <target-triple> [expected-tag]}
version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' apps/cli/Cargo.toml)
[ -n "$version" ] || { echo "error: workspace version not found in apps/cli/Cargo.toml" >&2; exit 1; }
if [ -n "${2:-}" ] && [ "$2" != "v$version" ]; then
    echo "error: tag $2 does not match workspace version $version" >&2
    exit 1
fi

cargo build --manifest-path apps/cli/Cargo.toml --release --locked --target "$target" -p xflow-app --bins

name=xflow-$version-$target
stage=dist/$name
rm -rf "$stage" "$stage.tar.gz"
mkdir -p "$stage/packaging" "$stage/config"
install -m755 "apps/cli/target/$target/release/xflow" "apps/cli/target/$target/release/xflowd" "$stage/"
cp -R packaging/gnome-extension "$stage/packaging/"
install -m644 packaging/xflow.service "$stage/packaging/"
install -m644 config/default.toml "$stage/config/"
install -m644 LICENSE README.md "$stage/"
# Third-party notice for the embedded SCOWL word lists (xflow-core/src/words.rs).
install -m644 apps/cli/crates/xflow-core/src/SCOWL-LICENSE.txt "$stage/"

# Deterministic archive: sorted entries, fixed owner, commit timestamp, no gzip name/time.
mtime=$(git log -1 --format=%ct 2>/dev/null || date +%s)
tar -C dist --sort=name --owner=0 --group=0 --numeric-owner --mode=go-w --mtime="@$mtime" -cf - "$name" |
    gzip -9n >"$stage.tar.gz"
rm -rf "$stage"
echo "$stage.tar.gz"
