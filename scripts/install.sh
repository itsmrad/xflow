#!/bin/sh
# Install prebuilt binaries; desktop/service activation is a separate explicit step.
set -eu
usage() { echo 'Usage: scripts/install.sh [--prefix PATH] [--bin-dir PATH]'; }
prefix=${HOME:?HOME is required}/.local
bindir=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; prefix=$2; shift 2 ;;
        --bin-dir) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; bindir=$2; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_dir=${bindir:-$root/target/release}
for binary in xflow xflowd; do
    [ -f "$source_dir/$binary" ] && [ -x "$source_dir/$binary" ] || {
        echo "error: missing executable $source_dir/$binary" >&2
        echo 'hint: build with cargo build --release --locked, or pass --bin-dir PATH' >&2
        exit 1
    }
done
mkdir -p "$prefix/bin"
for binary in xflow xflowd; do
    # Rename so upgrades do not truncate a running binary.
    temp="$prefix/bin/.$binary-install-$$"
    trap 'rm -f "$temp"' EXIT HUP INT TERM
    install -m755 "$source_dir/$binary" "$temp"
    mv -f "$temp" "$prefix/bin/$binary"
    trap - EXIT HUP INT TERM
done
printf 'Installed xflow and xflowd into %s/bin\nNext: %s/bin/xflow setup\n' "$prefix" "$prefix"
printf 'Add %s/bin to PATH if needed.\n' "$prefix"
