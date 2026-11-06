#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
pkg="$root/tools/local-stt/coreml"
out=${1:-"$PWD/xflow-stt-coreml"}

swift build -c release --arch arm64 --package-path "$pkg"
bin_dir=$(swift build -c release --arch arm64 --package-path "$pkg" --show-bin-path)
mkdir -p "$(dirname "$out")"
cp "$bin_dir/xflow-stt-coreml" "$out"
codesign --force --options runtime --sign "${XFLOW_SIGN_IDENTITY:--}" \
    --identifier com.xflow.stt-coreml "$out"
