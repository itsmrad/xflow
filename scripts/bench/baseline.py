#!/usr/bin/env python3
"""Isolated v1 baseline; synthetic suites and optional unauthenticated TLS probes."""

import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("headless", ROOT / "scripts/benchmark.py")
headless = importlib.util.module_from_spec(spec)
spec.loader.exec_module(headless)
HOSTS = ("api.groq.com", "api.openai.com", "api.deepgram.com", "api.assemblyai.com",
         "api.elevenlabs.io", "generativelanguage.googleapis.com", "api.mistral.ai",
         "openrouter.ai", "api.together.xyz", "api.fireworks.ai")


def summary(values, unit):
    return {"unit": unit, "samples": values, "count": len(values),
            "p50": headless.percentile(values, .5), "p95": headless.percentile(values, .95),
            "min": min(values) if values else None, "max": max(values) if values else None}


def text_command(command):
    try:
        return subprocess.check_output(command, cwd=ROOT, stderr=subprocess.DEVNULL,
                                       text=True, timeout=10).strip()
    except (OSError, subprocess.SubprocessError):
        return None


def stop(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


@contextmanager
def isolated():
    scratch = ROOT / 'target/btmp'
    scratch.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="xfb-", dir=scratch) as temp:
        root = Path(temp)
        for name in ("run", "config/xflow", "data", "cache", "home"):
            (root / name).mkdir(mode=0o700, parents=True)
        (root / "config/xflow/config.toml").write_text(
            '[stt]\nprovider = "custom"\nmodel = "bench-model"\n'
            'endpoint = "http://127.0.0.1:9/v1/audio/transcriptions"\n\n'
            '[privacy]\nhistory = false\noffline = true\n\n[injection]\nclipboard_only = true\n')
        env = {k: os.environ[k] for k in ("PATH", "LANG", "LC_ALL") if k in os.environ}
        env.update(HOME=str(root / "home"), XDG_RUNTIME_DIR=str(root / "run"),
                   XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                   XDG_CACHE_HOME=str(root / "cache"), TMPDIR=str(root),
                   XFLOW_BENCH_ISOLATED_BUS="1", YDOTOOL_SOCKET=str(root / "unused.sock"),
                   GIO_USE_VFS="local", GIO_USE_VOLUME_MONITOR="unix")
        address = f"unix:path={root}/run/bus"
        # No service directories: private probes must never activate keyring,
        # GVFS or any other desktop service installed on the host.
        config = root / "bus.conf"
        config.write_text('<busconfig><type>session</type><auth>EXTERNAL</auth>'
                          f'<listen>{address}</listen><policy context="default">'
                          '<allow send_destination="*"/><allow receive_sender="*"/>'
                          '<allow own="*"/></policy></busconfig>')
        with tempfile.TemporaryFile(mode="w+t") as log:
            bus = subprocess.Popen(["dbus-daemon", f"--config-file={config}", "--nofork", "--nopidfile"],
                                   env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.DEVNULL, stderr=log)
            try:
                deadline = time.monotonic() + 5
                while not (root / "run/bus").exists():
                    if bus.poll() is not None or time.monotonic() > deadline:
                        log.seek(0)
                        raise RuntimeError("private bus startup failed: " + log.read(4096))
                    time.sleep(.002)
                env["DBUS_SESSION_BUS_ADDRESS"] = address
                yield env, root / "run/xflow/daemon.sock"
            finally:
                stop(bus)


def proc(pid):
    root = Path(f"/proc/{pid}")
    rss, ticks, start = headless.proc_stats(pid)
    pss = next(int(line.split()[1]) for line in (root / "smaps_rollup").read_text().splitlines()
               if line.startswith("Pss:"))
    switches = {line.split(":")[0]: int(line.split()[1]) for line in
                (root / "status").read_text().splitlines() if line.startswith(
                    ("voluntary_ctxt_switches:", "nonvoluntary_ctxt_switches:"))}
    return {"rss_kib": rss, "pss_kib": pss, "cpu_ticks": ticks, "start_ticks": start,
            "threads": len(list((root / "task").iterdir())), "fds": len(list((root / "fd").iterdir())),
            "main_thread_switches": switches,
            "main_thread_schedstat": [int(v) for v in (root / "schedstat").read_text().split()]}


