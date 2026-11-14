import copy
import json
from pathlib import Path
import sys
import unittest
from unittest import mock
import tempfile
import tomllib

sys.path.insert(0, str(Path(__file__).parent))
from ci_identity_profile import BATCH_COUNTS, BATCH_FIXTURES, BATCH_HTTP_REQUESTS, BATCH_TEST, PACKAGES, admin_template_targets, audit_cloned_cases, audit_router_pilot, audit_target_template_uses, audit_template_uses, instrument_core, instrument_maximum_batch, instrument_postgres, instrument_units, profile_lock, profile_manifest, replace_once, summarize, summarize_maximum_batch, template_cases, validate_profile_lock


class IdentityProfileTests(unittest.TestCase):
    def test_profile_manifest_preserves_unrelated_patch_and_helper_target(self):
        prefix = '[package]\nname = "fixture"\nversion = "0.1.0"\n'
        unrelated = '[patch.crates-io]\nsqlx-sqlite = { git = "https://example.invalid/sqlx", rev = "fixed" }\n'
        for original in [prefix, prefix + unrelated]:
            patches = {name: f"/profile/{name}" for name in sorted(PACKAGES)}
            helper = '\n[[bin]]\nname = "ci-identity-template"\npath = "helper.rs"\n'
            result = tomllib.loads(profile_manifest(original, patches, helper))
            for name in PACKAGES:
                self.assertEqual(result["patch"]["crates-io"][name], {"path": f"/profile/{name}"})
            if original.endswith(unrelated):
                self.assertEqual(result["patch"]["crates-io"]["sqlx-sqlite"], {"git": "https://example.invalid/sqlx", "rev": "fixed"})
            self.assertEqual(result["bin"], [{"name": "ci-identity-template", "path": "helper.rs"}])

    def test_profile_manifest_rejects_existing_profiled_crate_override(self):
        for name in PACKAGES:
            with self.assertRaises(ValueError):
                profile_manifest(f'[patch.crates-io]\n{name} = {{ path = "/other" }}\n', {}, "")

    def test_profile_manifest_keeps_reviewed_git_alias_on_same_timed_core(self):
        source = '[patch."https://github.com/launchbadge/sqlx"]\nsqlx-core = { version = "=0.9.0" }\n'
        paths = {name: f"/profile/{name}" for name in sorted(PACKAGES)}
        result = tomllib.loads(profile_manifest(source, paths, ""))
        self.assertEqual(result["patch"]["https://github.com/launchbadge/sqlx"]["sqlx-core"], {"path": "/profile/sqlx-core"})
        for invalid in [source.replace("=0.9.0", "0.9"), source.replace("launchbadge", "other")]:
            with self.assertRaises(ValueError):
                profile_manifest(invalid, paths, "")

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

    def test_clone_all_route_injects_module_wide_template_use(self):
        source = (
            "static MASTER_POOL: i32 = 1;\n"
            '    let create_command = format!("create database {db_name:?}");\n'
            "    conn.execute(AssertSqlSafe(create_command)).await?;\n"
        )
        patched = instrument_postgres(source, (), "fn migration_fingerprint() {}", clone_all=True)
        self.assertIn("const CI_TEMPLATE_ALL: bool = true;", patched)
        self.assertIn("const CI_TEMPLATE_CASES: &[&str] = &[\n\n];", patched)
        self.assertIn("CI_TEMPLATE_ALL || CI_TEMPLATE_CASES.contains(&args.test_path)", patched)
        whitelisted = instrument_postgres(source, ["auth_me::case"], "fn migration_fingerprint() {}")
        self.assertIn("const CI_TEMPLATE_ALL: bool = false;", whitelisted)
        self.assertIn('"auth_me::case"', whitelisted)
        self.assertNotIn("CI_TEMPLATE_ALL ||", instrument_postgres(source))

    def test_cloned_route_audit_requires_every_executed_case_once(self):
        report = self.fixture()
        executed = ["identity_test::case", "pure::unit"]
        report["template_uses"] = [{"test": test, "template": "owned-template"} for test in executed]
        self.assertEqual(audit_cloned_cases(report, "owned-template"), {"cloned_cases": 2, "fresh_sqlx_cases": 0})
        for mutation in [
            lambda r: r["template_uses"].pop(),
            lambda r: r["template_uses"].append(r["template_uses"][0]),
            lambda r: r["template_uses"][0].update(template="stale"),
            lambda r: r["template_uses"][0].update(test="identity_test::other"),
        ]:
            changed = copy.deepcopy(report)
            mutation(changed)
            with self.assertRaises(ValueError):
                audit_cloned_cases(changed, "owned-template")

    def test_admin_scope_keeps_schema_and_migration_modules_fresh(self):
        root = Path(__file__).resolve().parents[1]
        targets = admin_template_targets(root)
        self.assertIn("admin_accounts_reset_password_handler", targets)
        for target in ("admin_schema", "admin_permissions_handler", "admin_preferences_handler", "admin_coins_handler"):
            self.assertNotIn(target, targets)
        with mock.patch("ci_identity_profile.classify_targets", return_value={"admin": []}):
            with self.assertRaises(ValueError):
                admin_template_targets(root)

    def test_target_route_requires_automatic_migrations(self):
        source = (
            "static MASTER_POOL: i32 = 1;\n"
            '    let create_command = format!("create database {db_name:?}");\n'
            "    conn.execute(AssertSqlSafe(create_command)).await?;\n"
        )
        patched = instrument_postgres(source, (), "fn migration_fingerprint() {}", targets=["admin_accounts_reset_password_handler"])
        self.assertIn("args.migrator.is_some() && CI_TEMPLATE_TARGETS.contains", patched)
        self.assertIn('"admin_accounts_reset_password_handler"', patched)
        self.assertIn("const CI_TEMPLATE_ALL: bool = false;", patched)
        self.assertIn("migration_fingerprint(args.migrator.expect", patched)

    def test_target_route_audit_keeps_manual_and_nonwhitelisted_cases_fresh(self):
        report = self.fixture()
        # Same target, but migrations=false: it has a database lifecycle without
        # an automatic migration event. A schema target has migrations but is
        # deliberately outside the approved target set.
        original = [t for t in report["timings"] if t["test"] == "identity_test::case"]
        for target, name, automatic in [("identity_test", "manual", False), ("schema_test", "case", True)]:
            report["executed"].append({"target": target, "name": name, "status": "ok"})
            report["timings"] += [{**t, "test": f"{target}::{name}"} for t in original if automatic or t["phase"] != "migration"]
        report["template_uses"] = [{"test": "identity_test::case", "template": "owned"}]
        result = audit_target_template_uses(report, {"identity_test"}, "owned")
        self.assertEqual((result["cloned_cases"], result["fresh_sqlx_cases"]), (1, 2))
        mutations = [lambda r: r.update(template_uses=[]),
                     lambda r: r["template_uses"].append(r["template_uses"][0]),
                     lambda r: r["template_uses"][0].update(template="foreign")]
        for case in ("identity_test::manual", "schema_test::case", "pure::unit"):
            mutations.append(lambda r, case=case: r["template_uses"].append({"test": case, "template": "owned"}))
        for mutate in mutations:
            changed = copy.deepcopy(report)
            mutate(changed)
            with self.assertRaises(ValueError):
                audit_target_template_uses(changed, {"identity_test"}, "owned")

    def test_unit_timing_uses_the_selected_module(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tests").mkdir()
            source = "#[tokio::test]\nasync fn unit_case() {}\n"
            for name in ("admin_case", "identity_case"):
                (root / "tests" / f"{name}.rs").write_text(source)
            with mock.patch("ci_identity_profile.classify_targets", return_value={"admin": ("admin_case",), "identity": ("identity_case",)}):
                units = instrument_units(root, "admin")
            self.assertEqual(units, [{"target": "admin_case", "name": "unit_case"}])
            self.assertIn("CiPhaseTimer", (root / "tests/admin_case.rs").read_text())
            self.assertEqual((root / "tests/identity_case.rs").read_text(), source)

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
