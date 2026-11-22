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

    def test_wait_for_baseline_aborts_safe_stale_transaction_immediately(self):
        host = Mock()
        baseline = {"deployment_manifest": {"source": {"git_sha": "a" * 40}}}
        host.baseline.side_effect = [DeploymentError("deployment_in_progress"), baseline]
        host.verify_existing.return_value = {"verified": True, "git_sha": "a" * 40}
        host.inspect.return_value = {"owner": "old", "safe_abort": True}
        host.abort_stale.return_value = {"aborted": True, "owner": "old"}
        with patch.object(ci, "validate_deployment"), patch.object(ci.time, "monotonic", return_value=0):
            result = ci.wait_for_baseline(host, seconds=15 * 60)
        self.assertEqual(result, baseline)
        host.abort_stale.assert_called_once_with()

    def test_wait_for_baseline_retries_when_stale_abort_is_still_busy(self):
        host = Mock()
        baseline = {"deployment_manifest": {"source": {"git_sha": "a" * 40}}}
        host.baseline.side_effect = [
            DeploymentError("deployment_in_progress"),
            DeploymentError("deployment_in_progress"),
            baseline,
        ]
        host.verify_existing.return_value = {"verified": True, "git_sha": "a" * 40}
        host.inspect.return_value = {"owner": "old", "safe_abort": True}
        host.abort_stale.side_effect = [DeploymentError("remote_operation_busy"), {"aborted": True}]
        with (
            patch.object(ci, "validate_deployment"),
            patch.object(ci.time, "monotonic", side_effect=[0, 1, 1, 1, 2, 2, 2, 3]),
            patch.object(ci.time, "sleep") as sleep,
        ):
            result = ci.wait_for_baseline(host, seconds=0)
        self.assertEqual(result, baseline)
        self.assertEqual(host.abort_stale.call_count, 2)
        self.assertIn(10, [call.args[0] for call in sleep.call_args_list])

    def test_summary_failure_does_not_replace_real_publication_outcome(self):
        with tempfile.TemporaryDirectory() as directory:
            for status in ('accepted', 'recovered'):
                with self.subTest(status=status), patch.dict(os.environ, {'GITHUB_STEP_SUMMARY': directory}), \
                     contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    result = ci.write_result(directory, {'status': status}, ci.time.monotonic())
                    self.assertEqual(result['status'], status)
                    self.assertEqual(json.loads((Path(directory) / 'result.json').read_text())['status'], status)


class WorkflowTests(unittest.TestCase):
    root = Path(__file__).resolve().parents[1]

    def test_public_workflow_only_routes_successful_main_ci_to_one_deployer(self):
        workflow = (self.root / ".github/workflows/deploy-tshb-test.yml").read_text()
        self.assertIn("workflow_run:", workflow)
        self.assertIn("branches: [main]", workflow)
        dispatch, deploy = workflow.split("\n  deploy:\n", 1)
        for job, operator in ((dispatch, "!="), (deploy, "==")):
            condition = job.split("if: >-\n", 1)[1].split("runs-on:", 1)[0]
            for clause in (
                f"vars.TSHB_TEST_VPC_DEPLOY_REPO {operator} ''",
                f"github.repository == '{ci.REPOSITORY}'",
                "github.event.workflow_run.head_repository.full_name == github.repository",
                "github.event.workflow_run.head_branch == 'main'",
                "(github.event.workflow_run.event == 'push' || github.event.workflow_run.event == 'workflow_dispatch')",
                "github.event.workflow_run.conclusion == 'success'",
            ):
                self.assertIn(clause, condition)
        self.assertIn("run: bash ops/dispatch_vpc_deploy.sh", dispatch)
        self.assertNotIn("TSHB_TEST_SSH_KEY", dispatch)
        self.assertIn("run: bash ops/run_deploy_ci.sh", deploy)

    def test_private_workflow_requires_main_and_a_verified_candidate_before_ssh(self):
        workflow = (self.root / "docs/features/backend-auto-deploy/deploy-tshb-test-vpc.yml").read_text()
        self.assertIn("if: github.event.repository.private == true && github.ref == 'refs/heads/main'", workflow)
        self.assertIn("runs-on: [self-hosted, linux, x64, tshb-test-vpc]", workflow)
        self.assertIn(f"repository: {ci.REPOSITORY}", workflow)
        self.assertIn("ref: refs/heads/main", workflow)
        self.assertIn("persist-credentials: false", workflow)
        for run in ("bash ops/check_vpc_deploy_ssh.sh", "bash ops/run_deploy_ci.sh"):
            step = next(step for step in workflow.split("      - name: ") if f"run: {run}" in step)
            self.assertIn("if: steps.candidate.outputs.current == 'true'", step)
        self.assertIn("include-hidden-files: true", workflow)
        self.assertNotIn("cargo build", workflow)


class WorkflowScriptTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.directory = Path(temp.name)
        self.calls = self.directory / "calls.jsonl"
        self.env = {**os.environ, "PATH": f"{self.directory}:{os.environ['PATH']}",
                    "FAKE_CALLS": str(self.calls), "GH_TOKEN": "fake-token", "DEPLOY_REPO": "owner/deploy",
                    "DEPLOY_SHA": "a" * 40, "DEPLOY_CI_RUN": "17", "DEPLOY_CI_ATTEMPT": "2"}

    def fake(self, name, body):
        script = self.directory / name
        script.write_text(f"#!{sys.executable}\nimport json, os, sys\n"
                          "with open(os.environ['FAKE_CALLS'], 'a') as log:\n"
                          "    log.write(json.dumps(sys.argv[1:]) + '\\n')\n" + body)
        script.chmod(0o700)

    def run_script(self, script, *arguments, **env):
        result = subprocess.run(["bash", str(WorkflowTests.root / "ops" / script), *arguments],
                                cwd=self.directory, env={**self.env, **env}, capture_output=True, text=True)
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()] if self.calls.exists() else []
        self.calls.unlink(missing_ok=True)
        return result, calls

    def fake_github(self):
        self.fake("gh", "if sys.argv[2] == 'repos/owner/deploy':\n"
                       "    print(os.environ.get('FAKE_REPOSITORY_MATCH', 'true'))\n"
                       "else:\n"
                       "    print(os.environ.get('FAKE_WORKFLOW_RUN', '123456'))\n"
                       "    sys.exit(int(os.environ.get('FAKE_DISPATCH_STATUS', '0')))\n")

    def test_dispatch_validates_inputs_without_calling_github(self):
        self.fake_github()
        for field, value in (("GH_TOKEN", ""), ("DEPLOY_REPO", "https://example.com/deploy"),
                             ("DEPLOY_SHA", "bad"), ("DEPLOY_CI_RUN", "-1"), ("DEPLOY_CI_ATTEMPT", "0")):
            with self.subTest(field=field):
                result, calls = self.run_script("dispatch_vpc_deploy.sh", **{field: value})
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls, [])

    def test_dispatch_rejects_a_public_repository_or_non_main_default_branch(self):
        self.fake_github()
        result, calls = self.run_script("dispatch_vpc_deploy.sh", FAKE_REPOSITORY_MATCH="false")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [["api", "repos/owner/deploy", "--jq", '.private == true and .default_branch == "main"']])

    def test_dispatch_only_sends_candidate_identity_and_reports_private_run(self):
        self.fake_github()
        result, calls = self.run_script("dispatch_vpc_deploy.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("123456", result.stdout)
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[1][:4], ["api", "--method", "POST",
                         "repos/owner/deploy/actions/workflows/deploy-tshb-test-vpc.yml/dispatches"])
        self.assertEqual(calls[1][8:], ["-f", "ref=main", "-f", f"inputs[sha]={self.env['DEPLOY_SHA']}",
                                        "-f", "inputs[run_id]=17", "-f", "inputs[attempt]=2"])

    def test_dispatch_error_or_missing_receipt_is_not_retried(self):
        self.fake_github()
        for env in ({"FAKE_DISPATCH_STATUS": "19"}, {"FAKE_WORKFLOW_RUN": ""}):
            with self.subTest(env=env):
                result, calls = self.run_script("dispatch_vpc_deploy.sh", **env)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertEqual(len(calls), 2)

    def test_vpc_requires_direct_private_ssh_before_connecting(self):
        interpreter = self.directory / "python3"
        interpreter.symlink_to(sys.executable)
        self.fake("ssh", "if sys.argv[1] == '-G':\n    print(os.environ['FAKE_SSH_CONFIGURATION'])\n")
        for config in ("hostname 8.8.8.8", "hostname invalid", "hostname 10.0.0.1\nproxyjump gateway",
                       "hostname 10.0.0.1\nproxycommand ssh gateway", "hostname 10.0.0.1\ncontrolpath /tmp/socket"):
            with self.subTest(config=config):
                result, calls = self.run_script("check_vpc_deploy_ssh.sh", FAKE_SSH_CONFIGURATION=config)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls, [["-G", "tshb-test"]])
        result, calls = self.run_script("check_vpc_deploy_ssh.sh", FAKE_SSH_CONFIGURATION="hostname 10.0.0.1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls[1], ["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o",
                                    "ConnectTimeout=10", "tshb-test", "true"])

    def test_runner_retries_only_known_transport_failures(self):
        (self.directory / "ops").mkdir()
        self.fake("sleep", "")
        self.fake("python3", "from pathlib import Path\n"
                             "if sys.argv[1] == '-c':\n"
                             "    os.execv(sys.executable, [sys.executable, *sys.argv[1:]])\n"
                             "directory = Path(sys.argv[sys.argv.index('--directory') + 1])\n"
                             "if directory.name == 'attempt-1':\n"
                             "    (directory / 'result.json').write_text(os.environ['FAKE_RESULT'])\n"
                             "    sys.exit(12)\n")
        for evidence, retry in (({"recovery_error": "remote_operation_busy"}, True),
                                ({"failed_stage": "artifact_upload_failed"}, True),
                                ({"failed_stage": "postflight_failed"}, False)):
            with self.subTest(evidence=evidence):
                result, calls = self.run_script("run_deploy_ci.sh", str(self.directory / "evidence"),
                                                FAKE_RESULT=json.dumps(evidence))
                self.assertEqual(result.returncode, 0 if retry else 12, result.stderr)
                controller_calls = [call for call in calls if call[:3] == ["-u", "ops/deploy_ci.py", "deploy"]]
                self.assertEqual([Path(call[-1]).name for call in controller_calls],
                                 ["attempt-1", "attempt-2"] if retry else ["attempt-1"])


if __name__ == "__main__":
    unittest.main()
