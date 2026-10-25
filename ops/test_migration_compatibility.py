import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))

from artifact_test_runtime import VerificationError
from migration_compatibility import baseline_identity, migration_interval, rehearse
from test_release_artifact_smoke import FakeRuntime, INFO, LEDGER, fixture


def captured_baseline(manifest):
    return {
        "deployment_manifest": {"source": manifest["source"],
                                "ci": {**manifest["ci"], "conclusion": "success"},
                                "artifact": manifest["artifact"]},
        "service": {"file_sha256": manifest["artifact"]["sha256"], "process_sha256": manifest["artifact"]["sha256"]},
        "postgresql": {"status": "ok", "transaction_read_only": "on", "server_version_num": "180003", "migration_ledger": LEDGER},
        "pid_stable": True, "deployment_lock_present": False,
    }


class MigrationRuntime(FakeRuntime):
    def __init__(self, info=INFO, refuse=False, mutate_on_refusal=False):
        super().__init__()
        self.info = info
        self.rows = copy.deepcopy(LEDGER)
        self.refuse = refuse
        self.mutate_on_refusal = mutate_on_refusal
        self.snap = {"public.fixture": {"rows": 1, "digest": "preserved"}, "schema_sha256": "baseline", "sequences": []}
        self.wordlist = {"id": "00000000-0000-4000-8000-000000000001", "item_id": "00000000-0000-4000-8000-000000000002"}

    def binary(self, artifact, *arguments):
        if arguments == ("deploy-info",):
            return json.dumps(self.info)
        if arguments[0] == "deploy-undo-migrations":
            if self.refuse:
                if self.mutate_on_refusal:
                    self.snap = {"public.fixture": {"rows": 1, "digest": "changed"}}
                raise VerificationError("binary_command_failed")
            self.rows = copy.deepcopy(LEDGER)
            return "{}"
        raise AssertionError("unexpected command")

    def start_app(self, artifact):
        self.app_starts += 1
        if self.app_starts == 2:
            self.rows = [{"version": row["version"], "checksum": row["checksum"], "success": True} for row in self.info["migrations"] if row["direction"] == "up"]

    def roundtrip(self, *, wordlist=None):
        return self.wordlist

    def sql(self, statement):
        return '{"id":"fixture","name":"preserved"}'

    def snapshot(self):
        return {**self.snap, "public._sqlx_migrations": {"rows": len(self.rows), "digest": json.dumps(self.rows)}}

    def ledger(self):
        return copy.deepcopy(self.rows)


def args_fixture(root):
    args = fixture(root)
    args.baseline_manifest = args.manifest
    args.baseline_artifact = args.artifact
    args.baseline_artifact_id = 101
    args.baseline_artifact_metadata = args.artifact_metadata
    args.baseline = root / "baseline.json"
    args.baseline.write_text(json.dumps(captured_baseline(json.loads(args.manifest.read_text()))))
    return args


def new_interval():
    result = copy.deepcopy(INFO)
    for version in (2, 3):
        result["migrations"] += [
            {"version": version, "direction": "up", "checksum": str(version) * 96, "transactional": True},
            {"version": version, "direction": "down", "checksum": "f" * 96, "transactional": True},
        ]
    return result


