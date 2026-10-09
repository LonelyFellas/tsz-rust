import argparse
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch
from backup_restore_drill import DrillError, command, validate


class RestoreSafetyTests(unittest.TestCase):
    def args(self, **changes):
        values = dict(synthetic_test_source=True, source_container="tsz-reliability-test-pg", database="reliability_test",
                      image="sha256:" + "a" * 64, target="tsz-restore-drill-test", output=Path("unused"), backup=None)
        values.update(changes)
        return argparse.Namespace(**values)

    def test_rejects_nonisolated_targets_sources_and_mutable_images_before_execution(self):
        for changes in [dict(synthetic_test_source=False), dict(source_container="production"), dict(database="app"),
                        dict(target="existing-app"), dict(image="postgres:16")]:
            with self.subTest(changes=changes), self.assertRaises(DrillError):
                validate(self.args(**changes))
        validate(self.args())

    def test_child_stderr_is_never_in_failure(self):
        result = subprocess.CompletedProcess([], 1, stdout=b"private-row", stderr=b"password-cookie-dsn")
        with patch("subprocess.run", return_value=result), self.assertRaisesRegex(DrillError, "^restore$"):
            command("restore", ["unused"])

    def test_interruption_is_not_swallowed(self):
        with patch("subprocess.run", side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            command("restore", ["unused"])


if __name__ == "__main__":
    unittest.main()
