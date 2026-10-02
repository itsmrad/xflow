#!/usr/bin/env python3
"""Measure complete CLI process latency against the acceptance harness's fake daemon.

Usage: python3 crates/xflow-app/src/cli/benchmark.py target/release/xflow [output.json]
No real daemon, audio, desktop, credentials or provider calls are used.
"""
import json
import hashlib
import os
import platform
from datetime import datetime, timezone
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import time

from verify import Daemon


def main():
    binary = Path(sys.argv[1]).resolve()
    results = {"binary": str(binary), "samples": 100, "warmups": 10,
               "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "recorded_at": datetime.now(timezone.utc).isoformat(),
               "system": platform.system(), "kernel": platform.release(),
               "architecture": platform.machine(), "python": platform.python_version(),
               "measurement": "spawn through exit; fake same-user Unix daemon; no audio or desktop",
               "commands": {}}
    # Keep the Unix socket below Linux's 108-byte pathname limit in long worktrees.
    with tempfile.TemporaryDirectory(prefix="xb-", dir=os.environ.get("TMPDIR")) as directory:
        root = Path(directory)
        for name in ("cfg", "data", "run"):
            (root / name).mkdir(mode=0o700)
        env = {k: v for k, v in os.environ.items()
               if k not in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS")}
        env.update(XDG_CONFIG_HOME=str(root / "cfg"), XDG_DATA_HOME=str(root / "data"),
                   XDG_RUNTIME_DIR=str(root / "run"), NO_COLOR="1")
        daemon = Daemon(root)
        try:
            for args in (("toggle", "--quiet"), ("config", "path"), ("--help",)):
                samples = []
                for sample in range(results["samples"] + results["warmups"]):
                    start = time.perf_counter_ns()
                    result = subprocess.run([str(binary), *args], env=env, stdout=subprocess.DEVNULL,
                                            stderr=subprocess.PIPE, timeout=5)
                    elapsed = (time.perf_counter_ns() - start) / 1_000_000
                    assert result.returncode == 0, result.stderr.decode()
                    if sample >= results["warmups"]:
                        samples.append(elapsed)
                samples.sort()
                results["commands"][" ".join(args)] = {
                    "min_ms": round(samples[0], 3), "median_ms": round(statistics.median(samples), 3),
                    "p95_ms": round(samples[94], 3), "max_ms": round(samples[-1], 3)}
            assert len(daemon.requests) == results["samples"] + results["warmups"]
            assert all(request["command"] == "toggle" for request in daemon.requests)
        finally:
            daemon.close()
    text = json.dumps(results, indent=2) + "\n"
    if len(sys.argv) > 2:
        Path(sys.argv[2]).write_text(text)
    print(text, end="")


if __name__ == "__main__":
    main()
