#!/usr/bin/env python3
"""Measure xflow daemon startup, idle RSS/CPU, and Unix status IPC latency.

Example: scripts/benchmark.py --daemon target/release/xflowd --cli target/release/xflow --output benchmark.json

Uses only Python's standard library. The child runs with isolated XDG runtime,
config, and data directories and a privacy-safe config. The CLI path is recorded
as environment metadata; no CLI command, microphone, provider, or injection is
invoked. Output is a standalone JSON artifact. Unsupported measurements are
null with reasons; this benchmark does not measure hotkeys, audio, providers,
text injection, or overlays.
"""

import argparse
import json
import math
import os
import platform
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path


READY_TIMEOUT_SECONDS = 15.0
CONNECT_TIMEOUT_SECONDS = 1.0
MAX_MESSAGE_BYTES = 64 * 1024


def percentile(values, fraction):
    ordered = sorted(values)
    if not ordered:
        return None
    index = max(0, min(len(ordered) - 1, int((len(ordered) - 1) * fraction + 0.5)))
    return ordered[index]


def request_status(socket_path, timeout=CONNECT_TIMEOUT_SECONDS):
    started = time.monotonic()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect(str(socket_path))
        connection.sendall(b'{"command":"status"}\n')
        frame = bytearray()
        while True:
            chunk = connection.recv(min(4096, MAX_MESSAGE_BYTES - len(frame) + 1))
            if not chunk:
                raise RuntimeError("daemon closed the status connection before replying")
            newline = chunk.find(b"\n")
            frame.extend(chunk if newline < 0 else chunk[: newline + 1])
            if len(frame) > MAX_MESSAGE_BYTES:
                raise RuntimeError("status response exceeded 64 KiB")
            if newline >= 0:
                break
    response = json.loads(frame)
    if not isinstance(response, dict) or response.get("ok") is not True or response.get("state") != "idle":
        raise RuntimeError(f"daemon returned an unsuccessful or non-idle status response: {response!r}")
    return time.monotonic() - started, response


def proc_stats(pid):
    """Read VmRSS and CPU ticks, parsing stat after its final ')' (comm may contain spaces)."""
    proc = Path("/proc") / str(pid)
    status_lines = (proc / "status").read_text().splitlines()
    rss_kib = None
    for line in status_lines:
        if line.startswith("VmRSS:"):
            rss_kib = int(line.split()[1])
            break
    if rss_kib is None:
        raise RuntimeError("VmRSS not available in /proc/<pid>/status")
    stat = (proc / "stat").read_text()
    close_paren = stat.rfind(")")
    if close_paren < 0:
        raise RuntimeError("could not parse /proc/<pid>/stat")
    fields = stat[close_paren + 1 :].split()  # starts at field 3 (state)
    return rss_kib, int(fields[11]) + int(fields[12]), int(fields[19])  # utime, stime, starttime


def environment_metadata(daemon, cli):
    return {
        "kernel": platform.release(),
        "system": platform.system(),
        "architecture": platform.machine(),
        "python": platform.python_version(),
        "daemon_binary": str(daemon.resolve()),
        "cli_binary": str(cli.resolve()),
    }


