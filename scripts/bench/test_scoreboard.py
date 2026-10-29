#!/usr/bin/env python3
"""Scoreboard aggregation, gate and rendering checks with fake samples; no daemon."""
import json
import os
from pathlib import Path
import subprocess
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
        tree = {1: [10, 20], 10: [11, 13], 11: [12], 20: [21]}
        self.assertEqual(sorted(scoreboard.descendants(10, lambda p: tree.get(p, []))), [10, 11, 12, 13])
        self.assertEqual(scoreboard.descendants(99, lambda p: tree.get(p, [])), [99])

    def test_children_of_this_process(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(5)"])
        try:
            self.assertIn(child.pid, scoreboard.descendants(os.getpid()))
        finally:
            child.kill()
            child.wait()

    def test_tree_total_sums_live_processes(self):
        a = {"rss_kib": 100, "mem_kib": 60, "cpu_ns": 5, "wakeups": 2, "peak_kib": 70}
        b = {"rss_kib": 40, "mem_kib": 30, "cpu_ns": 1, "wakeups": 1, "peak_kib": 35}
        self.assertEqual(scoreboard.tree_total({1: a, 2: b, 3: None}),
                         {"rss_kib": 140, "mem_kib": 90, "cpu_ns": 6, "wakeups": 3, "processes": 2})

    def test_wakeups_never_go_backwards_when_a_thread_exits(self):
        before = {1: {"wakeups": 15, "threads": {"1": 10, "2": 5}}, 2: {"wakeups": 7}}
        after = {1: {"wakeups": 12, "threads": {"1": 12}}, 2: {"wakeups": 9}, 3: {"wakeups": 4}, 4: None}
        self.assertEqual(scoreboard.wakeup_delta(before, after), 2 + 2 + 4)

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
                       "model": None, "cpu": "Fake CPU", "load_average": [1.0, 1.0, 1.0], "ram_bytes": 8 * 2**30, "os": "FakeOS", "rustc": "rustc 1",
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
            code = scoreboard.run(args)
            return json.loads(output.read_text()), output.with_suffix(".md").read_text(), code

    def test_sessions_are_skipped_without_a_key(self):
        data, markdown, code = self.fake_run({})
        self.assertEqual(code, 0)
        self.assertEqual(data["measurements"]["sessions"], {"count": 0, "skipped": "GROQ_API_KEY not set"})
        self.assertNotIn("Stop → text", markdown)
        self.assertIn("| Idle tree PSS | 5.9 MiB |", markdown)
        self.assertIn("not measured: installed but not running", markdown)
        self.assertIn("https://tldv.io/", markdown)

    def test_rate_limits_are_redone_a_bounded_number_of_times(self):
        limited, ok = {"state": "error", "message": "provider returned HTTP 429"}, {"state": "success"}
        replies = iter([limited, limited, ok])
        self.assertEqual(scoreboard.redo_rate_limited(lambda: next(replies), 0), (ok, 2))
        self.assertEqual(scoreboard.redo_rate_limited(lambda: limited, 0, retries=3), (limited, 4))
        other = {"state": "error", "message": "provider returned HTTP 500"}
        self.assertEqual(scoreboard.redo_rate_limited(lambda: other, 0), (other, 0))

    def no_successes(self, reply):
        results, limited = scoreboard.collect(lambda: dict(reply), 30, 0)
        self.assertEqual(len(results), scoreboard.GIVE_UP_AFTER)  # gives up instead of 60 attempts
        sessions = scoreboard.summarize(results, limited, 0, 7.0, 6.5, {"tree_rss_kib": 0, "tree_mem_kib": 0})
        with patch.object(scoreboard, "sessions", return_value=sessions), \
                patch.object(scoreboard, "make_clip", return_value=Path("clip.wav")):
            data, markdown, code = self.fake_run({"GROQ_API_KEY": "bad"})
        self.assertEqual(code, 1)
        s = data["measurements"]["sessions"]
        self.assertEqual((s["successes"], s["failures"], s["error"]), (0, 5, "no successful dictations"))
        self.assertIn("**no successful dictations**", markdown)
        return s, markdown

    def test_bad_key_reports_no_successes_without_crashing(self):
        message = "provider returned HTTP 401; response body omitted"
        s, markdown = self.no_successes({"state": "error", "message": message})
        self.assertEqual(s["failure_reasons"], {message: 5})
        self.assertIn(f"{message} ×5", markdown)

    def test_silent_clip_reports_no_successes_without_crashing(self):
        s, markdown = self.no_successes({"state": "idle", "message": "Didn't catch anything"})
        self.assertEqual(s["failure_reasons"], {"Didn't catch anything": 5})

    def test_early_failures_do_not_give_up_once_one_succeeds(self):
        replies = iter([{"state": "error", "message": "timeout"}] * 4 + [{"state": "success"}] * 5)
        results, _ = scoreboard.collect(lambda: next(replies), 5, 0)
        self.assertEqual([r["state"] for r in results].count("success"), 5)
        self.assertEqual(len(results), 9)

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

    def test_generated_docs_match_their_data(self):
        """Every number in these docs comes from the JSON beside it; editing either alone fails."""
        root = scoreboard.ROOT
        for data, docs in (("docs/benchmarks/scoreboard-macos.json", ["PERFORMANCE.md"]),
                           ("docs/benchmarks/ci-baseline.json", ["scripts/bench/README.md"])):
            block = scoreboard.render(json.loads((root / data).read_text()))
            self.assertEqual((root / data).with_suffix(".md").read_text(), block, data)
            for doc in docs:
                text = (root / doc).read_text()
                self.assertEqual(scoreboard.replace_block(text, block), text, f"{doc} drifted from {data}")


class Baseline(unittest.TestCase):
    @staticmethod
    def gate_artifact(pss, launches, run):
        return {"environment": {"ci_run": f"https://ci/{run}", "git_commit": "abcdef0123", "rustc": "rustc 1",
                                "cpu": "Fake CPU", "logical_cpus": 4},
                "result": dict(result(idle_pss_kib=pss), startup_p50_ms=2.0),
                "idle": {"runs": [{"mem_kib": {"p50": p}} for p in launches]}}

    def test_median_run_becomes_the_reference_and_all_runs_are_noise(self):
        gates = [self.gate_artifact(9500, [9400, 9500, 9600], 1), self.gate_artifact(9000, [8900, 9000, 9100], 2),
                 self.gate_artifact(9200, [9200, 9200, 9300], 3)]
        base = scoreboard.make_baseline(gates, LIMITS)
        self.assertEqual(base["result"]["idle_pss_kib"], 9200)
        self.assertEqual([r["ci_run"] for r in base["runs"]], ["https://ci/1", "https://ci/2", "https://ci/3"])
        text = scoreboard.render(base)
        self.assertIn("run medians 9,000–9,500 KiB (6 %)", text)
        self.assertIn("per-launch medians 8,900–9,600 KiB (8 %)", text)
        self.assertIn("allows +920 KiB, 1.3× the per-launch spread", text)
        self.assertIn("`xflowd` 5,000,000 B in every run", text)


if __name__ == "__main__":
    unittest.main()
