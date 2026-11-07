Generated from [`ci-baseline.json`](../../docs/benchmarks/ci-baseline.json) by `scoreboard.py render`; do not edit by hand. 1 CI runs of `cd020a0` on INTEL(R) XEON(R) PLATINUM 8573C, 4 CPUs ([1](https://github.com/itsmrad/xflow/actions/runs/42348728205/attempts/1)):

- Idle tree PSS: run medians 10,172–10,172 KiB (0 %), per-launch medians 10,080–10,183 KiB (1 %). The baseline is 9,873 KiB and the gate allows +987 KiB, 9.6× the per-launch spread.
- Binaries: `xflow` 12,236,328 B in every run; `xflowd` 12,925,480 B in every run. The gate allows +1 %.
- Warm startup p50: 2.1–2.1 ms against the absolute 100 ms budget; idle CPU at most 0.0000 % against 0.1 %.
