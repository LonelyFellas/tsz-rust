import copy
import contextlib
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile

sys.path.insert(0, str(Path(__file__).parent))

import deploy_ci as ci
from deploy_test import DeploymentError
from test_deploy_test import plan_fixture


class CandidateTests(unittest.TestCase):
    def fixture(self):
        sha = "a" * 40
        run = {"id": 17, "run_attempt": 2, "name": "CI", "path": ".github/workflows/ci.yml",
               "workflow_id": 8, "head_sha": sha, "head_branch": "main", "event": "push",
               "status": "completed", "conclusion": "success", "created_at": "2026-11-15T00:00:00Z",
               "repository": {"full_name": ci.REPOSITORY}, "head_repository": {"full_name": ci.REPOSITORY}}
        repo = ci.Github()
        def api(path):
            if path == "actions/runs/17/attempts/2":
                return copy.deepcopy(run)
            if path == "branches/main":
                return {"commit": {"sha": sha}}
            if path.startswith("actions/runs?"):
                return {"workflow_runs": [copy.deepcopy(run)]}
            raise AssertionError(path)
        repo.api = Mock(side_effect=api)
        return repo, run, sha

    def test_failed_or_foreign_ci_is_rejected_before_checkout_or_host_access(self):
        for changed in ({"event": "pull_request"}, {"conclusion": "failure"}, {"head_repository": None}):
            repo, run, sha = self.fixture()
            run.update(changed)
            with self.subTest(changed=changed), patch.object(ci, "git") as git:
                with self.assertRaisesRegex(DeploymentError, "candidate_ci_not_successful_main"):
                    repo.candidate(sha, 17, 2)
                git.assert_not_called()

    def test_older_main_or_attempt_is_superseded_without_preparing_deployment(self):
        for reason in ("main", "attempt"):
            repo, run, sha = self.fixture()
            original = repo.api.side_effect
            def api(path):
                if reason == "main" and path == "branches/main":
                    return {"commit": {"sha": "b" * 40}}
                if reason == "attempt" and path.startswith("actions/runs?"):
                    return {"workflow_runs": [{**run, "run_attempt": 3}]}
                return original(path)
            repo.api.side_effect = api
            with self.subTest(reason=reason), patch.object(ci, "git") as git:
                self.assertEqual(repo.candidate(sha, 17, 2)["status"], "superseded")
                git.assert_not_called()

    def test_current_success_requires_clean_exact_main_checkout(self):
        repo, _, sha = self.fixture()
        values = {("branch", "--show-current"): "main", ("rev-parse", "HEAD"): sha,
                  ("rev-parse", "HEAD^{tree}"): "b" * 40, ("status", "--porcelain"): ""}
        with patch.object(ci, "git", side_effect=lambda *args: values[args]):
            self.assertEqual(repo.candidate(sha, 17, 2)["git_tree"], "b" * 40)
            values[("status", "--porcelain")] = " M ops/deploy_ci.py"
            with self.assertRaisesRegex(DeploymentError, "clean_exact_main"):
                repo.candidate(sha, 17, 2)


