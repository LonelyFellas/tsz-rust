import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from ci_cargo_fetch import fetch_with_clean_fallback  # noqa: E402


class CiCargoFetchTests(unittest.TestCase):
    def test_cache_or_network_success_runs_once(self) -> None:
        calls: list[tuple[list[str], dict[str, str]]] = []

        def runner(command: list[str], **options: object) -> subprocess.CompletedProcess[str]:
            calls.append((command, options["env"]))
            return subprocess.CompletedProcess(command, 0)

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            github_env = root / "github.env"
            result = fetch_with_clean_fallback(
                runner_temp=root,
                github_env=github_env,
                environment={"PATH": "/usr/bin"},
                runner=runner,
            )

            self.assertEqual(result, 0)
            self.assertEqual(len(calls), 1)
            self.assertFalse(github_env.exists())

    def test_corrupt_cache_retries_with_clean_cargo_home_and_target(self) -> None:
        calls: list[tuple[list[str], dict[str, str]]] = []

        def runner(command: list[str], **options: object) -> subprocess.CompletedProcess[str]:
            environment = options["env"]
            calls.append((command, environment))
            return subprocess.CompletedProcess(command, 1 if len(calls) == 1 else 0)

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            github_env = root / "github.env"
            result = fetch_with_clean_fallback(
                runner_temp=root,
                github_env=github_env,
                environment={"PATH": "/usr/bin", "CARGO_HOME": "/cached/cargo"},
                runner=runner,
            )

            self.assertEqual(result, 0)
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[0][0], ["cargo", "fetch", "--locked"])
            self.assertEqual(calls[0][1]["CARGO_HOME"], "/cached/cargo")
            self.assertNotEqual(calls[1][1]["CARGO_HOME"], "/cached/cargo")
            self.assertIn("CARGO_TARGET_DIR", calls[1][1])
            rendered = github_env.read_text(encoding="utf-8")
            self.assertIn("CARGO_HOME=", rendered)
            self.assertIn("CARGO_TARGET_DIR=", rendered)

    def test_clean_retry_failure_remains_a_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            github_env = root / "github.env"
            result = fetch_with_clean_fallback(
                runner_temp=root,
                github_env=github_env,
                environment={"PATH": "/usr/bin"},
                runner=lambda command, **_options: subprocess.CompletedProcess(command, 9),
            )

            self.assertEqual(result, 9)
            self.assertFalse(github_env.exists())


if __name__ == "__main__":
    unittest.main()
