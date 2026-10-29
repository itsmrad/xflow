#!/usr/bin/env bash
# Manual, opt-in: this sends transcript text (no audio) to Groq and spends API credit.
# GROQ_API_KEY must already be exported; no credential store is read.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
bin="${XFLOW_FORMAT_BENCH:-$root/apps/cli/target/release/examples/format_bench}"
[[ -x "$bin" ]] || (cd "$root/apps/cli" && cargo build --release -p xflow-providers --example format_bench)
if [[ "${1:-}" == --latency ]]; then
  exec "$bin" "$@"
fi
exec "$bin" "$root/benchmarks/formatting/corpus.jsonl" "$@"
