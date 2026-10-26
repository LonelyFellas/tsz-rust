import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import uuid

sys.path.insert(0, str(Path(__file__).parent))

from deploy_test import Controller, DeploymentError, STEPS, atomic_json, build_plan, check_plan
from test_release_artifact_smoke import fixture, INFO, LEDGER
from test_deployment_manifest import create_args
from deployment_manifest import create_manifest
from artifact_test_runtime import VerificationError, digest


def plan_fixture(root):
    args = fixture(root)
    receipt = {"status": "passed", "source": json.loads(args.manifest.read_text())["source"],
               "ci": json.loads(args.manifest.read_text())["ci"], "artifact_id": 101,
               "verification_attempt": 2, "artifact_sha256": digest(args.artifact),
               "checks": {key: "passed" for key in ("startup", "health", "readiness", "embedded_migrations", "auth", "persistence", "redis_unavailable_rejected", "migration_failure_rejected")},
               "cleanup": {"complete": True, "remaining": []}}
    (root / "tsz-rust.manifest.json").write_text(args.manifest.read_text())
    (root / "artifact.json").write_text(args.artifact_metadata.read_text())
    (root / "release-smoke-receipt.json").write_text(json.dumps(receipt))
    (root / "deploy-info.json").write_text(json.dumps(INFO))
    old_dir = root / "previous"
    old_dir.mkdir()
    old_args = create_args(old_dir)
    old_args["git_sha"] = "c" * 40
    old_args["artifact"].write_bytes(b"previous-binary")
    baseline_manifest = create_manifest(**old_args)
    baseline = {"deployment_manifest": baseline_manifest,
                "service": {"file_sha256": baseline_manifest["artifact"]["sha256"], "process_sha256": baseline_manifest["artifact"]["sha256"]},
                "postgresql": {"migration_ledger": LEDGER, "transaction_read_only": "on"},
                "pid_stable": True, "deployment_lock_present": False}
    host = FakeHost(baseline)
    gate = FakeGate()
    return build_plan(root, gate, host), host, gate


class FakeGate:
    def __init__(self):
        self.calls = 0
        self.fail_call = None

    def check(self, plan):
        self.calls += 1
        if self.calls == self.fail_call:
            raise DeploymentError("main_advanced")


class FakeHost:
    def __init__(self, baseline):
        self.value = baseline
        self.owner = None
        self.mutated = False
        self.accepted = False
        self.recovered = False
        self.actions = []
        self.fail = None
        self.crash_after = None
        self.recovery_fail = False
        self.lose_acquire_response = False

    def baseline(self):
        return copy.deepcopy(self.value)

    def preflight(self, plan):
        if self.owner:
            raise DeploymentError("remote_deployment_busy")

    def acquire(self, plan):
        self.owner = plan["deployment_id"]
        if self.lose_acquire_response:
            raise DeploymentError("transport_failed")

    def recheck_baseline(self, plan):
        if self.fail == "baseline":
            raise DeploymentError("baseline_changed")

    def action(self, step, plan):
        self.actions.append(step)
        if self.fail == step:
            raise DeploymentError(step + "_failed")
        if step == "stop":self.mutated = True
        if step == "accept":self.accepted = True
        if step == "unlock":self.owner = None
        if self.crash_after == step:
            raise SystemExit("simulated_runner_loss")

    def observe(self, plan):
        return {"owner": self.owner, "mutation_started": self.mutated,
                "accepted": self.accepted, "acceptance_matches": self.accepted,
                "recovered": self.recovered, "recovery_matches": self.recovered}

    def finish_unlock(self, plan):
        self.owner = None

    def abort_unmutated(self, plan):
        if self.mutated:raise AssertionError("must not abort mutation")
        self.owner = None

    def recover(self, plan):
        if self.recovery_fail:
            raise DeploymentError("data_guard_refused")
        self.recovered = True
        self.owner = None


