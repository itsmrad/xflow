#!/usr/bin/env python3
"""xflow scoreboard: whole-process-tree footprint and latency, plus the Linux CI gate.

  run     reference run (macOS or Linux): binary sizes, cold/warm startup, idle tree
          RSS/footprint/CPU/wakeups, and with GROQ_API_KEY N live dictations through a
          test-support xflowd fed a WAV (never the microphone or speakers)
  gate    short Linux CI smoke: sizes, startup, idle PSS/CPU against a committed baseline
  render  JSON results -> docs/benchmarks/scoreboard.md and the PERFORMANCE.md block

Standard library only. Every daemon gets fresh XDG dirs, XFLOW_NO_KEYCHAIN=1, the
hotkey tap off (a fresh binary's tap can raise the Input Monitoring prompt) and no
notifications or sounds. See scripts/bench/README.md.
"""
import argparse
from contextlib import contextmanager
import difflib
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import queue
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import wave

import baseline

ROOT = baseline.ROOT
percentile = baseline.headless.percentile
MAC = sys.platform == "darwin"
MEMORY = "phys_footprint" if MAC else "pss"
MIB = 1024
BEGIN, END = "<!-- scoreboard:begin -->", "<!-- scoreboard:end -->"
RATE_LIMIT_RETRIES = 3
PHRASE = ("Deploy the API with TypeScript, then run the Postgres migration "
          "and open a pull request on GitHub.")
WISPR_APP = Path("/Applications/Wispr Flow.app")
# Third-party figures, never measured here. Shown beside, not as, our numbers.
WISPR_PUBLISHED = [
    {"figure": "748 MB idle, 989 MB recording, 11 processes (iMac M3, 8 GB)",
     "source": "tl;dv review (a competitor)", "url": "https://tldv.io/blog/wispr-flow-notetaker-review/",
     "date": "2026-09"},
    {"figure": "4 Electron helpers at 65/47/63/42 MiB + ~70 MB Swift helper; main process not reported",
     "source": "Wensen Wu, forensic investigation",
     "url": "https://www.wensenwu.com/thoughts/wispr-flow-investigation", "date": "2026-04"},
    {"figure": "\"<700 ms end-to-end\" P99 (vendor claim, method not published)",
     "source": "Baseten customer story", "url": "https://www.baseten.co/resources/customers/wispr-flow/",
     "date": "undated, retrieved 2026-10-29"},
    {"figure": "recommends 8 GB RAM, ~500 MB disk",
     "source": "Wispr system requirements",
     "url": "https://docs.wisprflow.ai/articles/1036674442-supported-devices-and-system-requirements",
     "date": "retrieved 2026-10-29"},
]


def summary(values, unit):
    return baseline.summary(values, unit) if values else {"unit": unit, "samples": [], "count": 0}


def config(provider_key=False):
    stt = ('[stt]\nprovider = "groq"\napi_key_env = "GROQ_API_KEY"\n' if provider_key else
           '[stt]\nprovider = "custom"\nmodel = "bench-model"\n'
           'endpoint = "http://127.0.0.1:9/v1/audio/transcriptions"\n')
    return (stt + '[hotkey]\nenabled = false\n[recording]\nmin_ms = 0\n'
            '[injection]\ntrailing_space = false\n[sounds]\nenabled = false\n'
            '[notifications]\nenabled = false\n')


@contextmanager
def sandbox(text):
    """Fresh private XDG root; on Linux also baseline's private D-Bus without activation."""
    if not MAC:
        with baseline.isolated() as (env, sock):
            root = Path(env["HOME"]).parent
            (root / "config/xflow/config.toml").write_text(text)
            env["XFLOW_NO_KEYCHAIN"] = "1"
            yield root, env, sock
        return
    # /private/tmp keeps the socket path under macOS's 104-byte limit and is canonical.
    root = Path(os.path.realpath(tempfile.mkdtemp(prefix="xfs", dir="/private/tmp")))
    try:
        for name in ("run", "config/xflow", "data", "cache", "home"):
            (root / name).mkdir(mode=0o700, parents=True)
        (root / "config/xflow/config.toml").write_text(text)
        env = {k: os.environ[k] for k in ("PATH", "LANG", "LC_ALL") if k in os.environ}
        env.update(HOME=str(root / "home"), XDG_RUNTIME_DIR=str(root / "run"),
                   XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                   XDG_CACHE_HOME=str(root / "cache"), TMPDIR=str(root), XFLOW_NO_KEYCHAIN="1",
                   DBUS_SESSION_BUS_ADDRESS=f"unix:path={root}/no-bus")
        yield root, env, root / "run/xflow/daemon.sock"
    finally:
        shutil.rmtree(root, ignore_errors=True)


