Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 1 CI runs of `d87469f` on AMD EPYC 9V45 96-Core Processor, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/42189270370/attempts/1)):

- Idle tree PSS: run medians 9,868–9,868 KiB (0 %), per-launch medians 9,669–9,872 KiB (2 %). The baseline is 9,873 KiB and the gate allows +987 KiB, 4.9× the per-launch spread.
- Binaries: `xflow` 11,149,960 B in every run; `xflowd` 12,574,728 B in every run. The gate allows +1 %.
- Warm startup p50: 1.9–1.9 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
