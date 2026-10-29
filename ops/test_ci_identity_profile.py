import copy
import json
from pathlib import Path
import sys
import unittest
import tempfile

sys.path.insert(0, str(Path(__file__).parent))
from ci_identity_profile import BATCH_COUNTS, BATCH_FIXTURES, BATCH_HTTP_REQUESTS, BATCH_TEST, PACKAGES, audit_router_pilot, audit_template_uses, instrument_core, instrument_maximum_batch, instrument_postgres, profile_lock, replace_once, summarize, summarize_maximum_batch, template_cases, validate_profile_lock


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

    def test_template_route_audit_rejects_missing_duplicate_wrong_template_and_unlisted_use(self):
        report = self.fixture()
        cases = ["identity_test::case"]
        report["template_uses"] = [{"test": cases[0], "template": "owned-template"}]
        self.assertEqual(audit_template_uses(report, cases, "owned-template"), {"cloned_cases": 1, "fresh_sqlx_cases": 0})
        for mutation in [lambda r: r.update(template_uses=[]), lambda r: r["template_uses"].append(r["template_uses"][0]), lambda r: r["template_uses"][0].update(template="stale"), lambda r: r["template_uses"][0].update(test="pure::unit"), lambda r: r["executed"].pop(0)]:
            changed = copy.deepcopy(report)
            mutation(changed)
            with self.assertRaises(ValueError):
                audit_template_uses(changed, cases, "owned-template")

    def test_whitelist_cannot_silently_expand_to_migration_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ops").mkdir()
            path = root / "ops/ci_template_cases.json"
            for cases in [[], ["avatar_migrations::up_down"], ["user_repository::case"] * 2, ["user_repository::case\"; SQL"]]:
                path.write_text(json.dumps(cases))
                with self.assertRaises(ValueError):
                    template_cases(root)
            path.write_text(json.dumps(["user_repository::case"]))
            self.assertEqual(template_cases(root), ["user_repository::case"])

    def batch_fixture(self):
        return {"executed": [{"target": "lexicon_handler", "name": BATCH_TEST.partition("::")[2], "status": "ok"}], "timings": [{"test": BATCH_TEST, "phase": "body", "elapsed_us": 100000}], "batch_fixture_phases": [{"ci_test": "maximum-batch", "phase": phase, "elapsed_ms": 10} for phase in BATCH_FIXTURES], "batch_timings": [{"test": BATCH_TEST, "phase": phase, "elapsed_us": 100, "complete": True} for phase, count in BATCH_COUNTS.items() for _ in range(count)]}

    def test_nested_batch_paths_and_concurrent_samples_are_audited(self):
        summary = summarize_maximum_batch(self.batch_fixture())
        self.assertEqual(summary["request_samples"]["forms-impact-save"]["count"], 50)
        self.assertEqual(summary["request_samples"]["list-read"]["count"], 20)
        self.assertEqual(summary["fixture_wall_ms"]["content-fixture"], 10)

    def test_missing_extra_invalid_and_unfinished_batch_measurements_fail(self):
        for mutation in [lambda r: r["batch_timings"].pop(), lambda r: r["batch_timings"].append(r["batch_timings"][0]), lambda r: r["batch_timings"][0].update(test="wrong::case"), lambda r: r["batch_timings"][0].update(complete=False), lambda r: r["batch_timings"][0].update(elapsed_us=-1), lambda r: r["batch_timings"][0].update(elapsed_us=1000000), lambda r: r["batch_fixture_phases"].pop(), lambda r: r["batch_fixture_phases"].append(r["batch_fixture_phases"][0]), lambda r: r["timings"][0].update(elapsed_us=1), lambda r: r["executed"][0].update(status="ignored")]:
            report = self.batch_fixture()
            mutation(report)
            with self.assertRaises(ValueError):
                summarize_maximum_batch(report)

    def test_batch_timer_edits_preserve_original_bytes_and_reject_anchor_drift(self):
        source = (Path(__file__).parents[1] / "tests/lexicon_handler/batch5.rs").read_text()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "tests/lexicon_handler/batch5.rs"
            path.parent.mkdir(parents=True)
            path.write_text(source)
            evidence = instrument_maximum_batch(root)
            self.assertTrue(evidence["reversible_timer_edits"])
            path.write_text(source.replace("save_v3_forms_after_impact(", "changed_helper("))
            with self.assertRaises(ValueError):
                instrument_maximum_batch(root)

    def test_router_pilot_requires_one_builder_and_every_original_request(self):
        report = self.batch_fixture()
        report["router_pilots"] = [{"test": BATCH_TEST, "router_builds": 1, "requests": dict(BATCH_HTTP_REQUESTS), "unique_request_ids": 343}]
        self.assertEqual(audit_router_pilot(report)["requests"], BATCH_HTTP_REQUESTS)
        for mutation in [lambda r: r.update(router_pilots=[]), lambda r: r["router_pilots"].append(r["router_pilots"][0]), lambda r: r["router_pilots"][0].update(router_builds=2), lambda r: r["router_pilots"][0].update(router_builds=True), lambda r: r["router_pilots"][0].update(unique_request_ids=242), lambda r: r["router_pilots"][0]["requests"].update(list=19), lambda r: r["router_pilots"][0]["requests"].update(other=1), lambda r: r["executed"][0].update(status="ignored")]:
            changed = copy.deepcopy(report)
            mutation(changed)
            with self.assertRaises(ValueError):
                audit_router_pilot(changed)


if __name__ == "__main__":
    unittest.main()
