Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 1 CI runs of `c742493` on AMD EPYC 9V45 96-Core Processor, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/42212391796/attempts/1)):

- Idle tree PSS: run medians 10,005–10,005 KiB (0 %), per-launch medians 9,840–10,085 KiB (2 %). The baseline is 9,873 KiB and the gate allows +987 KiB, 4.0× the per-launch spread.
- Binaries: `xflow` 11,265,032 B in every run; `xflowd` 12,596,872 B in every run. The gate allows +1 %.
- Warm startup p50: 1.9–1.9 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
