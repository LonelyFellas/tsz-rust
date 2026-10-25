import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))

from deployed_baseline import BaselineError, capture
from test_migration_compatibility import captured_baseline
from test_release_artifact_smoke import fixture


class DeployedBaselineTests(unittest.TestCase):
    def test_capture_streams_fixed_readonly_program_without_installing_remote_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = fixture(root)
            value = captured_baseline(json.loads(args.manifest.read_text()))
            value["target"] = "tshb-test"
            output = root / "baseline-capture.json"
            with patch("deployed_baseline.run", return_value=json.dumps(value)) as run, patch("deployment_manifest.validate_manifest"):
                self.assertEqual(capture(output), value)
            command = run.call_args.args[0]
            self.assertEqual(command[-2:], ["tshb-test", "python3 -I - --remote"])
            self.assertIn("StrictHostKeyChecking=yes", command)
            program = run.call_args.kwargs["data"]
            self.assertIn("BEGIN READ ONLY;", program)
            self.assertIn("ROLLBACK;", program)
            self.assertIn("default_transaction_read_only=on", program)
            self.assertNotIn("source /opt/tsz-rust/.env", program)
            self.assertNotIn("systemctl start", program)
            self.assertEqual(json.loads(output.read_text()), value)

    def test_existing_capture_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "capture.json"
            output.write_text("previous evidence")
            with patch("deployed_baseline.run") as run, self.assertRaises(BaselineError):
                capture(output)
            run.assert_not_called()
            self.assertEqual(output.read_text(), "previous evidence")

    def test_non_readonly_or_wrong_target_capture_is_rejected(self):
        for target, readonly in [("production", "on"), ("tshb-test", "off")]:
            with self.subTest(target=target, readonly=readonly), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                args = fixture(root)
                value = captured_baseline(json.loads(args.manifest.read_text()))
                value["target"] = target
                value["postgresql"]["transaction_read_only"] = readonly
                output = root / "capture.json"
                with patch("deployed_baseline.run", return_value=json.dumps(value)), patch("deployment_manifest.validate_manifest"), self.assertRaises(BaselineError):
                    capture(output)
                self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