def measure_daemon(args):
    startup, ipc, cli, failures = [], [], [], []
    idle = None
    for launch in range(args.startup_samples):
        with isolated() as (env, path), tempfile.TemporaryFile(mode="w+t") as log:
            t0 = time.monotonic()
            child = subprocess.Popen([str(Path(args.daemon).resolve())], env=env,
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log)
            try:
                while True:
                    if child.poll() is not None or time.monotonic() - t0 > 15:
                        log.seek(0)
                        raise RuntimeError("daemon startup failed: " + log.read(4096))
                    try:
                        headless.request_status(path)
                        break
                    except OSError:
                        time.sleep(.001)
                startup.append((time.monotonic() - t0) * 1000)
                if launch == 0:
                    for _ in range(args.samples):
                        elapsed, _ = headless.request_status(path)
                        ipc.append(elapsed * 1000)
                        t1 = time.monotonic()
                        reply = subprocess.check_output([str(Path(args.cli).resolve()), "--json", "status"],
                                                        env=env, stderr=subprocess.PIPE, timeout=5)
                        cli.append((time.monotonic() - t1) * 1000)
                        if json.loads(reply).get("state") != "idle":
                            raise RuntimeError("CLI status was not idle")
                    time.sleep(args.warmup_seconds)
                    before, t1, observations = proc(child.pid), time.monotonic(), []
                    while time.monotonic() - t1 < args.idle_seconds:
                        time.sleep(min(1, max(0, args.idle_seconds - (time.monotonic() - t1))))
                        observations.append(proc(child.pid))
                    elapsed, after = time.monotonic() - t1, observations[-1]
                    if before["start_ticks"] != after["start_ticks"] or child.poll() is not None:
                        raise RuntimeError("daemon changed during idle observation")
                    idle = {"seconds": elapsed, "warmup_seconds": args.warmup_seconds,
                            "clock_ticks_per_second": os.sysconf("SC_CLK_TCK"),
                            "before": before, "after": after, "observations": observations,
                            "cpu_percent": (after["cpu_ticks"] - before["cpu_ticks"]) /
                            os.sysconf("SC_CLK_TCK") / elapsed * 100,
                            "rss": summary([r["rss_kib"] for r in observations], "KiB"),
                            "pss": summary([r["pss_kib"] for r in observations], "KiB"),
                            "wakeups": {"value": None, "reason": "context switches/schedstat are not wakeup counters"}}
            except Exception as exc:
                failures.append({"launch": launch, "error": str(exc)})
            finally:
                stop(child)
    return {"startup_ms": summary(startup, "ms"), "status_ipc_ms": summary(ipc, "ms"),
            "cli_status_ms": summary(cli, "ms"), "idle": idle, "failures": failures,
            "cache_policy": "warm filesystem cache; fresh XDG dirs and private bus per launch"}


def suites(binary, samples):
    results = {}
    with isolated() as (env, _):
        for suite in ("wav", "stt", "dbus", "spawn", "tui", "json", "keyring-crypto"):
            command = [str(Path(binary).resolve()), suite, "--samples", str(1 if suite == "wav" else samples)]
            result = json.loads(subprocess.check_output(command, env=env, timeout=120))
            for metric in result["metrics"].values():
                if "samples" in metric:
                    metric.update(summary(metric["samples"], metric["unit"]))
            result["command"] = command
            results[suite] = result
        if text_command(["gjs", "--version"]):
            server = subprocess.Popen([str(Path(binary).resolve()), "dbus-serve"], env=env,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE)
            try:
                if not select.select([server.stdout], [], [], 5)[0] or server.stdout.readline() != b"ready\n":
                    raise RuntimeError("benchmark server did not become ready")
                result = json.loads(subprocess.check_output(
                    ["gjs", "-m", str(ROOT / "scripts/bench/dbus_client.js"), str(samples)], env=env, timeout=30))
                results["gjs_command"] = summary(result["samples"], "us")
            finally:
                server.stdin.close()
                try:
                    server.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    stop(server)
        else:
            results["gjs_command"] = {"value": None, "reason": "GJS unavailable"}
    return results


