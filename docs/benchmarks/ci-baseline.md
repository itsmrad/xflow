Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 1 CI runs of `2017a43` on AMD EPYC 7763 64-Core Processor, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/42227481151/attempts/1)):

- Idle tree PSS: run medians 10,114–10,114 KiB (0 %), per-launch medians 9,857–10,356 KiB (5 %). The baseline is 9,873 KiB and the gate allows +987 KiB, 2.0× the per-launch spread.
- Binaries: `xflow` 11,266,696 B in every run; `xflowd` 12,838,152 B in every run. The gate allows +1 %.
- Warm startup p50: 2.7–2.7 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
