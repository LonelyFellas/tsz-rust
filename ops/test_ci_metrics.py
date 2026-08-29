import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from ci_metrics import record_metric, run_timed  # noqa: E402


class CiMetricsTests(unittest.TestCase):
    def test_ci03_success_and_failure_keep_the_real_exit_code(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            summary = pathlib.Path(directory) / "summary.md"
            success = run_timed(
                "success",
                [sys.executable, "-c", "raise SystemExit(0)"],
                summary_path=summary,
            )
            failure = run_timed(
                "failure",
                [sys.executable, "-c", "raise SystemExit(7)"],
                summary_path=summary,
            )

            self.assertEqual(success, 0)
            self.assertEqual(failure, 7)
            rendered = summary.read_text(encoding="utf-8")
            self.assertIn("success", rendered)
            self.assertIn("failure", rendered)
            self.assertIn("elapsed_ms", rendered)

    def test_record_metric_appends_a_machine_and_human_readable_value(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            summary = pathlib.Path(directory) / "summary.md"
            metric = record_metric(
                "cargo-cache-hit", "true", "boolean", summary_path=summary
            )

            self.assertEqual(metric["value"], "true")
            self.assertIn("cargo-cache-hit", summary.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