class MigrationCompatibilityTests(unittest.TestCase):
    def test_complete_interval_is_not_only_last_commit(self):
        interval = migration_interval(new_interval(), LEDGER)
        self.assertEqual(interval["versions"], [2, 3])
        self.assertEqual(interval["baseline_version"], 1)
        self.assertEqual(interval["candidate_version"], 3)
        self.assertEqual(migration_interval(INFO, LEDGER)["classification"], "no_schema_change")

    def test_drift_missing_dirty_or_nontransactional_recovery_is_blocked(self):
        invalid = []
        drift = copy.deepcopy(INFO)
        drift["migrations"][0]["checksum"] = "c" * 96
        invalid.append((drift, LEDGER))
        invalid.append((INFO, [{**LEDGER[0], "success": False}]))
        invalid.append((INFO, LEDGER + LEDGER))
        missing = new_interval()
        missing["migrations"] = [row for row in missing["migrations"] if not (row["version"] == 2 and row["direction"] == "down")]
        invalid.append((missing, LEDGER))
        nontransactional = new_interval()
        nontransactional["migrations"][-1]["transactional"] = False
        invalid.append((nontransactional, LEDGER))
        historical_gap = new_interval()
        invalid.append((historical_gap, [LEDGER[0], {"version": 3, "checksum": "3" * 96, "success": True}]))
        for info, ledger in invalid:
            with self.subTest(info=info, ledger=ledger), self.assertRaises(VerificationError):
                migration_interval(info, ledger)

    def test_baseline_requires_accepted_source_process_and_readonly_capture(self):
        with tempfile.TemporaryDirectory() as directory:
            args = fixture(Path(directory))
            manifest = json.loads(args.manifest.read_text())
            baseline = captured_baseline(manifest)
            self.assertEqual(baseline_identity(baseline, manifest), LEDGER)
            for field in ("process", "readonly", "source", "lock"):
                broken = copy.deepcopy(baseline)
                if field == "process":broken["service"]["process_sha256"] = "0" * 64
                elif field == "readonly":broken["postgresql"]["transaction_read_only"] = "off"
                elif field == "source":broken["deployment_manifest"]["source"]["git_sha"] = "0" * 40
                else:broken["deployment_lock_present"] = True
                with self.subTest(field=field), self.assertRaises(VerificationError):
                    baseline_identity(broken, manifest)

    def test_no_schema_change_restarts_exact_old_binary_and_cleans(self):
        with tempfile.TemporaryDirectory() as directory:
            args = args_fixture(Path(directory))
            runtime = MigrationRuntime()
            result = rehearse(args, runtime_factory=lambda: runtime)
            self.assertEqual(result["status"], "passed")
            self.assertEqual(result["checks"]["bounded_down"], "not_needed_no_schema_change")
            self.assertEqual(runtime.app_starts, 3)
            self.assertTrue(runtime.closed)
            self.assertFalse(result["rollback_schema_safe"])

    def test_whole_interval_upgrade_down_and_old_binary_roundtrip(self):
        with tempfile.TemporaryDirectory() as directory:
            args = args_fixture(Path(directory))
            runtime = MigrationRuntime(new_interval())
            result = rehearse(args, runtime_factory=lambda: runtime)
            self.assertEqual(result["interval"]["versions"], [2, 3])
            self.assertEqual(result["checks"]["bounded_down"], "passed")
            self.assertEqual(result["checks"]["old_binary_after_recovery"], "passed")
            self.assertTrue(result["cleanup"]["complete"])

    def test_guard_refusal_has_no_partial_changes_and_never_authorizes_rollback(self):
        for mutate in (False, True):
            with self.subTest(mutate=mutate), tempfile.TemporaryDirectory() as directory:
                args = args_fixture(Path(directory))
                runtime = MigrationRuntime(new_interval(), refuse=True, mutate_on_refusal=mutate)
                with self.assertRaises(VerificationError):
                    rehearse(args, runtime_factory=lambda: runtime)
                receipt = json.loads(args.output.read_text())
                self.assertEqual(receipt["status"], "failed" if mutate else "blocked")
                self.assertFalse(receipt["rollback_schema_safe"])
                self.assertTrue(runtime.closed)

    def test_unknown_or_mismatched_baseline_never_creates_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            args = args_fixture(Path(directory))
            data = json.loads(args.baseline.read_text())
            data["service"]["process_sha256"] = "0" * 64
            args.baseline.write_text(json.dumps(data))
            factory = unittest.mock.Mock()
            with self.assertRaises(VerificationError):
                rehearse(args, runtime_factory=factory)
            factory.assert_not_called()


if __name__ == "__main__":
    unittest.main()
