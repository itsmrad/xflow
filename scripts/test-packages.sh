#!/bin/sh
# Install, reinstall, upgrade and remove the .deb (Ubuntu 24.04), the .rpm (Fedora 44) and
# the Arch PKGBUILD (built by makepkg as a non-root user) in clean containers.
# Usage: scripts/test-packages.sh dist/xflow-<version>-x86_64-unknown-linux-gnu.tar.gz
# Needs docker, curl and sha256sum.
set -eu
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
tarball=${1:?usage: $0 dist/xflow-<version>-x86_64-unknown-linux-gnu.tar.gz}
name=$(basename "$tarball" .tar.gz)
case $name in xflow-*-x86_64-unknown-linux-gnu) ;; *) echo "error: $tarball is not an x86_64 release tarball" >&2; exit 2 ;; esac
version=${name#xflow-}
version=${version%-x86_64-unknown-linux-gnu}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail() { echo "FAIL $*" >&2; exit 1; }

# preremove stops the service in each user manager on removal, never on upgrade.
mkdir "$work/shim"
printf '#!/bin/sh\necho "1000 alice no active"\n' >"$work/shim/loginctl"
# shellcheck disable=SC2016 # the shim expands $* itself
printf '#!/bin/sh\necho "$*" >>"%s/calls"\n' "$work" >"$work/shim/systemctl"
chmod 755 "$work/shim/loginctl" "$work/shim/systemctl"
for arg in upgrade failed-upgrade 1; do PATH=$work/shim:$PATH sh "$repo/packaging/linux/preremove.sh" "$arg"; done
[ ! -e "$work/calls" ] || fail "preremove stopped the service on upgrade"
for arg in remove 0; do PATH=$work/shim:$PATH sh "$repo/packaging/linux/preremove.sh" "$arg"; done
stop='--user -M alice@ stop xflow.service'
[ "$(cat "$work/calls")" = "$stop
$stop" ] || fail "preremove calls: $(cat "$work/calls")"
echo "PASS preremove stops the user service on removal only"

# The committed PKGBUILD's sums are those of the release it names.
tag=$(sed -n 's/^_tag=//p' "$repo/packaging/arch/PKGBUILD")
curl -fsSL --proto =https -o "$work/SHA256SUMS" "https://github.com/itsmrad/xflow/releases/download/$tag/SHA256SUMS"
for arch in x86_64 aarch64; do
    sum=$(sed -n "s/^sha256sums_$arch=('\([0-9a-f]*\)')\$/\1/p" "$repo/packaging/arch/PKGBUILD")
    grep -qx "$sum  xflow-${tag#v}-$arch-unknown-linux-gnu.tar.gz" "$work/SHA256SUMS" || fail "PKGBUILD $arch sum is not $tag's"
done
echo "PASS PKGBUILD sums match $tag"

# "old" is the given tarball; "new" repacks its files as 99.0.0, so the upgrade is a real one.
mkdir -p "$work/old" "$work/new" "$work/x"
cp "$tarball" "$work/old/"
tar -xzf "$tarball" -C "$work/x"
mv "$work/x/$name" "$work/x/xflow-99.0.0-x86_64-unknown-linux-gnu"
tar -C "$work/x" -czf "$work/new/xflow-99.0.0-x86_64-unknown-linux-gnu.tar.gz" xflow-99.0.0-x86_64-unknown-linux-gnu
"$repo/scripts/package-linux.sh" "$work/old/$name.tar.gz" "$work/new/xflow-99.0.0-x86_64-unknown-linux-gnu.tar.gz"
for file in "$work/old/xflow_$version-1_amd64.deb" "$work/old/xflow-$version-1.x86_64.rpm" \
    "$work/new/xflow_99.0.0-1_amd64.deb" "$work/new/xflow-99.0.0-1.x86_64.rpm"; do
    [ -f "$file" ] || fail "missing $file"
done
# GitHub renames release assets with "~", which would break SHA256SUMS.
[ -z "$(find "$work/old" "$work/new" -name '*~*')" ] || fail "package file names contain ~"
echo "PASS package file names"
# The PKGBUILD as committed, pointed at a local tarball; makepkg uses a source file it finds beside it.
pkgbuild() { # dir tarball tag
    mkdir -p "$work/arch/$1"
    cp "$2" "$repo/packaging/arch/xflow-bin.install" "$work/arch/$1/"
    sum=$(sha256sum "$2" | cut -d ' ' -f 1)
    sed -e "s/^_tag=.*/_tag=$3/" -e "s/^pkgver=.*/pkgver=$(echo "${3#v}" | tr -d -)/" \
        -e "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('$sum')/" "$repo/packaging/arch/PKGBUILD" >"$work/arch/$1/PKGBUILD"
}
pkgbuild old "$tarball" "v$version"
pkgbuild new "$work/new/xflow-99.0.0-x86_64-unknown-linux-gnu.tar.gz" v99.0.0
cat >"$work/lib.sh" <<'EOF'
set -eux
check() {
    xflow --version
    xflowd --version
    grep -qx 'ExecStart=/usr/bin/xflowd' /usr/lib/systemd/user/xflow.service
}
gone() {
    for path in /usr/bin/xflow /usr/bin/xflowd /usr/lib/systemd/user/xflow.service; do
        test ! -e "$path"
    done
}
EOF
chmod -R a+rX "$work"
run() { docker run --rm -i -v "$work:/w:ro" "$1" sh -s; }

# Packages must pull every library the binaries link: the images start without them.
run ubuntu:24.04@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852 <<'EOF'
. /w/lib.sh
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -qq -y --no-install-recommends /w/old/*.deb
check
apt-get install -qq -y --no-install-recommends --reinstall /w/old/*.deb
check
apt-get install -qq -y --no-install-recommends /w/new/*.deb
check
test "$(dpkg-query -W -f '${Version}' xflow)" = 99.0.0-1
apt-get remove -qq -y xflow
gone
EOF
echo "PASS deb install, reinstall, upgrade, remove on Ubuntu 24.04"

run fedora:44@sha256:15da97ed9aa94dd3d92c3925eb89520f6c80e07ea7ee24cba848d1f8b6912b3e <<'EOF'
. /w/lib.sh
dnf -y -q --setopt=install_weak_deps=False install /w/old/*.rpm
check
dnf -y -q reinstall /w/old/*.rpm
check
dnf -y -q --setopt=install_weak_deps=False upgrade /w/new/*.rpm
check
test "$(rpm -q --qf '%{VERSION}' xflow)" = 99.0.0
dnf -y -q remove xflow
gone
EOF
echo "PASS rpm install, reinstall, upgrade, remove on Fedora 44"

run archlinux:base@sha256:b1c2fe6e717bfd5673af8fc217e1e1c3ce0dcbb1d53c3414c8425e7ce79fc6d9 <<'EOF'
. /w/lib.sh
pacman -Syu --noconfirm --needed fakeroot >/dev/null
useradd -m builder
cp -R /w/arch /home/builder/arch
chown -R builder /home/builder/arch
su builder -c 'cd ~/arch/old && makepkg --nodeps --noconfirm && cd ../new && makepkg --nodeps --noconfirm'
pacman -U --noconfirm /home/builder/arch/old/*.pkg.tar.zst
check
pacman -U --noconfirm /home/builder/arch/old/*.pkg.tar.zst
check
pacman -U --noconfirm /home/builder/arch/new/*.pkg.tar.zst
check
test "$(pacman -Q xflow-bin)" = 'xflow-bin 99.0.0-1'
pacman -R --noconfirm xflow-bin
gone
EOF
echo "PASS PKGBUILD build (non-root), install, reinstall, upgrade, remove on Arch"
