import copy
import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).parent))
from ci_identity_profile import PACKAGES, instrument_core, instrument_postgres, profile_lock, replace_once, summarize, validate_profile_lock


class IdentityProfileTests(unittest.TestCase):
    def test_lock_guard_allows_only_source_change_for_two_exact_versions(self):
        original = {(name, "0.9.0"): {"name": name, "version": "0.9.0", "source": "registry", "checksum": "locked", "dependencies": ["same"]} for name in PACKAGES}
        original[("other", "1.0")] = {"name": "other", "version": "1.0", "checksum": "unchanged"}
        candidate = copy.deepcopy(original)
        for name in PACKAGES:
            candidate[(name, "0.9.0")].pop("source")
            candidate[(name, "0.9.0")].pop("checksum")
        validate_profile_lock(original, candidate)
        for alteration in [lambda d: d.pop(("other", "1.0")), lambda d: d[("other", "1.0")].update(checksum="changed"), lambda d: d[("sqlx-core", "0.9.0")].update(dependencies=["different"])]:
            changed = copy.deepcopy(candidate)
            alteration(changed)
            with self.assertRaises(ValueError):
                validate_profile_lock(original, changed)

    def test_profile_lock_keeps_transitive_pins_and_unrelated_blocks(self):
        text = 'version = 4\n\n[[package]]\nname = "sqlx-core"\nversion = "0.9.0"\nsource = "registry"\nchecksum = "original"\n\n[[package]]\nname = "other"\nversion = "1.0"\nsource = "same"\nchecksum = "keep"\n'
        changed = profile_lock(text)
        self.assertNotIn('checksum = "original"', changed)
        self.assertIn('name = "other"\nversion = "1.0"\nsource = "same"\nchecksum = "keep"', changed)

    def test_source_anchors_reject_missing_or_repeated_locations(self):
        self.assertEqual(replace_once("old", "old", "new"), "new")
        for source in ["missing", "old old"]:
            with self.assertRaises(ValueError):
                replace_once(source, "old", "new")
        with self.assertRaises(ValueError):
            instrument_core("changed upstream")
        with self.assertRaises(ValueError):
            instrument_postgres("changed upstream")

    def fixture(self):
        phases = {"total": 100, "db_setup": 20, "db_create": 10, "migration": 30, "test_callback": 40, "body": 35, "pool_connect": 2, "pool_close": 3, "cleanup": 5}
        return {"sha": "exact-sha", "executed": [{"target": "identity_test", "name": "case", "status": "ok"}, {"target": "pure", "name": "unit", "status": "ok"}], "timings": [{"test": "pure::unit", "phase": "body", "elapsed_us": 1, "complete": True}] + [{"test": "identity_test::case", "phase": phase, "elapsed_us": value, "complete": True} for phase, value in phases.items()]}

    def test_hierarchical_phases_are_not_added_as_independent_wall_time(self):
        summary = summarize(self.fixture(), [{"target": "pure", "name": "unit"}])
        self.assertEqual(summary["profiled_sqlx_cases"], 1)
        self.assertEqual(summary["executed_cases"], 2)
        self.assertEqual(summary["cumulative_phase_ms"]["db_setup"], 0.02)
        self.assertEqual(summary["cumulative_phase_ms"]["db_create"], 0.01)
        self.assertEqual(summary["without_sqlx_phases"], [])

    def test_missing_duplicate_unknown_incomplete_or_invalid_timings_fail(self):
        for mutation in [lambda r: r.update(timings=[]), lambda r: r["timings"].pop(), lambda r: r["timings"].append(r["timings"][0]), lambda r: r["timings"][0].update(test="unknown::case"), lambda r: r["timings"][0].update(phase="unknown"), lambda r: r["timings"][0].update(complete=False), lambda r: r["timings"][0].update(elapsed_us=-1), lambda r: r["timings"][1].update(elapsed_us=1)]:
            report = self.fixture()
            mutation(report)
            with self.assertRaises(ValueError):
                summarize(report, [{"target": "pure", "name": "unit"}])

    def test_explicit_no_auto_migrations_retains_the_body_measurement(self):
        report = self.fixture()
        report["timings"] = [row for row in report["timings"] if row["phase"] != "migration"]
        self.assertEqual(summarize(report, [{"target": "pure", "name": "unit"}])["profiled_sqlx_cases"], 1)


if __name__ == "__main__":
    unittest.main()
