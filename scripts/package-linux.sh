#!/bin/sh
# Build a .deb and an .rpm next to each Linux release tarball, from its binaries and unit.
# Usage: scripts/package-linux.sh dist/xflow-<version>-<target>.tar.gz...
# Runs the pinned nfpm release after checking its SHA-256; NFPM=PATH uses a local copy.
set -eu
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
die() { echo "error: $*" >&2; exit 1; }
[ "$#" -gt 0 ] || { echo "usage: $0 TARBALL..." >&2; exit 2; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

nfpm=${NFPM:-}
if [ -z "$nfpm" ]; then
    version=2.48.0
    case $(uname -m) in
        x86_64) file=nfpm_${version}_Linux_x86_64.tar.gz sum=6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750 ;;
        aarch64) file=nfpm_${version}_Linux_arm64.tar.gz sum=a9a75213763783b7669719e505474c748c5ad7dc66591a195619deb50f85c2f4 ;;
        *) die "no pinned nfpm for $(uname -m); pass NFPM=PATH" ;;
    esac
    curl -fsSL --proto =https -o "$work/$file" "https://github.com/goreleaser/nfpm/releases/download/v$version/$file"
    echo "$sum  $work/$file" | sha256sum -c - >/dev/null || die "nfpm checksum mismatch"
    tar -xzf "$work/$file" -C "$work" nfpm
    nfpm=$work/nfpm
fi

for tarball in "$@"; do
    name=$(basename "$tarball" .tar.gz)
    case $name in
        xflow-*-x86_64-unknown-linux-gnu) arch=amd64 rpmarch=x86_64 ;;
        xflow-*-aarch64-unknown-linux-gnu) arch=arm64 rpmarch=aarch64 ;;
        *) die "not a Linux release tarball: $tarball" ;;
    esac
    version=${name#xflow-}
    version=${version%-*-unknown-linux-gnu}
    out=$(CDPATH='' cd -- "$(dirname -- "$tarball")" && pwd)
    rm -rf "$work/x" "$work/root"
    mkdir -p "$work/x" "$work/root/doc"
    tar -xzf "$tarball" -C "$work/x"
    src=$work/x/$name
    install -m755 "$src/xflow" "$src/xflowd" "$repo/packaging/linux/preremove.sh" "$work/root/"
    # The tarball's unit runs ~/.local/bin/xflowd (install.sh); packages install to /usr/bin.
    sed 's|^ExecStart=%h/\.local/bin/xflowd$|ExecStart=/usr/bin/xflowd|' "$src/packaging/xflow.service" >"$work/root/xflow.service"
    grep -qx 'ExecStart=/usr/bin/xflowd' "$work/root/xflow.service" || die "unexpected ExecStart in $tarball"
    install -m644 "$src/LICENSE" "$src/README.md" "$src/SCOWL-LICENSE.txt" "$src/BPE-LICENSE.txt" "$work/root/doc/"
    # Package versions sort betas first (0.2.1~beta); file names keep the tag's "-beta",
    # since GitHub release assets don't keep "~".
    for packager in deb:"xflow_$version-1_$arch.deb" rpm:"xflow-$version-1.$rpmarch.rpm"; do
        # Timestamps come from the tarball, so the same tarball gives the same package.
        (cd "$work" && SOURCE_DATE_EPOCH=$(stat -c %Y "$src/xflow") ARCH=$arch VERSION=$version \
            "$nfpm" package -f "$repo/packaging/linux/nfpm.yaml" -p "${packager%%:*}" -t "$out/${packager#*:}")
    done
done
