"""Connect the controller to real host state/files/locks with only service IO replaced."""

import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))

from deploy_test import Controller, DeploymentError
import deployment_host as host
from test_deploy_test import plan_fixture
from test_release_artifact_smoke import INFO, LEDGER


class LocalHostAdapter:
    def __init__(self, fail=None, crash_after=None):
        self.fail = fail
        self.crash_after = crash_after
        self.calls = []

    def call(self, action, plan):
        try:
            return host.dispatch(action, plan, None)
        except host.HostError as error:
            raise DeploymentError(str(error)) from error

    def preflight(self, plan):self.call('preflight', plan)
    def acquire(self, plan):self.call('acquire', plan)
    def recheck_baseline(self, plan):self.call('recheck_baseline', plan)
    def observe(self, plan):return self.call('observe', plan)
    def finish_unlock(self, plan):self.call('finish_unlock', plan)
    def abort_unmutated(self, plan):self.call('abort', plan)
    def recover(self, plan):self.call('recover', plan)

    def action(self, action, plan):
        self.calls.append(action)
        if action == self.fail:
            raise DeploymentError(action + '_transport_failed')
        if action == 'upload':
            with contextlib.redirect_stdout(io.StringIO()):
                host.receive(plan, io.BytesIO(Path(plan['artifact_path']).read_bytes()))
        else:
            self.call(action, plan)
        if action == self.crash_after:
            raise SystemExit('runner_disconnected')


class DeploymentTransactionTests(unittest.TestCase):
    @contextlib.contextmanager
    def transaction(self, root, *, fail=None, crash_after=None):
        plan, _, gate = plan_fixture(root)
        service = {'running': True}
        server = root/'server'
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.multiple(host, ROOT=server, LOCK=server/'deploy.lock',
                BINARY=server/'target/release/tsz-rust', MANIFEST=server/'api.json',
                TRANSACTIONS=server/'transactions'))
            host.BINARY.parent.mkdir(parents=True)
            host.BINARY.write_bytes(b'previous-binary')
            host.MANIFEST.write_text(json.dumps(plan['baseline']['deployment_manifest']))
            def baseline(candidate):
                host.require(host.sha(host.BINARY)==candidate['baseline']['service']['file_sha256'], 'baseline_binary_changed')
                host.require(json.loads(host.MANIFEST.read_text())==candidate['baseline']['deployment_manifest'], 'baseline_manifest_changed')
                host.require(service['running'], 'baseline_service_stopped')
            def preflight(candidate):
                host.require(not host.LOCK.exists(), 'remote_deployment_busy')
                baseline(candidate)
            def run(args, **kwargs):
                if args[:2]==['systemctl','stop']:
                    service['running']=False
                    return b''
                if args[:2]==['systemctl','start']:
                    service['running']=True
                    return b''
                if args[:2]==['systemctl','show']:
                    return b'42' if service['running'] else b'0'
                if args[-1:] == ['deploy-info']:
                    return json.dumps(INFO).encode()
                raise AssertionError('Unexpected service command: '+str(args))
            def candidate_start(candidate):
                host.require(not service['running'], 'formal_service_not_stopped')
                host.require(host.sha(host.BINARY)==candidate['artifact_sha256'], 'candidate_digest_mismatch')
                value=host.state(candidate)
                value['candidate_pid']=99
                host.atomic(host.state_path(candidate),value)
            def candidate_stop(candidate):
                value=host.state(candidate)
                value['candidate_pid']=None
                host.atomic(host.state_path(candidate),value)
            def auth_smoke(candidate, mode, credentials=None):
                value=host.state(candidate)
                value.setdefault('auth_fixtures',{})[mode]={'cleanup_complete':True}
                host.atomic(host.state_path(candidate),value)
            def business_smoke(candidate):
                value=host.state(candidate)
                value['business']={'cleanup_complete':True}
                host.atomic(host.state_path(candidate),value)
            def formal(expected):
                host.require(service['running'] and host.sha(host.BINARY)==expected,'formal_identity_mismatch')
                return 42
            for name, replacement in [('baseline_matches',baseline),('preflight',preflight),
                                      ('run',run),('start_candidate',candidate_start),
                                      ('stop_candidate',candidate_stop),('auth_smoke',auth_smoke),
                                      ('business_smoke',business_smoke)]:
                stack.enter_context(patch.object(host,name,side_effect=replacement))
            stack.enter_context(patch.object(host,'wait_formal',side_effect=formal))
            stack.enter_context(patch.object(host.auth,'check_formal',side_effect=formal))
            stack.enter_context(patch.object(host,'ledger',return_value=LEDGER))
            adapter=LocalHostAdapter(fail=fail,crash_after=crash_after)
            yield plan, Controller(root/'state',gate,adapter), adapter, service

    def test_controller_and_host_complete_actual_file_transaction(self):
        with tempfile.TemporaryDirectory() as directory, self.transaction(Path(directory)) as (plan,controller,adapter,service):
            result=controller.apply(plan)
            self.assertEqual(result['status'],'accepted')
            self.assertEqual(host.sha(host.BINARY),plan['artifact_sha256'])
            manifest=host.manifests.verify_manifest(host.MANIFEST,host.BINARY)
            self.assertEqual(manifest['schema_version'],2)
            self.assertEqual(manifest['ci']['artifact_id'],plan['ci']['artifact_id'])
            self.assertTrue(service['running'])
            self.assertFalse(host.LOCK.exists())
            self.assertTrue((host.transaction(plan)/'released-lock/owner').is_file())
            self.assertFalse((controller.directory/'active').exists())

    def test_formal_verification_failure_restores_exact_v1_backup_and_unlocks(self):
        with tempfile.TemporaryDirectory() as directory, self.transaction(Path(directory),fail='formal_verify') as (plan,controller,adapter,service):
            result=controller.apply(plan)
            self.assertEqual(result['status'],'recovered')
            self.assertEqual(host.BINARY.read_bytes(),b'previous-binary')
            self.assertEqual(json.loads(host.MANIFEST.read_text()),plan['baseline']['deployment_manifest'])
            self.assertTrue(service['running'])
            self.assertFalse(host.LOCK.exists())
            self.assertFalse((controller.directory/'active').exists())

    def test_interrupted_candidate_is_recovered_without_replaying_install(self):
        with tempfile.TemporaryDirectory() as directory, self.transaction(Path(directory),crash_after='candidate_start') as (plan,controller,adapter,service):
            with self.assertRaises(SystemExit):controller.apply(plan)
            self.assertTrue(host.LOCK.exists())
            calls=list(adapter.calls)
            result=controller.resume(plan['deployment_id'])
            self.assertEqual(result['status'],'recovered')
            self.assertEqual(adapter.calls,calls)
            self.assertEqual(host.BINARY.read_bytes(),b'previous-binary')
            self.assertTrue(service['running'])
            self.assertFalse(host.LOCK.exists())


if __name__=='__main__':
    unittest.main()
