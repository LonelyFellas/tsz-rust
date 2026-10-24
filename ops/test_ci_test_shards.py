import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).parent))
from ci_test_modules import ClassificationError, classify_targets, discover_targets
from ci_test_shards import (PAGINATION, ROOT, SHARD_MODULES, build, filters_for, list_cases, run, selected_cases, targets_for, verify_manifests)


class IntegrationShardsTests(unittest.TestCase):
    def setUp(self):
        self.grouped = {
            "lexicon": ("lexicon_handler", "learning_runs_handler", "wordlists_test"),
            "admin": ("admin_test",), "identity": ("identity_test",), "platform": ("platform_test",),
        }
        self.reports = []
        for shard in SHARD_MODULES:
            inventory, executed = {}, []
            for target in targets_for(shard, self.grouped):
                names = [PAGINATION, "ordinary", "ignored_case"] if target == "lexicon_handler" else ["ordinary"]
                ignored = ["ignored_case"] if target == "lexicon_handler" else []
                inventory[target] = {"names": names, "ignored": ignored}
                for name in selected_cases(shard, target, names):
                    executed.append({"target": target, "name": name, "status": "ignored" if name in ignored else "ok"})
            self.reports.append({"shard": shard, "sha": "exact-sha", "inventory": inventory, "executed": executed})

    def test_complete_partition_and_workflow_matrix(self):
        self.assertEqual(verify_manifests(self.reports, self.grouped, "exact-sha"), {"targets": 6, "cases": 8, "shards": 6})
        import re
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        matrix = re.search(r"module: \[([^\]]+)\]", workflow)[1].split(", ")
        self.assertEqual(matrix, list(SHARD_MODULES))
        grouped = classify_targets(discover_targets(ROOT))
        self.assertEqual(len({target for shard in SHARD_MODULES for target in targets_for(shard, grouped)}), 95)

    def test_omitted_duplicate_or_unexpected_execution_fails(self):
        for mutation in [lambda r: r[1]["executed"].pop(), lambda r: r[1]["executed"].append(r[1]["executed"][0]), lambda r: r[1]["executed"].append({"target": "unknown", "name": "extra", "status": "ok"})]:
            reports = copy.deepcopy(self.reports)
            mutation(reports)
            with self.assertRaises(ClassificationError):
                verify_manifests(reports, self.grouped, "exact-sha")

    def test_missing_duplicate_wrong_version_and_disagreeing_inventory_fail(self):
        for mutation in [lambda r: r.pop(), lambda r: r.append(r[0]), lambda r: r[0].update(sha="old-sha"), lambda r: r[1]["inventory"]["lexicon_handler"]["names"].append("new_case")]:
            reports = copy.deepcopy(self.reports)
            mutation(reports)
            with self.assertRaises(ClassificationError):
                verify_manifests(reports, self.grouped, "exact-sha")

    def test_newly_ignored_or_failed_result_cannot_count_as_success(self):
        for status in ["ignored", "FAILED"]:
            reports = copy.deepcopy(self.reports)
            reports[0]["executed"][0]["status"] = status
            with self.assertRaises(ClassificationError):
                verify_manifests(reports, self.grouped, "exact-sha")

    def test_missing_pagination_or_ambiguous_skip_is_rejected(self):
        for names in [["ordinary"], [PAGINATION, PAGINATION], [PAGINATION, PAGINATION + "_extra"]]:
            with self.assertRaises(ClassificationError):
                selected_cases("lexicon-handler", "lexicon_handler", names)
        self.assertEqual(filters_for("lexicon-pagination", "lexicon_handler"), [PAGINATION, "--exact"])
        self.assertEqual(filters_for("lexicon-handler", "lexicon_handler"), ["--skip", PAGINATION])
        self.assertEqual(filters_for("lexicon-pagination", "learning_runs_handler"), [])
        self.assertEqual(targets_for("lexicon-pagination", self.grouped), ("lexicon_handler", "learning_runs_handler"))
        self.assertEqual(targets_for("lexicon-other", self.grouped), ("wordlists_test",))

    def test_build_uses_locked_all_feature_target_and_records_real_executable(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch("ci_test_shards.subprocess.run") as runner:
            runner.return_value.stdout = "\n".join(json.dumps({"reason": "compiler-artifact", "target": {"name": target}, "executable": f"/{target}"}) for target in ("lexicon_handler", "learning_runs_handler"))
            build("lexicon-pagination", self.grouped, Path(directory))
            self.assertEqual(runner.call_args.args[0], ["cargo", "test", "--locked", "--all-features", "--test", "lexicon_handler", "--test", "learning_runs_handler", "--no-run", "--message-format=json"])
            self.assertEqual(json.loads((Path(directory) / "lexicon-pagination-executables.json").read_text()), {"lexicon_handler": "/lexicon_handler", "learning_runs_handler": "/learning_runs_handler"})

    def test_malformed_list_is_rejected(self):
        with mock.patch("ci_test_shards.subprocess.run") as runner:
            runner.return_value.stdout = "ordinary: test\n2 tests, 0 benchmarks\n"
            with self.assertRaises(ClassificationError):
                list_cases("/binary")

    def test_actual_binary_results_match_both_filters_and_preserve_ignored_cases(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "harness"
            binary.write_text(f'''#!{sys.executable}
import sys
names = { [PAGINATION, 'ordinary', 'ignored_case']!r }
args = sys.argv[1:]
if '--list' in args:
    if '--ignored' in args: names = ['ignored_case']
    for name in names: print(name + ': test')
    print(str(len(names)) + ' tests, 0 benchmarks')
else:
    if '--exact' in args: names = [args[0]]
    if '--skip' in args: names = [name for name in names if args[args.index('--skip')+1] not in name]
    for name in names: print('test ' + name + ' ... ' + ('ignored' if name == 'ignored_case' else 'ok'))
''')
            binary.chmod(0o755)
            learning = root / "learning"
            learning.write_text(f'''#!{sys.executable}
import sys
assert '--exact' not in sys.argv and '--skip' not in sys.argv
if '--list' in sys.argv:
    if '--ignored' not in sys.argv: print('learning_case: test')
    print(('0' if '--ignored' in sys.argv else '1') + ' tests, 0 benchmarks')
else:
    print('test learning_case ... ok')
''')
            learning.chmod(0o755)
            for shard in ["lexicon-pagination", "lexicon-handler"]:
                executables = {target: str(binary if target == "lexicon_handler" else learning) for target in targets_for(shard, self.grouped)}
                (root / f"{shard}-executables.json").write_text(json.dumps(executables))
                with mock.patch.dict("os.environ", {"GITHUB_SHA": "exact-sha"}):
                    run(shard, self.grouped, root, root / f"{shard}-report.json")
            heavy = json.loads((root / "lexicon-pagination-report.json").read_text())
            rest = json.loads((root / "lexicon-handler-report.json").read_text())
            self.assertEqual([row["name"] for row in heavy["executed"]], ["learning_case", PAGINATION])
            self.assertEqual([row["name"] for row in rest["executed"]], ["ordinary", "ignored_case"])
            self.assertEqual(heavy["inventory"]["lexicon_handler"], rest["inventory"]["lexicon_handler"])


if __name__ == "__main__":
    unittest.main()
