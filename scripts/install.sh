#!/bin/sh
# Install xflow and xflowd into a user prefix, without sudo. Service and desktop
# activation stay separate explicit steps.
#
# With no local build next to this script (e.g. `curl -fsSL URL | sh`), it downloads
# this machine's Linux release tarball and SHA256SUMS, and installs only after the
# checksum matches. From a checkout with a release build, or an extracted release
# tarball, it installs those binaries; --bin-dir PATH picks another directory.
# XFLOW_API_URL and XFLOW_DOWNLOAD_URL override the GitHub endpoints (mirrors, tests).
set -eu
usage() { echo 'Usage: install.sh [--prefix PATH] [--version TAG | --bin-dir PATH]'; }
die() { echo "error: $*" >&2; exit 1; }
prefix=${HOME:?HOME is required}/.local
bindir=
version=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; prefix=$2; shift 2 ;;
        --bin-dir) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; bindir=$2; shift 2 ;;
        --version) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; version=$2; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
[ -z "$bindir" ] || [ -z "$version" ] || { usage >&2; exit 2; }

tmp=
staged=
cleanup() {
    [ -z "$tmp" ] || rm -rf "$tmp"
    [ -z "$staged" ] || rm -f "$prefix/bin/.xflow-install-$$" "$prefix/bin/.xflowd-install-$$"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

# A local build beside this script wins, unless a release was asked for.
if [ -z "$bindir" ] && [ -z "$version" ]; then
    case $0 in
        *install.sh)
            root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
            for dir in "$root" "$root/apps/cli/target/release"; do
                if [ -x "$dir/xflow" ] && [ -x "$dir/xflowd" ]; then bindir=$dir; break; fi
            done ;;
    esac
fi

if [ -z "$bindir" ]; then
    [ "$(uname -s)" = Linux ] || die "prebuilt releases are Linux-only; build from source (see README)"
    arch=$(uname -m)
    case $arch in
        x86_64|amd64) target=x86_64-unknown-linux-gnu ;;
        aarch64|arm64) target=aarch64-unknown-linux-gnu ;;
        *) die "no prebuilt release for $arch (only x86_64 and aarch64); build from source (see README)" ;;
    esac
    if command -v curl >/dev/null 2>&1; then
        fetch() { curl -fsSL --proto-redir =https -o "$2" "$1"; }
    elif command -v wget >/dev/null 2>&1; then
        fetch() { wget -q -O "$2" "$1"; }
    else
        die "curl or wget is required to download a release"
    fi
    command -v sha256sum >/dev/null 2>&1 || die "sha256sum is required to verify the download"
    tmp=$(mktemp -d)
    if [ -z "$version" ]; then
        # shortcut: newest release including prereleases (all are betas today); switch to /releases/latest once stable releases exist.
        api="${XFLOW_API_URL:-https://api.github.com/repos/itsmrad/xflow}/releases?per_page=1"
        fetch "$api" "$tmp/releases.json" || die "cannot look up the latest release at $api; pass --version TAG"
        version=$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$tmp/releases.json" | head -n 1)
        [ -n "$version" ] || die "no release found at $api; pass --version TAG"
    fi
    case $version in ''|*[!A-Za-z0-9._-]*) die "invalid release tag: $version" ;; esac
    base=${XFLOW_DOWNLOAD_URL:-https://github.com/itsmrad/xflow/releases/download}/$version
    echo "Downloading xflow $version for $target" >&2
    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "cannot download $base/SHA256SUMS; refusing to install unverified files"
    # The sums file names the tarball, so "latest" needs no version parsing.
    pattern="^[0-9a-f]{64} [ *]xflow-[A-Za-z0-9._-]+-$target\.tar\.gz\$"
    [ "$(grep -cE "$pattern" "$tmp/SHA256SUMS" || true)" = 1 ] || die "SHA256SUMS of $version has no single entry for $target; refusing to install"
    entry=$(grep -E "$pattern" "$tmp/SHA256SUMS")
    expected=${entry%% *}
    file=${entry##*[ *]}
    fetch "$base/$file" "$tmp/$file" || die "cannot download $base/$file"
    actual=$(sha256sum "$tmp/$file" | cut -d ' ' -f 1)
    [ "$actual" = "$expected" ] || die "checksum mismatch for $file (expected $expected, got $actual); nothing was installed"
    mkdir "$tmp/x"
    tar -xzf "$tmp/$file" -C "$tmp/x"
    bindir=$tmp/x/${file%.tar.gz}
fi

for binary in xflow xflowd; do
    [ -f "$bindir/$binary" ] && [ -x "$bindir/$binary" ] && continue
    {
        echo "error: missing executable $bindir/$binary" >&2
        [ -n "$tmp" ] || echo 'hint: build with cargo build --manifest-path apps/cli/Cargo.toml --release --locked, pass --bin-dir PATH, or run with --version TAG to download a release' >&2
        exit 1
    }
done
mkdir -p "$prefix/bin"
# Stage both, then rename: upgrades never truncate a running binary or leave a half copy.
staged=1
for binary in xflow xflowd; do
    install -m755 "$bindir/$binary" "$prefix/bin/.$binary-install-$$"
done
for binary in xflow xflowd; do
    mv -f "$prefix/bin/.$binary-install-$$" "$prefix/bin/$binary"
done
staged=
printf 'Installed xflow and xflowd into %s/bin\nNext: %s/bin/xflow setup\n' "$prefix" "$prefix"
case :$PATH: in *:"$prefix/bin":*) ;; *) printf 'Add %s/bin to PATH if needed.\n' "$prefix" ;; esac
[ "$prefix" = "$HOME/.local" ] || echo "note: the systemd user unit (xflow service install) runs ~/.local/bin/xflowd"
