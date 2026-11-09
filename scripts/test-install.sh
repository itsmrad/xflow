#!/bin/sh
# Exercise scripts/install.sh's download mode against a loopback release server:
# good (curl and wget, both arches), tampered, unsupported arch, missing sums, and
# the local --bin-dir path. Needs python3, curl, wget, tar, sha256sum.
set -eu
cd "$(dirname "$0")/.."
work=$(mktemp -d)
server=
trap '[ -z "$server" ] || kill "$server"; rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM
fail() { echo "FAIL $*" >&2; exit 1; }

# Fake releases: v1 and v2 are good, bad has a tampered tarball, nosums has no SHA256SUMS.
srv=$work/srv
release() { # tag label [tamper]
    dir=$srv/download/$1
    mkdir -p "$dir"
    for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
        name=xflow-${1#v}-$target
        mkdir -p "$work/stage/$name"
        for bin in xflow xflowd; do
            printf '#!/bin/sh\necho %s %s %s\n' "$bin" "$2" "$target" >"$work/stage/$name/$bin"
            chmod 755 "$work/stage/$name/$bin"
        done
        tar -C "$work/stage" -czf "$dir/$name.tar.gz" "$name"
    done
    (cd "$dir" && sha256sum -- *.tar.gz >SHA256SUMS)
    if [ -n "${3:-}" ]; then
        for tarball in "$dir"/*.tar.gz; do printf 'x' >>"$tarball"; done
    fi
}
release v1 one
release v2 two
release bad evil tamper
release nosums none
rm "$srv/download/nosums/SHA256SUMS"
mkdir -p "$srv/api"
printf '[\n  {\n    "tag_name": "v2",\n    "prerelease": true\n  }\n]\n' >"$srv/api/releases"

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
    FAKE_ARCH=$arch PATH=$work/shim:$path sh -s -- --prefix "$prefix" "$@" <scripts/install.sh >"$work/out" 2>&1
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
expect_fail "checksum mismatch" x86_64 "$PATH" "$prefix" --version bad
[ "$(sha256sum "$prefix/bin/xflow" "$prefix/bin/xflowd")" = "$before" ] || fail "tampered release changed the install"
[ -z "$(find "$prefix/bin" -name '.*')" ] || fail "temporary files left behind"
echo "PASS tampered tarball refused, old install unchanged"

expect_fail "no prebuilt release for riscv64" riscv64 "$PATH" "$work/p2"
expect_fail "refusing to install unverified files" x86_64 "$PATH" "$work/p2" --version nosums
expect_fail "invalid release tag" x86_64 "$PATH" "$work/p2" --version '../v1'
[ ! -e "$work/p2/bin/xflow" ] || fail "a refused install wrote binaries"
echo "PASS unsupported arch, missing sums and bad tag refused"

# Local path: an extracted release tarball installs its own binaries with no network.
tar -C "$work" -xzf "$srv/download/v1/xflow-1-x86_64-unknown-linux-gnu.tar.gz"
mkdir "$work/xflow-1-x86_64-unknown-linux-gnu/scripts"
cp scripts/install.sh "$work/xflow-1-x86_64-unknown-linux-gnu/scripts/"
XFLOW_DOWNLOAD_URL=http://127.0.0.1:9/none sh "$work/xflow-1-x86_64-unknown-linux-gnu/scripts/install.sh" --prefix "$work/p3 with space" >"$work/out" 2>&1 ||
    fail "tarball-local install: $(cat "$work/out")"
[ "$("$work/p3 with space/bin/xflowd")" = "xflowd one x86_64-unknown-linux-gnu" ] || fail "tarball-local install"
echo "PASS install from an extracted tarball"