class DeploymentTests(unittest.TestCase):
    def test_change_selection_uses_whole_accepted_interval_and_keeps_runtime_renames(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-C", directory, *args], text=True).strip()
            git("init", "-q")
            git("config", "user.name", "fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("fn original() {}")
            git("add", "."); git("commit", "-qm", "baseline")
            baseline = git("rev-parse", "HEAD")
            (root / "AGENTS.md").write_text("test scope")
            git("add", "."); git("commit", "-qm", "rules")
            with patch.object(ci, "ROOT", root):
                self.assertFalse(ci.inputs_changed(baseline, git("rev-parse", "HEAD")))
                (root / "src/lib.rs").write_text("fn changed() {}")
                git("add", "."); git("commit", "-qm", "runtime")
                (root / "AGENTS.md").write_text("more rules")
                git("add", "."); git("commit", "-qm", "rules after runtime")
                self.assertTrue(ci.inputs_changed(baseline, git("rev-parse", "HEAD")))
                (root / "docs").mkdir()
                (root / "src/lib.rs").rename(root / "docs/source.md")
                git("add", "-A"); git("commit", "-qm", "rename")
                self.assertTrue(ci.inputs_changed(baseline, git("rev-parse", "HEAD")))
            self.assertTrue(ci.publication_input(" docs/source.rs"))
            self.assertTrue(ci.publication_input("docs/openapi.json"))
            self.assertTrue(ci.publication_input("docs/runtime.rs"))
            self.assertFalse(ci.publication_input("docs/features/plan.md"))
            self.assertFalse(ci.publication_input("tests/boundary.rs"))

    def test_unchanged_inputs_verify_old_identity_without_artifact_download_or_apply(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, repo = plan_fixture(root)
            accepted = plan["baseline"]["deployment_manifest"]["source"]["git_sha"]
            host.verify_existing = Mock(return_value={"verified": True, "git_sha": accepted})
            repo.candidate = Mock(return_value={"status": "current", "candidate_sha": "a" * 40})
            with patch.object(ci, "inputs_changed", return_value=False), patch.object(ci, "prepare_bundle") as prepare, patch.object(ci, "Controller") as controller:
                result = ci.deploy(repo, host, "a" * 40, 17, 2, root / "deployment")
                self.assertEqual(result["status"], "skipped")
                self.assertEqual(result["accepted_sha"], accepted)
                host.verify_existing.assert_called_once()
                prepare.assert_not_called()
                controller.assert_not_called()

    def test_publication_and_failure_use_existing_controller_once_with_persisted_state(self):
        for fail in (None, "formal_verify"):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                plan, host, repo = plan_fixture(root)
                sha = plan["source"]["git_sha"]
                accepted = plan["baseline"]["deployment_manifest"]["source"]["git_sha"]
                host.verify_existing = Mock(return_value={"verified": True, "git_sha": accepted})
                host.fail = fail
                repo.candidate = Mock(return_value={"status": "current", "candidate_sha": sha,
                                                  "ci_run_id": 17, "attempt": 2})
                with patch.object(ci, "inputs_changed", return_value=True), patch.object(ci, "prepare_bundle", return_value=root), patch.object(ci, "migration_evidence"):
                    result = ci.deploy(repo, host, sha, 17, 2, root / "deployment")
                self.assertEqual(result["status"], "recovered" if fail else "accepted")
                state = json.loads((root / "deployment/state" / result["deployment_id"] / "state.json").read_text())
                self.assertEqual(state["status"], result["status"])
                self.assertEqual(state["plan"]["recovery_policy"], {"kind": "forward_only"})
                self.assertEqual(host.actions.count("install"), 1)
                self.assertFalse((root / "deployment/state/active").exists())

    def test_archive_rejects_paths_links_and_duplicate_files(self):
        for kind in ("traversal", "link", "duplicate"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive = root / "artifact.zip"
                with zipfile.ZipFile(archive, "w") as bundle:
                    if kind == "traversal":
                        bundle.writestr("../tsz-rust", b"outside")
                    elif kind == "link":
                        entry = zipfile.ZipInfo("tsz-rust")
                        entry.external_attr = (stat.S_IFLNK | 0o777) << 16
                        bundle.writestr(entry, b"/etc/passwd")
                    else:
                        bundle.writestr("tsz-rust", b"one")
                        bundle.writestr("tsz-rust", b"two")
                with self.assertRaises(DeploymentError):
                    ci.extract_artifact(archive, root / "bundle", {"tsz-rust"})
                self.assertFalse((root / "tsz-rust").exists())

    def test_receipt_mismatch_is_rejected_before_executing_downloaded_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source'; source.mkdir()
            plan, _, _ = plan_fixture(source)
            receipt = json.loads((source / 'release-smoke-receipt.json').read_text())
            receipt['artifact_sha256'] = '0' * 64
            metadata = json.loads((source / 'artifact.json').read_text())
            repo = Mock()
            repo.artifacts.return_value = [{'id': 102, 'name': 'release-smoke-' + plan['source']['git_sha'] + '-attempt-2', 'expired': False}]
            repo.api.return_value = metadata
            def download(record, destination, expected):
                destination.mkdir(parents=True)
                if expected == {'release-smoke-receipt.json'}:
                    (destination / 'release-smoke-receipt.json').write_text(json.dumps(receipt))
                else:
                    for name in ['tsz-rust', 'tsz-rust.manifest.json']:
                        (destination / name).write_bytes((source / name).read_bytes())
                    (destination / 'tsz-rust.sha256').write_text(plan['artifact_sha256'])
            repo.download.side_effect = download
            candidate = {'candidate_sha': plan['source']['git_sha'], 'ci_run_id': plan['ci']['run_id'],
                         'attempt': 2, 'git_tree': plan['source']['git_tree']}
            with patch.object(ci.subprocess, 'run') as execute, self.assertRaisesRegex(DeploymentError, 'release_verification_mismatch'):
                ci.prepare_bundle(repo, candidate, root / 'download')
            execute.assert_not_called()

    def test_expired_baseline_uses_verified_deployed_artifact_for_forward_upgrade(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, repo = plan_fixture(root)
            baseline = copy.deepcopy(plan['baseline'])
            baseline['deployment_manifest']['ci']['artifact_id'] = 202
            info = json.loads((root / 'deploy-info.json').read_text())
            info['migrations'].append({'version': 2, 'direction': 'up', 'checksum': 'd' * 96, 'transactional': True})
            (root / 'deploy-info.json').write_text(json.dumps(info))
            repo.api = Mock(return_value={'id': 202, 'expired': True})
            host.download_baseline = Mock()
            with patch.object(ci, 'prepare_runtime_images') as images, patch.object(ci, 'rehearse') as upgrade:
                ci.migration_evidence(repo, host, root, baseline)
            images.assert_called_once()
            host.download_baseline.assert_called_once_with(baseline, root.parent / 'baseline-artifact')
            self.assertEqual(upgrade.call_args.args[0].recovery_mode, 'forward_only')
            self.assertTrue(upgrade.call_args.args[0].baseline_deployed)

    def test_only_identified_parallel_deployments_wait(self):
        host = Mock()
        host.baseline.side_effect = DeploymentError("transport_failed")
        with patch.object(ci.time, "sleep") as sleep, self.assertRaises(DeploymentError):
            ci.wait_for_baseline(host)
        sleep.assert_not_called()

    def test_wait_for_baseline_aborts_a_stale_unmutated_transaction_once(self):
        host = Mock()
        baseline = {"deployment_manifest": {"source": {"git_sha": "a" * 40}}}
        host.baseline.side_effect = [DeploymentError("deployment_in_progress"), baseline]
        host.verify_existing.return_value = {"verified": True, "git_sha": "a" * 40}
        host.inspect.return_value = {"owner": "old", "safe_abort": True}
        host.abort_stale.return_value = {"aborted": True, "owner": "old"}
        with patch.object(ci, "validate_deployment"), patch.object(ci.time, "monotonic", side_effect=[0, 1, 1, 1]):
            result = ci.wait_for_baseline(host, seconds=0)
        self.assertEqual(result, baseline)
        host.abort_stale.assert_called_once_with()

    def test_summary_failure_does_not_replace_real_publication_outcome(self):
        with tempfile.TemporaryDirectory() as directory:
            for status in ('accepted', 'recovered'):
                with self.subTest(status=status), patch.dict(os.environ, {'GITHUB_STEP_SUMMARY': directory}), \
                     contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    result = ci.write_result(directory, {'status': status}, ci.time.monotonic())
                    self.assertEqual(result['status'], status)
                    self.assertEqual(json.loads((Path(directory) / 'result.json').read_text())['status'], status)

    def test_workflow_binds_successful_main_ci_and_uses_no_rebuild(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/deploy-tshb-test.yml").read_text()
        self.assertIn("workflow_run:", workflow)
        self.assertIn("branches: [main]", workflow)
        self.assertIn("github.event.workflow_run.conclusion == 'success'", workflow)
        self.assertIn("github.event.workflow_run.head_repository.full_name == github.repository", workflow)
        self.assertIn(ci.REPOSITORY, workflow)
        self.assertNotIn("pull_request:", workflow)
        self.assertNotIn("cargo build", workflow)
        self.assertIn("cancel-in-progress: false", workflow)
        self.assertIn("include-hidden-files: true", workflow)
        self.assertIn("python3 -u ops/deploy_ci.py deploy", workflow)


if __name__ == "__main__":
    unittest.main()
