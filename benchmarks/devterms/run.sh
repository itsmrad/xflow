#!/usr/bin/env bash
# Manual, opt-in: this sends synthetic speech to Groq and spends API credit.
set -euo pipefail
cd "$(dirname "$0")"
exec python3 run.py "$@"
