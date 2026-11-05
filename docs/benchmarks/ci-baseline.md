Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 3 CI runs of `71e4aa6` on AMD EPYC 9V45 96-Core Processor, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/41375047607/attempts/1), [2](https://github.com/itsmrad/xflow/actions/runs/41375047607/attempts/2), [3](https://github.com/itsmrad/xflow/actions/runs/41375047607/attempts/3)):

- Idle tree PSS: run medians 9,710–9,877 KiB (2 %), per-launch medians 9,651–9,974 KiB (3 %). The baseline is 9,873 KiB and the gate allows +987 KiB, 3.1× the per-launch spread.
- Binaries: `xflow` 8,098,656 B in every run; `xflowd` 12,147,400 B in every run. The gate allows +1 %.
- Warm startup p50: 1.6–2.0 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