# ---- process sampling -------------------------------------------------------

if MAC:
    import ctypes

    class _Rusage(ctypes.Structure):  # struct rusage_info_v4, <sys/resource.h>
        _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(n, ctypes.c_uint64) for n in (
            "user_time system_time pkg_idle_wkups interrupt_wkups pageins wired_size resident_size "
            "phys_footprint proc_start_abstime proc_exit_abstime child_user_time child_system_time "
            "child_pkg_idle_wkups child_interrupt_wkups child_pageins child_elapsed_abstime "
            "diskio_bytesread diskio_byteswritten qos_default qos_maintenance qos_background "
            "qos_utility qos_legacy qos_user_initiated qos_user_interactive billed_system_time "
            "serviced_system_time logical_writes lifetime_max_phys_footprint instructions cycles "
            "billed_energy serviced_energy interval_max_phys_footprint runnable_time").split()]

    class _Timebase(ctypes.Structure):
        _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]

    _libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
    _timebase = _Timebase()
    ctypes.CDLL(None).mach_timebase_info(ctypes.byref(_timebase))


def sample(pid):
    """One process: RSS, footprint (macOS) or PSS (Linux), peak, CPU ns, wakeups; None if gone."""
    try:
        if MAC:
            r = _Rusage()
            if _libproc.proc_pid_rusage(pid, 4, ctypes.byref(r)) != 0:
                return None
            return {"rss_kib": r.resident_size // 1024, "mem_kib": r.phys_footprint // 1024,
                    "peak_kib": r.lifetime_max_phys_footprint // 1024,
                    # Apple Silicon reports these in mach ticks, not nanoseconds.
                    "cpu_ns": (r.user_time + r.system_time) * _timebase.numer // _timebase.denom,
                    "wakeups": r.interrupt_wkups + r.pkg_idle_wkups}
        proc = Path(f"/proc/{pid}")
        status = dict(line.split(":", 1) for line in (proc / "status").read_text().splitlines())
        pss = next(int(line.split()[1]) for line in (proc / "smaps_rollup").read_text().splitlines()
                   if line.startswith("Pss:"))
        _, ticks, _ = baseline.headless.proc_stats(pid)
        # Linux has no wakeup counter; voluntary switches over all threads approximate one.
        threads = {task.name: int(line.split()[1]) for task in (proc / "task").iterdir()
                   for line in (task / "status").read_text().splitlines()
                   if line.startswith("voluntary_ctxt_switches:")}
        return {"rss_kib": int(status["VmRSS"].split()[0]), "mem_kib": pss,
                "peak_kib": int(status["VmHWM"].split()[0]),
                "cpu_ns": ticks * 10**9 // os.sysconf("SC_CLK_TCK"),
                "wakeups": sum(threads.values()), "threads": threads}
    except (OSError, StopIteration, KeyError, ValueError):
        return None


def children(pid):
    if MAC:
        pids = (ctypes.c_int * 1024)()
        count = _libproc.proc_listchildpids(pid, pids, ctypes.sizeof(pids))
        return list(pids[:max(0, count)])
    try:
        return [int(child) for task in Path(f"/proc/{pid}/task").iterdir()
                for child in (task / "children").read_text().split()]
    except OSError:
        return []


def descendants(root, children_of=children):
    """`root` and every descendant."""
    found, stack = [], [root]
    while stack:
        pid = stack.pop()
        if pid not in found:
            found.append(pid)
            stack.extend(children_of(pid))
    return found


def tree_total(samples):
    """Sum per-process samples (missing processes skipped) into one tree observation."""
    live = {pid: s for pid, s in samples.items() if s}
    total = {key: sum(s[key] for s in live.values()) for key in ("rss_kib", "mem_kib", "cpu_ns", "wakeups")}
    total["processes"] = len(live)
    return total


