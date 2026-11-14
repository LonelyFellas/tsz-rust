import copy
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from ci_run_summary import WALL_CLOCK_BUDGET_SECONDS, budget, summarize, markdown


class SummaryTests(unittest.TestCase):
    def setUp(self):
        self.run = dict(id=10, run_attempt=2, head_sha="abc", event="push", run_started_at="2026-11-03T00:00:00Z")
        self.job = dict(id=1, run_id=10, run_attempt=2, name="Integration (a)", status="completed", conclusion="success",
                        created_at="2026-11-03T00:00:00Z", started_at="2026-11-03T00:00:10Z", completed_at="2026-11-03T00:02:10Z",
                        steps=[dict(name="Compile a integration tests", conclusion="success", started_at="2026-11-03T00:00:20Z", completed_at="2026-11-03T00:01:20Z")])

    def test_parallel_wall_is_not_sum_and_failed_jobs_are_counted(self):
        second = dict(self.job, id=2, name="Integration (b)", conclusion="failure", completed_at="2026-11-03T00:03:10Z")
        report = summarize(self.run, [self.job, second])
        self.assertEqual(report["measured_latency_seconds"], 190)
        self.assertEqual(report["completed_runner_minutes"], 5)
        self.assertEqual(report["longest_job"], "Integration (b)")
        self.assertIn("failure", markdown(report))
        self.assertEqual(report["jobs"][0]["phases_seconds"]["compile_lint"], 60)

    def test_skipped_negative_timestamps_and_current_summary_are_excluded(self):
        skipped = dict(self.job, id=2, conclusion="skipped", completed_at="2026-11-02T00:00:00Z", steps=[])
        own = dict(self.job, id=3, name="CI timing summary", status="in_progress", completed_at=None)
        report = summarize(self.run, [self.job, skipped, own])
        self.assertEqual(report["completed_runner_minutes"], 2)
        self.assertEqual(len(report["jobs"]), 2)
        self.assertTrue(report["all_measured_jobs_completed"])

    def test_unknown_duration_not_fabricated_and_secrets_not_serialized(self):
        job = dict(self.job, completed_at=None, status="in_progress", conclusion=None, secret="do-not-record")
        report = summarize(self.run, [job])
        self.assertIsNone(report["jobs"][0]["wall_seconds"])
        self.assertFalse(report["all_measured_jobs_completed"])
        self.assertNotIn("do-not-record", str(report))

    def test_github_relabels_reused_jobs_with_current_attempt(self):
        reused = dict(self.job, id=2, started_at="2026-11-02T23:00:00Z", completed_at="2026-11-02T23:20:00Z")
        report = summarize(self.run, [self.job, reused])
        self.assertEqual(len(report["reused_jobs"]), 1)
        self.assertEqual(len(report["jobs"]), 1)
        self.assertEqual(report["completed_runner_minutes"], 2)
        self.assertEqual(report["measured_latency_seconds"], 130)

    def test_wall_clock_budget_is_reported_without_changing_gates(self):
        under = summarize(self.run, [self.job])
        self.assertLess(under["measured_latency_seconds"], WALL_CLOCK_BUDGET_SECONDS)
        self.assertFalse(budget(under)["over_budget"])
        self.assertIn(f"Wall-clock budget: {WALL_CLOCK_BUDGET_SECONDS} s (within", markdown(under))
        slow = dict(self.job, completed_at="2026-11-03T00:30:00Z")
        over = summarize(self.run, [slow])
        self.assertTrue(budget(over)["over_budget"])
        self.assertIn("(over the measured latency)", markdown(over))
        no_latency = summarize(self.run, [dict(self.job, completed_at=None, status="in_progress", conclusion=None)])
        self.assertIsNone(budget(no_latency))
        self.assertNotIn("Wall-clock budget", markdown(no_latency))

    def test_reused_attempt_and_duplicate_job_are_rejected(self):
        for change in ({"run_attempt": 1}, {"run_id": 11}):
            with self.assertRaises(ValueError):
                summarize(self.run, [dict(self.job, **change)])
        with self.assertRaises(ValueError):
            summarize(self.run, [self.job, copy.deepcopy(self.job)])


if __name__ == "__main__":
    unittest.main()
