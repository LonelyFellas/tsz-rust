import pathlib
import subprocess
import sys
import traceback
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from deployment_preflight import PreflightError, collect_snapshot, main  # noqa: E402


class DeploymentPreflightTests(unittest.TestCase):
    def test_db01_every_scalar_uses_an_independent_psql_call(self) -> None:
        calls: list[list[str]] = []
        values = iter(["5\n", "5\n", "41\n", "20260829100000\n"])
        database_url = "postgres://secret@example.invalid/db"

        def runner(command: list[str], **options: object) -> subprocess.CompletedProcess[str]:
            calls.append(command)
            self.assertEqual(command[0:3], ["psql", "--dbname", database_url])
            environment = options["env"]
            self.assertIsInstance(environment, dict)
            self.assertEqual(set(environment), {"PATH", "LC_ALL", "PGCONNECT_TIMEOUT"})
            self.assertNotIn("PGDATABASE", environment)
            query = command[command.index("-c") + 1]
            self.assertEqual(query.upper().count("SELECT"), 1)
            return subprocess.CompletedProcess(command, 0, next(values), "")

        snapshot = collect_snapshot(database_url=database_url, runner=runner)

        self.assertEqual(snapshot["entries"], 5)
        self.assertEqual(snapshot["v3_entries"], 5)
        self.assertEqual(snapshot["successful_migrations"], 41)
        self.assertEqual(snapshot["latest_migration"], 20260829100000)
        self.assertEqual(len(calls), 4)
        self.assertNotIn("postgres://secret", str(snapshot))

    def test_db02_empty_non_integer_multiline_and_failure_are_rejected(self) -> None:
        cases = [
            subprocess.CompletedProcess([], 0, "", ""),
            subprocess.CompletedProcess([], 0, "five\n", ""),
            subprocess.CompletedProcess([], 0, "5\n0\n", ""),
            subprocess.CompletedProcess([], 2, "", "psql failed"),
        ]
        for result in cases:
            with self.subTest(result=result):
                with self.assertRaises(PreflightError):
                    collect_snapshot(
                        database_url="postgres://secret@example.invalid/db",
                        runner=lambda *_args, **_kwargs: result,
                    )

    def test_unknown_cli_arguments_are_rejected(self) -> None:
        self.assertEqual(main(["unexpected"]), 2)

    def test_missing_psql_fails_closed_without_exposing_the_dsn(self) -> None:
        database_url = "postgres://secret@example.invalid/db"

        def missing_runner(
            *_args: object, **_kwargs: object
        ) -> subprocess.CompletedProcess[str]:
            raise FileNotFoundError(2, f"missing while connecting to {database_url}")

        with self.assertRaisesRegex(PreflightError, "could not start") as raised:
            collect_snapshot(
                database_url=database_url,
                runner=missing_runner,
            )
        self.assertNotIn(database_url, str(raised.exception))
        rendered = "".join(
            traceback.format_exception(
                type(raised.exception), raised.exception, raised.exception.__traceback__
            )
        )
        self.assertNotIn(database_url, rendered)
        self.assertIsNone(raised.exception.__cause__)


if __name__ == "__main__":
    unittest.main()