def wakeup_delta(before, after):
    """Wakeups between two per-process sample sets. Linux counts per thread, so a thread
    that exits in between drops out instead of making the total go backwards."""
    total = 0
    for pid, s in after.items():
        if not s:
            continue
        b = before.get(pid) or {}
        if "threads" in s:
            total += sum(n - b.get("threads", {}).get(tid, 0) for tid, n in s["threads"].items())
        else:
            total += s["wakeups"] - b.get("wakeups", 0)
    return total


def sample_tree(root, extra=()):
    pids = descendants(root) + [p for p in extra if p]
    samples = {pid: sample(pid) for pid in pids}
    return tree_total(samples), samples


# ---- daemon lifecycle --------------------------------------------------------

def stop(child):
    baseline.stop(child)


def launch(daemon, env, sock, timeout=15):
    """Spawn the daemon and poll status every 1 ms; returns (child, ms to first status)."""
    log = tempfile.TemporaryFile(mode="w+t")
    t0 = time.monotonic()
    child = subprocess.Popen([str(daemon)], env=env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=log)
    while True:
        if child.poll() is not None or time.monotonic() - t0 > timeout:
            stop(child)
            log.seek(0)
            raise RuntimeError("daemon startup failed: " + log.read(4096))
        try:
            baseline.headless.request_status(sock)
            return child, (time.monotonic() - t0) * 1000
        except OSError:
            time.sleep(.001)


def startup(daemon, samples):
    """Cold: a fresh copy of the binary (new inode, so macOS revalidates its signature)
    and fresh dirs. Warm: the same copy relaunched on its existing dirs. The OS file
    cache is never purged (that needs root), so cold is first-launch-after-install."""
    cold, warm = [], []
    for _ in range(samples):
        with sandbox(config()) as (root, env, sock):
            copy = root / "bin/xflowd"
            copy.parent.mkdir()
            shutil.copy2(daemon, copy)
            child, ms = launch(copy, env, sock)
            stop(child)
            cold.append(ms)
            child, ms = launch(copy, env, sock)
            stop(child)
            warm.append(ms)
    return {"cold_ms": summary(cold, "ms"), "warm_ms": summary(warm, "ms")}


def idle(daemon, seconds, warmup, launches=1):
    """Idle tree after `warmup`; 1 Hz samples. Per launch: median memory, CPU %, wakeups/s."""
    runs = []
    for _ in range(launches):
        with sandbox(config()) as (_, env, sock):
            child, _ = launch(daemon, env, sock)
            try:
                time.sleep(warmup)
                first, first_samples = sample_tree(child.pid)
                t0, observations = time.monotonic(), []
                while time.monotonic() - t0 < seconds:
                    time.sleep(min(1, max(0, seconds - (time.monotonic() - t0))))
                    last, last_samples = sample_tree(child.pid)
                    observations.append(last)
                elapsed = time.monotonic() - t0
                if child.poll() is not None:
                    raise RuntimeError("daemon exited during idle observation")
            finally:
                stop(child)
        runs.append({
            "seconds": elapsed, "warmup_seconds": warmup, "processes": last["processes"],
            "rss_kib": summary([o["rss_kib"] for o in observations], "KiB"),
            "mem_kib": summary([o["mem_kib"] for o in observations], "KiB"),
            "cpu_percent": (last["cpu_ns"] - first["cpu_ns"]) / 1e9 / elapsed * 100,
            "wakeups_per_second": wakeup_delta(first_samples, last_samples) / elapsed})
    return {"memory_metric": MEMORY, "runs": runs,
            "rss_kib": percentile([r["rss_kib"]["p50"] for r in runs], .5),
            "mem_kib": percentile([r["mem_kib"]["p50"] for r in runs], .5),
            "cpu_percent": max(r["cpu_percent"] for r in runs),
            "wakeups_per_second": percentile([r["wakeups_per_second"] for r in runs], .5),
            "wakeup_metric": ("interrupt + package-idle wakeups (proc_pid_rusage)" if MAC else
                              "voluntary context switches, all threads (no Linux wakeup counter)")}


# ---- live dictation ----------------------------------------------------------

def make_clip(directory):
    """`say` speech at 48 kHz mono (the built-in mic's format) plus 1 s of trailing silence."""
    for tool in ("say", "afconvert"):
        if not shutil.which(tool):
            raise FileNotFoundError(f"{tool} not found; pass --clip")
    aiff, speech, clip = directory / "phrase.aiff", directory / "speech.wav", directory / "clip.wav"
    subprocess.run(["say", "-r", "175", "-o", aiff, PHRASE], check=True, timeout=60)
    subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@48000", "-c", "1", aiff, speech],
                   check=True, timeout=60)
    with wave.open(str(speech)) as r, wave.open(str(clip), "wb") as w:
        w.setparams(r.getparams())
        w.writeframes(r.readframes(r.getnframes()) + b"\0" * (r.getframerate() * r.getsampwidth()))
    return clip


