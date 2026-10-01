#!/bin/sh
# The one verification entry point shared by local runs and CI.
# Usage: scripts/check.sh [fmt|extension|clippy|test]...   (no arguments: every stage)
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
        dbus-run-session -- sh -c 'cargo test --workspace --locked --features xflow-app/test-support && cargo test -p xflow-platform --locked -- --ignored'
    else
        missing dbus-run-session "the D-Bus bridge test"
        cargo test --workspace --locked --features xflow-app/test-support
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

[ "$#" -gt 0 ] || set -- fmt extension clippy test
for stage in "$@"; do
    echo "==> $stage" >&2
    case "$stage" in
        fmt) cargo fmt --all -- --check ;;
        extension) check_extension ;;
        clippy) cargo clippy --workspace --all-targets --locked --features xflow-app/test-support -- -D warnings ;;
        test) run_tests ;;
        *) echo "usage: $0 [fmt|extension|clippy|test]..." >&2; exit 2 ;;
    esac
done
