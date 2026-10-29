#!/usr/bin/env python3
"""Scoreboard aggregation, gate and rendering checks with fake samples; no daemon."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import scoreboard

LIMITS = {"binary_pct": 10, "pss_kib": 1024, "pss_pct": 10, "cpu_percent": 0.1, "startup_ms": 100}
BASE = {"binary_bytes": {"xflowd": 5_000_000, "xflow": 6_000_000}, "idle_pss_kib": 6000}


def result(**changes):
    value = {"binary_bytes": dict(BASE["binary_bytes"]), "idle_pss_kib": 6000, "idle_cpu_percent": 0.0,
             "startup_p50_ms": 4.0, "idle_processes": 1}
    value.update(changes)
    return value


def failed(rows):
    return [r["metric"] for r in rows if not r["ok"]]


class Aggregation(unittest.TestCase):
    def test_descendants_follow_the_tree_only(self):
        pairs = [(10, 1), (11, 10), (12, 11), (13, 10), (20, 1), (21, 20)]
        self.assertEqual(sorted(scoreboard.descendants(10, pairs)), [10, 11, 12, 13])
        self.assertEqual(scoreboard.descendants(99, pairs), [99])

    def test_tree_total_sums_live_processes(self):
        a = {"rss_kib": 100, "mem_kib": 60, "cpu_ns": 5, "wakeups": 2, "peak_kib": 70}
        b = {"rss_kib": 40, "mem_kib": 30, "cpu_ns": 1, "wakeups": 1, "peak_kib": 35}
        self.assertEqual(scoreboard.tree_total({1: a, 2: b, 3: None}),
                         {"rss_kib": 140, "mem_kib": 90, "cpu_ns": 6, "wakeups": 3, "processes": 2})

    def test_percentiles_are_nearest_rank(self):
        s = scoreboard.summary(list(range(30, 0, -1)), "ms")
        self.assertEqual((s["p50"], s["p95"], s["min"], s["max"], s["count"]), (16, 29, 1, 30, 30))
        self.assertEqual(scoreboard.summary([], "ms")["count"], 0)

    def test_sample_reads_this_process_and_misses_a_dead_one(self):
        s = scoreboard.sample(os.getpid())
        self.assertGreater(s["rss_kib"], 1000)
        self.assertGreater(s["mem_kib"], 0)
        self.assertGreaterEqual(s["peak_kib"], s["mem_kib"] if sys.platform == "darwin" else s["rss_kib"])
        self.assertGreater(s["cpu_ns"], 0)
        self.assertIsNone(scoreboard.sample(2**22 + 1))


class Gate(unittest.TestCase):
    def test_unchanged_build_passes(self):
        self.assertEqual(failed(scoreboard.check(result(), BASE, LIMITS)), [])

    def test_pss_fails_on_ten_percent_of_a_small_baseline(self):
        self.assertEqual(failed(scoreboard.check(result(idle_pss_kib=6600), BASE, LIMITS)), [])
        self.assertEqual(failed(scoreboard.check(result(idle_pss_kib=6601), BASE, LIMITS)), ["idle tree PSS"])

    def test_pss_fails_on_one_mib_of_a_large_baseline(self):
        base = dict(BASE, idle_pss_kib=20_000)
        self.assertEqual(failed(scoreboard.check(result(idle_pss_kib=21_024), base, LIMITS)), [])
        self.assertEqual(failed(scoreboard.check(result(idle_pss_kib=21_025), base, LIMITS)), ["idle tree PSS"])

    def test_binary_cpu_startup_and_idle_helpers_fail(self):
        rows = scoreboard.check(result(binary_bytes={"xflowd": 5_500_001, "xflow": 6_000_000},
                                       idle_cpu_percent=0.2, startup_p50_ms=101, idle_processes=2), BASE, LIMITS)
        self.assertEqual(failed(rows), ["xflowd bytes", "idle tree CPU", "startup p50", "idle processes"])

    def test_committed_baseline_is_well_formed(self):
        base = json.loads((scoreboard.ROOT / "docs/benchmarks/ci-baseline.json").read_text())
        self.assertEqual(failed(scoreboard.check(base["result"], base["result"], base["limits"])), [])


class Run(unittest.TestCase):
    def fake_run(self, env):
        idle = {"memory_metric": "pss", "runs": [{"seconds": 30.0, "warmup_seconds": 5, "processes": 1}],
                "rss_kib": 9000, "mem_kib": 6000, "cpu_percent": 0.0, "wakeups_per_second": 0.1,
                "wakeup_metric": "fake"}
        startup = {"cold_ms": scoreboard.summary([3.0, 4.0], "ms"), "warm_ms": scoreboard.summary([2.0], "ms")}
        environment = {"recorded_utc": "2026-10-29T00:00:00Z", "git_commit": "abcdef0123", "git_dirty": False,
                       "model": None, "cpu": "Fake CPU", "ram_bytes": 8 * 2**30, "os": "FakeOS", "rustc": "rustc 1",
                       "binaries": {"xflowd": {"bytes": 5 * 2**20}, "xflow": {"bytes": 6 * 2**20}}}
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, env, clear=True), \
                patch.object(scoreboard, "startup", return_value=startup), \
                patch.object(scoreboard, "idle", return_value=idle), \
                patch.object(scoreboard, "environment", return_value=environment), \
                patch.object(scoreboard, "wispr", return_value={"version": "1.0", "measured": False,
                                                                "reason": "installed but not running"}):
            output = Path(temp) / "out.json"
            args = scoreboard.argparse.Namespace(daemon="d", cli="c", session_daemon="s", runs=30, clip=None,
                                                 startup_samples=2, idle_seconds=30, warmup_seconds=5,
                                                 output=str(output))
            scoreboard.run(args)
            return json.loads(output.read_text()), output.with_suffix(".md").read_text()

    def test_sessions_are_skipped_without_a_key(self):
        data, markdown = self.fake_run({})
        self.assertEqual(data["measurements"]["sessions"], {"count": 0, "skipped": "GROQ_API_KEY not set"})
        self.assertNotIn("Stop → text", markdown)
        self.assertIn("| Idle tree PSS | 5.9 MiB |", markdown)
        self.assertIn("not measured: installed but not running", markdown)
        self.assertIn("https://tldv.io/", markdown)

    def test_missing_say_is_reported(self):
        with patch.object(scoreboard.shutil, "which", return_value=None), \
                self.assertRaisesRegex(FileNotFoundError, "say not found"):
            scoreboard.make_clip(Path("."))

    def test_wispr_absent_and_present(self):
        with patch.object(scoreboard, "WISPR_APP", Path("/nonexistent/Wispr Flow.app")):
            self.assertEqual(scoreboard.wispr([])["reason"], "not installed")
            me = [(os.getpid(), "/nonexistent/Wispr Flow.app/Contents/MacOS/Wispr Flow"), (1, "/sbin/launchd")]
            measured = scoreboard.wispr(me)
        self.assertTrue(measured["measured"])
        self.assertEqual(measured["processes"], 1)

    def test_render_replaces_only_the_marked_block(self):
        text = f"intro\n{scoreboard.BEGIN}\nold\n{scoreboard.END}\noutro\n"
        once = scoreboard.replace_block(text, "new\n")
        self.assertEqual(once, f"intro\n{scoreboard.BEGIN}\nnew\n{scoreboard.END}\noutro\n")
        self.assertEqual(scoreboard.replace_block(once, "new\n"), once)
        with self.assertRaises(ValueError):
            scoreboard.replace_block("no markers", "new\n")

    def test_performance_md_has_the_block(self):
        text = (scoreboard.ROOT / "PERFORMANCE.md").read_text()
        self.assertIn(scoreboard.BEGIN, text)
        self.assertIn(scoreboard.END, text)


if __name__ == "__main__":
    unittest.main()