def wav_seconds(path):
    with wave.open(str(path)) as r:
        return r.getnframes() / r.getframerate()


def words(text):
    return re.findall(r"[a-z0-9]+", text.lower())


class Pill:
    """The real `xflow overlay-pill` helper fed the daemon's own event stream, exactly as
    xflowd does natively; test-support xflowd never spawns it, so the harness does."""

    def __init__(self, cli, config_path):
        self.cli, self.config, self.child = cli, config_path, None

    def forward(self, event):
        state = event.get("state")
        active = state in ("listening", "processing")
        if active and self.child is None:
            self.child = subprocess.Popen([str(self.cli), "--config", str(self.config), "overlay-pill"],
                                          stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                          stderr=subprocess.DEVNULL)
        if self.child and self.child.stdin:
            frame = {"state": state, "level": max(0.0, min(1.0, float(event.get("level") or 0)))}
            try:
                self.child.stdin.write(json.dumps(frame).encode() + b"\n")
                self.child.stdin.flush()
            except OSError:
                pass
        if not active and self.child:
            self.close()

    def close(self):
        child, self.child = self.child, None
        if child:
            try:
                child.stdin.close()
                child.wait(timeout=4)  # the daemon allows the exit animation up to 4 s
            except (OSError, subprocess.TimeoutExpired):
                stop(child)

    @property
    def pid(self):
        return self.child.pid if self.child else None


def sessions(daemon, cli, clip, runs, pill, rate_limit_wait=30):
    """`runs` live dictations on one daemon. Stop is sent 0.5 s into the trailing silence,
    like a key release; stop->text is client-observed, stop request -> delivery event."""
    hold = wav_seconds(clip) - 0.5
    with sandbox(config(provider_key=True)) as (root, env, sock):
        shutil.copy(clip, root / "input.wav")
        env.update(GROQ_API_KEY=os.environ["GROQ_API_KEY"], XFLOW_TEST_ROOT=str(root),
                   XFLOW_TEST_AUDIO="wav", XFLOW_TEST_SINK=str(root / "delivered.txt"))
        child, _ = launch(daemon, env, sock)
        helper = Pill(cli, root / "config/xflow/config.toml") if pill else None
        peaks, done, results, rate_limited = {"tree_rss_kib": 0, "tree_mem_kib": 0}, threading.Event(), [], 0
        lifetime = {}

        def watch():  # 20 Hz: tree peak, and each process's own lifetime peak
            while not done.wait(.05):
                total, samples = sample_tree(child.pid, [helper and helper.pid])
                peaks["tree_rss_kib"] = max(peaks["tree_rss_kib"], total["rss_kib"])
                peaks["tree_mem_kib"] = max(peaks["tree_mem_kib"], total["mem_kib"])
                for pid, s in samples.items():
                    if s:
                        name = "daemon" if pid == child.pid else "pill" if helper and pid == helper.pid else "other"
                        lifetime[name] = max(lifetime.get(name, 0), s["peak_kib"])

        watcher = threading.Thread(target=watch, daemon=True)
        watcher.start()
        try:
            with socket.socket(socket.AF_UNIX) as sub:
                sub.connect(str(sock))
                sub.sendall(b'{"command":"subscribe"}\n')
                events = queue.Queue()
                threading.Thread(target=pump, args=(sub.makefile(), events), daemon=True).start()
                events.get(timeout=10)  # snapshot
                # `runs` successful samples, at most 2 * runs attempts; every failure is kept.
                while sum(r["state"] == "success" for r in results) < runs and len(results) < 2 * runs:
                    result, limited = redo_rate_limited(
                        lambda: dictate(sock, events, hold, root, helper), rate_limit_wait)
                    rate_limited += limited
                    results.append(result)
                    print(f"attempt {len(results)}: {result['state']} {result.get('stop_to_text_ms')} ms "
                          f"({rate_limited} rate-limited so far)", file=sys.stderr)
        finally:
            done.set()
            watcher.join()
            if helper:
                helper.close()
            stop(child)
    ok = [r for r in results if r["state"] == "success"]
    reference = words(PHRASE)
    return {
        "runs": results, "count": len(results), "failures": len(results) - len(ok),
        "rate_limited_attempts": rate_limited, "rate_limit_wait_seconds": rate_limit_wait,
        "failure_reasons": {m: sum(r.get("message") == m for r in results if r["state"] != "success")
                            for m in {r.get("message") for r in results if r["state"] != "success"}},
        "clip_seconds": wav_seconds(clip), "stop_after_seconds": hold,
        "stop_to_text_ms": summary([r["stop_to_text_ms"] for r in ok], "ms"),
        "daemon_total_ms": summary([r["timings"]["total_ms"] for r in ok if r.get("timings")], "ms"),
        "stt_ms": summary([r["timings"]["stt_ms"] for r in ok if r.get("timings")], "ms"),
        "start_to_capture_ms": summary([r["timings"]["hotkey_ms"] for r in ok
                                        if (r.get("timings") or {}).get("hotkey_ms") is not None], "ms"),
        "word_similarity": summary([round(difflib.SequenceMatcher(None, reference, words(r["text"]),
                                                                  autojunk=False).ratio(), 4) for r in ok], "ratio"),
        "peak": {**peaks, "lifetime_kib": lifetime, "memory_metric": MEMORY,
                 "pill": "harness-driven real overlay-pill" if pill else "none on this platform"},
    }


