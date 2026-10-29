Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 3 CI runs of `8c44fe2` on AMD EPYC 7763 64-Core Processor, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/40911629185/attempts/1), [2](https://github.com/itsmrad/xflow/actions/runs/40911629185/attempts/2), [3](https://github.com/itsmrad/xflow/actions/runs/40911629185/attempts/3)):

- Idle tree PSS: run medians 9,385–9,478 KiB (1 %), per-launch medians 9,221–9,653 KiB (5 %). The baseline is 9,398 KiB and the gate allows +939 KiB, 2.2× the per-launch spread.
- Binaries: `xflow` 7,975,512 B in every run; `xflowd` 11,560,584 B in every run. The gate allows +5 %.
- Warm startup p50: 2.2–2.7 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