class ControllerTests(unittest.TestCase):
    def test_success_persists_intent_result_and_accepts_only_after_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            controller = Controller(root / "state", gate, host)
            result = controller.apply(plan)
            self.assertEqual(result["status"], "accepted")
            self.assertEqual(host.actions, list(STEPS))
            self.assertIn('preparation_timings', plan)
            self.assertEqual(result['scheduled_backup']['status'], 'unknown')
            self.assertTrue(all(row['status'] == 'passed' for row in result['timings']))
            self.assertIn('upload', {row['phase'] for row in result['timings']})
            self.assertFalse((root / "state/active").exists())
            self.assertEqual(controller.status(plan["deployment_id"])["status"], "accepted")
            self.assertNotIn("password", controller.path(plan["deployment_id"]).read_text())
            self.assertLess(host.actions.index("business_verify"), host.actions.index("accept"))

    def test_total_deadline_stops_before_mutation_and_keeps_recovery_available(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            clock = [0]
            def expired():
                clock[0] += 1000
                return clock[0]
            with patch('deploy_test.time.monotonic', side_effect=expired):
                result = Controller(root / 'state', gate, host).apply(plan)
            self.assertEqual(result['status'], 'blocked')
            self.assertEqual(result['failed_stage'], 'deployment_deadline_exceeded')
            self.assertFalse(host.mutated)
            self.assertEqual(host.actions, [])

    def test_every_forward_failure_recovers_or_aborts_without_replaying(self):
        for stage in STEPS:
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                plan, host, gate = plan_fixture(root)
                host.fail = stage
                controller = Controller(root / "state", gate, host)
                result = controller.apply(plan)
                expected = "accepted" if stage == "unlock" else "recovered" if host.mutated else "blocked"
                self.assertEqual(result["status"], expected)
                self.assertIsNone(host.owner)
                self.assertEqual(host.actions.count(stage), 1)
                self.assertFalse((root / "state/active").exists())

    def test_moving_main_before_mutation_aborts_but_after_mutation_is_pinned(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            gate.fail_call = gate.calls + 2
            result = Controller(root / "state", gate, host).apply(plan)
            self.assertEqual(result["status"], "blocked")
            self.assertNotIn("stop", host.actions)
            self.assertFalse(host.mutated)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            gate.fail_call = gate.calls + 3
            result = Controller(root / "state", gate, host).apply(plan)
            self.assertEqual(result["status"], "accepted")

    def test_runner_loss_after_every_step_uses_observation_not_step_replay(self):
        for stage in STEPS:
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                plan, host, gate = plan_fixture(root)
                host.crash_after = stage
                controller = Controller(root / "state", gate, host)
                with self.assertRaises(SystemExit):controller.apply(plan)
                self.assertTrue((root / "state/active").exists())
                count = len(host.actions)
                result = Controller(root / "state", gate, host).resume(plan["deployment_id"])
                self.assertIn(result["status"], {"accepted", "recovered", "blocked"})
                self.assertEqual(len(host.actions), count)
                self.assertIsNone(host.owner)

    def test_failed_recovery_retains_lock_and_can_resume_recovery(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            host.fail = "candidate_verify"
            host.recovery_fail = True
            controller = Controller(root / "state", gate, host)
            result = controller.apply(plan)
            self.assertEqual(result["status"], "manual_required")
            self.assertEqual(host.owner, plan["deployment_id"])
            self.assertTrue((root / "state/active").exists())
            host.recovery_fail = False
            result = controller.resume(plan["deployment_id"])
            self.assertEqual(result["status"], "recovered")
            self.assertEqual(host.actions.count("candidate_verify"), 1)

    def test_other_owner_and_parallel_local_writer_are_never_stolen(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            controller = Controller(root / "state", gate, host)
            host.fail = "formal_verify"
            host.recovery_fail = True
            controller.apply(plan)
            host.owner = str(uuid.uuid4())
            result = controller.resume(plan["deployment_id"])
            self.assertEqual(result["status"], "manual_required")
            self.assertNotEqual(host.owner, plan["deployment_id"])
            with controller.writer(plan["deployment_id"]), self.assertRaisesRegex(DeploymentError, "local_writer_busy"):
                Controller(root / "state", gate, host).resume(plan["deployment_id"])

    def test_initialization_can_resume_before_or_after_active_acquisition(self):
        for acquired in (False, True):
            with self.subTest(acquired=acquired), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                plan, host, gate = plan_fixture(root)
                controller = Controller(root / 'state', gate, host)
                acquire = controller.active
                def interrupted(owner, action):
                    if acquired:
                        acquire(owner, action)
                    raise SystemExit('initialization_interrupted')
                with patch.object(controller, 'active', side_effect=interrupted), self.assertRaises(SystemExit):
                    controller.apply(plan)
                self.assertTrue(controller.path(plan['deployment_id']).exists())
                self.assertEqual(controller.resume(plan['deployment_id'])['status'], 'blocked')
                self.assertFalse((root / 'state/active').exists())
                self.assertEqual(host.actions, [])

    def test_lost_acquire_response_is_cleanly_aborted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            host.lose_acquire_response = True
            result = Controller(root / "state", gate, host).apply(plan)
            self.assertEqual(result["status"], "blocked")
            self.assertIsNone(host.owner)

    def test_tampered_plan_artifact_or_verification_never_writes_remote(self):
        for field in ("artifact", "receipt", "baseline"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                plan, host, gate = plan_fixture(root)
                if field == "artifact":Path(plan["artifact_path"]).write_bytes(b"tampered")
                elif field == "receipt":Path(plan["verification_receipt_path"]).write_text('{}')
                else:plan["baseline"]["service"]["process_sha256"] = "0" * 64
                with self.assertRaises(Exception):Controller(root / "state", gate, host).apply(plan)
                self.assertIsNone(host.owner)
                self.assertEqual(host.actions, [])

    def test_artifact_identity_error_before_stop_enters_unmutated_abort(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            with patch.object(gate, 'check', side_effect=[None, VerificationError('artifact_metadata_mismatch')]):
                result = Controller(root / 'state', gate, host).apply(plan)
            self.assertEqual(result['status'], 'blocked')
            self.assertEqual(result['failed_stage'], 'artifact_metadata_mismatch')
            self.assertEqual(host.actions, ['backup', 'upload'])
            self.assertIsNone(host.owner)
            self.assertFalse((root / 'state/active').exists())

    def test_plan_source_and_ci_identity_must_match_the_artifact_manifest(self):
        for section, key, value in [('source', 'repository', 'other/repository'), ('ci', 'workflow', 'other')]:
            with self.subTest(section=section), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                plan, host, gate = plan_fixture(root)
                plan[section][key] = value
                with self.assertRaisesRegex(DeploymentError, 'plan_differs_from_artifact_identity'):
                    Controller(root / 'state', gate, host).apply(plan)
                self.assertIsNone(host.owner)
                self.assertEqual(host.actions, [])

    def test_changed_tools_and_missing_migration_evidence_block_before_host_write(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            with patch('deploy_test.tools_identity', return_value={'different': 'tool'}):
                with self.assertRaisesRegex(DeploymentError, 'deployment_tools_changed'):
                    Controller(root/'state', gate, host).apply(plan)
            self.assertIsNone(host.owner)
            plan['migration'].update(candidate_version=2, versions=[2])
            with self.assertRaises(FileNotFoundError):
                Controller(root/'state', gate, host).apply(plan)
            self.assertIsNone(host.owner)

    def test_manual_recovery_accepts_atomic_refusal_evidence_but_never_enables_down(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            plan['migration'].update(candidate_version=2, versions=[2], classification='new_migrations')
            manifest = json.loads(Path(plan['manifest_path']).read_text())
            evidence = {
                'status':'blocked','failed_stage':'recovery_refused_requires_manual_plan',
                'cleanup':{'complete':True},'candidate_source':manifest['source'],'candidate_ci':manifest['ci'],
                'candidate_artifact_id':plan['ci']['artifact_id'],'candidate_artifact_sha256':plan['artifact_sha256'],
                'baseline_ledger':plan['baseline']['postgresql']['migration_ledger'],'interval':plan['migration'],
                'baseline_source':plan['baseline']['deployment_manifest']['source'],
                'baseline_artifact_sha256':plan['baseline']['service']['file_sha256'],
                'checks':{'upgrade_and_persistence':'passed','recovery_refusal_atomic':'passed'},
            }
            Path(plan['migration_receipt_path']).write_text(json.dumps(evidence))
            plan['recovery_policy'] = {
                'kind':'manual','baseline_sha':plan['baseline']['deployment_manifest']['source']['git_sha'],
                'candidate_sha':manifest['source']['git_sha'],'baseline_version':1,'candidate_version':2,
                'approval_evidence_sha256':'e'*64,
            }
            check_plan(plan)
            plan['recovery_policy']['kind']='verified_down'
            with self.assertRaisesRegex(DeploymentError,'migration_rehearsal_mismatch'):
                check_plan(plan)

    def test_recovery_completed_before_response_loss_does_not_repeat_restore(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            host.crash_after = 'formal_verify'
            controller = Controller(root/'state', gate, host)
            with self.assertRaises(SystemExit):controller.apply(plan)
            host.recovered = True
            host.owner = None
            with patch.object(host, 'recover') as recover:
                result = controller.resume(plan['deployment_id'])
                recover.assert_not_called()
            self.assertEqual(result['status'], 'recovered')

    def test_terminal_state_persisted_before_unlock_response_is_reentrant(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            controller = Controller(root/'state', gate, host)
            release = controller.active
            def crash_before_local_release(owner, acquire):
                if not acquire:raise SystemExit('local_unlock_interrupted')
                return release(owner, acquire)
            with patch.object(controller, 'active', side_effect=crash_before_local_release):
                with self.assertRaises(SystemExit):controller.apply(plan)
            self.assertEqual(controller.status(plan['deployment_id'])['status'], 'accepted')
            result = Controller(root/'state', gate, host).resume(plan['deployment_id'])
            self.assertEqual(result['status'], 'accepted')
            self.assertFalse((root/'state/active').exists())

    def test_direct_cli_transport_error_enters_recovery_without_network(self):
        import os
        import subprocess
        script = str(Path(__file__).with_name('deploy_test.py'))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, _, _ = plan_fixture(root)
            atomic_json(root / 'plan.json', plan)
            tools = root / 'tools'
            tools.mkdir()
            for name, body in {
                'gh': "import json; print(json.dumps({'commit':{'sha':'0'*40}}))",
                'ssh': "import json; print(json.dumps({'ok':True,'result':{'owner':None,'mutation_started':False}}))",
            }.items():
                path = tools / name
                path.write_text('#!' + sys.executable + '\n' + body + '\n')
                path.chmod(0o755)
            env = {**os.environ, 'PATH': str(tools) + os.pathsep + os.environ['PATH']}
            result = subprocess.run([sys.executable, script, '--state-dir', str(root/'state'),
                                     'apply', '--plan', str(root/'plan.json')], capture_output=True, text=True, env=env)
            value = json.loads((root/'state'/plan['deployment_id']/'state.json').read_text())
            self.assertEqual(value['failed_stage'], 'main_advanced')
            self.assertEqual(value['status'], 'blocked')
            self.assertFalse((root/'state/active').exists())
        self.assertEqual(result.returncode, 1)
        self.assertIn('"status": "blocked"', result.stdout)
        self.assertNotIn('Traceback', result.stderr)

    def test_resume_refuses_modified_persisted_plan(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan, host, gate = plan_fixture(root)
            host.crash_after = "install"
            controller = Controller(root / "state", gate, host)
            with self.assertRaises(SystemExit):controller.apply(plan)
            path = controller.path(plan["deployment_id"])
            value = json.loads(path.read_text())
            value["plan"]["artifact_sha256"] = "0" * 64
            atomic_json(path, value)
            with self.assertRaisesRegex(DeploymentError, "state_plan_mismatch"):
                controller.resume(plan["deployment_id"])


if __name__ == "__main__":
    unittest.main()