def redo_rate_limited(attempt, wait, retries=RATE_LIMIT_RETRIES):
    """HTTP 429 is the shared dev key's quota, not latency: wait and redo, at most `retries`
    times. Returns the last result and how many attempts were rate-limited."""
    for limited in range(retries + 1):
        result = attempt()
        if "HTTP 429" not in (result.get("message") or "") or limited == retries:
            return result, limited + ("HTTP 429" in (result.get("message") or ""))
        time.sleep(wait)


def pump(lines, events):
    """Subscription lines -> queue; None at end of stream."""
    for line in lines:
        events.put(json.loads(line))
    events.put(None)


def dictate(sock, events, hold, root, helper):
    sink = root / "delivered.txt"
    sink.unlink(missing_ok=True)
    t0_us = time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000  # the daemon's clock
    reply = request(sock, {"command": "start", "t0_us": t0_us})
    if not reply.get("ok"):
        return {"state": "start_failed", "message": reply.get("message")}
    started, stopped = time.monotonic(), None
    while time.monotonic() - started < hold + 60:
        if stopped is None and time.monotonic() - started >= hold:
            stopped = time.monotonic()
            # Like a key release; the reply may wait for the result, so don't block on it.
            threading.Thread(target=request, args=(sock, {"command": "stop"}), daemon=True).start()
        wait = max(.001, started + hold - time.monotonic()) if stopped is None else 1
        try:
            event = events.get(timeout=wait)
        except queue.Empty:
            continue
        if event is None:
            raise RuntimeError("daemon closed the event stream")
        received = time.monotonic()  # before the pill, whose exit can take seconds
        if helper:
            helper.forward(event)
        if stopped and event["state"] in ("success", "error", "idle"):
            return {"state": event["state"], "message": event.get("message"),
                    "stop_to_text_ms": round((received - stopped) * 1000, 1),
                    # Error events repeat the previous session's timings; keep only real ones.
                    "timings": event.get("timings") if event["state"] == "success" else None,
                    "text": sink.read_text() if sink.exists() else ""}
    return {"state": "timeout"}


def request(sock, payload):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(10)
        s.connect(str(sock))
        s.sendall(json.dumps(payload).encode() + b"\n")
        return json.loads(s.makefile().readline())


# ---- environment, Wispr ------------------------------------------------------

def text_command(command):
    return baseline.text_command(command)


