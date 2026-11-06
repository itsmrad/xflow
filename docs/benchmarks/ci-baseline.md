Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 1 CI runs of `100a18c` on AMD EPYC 9V74 80-Core Processor, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/42154746953/attempts/1)):

- Idle tree PSS: run medians 9,969–9,969 KiB (0 %), per-launch medians 9,944–10,072 KiB (1 %). The baseline is 9,873 KiB and the gate allows +987 KiB, 7.7× the per-launch spread.
- Binaries: `xflow` 8,226,512 B in every run; `xflowd` 12,472,744 B in every run. The gate allows +1 %.
- Warm startup p50: 1.7–1.7 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
