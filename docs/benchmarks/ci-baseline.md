Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 1 CI runs of `c977e58` on AMD EPYC 9V74 80-Core Processor, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/42169619094/attempts/1)):

- Idle tree PSS: run medians 10,124–10,124 KiB (0 %), per-launch medians 9,516–10,192 KiB (7 %). The baseline is 9,873 KiB and the gate allows +987 KiB, 1.5× the per-launch spread.
- Binaries: `xflow` 10,594,824 B in every run; `xflowd` 12,537,352 B in every run. The gate allows +1 %.
- Warm startup p50: 1.8–1.8 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