def environment(binaries):
    if MAC:
        hw = {"model": text_command(["sysctl", "-n", "hw.model"]),
              "cpu": text_command(["sysctl", "-n", "machdep.cpu.brand_string"]),
              "ram_bytes": int(text_command(["sysctl", "-n", "hw.memsize"]) or 0),
              "os": f"macOS {platform.mac_ver()[0]}"}
    else:
        os_release = platform.freedesktop_os_release()
        hw = {"model": None, "cpu": next((line.split(":", 1)[1].strip() for line in
                                          Path("/proc/cpuinfo").read_text().splitlines()
                                          if line.startswith("model name")), None),
              "ram_bytes": int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines()
                                    if line.startswith("MemTotal:"))) * 1024,
              "os": os_release.get("PRETTY_NAME")}
    return {**hw, "kernel": platform.release(), "architecture": platform.machine(),
            "logical_cpus": os.cpu_count(), "load_average": list(os.getloadavg()),
            "recorded_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "git_commit": text_command(["git", "rev-parse", "HEAD"]),
            "git_dirty": bool(text_command(["git", "status", "--porcelain", "--", "apps/cli"])),
            "rustc": text_command(["rustc", "--version"]), "python": platform.python_version(),
            "binaries": {name: {"bytes": Path(path).stat().st_size,
                                "sha256": hashlib.sha256(Path(path).read_bytes()).hexdigest()}
                         for name, path in binaries.items()}}


def wispr(pairs_with_command=None):
    """Sum every running Wispr Flow process; never launches it or reads its data."""
    version = None
    try:
        with open(WISPR_APP / "Contents/Info.plist", "rb") as f:
            version = plistlib.load(f).get("CFBundleShortVersionString")
    except OSError:
        pass
    if pairs_with_command is None:
        out = subprocess.run(["ps", "-A", "-o", "pid=,comm="], capture_output=True, text=True, timeout=5).stdout
        pairs_with_command = [(int(line.split(None, 1)[0]), line.split(None, 1)[1])
                              for line in out.splitlines() if len(line.split(None, 1)) == 2]
    pids = [pid for pid, command in pairs_with_command if str(WISPR_APP) + "/" in command]
    if not pids:
        return {"version": version, "measured": False,
                "reason": "not installed" if version is None else "installed but not running (never launched here)"}
    total = tree_total({pid: sample(pid) for pid in pids})
    return {"version": version, "measured": True, "memory_metric": MEMORY, **total}


# ---- CI gate -----------------------------------------------------------------

def check(result, base, limits):
    """Pass/fail rows. Memory may grow by at most min(limit_kib, limit_pct) over the baseline."""
    rows = []

    def row(name, value, limit, unit):
        rows.append({"metric": name, "value": value, "limit": limit, "unit": unit, "ok": value <= limit})

    for name, size in result["binary_bytes"].items():
        row(f"{name} bytes", size, int(base["binary_bytes"][name] * (1 + limits["binary_pct"] / 100)), "B")
    pss = base["idle_pss_kib"]
    row("idle tree PSS", result["idle_pss_kib"],
        pss + min(limits["pss_kib"], int(pss * limits["pss_pct"] / 100)), "KiB")
    row("idle tree CPU", round(result["idle_cpu_percent"], 4), limits["cpu_percent"], "%")
    row("startup p50", round(result["startup_p50_ms"], 2), limits["startup_ms"], "ms")
    row("idle processes", result["idle_processes"], 1, "")
    return rows


def gate(args):
    binaries = {"xflowd": Path(args.daemon), "xflow": Path(args.cli)}
    measured_startup = startup(args.daemon, args.startup_samples)
    measured_idle = idle(args.daemon, args.idle_seconds, args.warmup_seconds, args.launches)
    result = {"binary_bytes": {k: v.stat().st_size for k, v in binaries.items()},
              "idle_pss_kib": measured_idle["mem_kib"], "idle_rss_kib": measured_idle["rss_kib"],
              "idle_cpu_percent": measured_idle["cpu_percent"],
              "idle_processes": max(r["processes"] for r in measured_idle["runs"]),
              "startup_p50_ms": measured_startup["warm_ms"]["p50"]}
    out = {"schema": "xflow-scoreboard-gate/1", "environment": environment(binaries), "result": result,
           "startup": measured_startup, "idle": measured_idle}
    base = json.loads(Path(args.baseline).read_text()) if Path(args.baseline).exists() else None
    if base:
        out["baseline"], out["checks"] = base, check(result, base["result"], base["limits"])
    write_json(args.output, out)
    table = "| Check | Value | Limit | |\n| --- | ---: | ---: | --- |\n" + "".join(
        f"| {c['metric']} | {c['value']:,} {c['unit']} | {c['limit']:,} {c['unit']} | {'pass' if c['ok'] else 'FAIL'} |\n"
        for c in out.get("checks", []))
    print(table if base else json.dumps(result, indent=2))
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as f:
            f.write("### Scoreboard gate\n\n" + (table if base else "No baseline: record only.\n"))
    if not base:
        print(f"error: baseline {args.baseline} missing; commit one from this result", file=sys.stderr)
        return 1
    return 0 if all(c["ok"] for c in out["checks"]) else 1