def network(samples, binary):
    result = {"scope": "unauthenticated GET /; redirects disabled; no audio or credentials",
              "curl_version": text_command(["curl", "--version"]), "hosts": {}}
    with isolated() as (env, _):
        for host in HOSTS:
            runs = []
            for _ in range(samples):
                command = ["curl", "-q", "--noproxy", "*", "--silent", "--show-error",
                           "--connect-timeout", "5", "--max-time", "10", "--output", "/dev/null",
                           "--write-out", "%{json}", f"https://{host}/"]
                child = subprocess.run(command, env=env, capture_output=True, text=True, timeout=15)
                data = json.loads(child.stdout) if child.stdout.strip() else {}
                keys = ("time_namelookup", "time_connect", "time_appconnect", "time_starttransfer",
                        "time_total", "http_version", "http_code", "num_connects")
                row = {k: data.get(k) for k in keys}
                row.update(exit_code=child.returncode, error=child.stderr.strip() or None)
                if child.returncode == 0:
                    row.update(dns_ms=data["time_namelookup"] * 1000,
                               tcp_ms=(data["time_connect"] - data["time_namelookup"]) * 1000,
                               tls_ms=(data["time_appconnect"] - data["time_connect"]) * 1000,
                               setup_ms=data["time_appconnect"] * 1000)
                runs.append(row)
            result["hosts"][host] = {"runs": runs, "attempts": samples,
                "setup_ms": summary([r["setup_ms"] for r in runs if r["exit_code"] == 0], "ms"),
                "http_versions": sorted({r["http_version"] for r in runs if r["exit_code"] == 0}),
                "failures": sum(r["exit_code"] != 0 for r in runs)}
        if binary:
            result["reqwest"] = json.loads(subprocess.check_output(
                [str(Path(binary).resolve()), "net", "--hosts", ",".join(HOSTS), "--samples", str(samples)],
                env=env, timeout=600))
            for metric in result["reqwest"]["metrics"].values():
                if "samples" in metric:
                    metric.update(summary(metric["samples"], metric["unit"]))
    return result


def run(args):
    binaries = {name: Path(getattr(args, name)) for name in ("daemon", "cli")}
    for path in binaries.values():
        if not path.is_file() or not os.access(path, os.X_OK):
            raise FileNotFoundError(f"executable not found: {path}")
    cpu = next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")), None)
    result = {"schema_version": 2, "parameters": vars(args), "environment": {
        "recorded_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "kernel": platform.release(), "os_release": platform.freedesktop_os_release(),
        "architecture": platform.machine(), "cpu_model": cpu, "logical_cpus": os.cpu_count(),
        "load_average": list(os.getloadavg()), "python": platform.python_version(),
        "ram_total_kib": int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines()
                                  if line.startswith('MemTotal:'))),
        "cpu_governor": text_command(['cat', '/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor']),
        "rustc": text_command(["rustc", "--version"]), "git_commit": text_command(["git", "rev-parse", "HEAD"]),
        "git_status": text_command(["git", "status", "--porcelain"]),
        "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
        "binaries": {k: {"bytes": v.stat().st_size, "sha256": hashlib.sha256(v.read_bytes()).hexdigest()}
                     for k, v in binaries.items()}}, "build_profile": args.profile_label,
        "measurements": measure_daemon(args), "error": None,
        "unmeasured": {"hotkey_to_first_audio": "requires physical capture and shortcut trace",
            "provider_inference": "synthetic loopback only; no paid calls",
            "text_injection": "help/version only; no desktop writes", "overlay_frame_time": "no GNOME rendering"}}
    if args.bench:
        bench = Path(args.bench)
        result['environment']['benchmark_binary'] = {
            'bytes': bench.stat().st_size, 'sha256': hashlib.sha256(bench.read_bytes()).hexdigest()}
        result['environment']['benchmark_lock_sha256'] = hashlib.sha256(
            (ROOT / 'scripts/bench/rust/Cargo.lock').read_bytes()).hexdigest()
        result["microbenchmarks"] = suites(args.bench, args.samples)
    if args.network:
        result["network"] = network(args.network_samples, args.bench)
    if result["measurements"]["failures"]:
        result["error"] = "daemon measurements failed; inspect failures"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("daemon", "cli", "output"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--bench", help="optional xflow-bench executable")
    parser.add_argument("--idle-seconds", type=float, default=60)
    parser.add_argument("--warmup-seconds", type=float, default=5)
    parser.add_argument("--startup-samples", type=int, default=30)
    parser.add_argument("--samples", type=int, default=200)
    parser.add_argument("--profile-label", default="release: opt=3 lto=thin cgu=1 panic=unwind strip=true")
    parser.add_argument("--network", action="store_true")
    parser.add_argument("--network-samples", type=int, default=3)
    args = parser.parse_args()
    if (not math.isfinite(args.idle_seconds) or args.idle_seconds <= 0 or
            not math.isfinite(args.warmup_seconds) or args.warmup_seconds < 0 or
            min(args.samples, args.startup_samples, args.network_samples) < 1):
        parser.error("durations must be finite, idle positive, warmup nonnegative, counts positive")
    try:
        result = run(args)
    except Exception as exc:
        result = {"schema_version": 2, "parameters": vars(args), "error": f"{type(exc).__name__}: {exc}"}
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    if result["error"]:
        print(result["error"])
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
