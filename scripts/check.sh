#!/bin/sh
# The one verification entry point shared by local runs and CI.
# Usage: scripts/check.sh [fmt|extension|install|clippy|test]...   (no arguments: every stage)
# Missing optional tools are skipped with a notice locally but fail under CI.
set -eu
cd "$(dirname "$0")/.."

missing() {
    if [ -n "${CI:-}" ]; then
        echo "error: $1 is required in CI" >&2
        exit 1
    fi
    echo "notice: $1 not found; skipping $2" >&2
}

run_tests() {
    if command -v dbus-run-session >/dev/null 2>&1; then
        # A private session bus keeps tests off the desktop bus and lets the
        # #[ignore]d bridge test own its well-known name.
        dbus-run-session -- sh -c 'cargo test --manifest-path apps/cli/Cargo.toml --workspace --locked --features xflow-app/test-support && cargo test --manifest-path apps/cli/Cargo.toml -p xflow-platform --locked -- --ignored'
    else
        missing dbus-run-session "the D-Bus bridge test"
        cargo test --manifest-path apps/cli/Cargo.toml --workspace --locked --features xflow-app/test-support
    fi
}

check_extension() {
    dir=packaging/gnome-extension
    if command -v node >/dev/null 2>&1; then
        find "$dir" -type f \( -name '*.js' -o -name '*.mjs' \) | sort | while read -r file; do
            node --input-type=module --check <"$file" || { echo "error: $file has a syntax error" >&2; exit 1; }
        done
        tests=$(find "$dir" -type f \( -name '*.test.js' -o -name '*.test.mjs' \) | sort)
        # shellcheck disable=SC2086 # repository paths contain no whitespace
        if [ -n "$tests" ]; then node --test $tests; fi
    else
        missing node "GNOME extension JavaScript checks"
    fi
    if command -v glib-compile-schemas >/dev/null 2>&1; then
        glib-compile-schemas --strict --dry-run "$dir/schemas"
    else
        missing glib-compile-schemas "GSettings schema validation"
    fi
}

check_install() {
    if command -v shellcheck >/dev/null 2>&1; then
        shellcheck scripts/install.sh scripts/test-install.sh scripts/check-glibc.sh scripts/package-linux.sh scripts/test-packages.sh packaging/linux/preremove.sh
    else
        missing shellcheck "installer lint"
    fi
    for tool in python3 curl wget sha256sum readelf; do
        command -v "$tool" >/dev/null 2>&1 || { missing "$tool" "installer tests"; return 0; }
    done
    scripts/test-install.sh
    # The glibc gate must reject a floor below what any real binary needs.
    if GLIBC_FLOOR=2.0 scripts/check-glibc.sh /bin/sh 2>&1 | grep -q 'above the GLIBC_2.0 floor'; then :; else
        echo "error: check-glibc.sh did not reject /bin/sh against a GLIBC_2.0 floor" >&2
        exit 1
    fi
}

[ "$#" -gt 0 ] || set -- fmt extension install clippy test
for stage in "$@"; do
    echo "==> $stage" >&2
    case "$stage" in
        fmt) cargo fmt --manifest-path apps/cli/Cargo.toml --all -- --check ;;
        extension) check_extension ;;
        install) check_install ;;
        clippy) cargo clippy --manifest-path apps/cli/Cargo.toml --workspace --all-targets --locked --features xflow-app/test-support -- -D warnings ;;
        test) run_tests ;;
        *) echo "usage: $0 [fmt|extension|install|clippy|test]..." >&2; exit 2 ;;
    esac
done