# ---- rendering ---------------------------------------------------------------

def mib(kib):
    return "—" if kib is None else f"{kib / MIB:.1f} MiB"


def ms(value):
    return "—" if value is None else "< 1 ms" if value == 0 else f"{value:,.0f} ms" if value >= 100 else f"{value:.1f} ms"


def render(data):
    env, m = data["environment"], data["measurements"]
    s, i = m.get("sessions"), m["idle"]
    mem = "footprint" if i["memory_metric"] == "phys_footprint" else "PSS"
    lines = [
        f"Measured {env['recorded_utc'][:10]} at `{(env['git_commit'] or '')[:7]}`"
        f"{' (dirty)' if env['git_dirty'] else ''} on {env['model'] + ', ' if env['model'] else ''}{env['cpu']}, "
        f"{env['ram_bytes'] / 2**30:.0f} GB, {env['os']}, {env['rustc']}, load average "
        f"{env['load_average'][0]:.1f} at start. "
        "Generated by `scripts/bench/scoreboard.py`; do not edit by hand.", "",
        "| Metric | xflow | How |", "| --- | ---: | --- |",
        f"| Idle tree {mem} | {mib(i['mem_kib'])} | median of 1 Hz samples, {i['runs'][0]['seconds']:.0f} s after "
        f"{i['runs'][0]['warmup_seconds']:.0f} s warmup; {i['runs'][0]['processes']} process(es) |",
        f"| Idle tree RSS | {mib(i['rss_kib'])} | same samples |",
        f"| Idle CPU | {i['cpu_percent']:.4f} % | CPU time / wall time, 100 % = one core |",
        f"| Idle wakeups | {i['wakeups_per_second']:.2f} /s | {i['wakeup_metric']} |",
        f"| Cold start → status p50 / p95 | {ms(m['startup']['cold_ms']['p50'])} / {ms(m['startup']['cold_ms']['p95'])}"
        f" | {m['startup']['cold_ms']['count']} fresh binary copies and dirs; file cache not purged |",
        f"| Warm start → status p50 / p95 | {ms(m['startup']['warm_ms']['p50'])} / {ms(m['startup']['warm_ms']['p95'])}"
        f" | {m['startup']['warm_ms']['count']} relaunches |",
    ]
    for name, b in env["binaries"].items():
        if name != "session_daemon":
            lines.append(f"| `{name}` binary | {b['bytes'] / 2**20:.2f} MiB | release, stripped |")
    if s and s.get("count"):
        p = s["peak"]
        lines += [
            f"| Stop → text p50 / p95 | {ms(s['stop_to_text_ms']['p50'])} / {ms(s['stop_to_text_ms']['p95'])} | "
            f"{s['stop_to_text_ms']['count']} successful live Groq dictations of {s['count']} attempts "
            f"({s['failures']} failed, plus {s.get('rate_limited_attempts', 0)} HTTP 429s redone), "
            f"{s['clip_seconds']:.1f} s WAV, stop request → text in a file sink (real paste excluded) |",
            f"| Start → capture running p50 / p95 | {ms(s['start_to_capture_ms'].get('p50'))} / "
            f"{ms(s['start_to_capture_ms'].get('p95'))} | daemon `hotkey_ms`; WAV source, so real mic open is excluded |",
            f"| Peak tree {mem} while dictating | {mib(p['tree_mem_kib'])} | 20 Hz, daemon + pill |",
            f"| Peak tree RSS while dictating | {mib(p['tree_rss_kib'])} | same |",
            f"| Transcript word similarity p50 | {s['word_similarity'].get('p50')} | vs the spoken phrase |",
        ]
    w = m.get("wispr") or {}
    lines += ["", f"**Wispr Flow** ({'version ' + w['version'] if w.get('version') else 'not installed'}): "
              + (f"measured on this Mac, {w['processes']} processes, {mib(w['mem_kib'])} {mem}, {mib(w['rss_kib'])} RSS."
                 if w.get("measured") else f"not measured: {w.get('reason', 'not sampled')}. "
                 "Published third-party figures, not comparable measurements:"), ""]
    if not w.get("measured"):
        lines += [f"- {p['figure']} — [{p['source']}]({p['url']}), {p['date']}" for p in WISPR_PUBLISHED]
    return "\n".join(lines) + "\n"


