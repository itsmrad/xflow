#!/bin/sh
# Exercise scripts/install.sh's download mode against a loopback release server:
# good (curl and wget, both arches), tampered, unsupported arch, missing sums, and
# installs from an extracted tarball. Needs python3, curl, wget, tar, sha256sum.
set -eu
cd "$(dirname "$0")/.."
work=$(mktemp -d)
server=
trap '[ -z "$server" ] || kill "$server"; rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM
fail() { echo "FAIL $*" >&2; exit 1; }

# Fake releases: v1 and v2 are good; the others are broken as their tags say.
srv=$work/srv
release() { # tag label [tamper|crash|dupe]
    dir=$srv/download/$1
    mkdir -p "$dir"
    for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
        name=xflow-${1#v}-$target
        mkdir -p "$work/stage/$name"
        for bin in xflow xflowd; do
            printf '#!/bin/sh\necho %s %s %s\n' "$bin" "$2" "$target" >"$work/stage/$name/$bin"
            [ "${3:-}" != crash ] || printf '#!/bin/sh\nexit 1\n' >"$work/stage/$name/$bin"
            chmod 755 "$work/stage/$name/$bin"
        done
        tar -C "$work/stage" -czf "$dir/$name.tar.gz" "$name"
    done
    (cd "$dir" && sha256sum -- *.tar.gz >SHA256SUMS)
    case ${3:-} in
        tamper) for tarball in "$dir"/*.tar.gz; do printf 'x' >>"$tarball"; done ;;
        dupe) sed -i p "$dir/SHA256SUMS" ;;
    esac
}
release v1 one
release v2 two
release v3-tampered evil tamper
release v4-nosums none
rm "$srv/download/v4-nosums/SHA256SUMS"
release v5-crash crash crash
release v6-dupe dupe dupe
mkdir -p "$srv/api" "$srv/api-empty"
printf '[\n  {\n    "tag_name": "v2",\n    "prerelease": true\n  }\n]\n' >"$srv/api/releases"
echo '[]' >"$srv/api-empty/releases"

python3 -u -m http.server 0 --bind 127.0.0.1 --directory "$srv" >"$work/server.log" 2>&1 &
server=$!
port=
for _ in $(seq 50); do
    port=$(sed -n 's/.* port \([0-9]*\).*/\1/p' "$work/server.log")
    [ -z "$port" ] || break
    sleep 0.1
done
[ -n "$port" ] || fail "fixture server did not start: $(cat "$work/server.log")"
export XFLOW_API_URL="http://127.0.0.1:$port/api" XFLOW_DOWNLOAD_URL="http://127.0.0.1:$port/download"

# uname reports $FAKE_ARCH; the tools farm lets a case hide curl.
mkdir "$work/shim" "$work/tools"
# shellcheck disable=SC2016 # the shim expands $1 itself
printf '#!/bin/sh\ncase "$1" in -m) echo "$FAKE_ARCH" ;; *) echo Linux ;; esac\n' >"$work/shim/uname"
chmod 755 "$work/shim/uname"
for tool in sh cat cut dirname grep head install mkdir mktemp mv rm sed sha256sum tar gzip wget; do
    ln -s "$(command -v "$tool")" "$work/tools/$tool"
done
# Run the installer the way `curl | sh` does ($0 is sh), so no local build is picked up.
run() { # arch path prefix args...
    arch=$1 path=$2 prefix=$3
    shift 3
    FAKE_ARCH=$arch PATH=$work/shim:$path sh -s -- --prefix "$prefix" "$@" <"${script:-scripts/install.sh}" >"$work/out" 2>&1
}
expect_fail() { # message arch args...
    message=$1
    shift
    if run "$@"; then fail "installed although: $message"; fi
    grep -q "$message" "$work/out" || fail "expected '$message', got: $(cat "$work/out")"
}

prefix=$work/prefix
run x86_64 "$PATH" "$prefix" || fail "latest install: $(cat "$work/out")"
[ "$("$prefix/bin/xflowd")" = "xflowd two x86_64-unknown-linux-gnu" ] || fail "latest did not install v2"
echo "PASS latest release via curl"

run aarch64 "$work/tools" "$prefix" --version v1 || fail "wget install: $(cat "$work/out")"
[ "$("$prefix/bin/xflow")" = "xflow one aarch64-unknown-linux-gnu" ] || fail "pinned aarch64 install via wget"
echo "PASS pinned version, aarch64, wget"

before=$(sha256sum "$prefix/bin/xflow" "$prefix/bin/xflowd")
unchanged() {
    [ "$(sha256sum "$prefix/bin/xflow" "$prefix/bin/xflowd")" = "$before" ] || fail "$1 changed the install"
    [ -z "$(find "$prefix/bin" -name '.*')" ] || fail "$1 left temporary files behind"
}
expect_fail "checksum mismatch" x86_64 "$PATH" "$prefix" --version v3-tampered
unchanged "a tampered release"
echo "PASS tampered tarball refused, old install unchanged"
expect_fail "xflow v5-crash does not run here" x86_64 "$PATH" "$prefix" --version v5-crash
unchanged "a release that cannot run"
echo "PASS binaries that cannot run here refused, old install unchanged"

expect_fail "no prebuilt release for riscv64" riscv64 "$PATH" "$work/p2"
expect_fail "refusing to install unverified files" x86_64 "$PATH" "$work/p2" --version v4-nosums
expect_fail "no single entry for x86_64" x86_64 "$PATH" "$work/p2" --version v6-dupe
for tag in ../v1 v1/../v2 v1..2 -v1 latest; do
    expect_fail "invalid release tag" x86_64 "$PATH" "$work/p2" --version "$tag"
done
(XFLOW_API_URL="http://127.0.0.1:$port/api-empty" && expect_fail "no release found" x86_64 "$PATH" "$work/p2")
[ ! -e "$work/p2/bin/xflow" ] || fail "a refused install wrote binaries"
echo "PASS unsupported arch, missing or ambiguous sums, bad tags and no releases refused"

# A release's install.sh asset is stamped with its tag (as release.yml does) and installs that tag.
sed 's/^pinned=$/pinned=v1/' scripts/install.sh >"$work/pinned.sh"
grep -qx 'pinned=v1' "$work/pinned.sh" || fail "pinned= line not found to stamp"
(script=$work/pinned.sh && run x86_64 "$PATH" "$work/p4") || fail "pinned asset install: $(cat "$work/out")"
[ "$("$work/p4/bin/xflowd")" = "xflowd one x86_64-unknown-linux-gnu" ] || fail "pinned asset did not install v1"
echo "PASS stamped release asset installs its own tag"

# Local path: an extracted release tarball installs its own binaries with no network.
tar -C "$work" -xzf "$srv/download/v1/xflow-1-x86_64-unknown-linux-gnu.tar.gz"
mkdir "$work/xflow-1-x86_64-unknown-linux-gnu/scripts"
cp scripts/install.sh "$work/xflow-1-x86_64-unknown-linux-gnu/scripts/"
XFLOW_DOWNLOAD_URL=http://127.0.0.1:9/none sh "$work/xflow-1-x86_64-unknown-linux-gnu/scripts/install.sh" --prefix "$work/p3 with space" >"$work/out" 2>&1 ||
    fail "tarball-local install: $(cat "$work/out")"
[ "$("$work/p3 with space/bin/xflowd")" = "xflowd one x86_64-unknown-linux-gnu" ] || fail "tarball-local install"
echo "PASS install from an extracted tarball"
