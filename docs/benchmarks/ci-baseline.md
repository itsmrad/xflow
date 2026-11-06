Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 1 CI runs of `82ede7e` on AMD EPYC 7763 64-Core Processor, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/42185588883/attempts/1)):

- Idle tree PSS: run medians 10,167–10,167 KiB (0 %), per-launch medians 10,157–10,265 KiB (1 %). The baseline is 9,873 KiB and the gate allows +987 KiB, 9.1× the per-launch spread.
- Binaries: `xflow` 10,929,160 B in every run; `xflowd` 12,537,480 B in every run. The gate allows +1 %.
- Warm startup p50: 2.8–2.8 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
