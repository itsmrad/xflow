#!/bin/sh
# Fail when a release binary needs a newer glibc than the oldest supported distro ships.
# Floor 2.39 = Ubuntu 24.04 (Debian 13 ships 2.41, Fedora 41 2.40). Usage: scripts/check-glibc.sh BINARY...
set -eu
floor=${GLIBC_FLOOR:-2.39}
[ "$#" -gt 0 ] || { echo "usage: $0 BINARY..." >&2; exit 2; }
for bin in "$@"; do
    max=$(readelf -W --dyn-syms "$bin" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//; s/\.$//' | sort -uV | tail -n 1)
    [ -n "$max" ] || { echo "error: no GLIBC_ symbol versions in $bin" >&2; exit 1; }
    if [ "$(printf '%s\n%s\n' "$floor" "$max" | sort -V | tail -n 1)" != "$floor" ]; then
        echo "error: $bin needs GLIBC_$max, above the GLIBC_$floor floor" >&2
        exit 1
    fi
    echo "$bin: newest glibc symbol GLIBC_$max (floor $floor)"
done
