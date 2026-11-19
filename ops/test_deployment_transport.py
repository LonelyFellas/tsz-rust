import copy
import hashlib
import json
from pathlib import Path
import shlex
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).parent))

from deploy_test import DeploymentError
import deployment_transport as transport
from test_deploy_test import plan_fixture


class GateTests(unittest.TestCase):
    def test_only_known_parallel_deployment_errors_are_reported_as_waitable(self):
        for stage, expected in [('deployment_in_progress', 'deployment_in_progress'), ('other', 'transport_failed')]:
            response = transport.subprocess.CompletedProcess([], 1, json.dumps({'status': 'failed', 'stage': stage}), 'private detail')
            with self.subTest(stage=stage), patch.object(transport.subprocess, 'run', return_value=response), \
                 self.assertRaisesRegex(DeploymentError, expected):
                transport.command(['metadata'], failure_stages=('deployment_in_progress',))

    def fixture(self, root):
        plan, _, _ = plan_fixture(root)
        sha = plan['source']['git_sha']
        metadata = json.loads(Path(plan['artifact_metadata_path']).read_text())
        runs = {'workflow_runs': [{'workflow_id': 8, 'name': 'CI', 'id': plan['ci']['run_id'],
                 'run_attempt': 2, 'status': 'completed', 'conclusion': 'success',
                 'head_sha': sha, 'head_branch': 'main', 'event': 'push', 'created_at': '2026-10-25T00:00:00Z',
                 'repository': {'full_name': transport.REPOSITORY}, 'head_repository': {'full_name': transport.REPOSITORY}}]}
        artifacts = {'artifacts': [{'id': 102, 'name': 'release-smoke-' + sha + '-attempt-2', 'expired': False}]}
        def api(path):
            if path == 'branches/main':return {'commit': {'sha': sha}}
            if path == 'actions/workflows':return {'workflows': [{'id': 8, 'path': '.github/workflows/ci.yml', 'name': 'CI'}]}
            if path.startswith('actions/runs?'):return copy.deepcopy(runs)
            if path == 'actions/artifacts/101':return copy.deepcopy(metadata)
            if path.endswith('/artifacts?per_page=100'):return copy.deepcopy(artifacts)
            raise AssertionError('unexpected API: ' + path)
        def command(args, **kwargs):
            if args[0] == 'git':
                tail = args[3:]
                if tail == ['branch', '--show-current']:return 'main\n'
                if tail == ['rev-parse', 'HEAD']:return sha + '\n'
                if tail == ['rev-parse', 'HEAD^{tree}']:return plan['source']['git_tree'] + '\n'
                if tail == ['status', '--porcelain']:return ''
                if tail[:1] == ['ls-tree']:return 'migrations/1_base.up.sql\nmigrations/1_base.down.sql\n'
            if args[:3] == ['gh', 'run', 'download']:
                destination = Path(args[args.index('--dir')+1])
                (destination/'release-smoke-receipt.json').write_bytes(Path(plan['verification_receipt_path']).read_bytes())
                return ''
            raise AssertionError('unexpected command: ' + str(args))
        return plan, api, command, runs, metadata, artifacts

    def test_ci_gate_revalidates_exact_run_artifact_receipt_and_migrations(self):
        with tempfile.TemporaryDirectory() as directory:
            plan, api, command, _, _, _ = self.fixture(Path(directory))
            import migration_compatibility
            with patch.object(transport.GithubGate, 'api', side_effect=api), patch.object(transport, 'command', side_effect=command), patch.object(transport.subprocess, 'check_output', return_value=b'migration'), patch.object(transport, 'migration_interval', return_value=plan['migration']):
                transport.GithubGate().check(plan)

    def test_wrong_event_failed_or_rerun_ci_never_reaches_server(self):
        for changed in ({'conclusion': 'failure'}, {'event':'pull_request'}, {'run_attempt':3}, {'head_branch':'feature'}):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as directory:
                plan, api, command, runs, _, _ = self.fixture(Path(directory))
                runs['workflow_runs'][0].update(changed)
                with patch.object(transport.GithubGate, 'api', side_effect=api), patch.object(transport, 'command', side_effect=command), self.assertRaisesRegex(DeploymentError,'exact_ci_changed'):
                    transport.GithubGate().check(plan)

    def test_expired_artifact_or_missing_latest_receipt_is_rejected(self):
        for field in ('expired','receipt'):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                plan, api, command, _, metadata, artifacts = self.fixture(Path(directory))
                if field=='expired':metadata['expired']=True
                else:artifacts['artifacts']=[]
                with patch.object(transport.GithubGate,'api',side_effect=api), patch.object(transport,'command',side_effect=command), self.assertRaises(Exception):
                    transport.GithubGate().check(plan)

    def test_transport_keeps_payload_out_of_argv_and_satisfies_module_imports(self):
        with patch.object(transport, 'command', return_value='{"ok":true,"result":{"ready":true}}') as command:
            self.assertEqual(transport.SshHost().request('preflight', {'source':'synthetic'}), {'ready':True})
        argv = command.call_args.args[0]
        self.assertEqual(argv[-2], 'tshb-test')
        self.assertNotIn('synthetic',' '.join(argv))
        payload=json.loads(command.call_args.kwargs['data'])
        self.assertEqual(set(payload['modules']),set(transport.MODULES))
        bootstrap=shlex.split(argv[-1])[-1]
        compile(bootstrap,'bootstrap','exec')
        self.assertNotIn('source /opt',bootstrap)

    def test_upload_retries_remote_operation_busy(self):
        host = transport.SshHost()
        host._upload_once = Mock(
            side_effect=[DeploymentError("remote_operation_busy"), None]
        )
        with (
            patch.object(transport.time, "monotonic", side_effect=[0, 1, 2, 3]),
            patch.object(transport.time, "sleep") as sleep,
        ):
            host._upload({"artifact_path": "/tmp/unused"})
        self.assertEqual(host._upload_once.call_count, 2)
        sleep.assert_called_once_with(10)

    def test_upload_does_not_repeat_a_timed_out_attempt_inside_the_controller(self):
        host = transport.SshHost()
        host._upload_once = Mock(side_effect=DeploymentError("artifact_upload_failed"))
        with self.assertRaisesRegex(DeploymentError, "artifact_upload_failed"):
            host._upload({"artifact_path": "/tmp/unused"})
        self.assertEqual(host._upload_once.call_count, 1)

    def test_upload_bounds_remote_receiver_and_allows_it_to_release_the_lock(self):
        process = Mock(returncode=0)
        process.communicate.return_value = (b'{"ok": true}', b"")
        host = transport.SshHost()
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "artifact"
            artifact.write_bytes(b"artifact")
            with patch.object(transport.subprocess, "Popen", return_value=process) as popen:
                host._upload_once({"artifact_path": str(artifact)})
        command = popen.call_args.args[0]
        remote_command = command[-1]
        self.assertIn("/usr/bin/timeout", remote_command)
        self.assertIn("--kill-after=5s", remote_command)
        self.assertIn(f"{transport.REMOTE_UPLOAD_TIMEOUT_SECONDS}s python3", remote_command)
        payload = process.communicate.call_args.kwargs["input"]
        self.assertIn(b'"compression": "gzip"', payload.splitlines()[0])
        self.assertEqual(
            process.communicate.call_args.kwargs["timeout"],
            transport.REMOTE_UPLOAD_TIMEOUT_SECONDS + transport.LOCAL_UPLOAD_GRACE_SECONDS,
        )


if __name__ == '__main__':
    unittest.main()