def replace_block(text, block):
    if BEGIN not in text or END not in text:
        raise ValueError(f"missing {BEGIN} / {END} markers")
    head, rest = text.split(BEGIN, 1)
    return head + BEGIN + "\n" + block + END + rest.split(END, 1)[1]


def write_json(path, data):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")


# ---- commands ----------------------------------------------------------------

def run(args):
    binaries = {"xflowd": args.daemon, "xflow": args.cli}
    if args.session_daemon:
        binaries["session_daemon"] = args.session_daemon
    data = {"schema": "xflow-scoreboard/1",
            "parameters": {k: os.path.relpath(v, ROOT) if isinstance(v, Path) else v
                           for k, v in vars(args).items() if k != "func"},
            "environment": environment(binaries), "measurements": {}}
    m = data["measurements"]
    m["startup"] = startup(args.daemon, args.startup_samples)
    m["idle"] = idle(args.daemon, args.idle_seconds, args.warmup_seconds)
    m["wispr"] = wispr()
    if args.runs and not os.environ.get("GROQ_API_KEY"):
        m["sessions"] = {"count": 0, "skipped": "GROQ_API_KEY not set"}
    elif args.runs and not args.session_daemon:
        m["sessions"] = {"count": 0, "skipped": "--session-daemon (a test-support xflowd) not given"}
    elif args.runs:
        with tempfile.TemporaryDirectory(prefix="xfclip") as directory:
            clip = Path(args.clip) if args.clip else make_clip(Path(directory))
            m["sessions"] = sessions(args.session_daemon, args.cli, clip, args.runs, MAC)
    data["environment"]["load_average_end"] = list(os.getloadavg())
    write_json(args.output, data)
    markdown = Path(args.output).with_suffix(".md")
    markdown.write_text(render(data))
    print(f"wrote {args.output} and {markdown}")
    return 0


def render_command(args):
    block = render(json.loads(Path(args.result).read_text()))
    for target in args.into:
        path = Path(target)
        path.write_text(replace_block(path.read_text(), block))
        print(f"updated {path}")
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(required=True)
    for name, func in (("run", run), ("gate", gate)):
        sub = commands.add_parser(name)
        sub.set_defaults(func=func)
        sub.add_argument("--daemon", default="apps/cli/target/release/xflowd")
        sub.add_argument("--cli", default="apps/cli/target/release/xflow")
        sub.add_argument("--output", required=True)
        sub.add_argument("--warmup-seconds", type=float, default=5)
    run_args, gate_args = commands.choices["run"], commands.choices["gate"]
    run_args.add_argument("--session-daemon", help="test-support xflowd for live dictations")
    run_args.add_argument("--runs", type=int, default=30, help="successful dictations wanted")
    run_args.add_argument("--clip", help="WAV to dictate (default: generated with macOS `say`)")
    run_args.add_argument("--idle-seconds", type=float, default=300)
    run_args.add_argument("--startup-samples", type=int, default=30)
    gate_args.add_argument("--baseline", default="docs/benchmarks/ci-baseline.json")
    gate_args.add_argument("--idle-seconds", type=float, default=30)
    gate_args.add_argument("--startup-samples", type=int, default=20)
    gate_args.add_argument("--launches", type=int, default=3)
    sub = commands.add_parser("render")
    sub.set_defaults(func=render_command)
    sub.add_argument("result")
    sub.add_argument("--into", nargs="+", default=["PERFORMANCE.md"])
    args = parser.parse_args(argv)
    for name in ("daemon", "cli", "session_daemon"):
        path = getattr(args, name, None)
        if path and not os.access(path, os.X_OK):
            parser.error(f"--{name.replace('_', '-')}: not an executable: {path}")
        if path:
            setattr(args, name, Path(path).resolve())
    if min(getattr(args, "idle_seconds", 1), getattr(args, "startup_samples", 1), getattr(args, "launches", 1)) <= 0 \
            or getattr(args, "runs", 0) < 0 or getattr(args, "warmup_seconds", 0) < 0:
        parser.error("durations and counts must be positive")
    return args.func(args)


if __name__ == "__main__":
    os.chdir(ROOT)
    raise SystemExit(main())