def run(args):
    daemon = Path(args.daemon)
    cli = Path(args.cli)
    for label, binary in (("daemon", daemon), ("CLI", cli)):
        if not binary.is_file():
            raise FileNotFoundError(f"{label} binary not found: {binary}")
        if not os.access(binary, os.X_OK):
            raise PermissionError(f"{label} binary is not executable: {binary}")

    result = {
        "schema_version": 1,
        "environment": environment_metadata(daemon, cli),
        "parameters": {"idle_seconds": args.idle_seconds, "samples": args.samples},
        "measurements": {
            "startup_seconds": None,
            "idle_rss_kib": None,
            "idle_cpu_percent": None,
            "status_latency_ms": {"p50": None, "p95": None, "samples": 0},
            "hotkey": {"value": None, "reason": "requires desktop keyboard integration"},
            "audio": {"value": None, "reason": "not measured; benchmark never accesses microphone"},
            "provider": {"value": None, "reason": "not measured; benchmark makes no provider requests"},
            "text_injection": {"value": None, "reason": "not measured; benchmark sends status only"},
            "overlay": {"value": None, "reason": "requires desktop UI integration"},
        },
        "error": None,
    }
    with tempfile.TemporaryDirectory(prefix="xflow-benchmark-") as temp:
        root = Path(temp)
        runtime = root / "runtime"
        config_home = root / "config"
        data_home = root / "data"
        for directory in (runtime, config_home / "xflow", data_home):
            directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(runtime, 0o700)
        (config_home / "xflow" / "config.toml").write_text(
            "[privacy]\nhistory = false\n\n[injection]\nclipboard_only = true\n",
            encoding="utf-8",
        )
        # Do not expose inherited provider credentials or unrelated secrets.
        env = {key: os.environ[key] for key in ("PATH", "HOME", "LANG", "LC_ALL") if key in os.environ}
        env.update({"XDG_RUNTIME_DIR": str(runtime), "XDG_CONFIG_HOME": str(config_home), "XDG_DATA_HOME": str(data_home)})
        socket_path = runtime / "xflow" / "daemon.sock"
        latencies = []
        started = time.monotonic()
        # A file avoids blocking the daemon if it writes more than a pipe can hold.
        with tempfile.TemporaryFile(mode="w+t", encoding="utf-8") as daemon_log:
            child = subprocess.Popen([str(daemon.resolve())], env=env, stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=daemon_log)
            try:
                deadline = started + READY_TIMEOUT_SECONDS
                last_error = None
                while time.monotonic() < deadline:
                    if child.poll() is not None:
                        daemon_log.seek(0)
                        detail = daemon_log.read(4096).strip()
                        raise RuntimeError(f"daemon exited during startup ({child.returncode})" + (f": {detail}" if detail else ""))
                    try:
                        request_status(socket_path, timeout=min(CONNECT_TIMEOUT_SECONDS, max(0.01, deadline - time.monotonic())))
                        result["measurements"]["startup_seconds"] = time.monotonic() - started
                        break
                    except OSError as exc:
                        last_error = exc
                        time.sleep(min(0.05, max(0, deadline - time.monotonic())))
                else:
                    if child.poll() is not None:
                        daemon_log.seek(0)
                        detail = daemon_log.read(4096).strip()
                        raise RuntimeError(f"daemon exited during startup ({child.returncode})" + (f": {detail}" if detail else ""))
                    raise TimeoutError(f"daemon did not become ready within {READY_TIMEOUT_SECONDS:g} seconds: {last_error}")

                for _ in range(args.samples):
                    elapsed, _ = request_status(socket_path)
                    result["measurements"]["status_latency_ms"]["samples"] += 1
                    latencies.append(elapsed * 1000)

                if child.poll() is not None:
                    raise RuntimeError(f"daemon exited before idle measurement ({child.returncode})")
                _, before_ticks, before_start = proc_stats(child.pid)
                before_time = time.monotonic()
                time.sleep(args.idle_seconds)
                if child.poll() is not None:
                    raise RuntimeError(f"daemon exited during idle measurement ({child.returncode})")
                after_rss, after_ticks, after_start = proc_stats(child.pid)
                if before_start != after_start:
                    raise RuntimeError("daemon PID changed during idle measurement")
                elapsed = time.monotonic() - before_time
                ticks_per_second = os.sysconf("SC_CLK_TCK")
                cpu_percent = (after_ticks - before_ticks) / ticks_per_second / elapsed * 100.0
                result["measurements"]["idle_rss_kib"] = after_rss
                result["measurements"]["idle_cpu_percent"] = max(0.0, cpu_percent)
                result["measurements"]["status_latency_ms"].update({"p50": percentile(latencies, 0.50), "p95": percentile(latencies, 0.95)})
            finally:
                if child.poll() is None:
                    try:
                        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                            connection.settimeout(1.0)
                            connection.connect(str(socket_path))
                            connection.sendall(b'{"command":"shutdown"}\n')
                    except OSError:
                        pass
                    try:
                        child.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        child.terminate()
                        try:
                            child.wait(timeout=2)
                        except subprocess.TimeoutExpired:
                            child.kill()
                            child.wait()
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--daemon", required=True, help="path to xflowd")
    parser.add_argument("--cli", required=True, help="path to xflow CLI (recorded; never invoked)")
    parser.add_argument("--output", required=True, help="write standalone JSON artifact here")
    parser.add_argument("--idle-seconds", type=float, default=10, help="idle measurement duration (default: 10)")
    parser.add_argument("--samples", type=int, default=100, help="status latency samples (default: 100)")
    args = parser.parse_args()
    if not math.isfinite(args.idle_seconds) or args.idle_seconds <= 0 or args.samples < 1:
        parser.error("--idle-seconds must be finite and positive; --samples must be positive")
    try:
        artifact = run(args)
    except Exception as exc:
        artifact = {
            "schema_version": 1,
            "environment": environment_metadata(Path(args.daemon), Path(args.cli)),
            "parameters": {"idle_seconds": args.idle_seconds, "samples": args.samples},
            "measurements": {},
            "error": f"{type(exc).__name__}: {exc}",
        }
        exit_code = 1
    else:
        exit_code = 0
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(artifact, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if exit_code:
        print(artifact["error"], file=sys.stderr)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
