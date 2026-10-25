import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))

import deployment_host as host
from deploy_test import identity_hash
from test_deploy_test import plan_fixture


class HostOperationTests(unittest.TestCase):
    def environment(self, root):
        return patch.multiple(host, ROOT=root / "server", LOCK=root / "server/deploy.lock",
                              BINARY=root / "server/target/release/tsz-rust", MANIFEST=root / "api.json",
                              TRANSACTIONS=root / "server/transactions")

    def prepare(self, root):
        plan, _, _ = plan_fixture(root)
        host.BINARY.parent.mkdir(parents=True)
        host.BINARY.write_bytes(b"previous-binary")
        host.MANIFEST.write_text(json.dumps(plan["baseline"]["deployment_manifest"]))
        return plan

    def acquire(self, plan):
        with patch.object(host, "baseline_matches"):
            host.dispatch("acquire", plan, None)

    def test_owner_operation_lock_and_full_plan_hash_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.environment(root):
                plan = self.prepare(root)
                self.acquire(plan)
                value = host.state(plan)
                self.assertEqual(value["plan_sha256"], identity_hash(plan))
                changed = dict(plan, artifact_sha256="0" * 64)
                with self.assertRaisesRegex(host.HostError, "remote_state_mismatch"):
                    host.state(changed)
                with host.operation(plan), self.assertRaisesRegex(host.HostError, "remote_operation_busy"):
                    host.observe(plan)
                (host.LOCK / "owner").write_text("another-owner")
                with self.assertRaisesRegex(host.HostError, "owner_mismatch"):
                    host.dispatch("stop", plan, None)

    def test_upload_is_owned_digest_checked_and_interrupt_leaves_no_live_change(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.environment(root):
                plan = self.prepare(root)
                self.acquire(plan)
                original = host.BINARY.read_bytes()
                host.dispatch("backup", plan, None)
                with patch.object(host.sys, "stdout", io.StringIO()), self.assertRaises(SystemExit):
                    host.receive(plan, io.BytesIO(b"partial wrong artifact"))
                self.assertEqual(host.BINARY.read_bytes(), original)
                self.assertEqual(host.state(plan)["pending"], "upload")
                with patch.object(host, "baseline_matches"):
                    host.dispatch("abort", plan, None)
                self.assertFalse(host.LOCK.exists())
                self.assertTrue((host.transaction(plan) / "incoming").is_file())

    def test_backup_and_atomic_install_bind_original_and_ci_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.environment(root):
                plan = self.prepare(root)
                self.acquire(plan)
                host.dispatch("backup", plan, None)
                with patch.object(host.sys, "stdout", io.StringIO()):
                    host.receive(plan, io.BytesIO(Path(plan["artifact_path"]).read_bytes()))
                # Stop is the explicit mutation boundary, simulated locally.
                value = host.state(plan)
                value["mutation_started"] = True
                value["completed"].append("stop")
                host.atomic(host.state_path(plan), value)
                host.dispatch("install", plan, None)
                self.assertEqual(host.sha(host.BINARY), plan["artifact_sha256"])
                self.assertFalse(host.MANIFEST.exists())
                self.assertEqual((host.transaction(plan) / "backup/tsz-rust").read_bytes(), b"previous-binary")

    def test_failed_migration_recovery_never_restores_old_binary_or_unlocks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.environment(root):
                plan = self.prepare(root)
                self.acquire(plan)
                host.dispatch("backup", plan, None)
                plan["recovery_policy"] = None
                # Match persisted plan after controlled fixture change.
                value = json.loads(host.state_path(plan).read_text())
                value.update(plan_sha256=identity_hash(plan), mutation_started=True)
                host.atomic(host.state_path(plan), value)
                advanced = plan["baseline"]["postgresql"]["migration_ledger"] + [{"version": plan["migration"]["baseline_version"] + 1, "checksum": "a" * 96, "success": True}]
                plan["migration"]["candidate_version"] = advanced[-1]["version"]
                plan["migration"]["versions"] = [advanced[-1]["version"]]
                plan["migration"]["migrations"] = [{"up": {"version": advanced[-1]["version"], "checksum": "a" * 96}}]
                value["plan_sha256"] = identity_hash(plan)
                host.atomic(host.state_path(plan), value)
                current = host.BINARY.read_bytes()
                with patch.object(host, "stop_candidate"), patch.object(host, "run", return_value=b""), patch.object(host, "cleanup_business"), patch.object(host, "ledger", return_value=advanced), patch.object(host.manifests, "restore_deployment") as restore:
                    with self.assertRaisesRegex(host.HostError, "manual_migration_recovery_required"):
                        host.dispatch("recover", plan, None)
                    restore.assert_not_called()
                self.assertEqual(host.BINARY.read_bytes(), current)
                self.assertTrue(host.LOCK.exists())

    def test_accept_requires_completed_fixture_cleanup_and_process_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.environment(root):
                plan = self.prepare(root)
                self.acquire(plan)
                value = host.state(plan)
                value["completed"] = ["backup", "upload", "stop", "install", "candidate_start", "candidate_verify", "formal_start", "formal_verify", "business_verify"]
                host.atomic(host.state_path(plan), value)
                with patch.object(host.auth, "check_formal") as process, patch.object(host.manifests, "create_manifest") as create:
                    with self.assertRaisesRegex(host.HostError, "business_cleanup_not_complete"):
                        host.dispatch("accept", plan, None)
                    process.assert_not_called()
                    create.assert_not_called()

    def test_mutation_subprocess_inherits_operation_lock_and_order_cannot_be_skipped(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.environment(root):
                plan = self.prepare(root)
                self.acquire(plan)
                with self.assertRaisesRegex(host.HostError, 'forward_step_out_of_order'):
                    host.dispatch('accept', plan, None)
                import subprocess
                with host.operation(plan), patch.object(host.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'')) as run:
                    host.run(['synthetic-command'])
                    self.assertEqual(len(run.call_args.kwargs['pass_fds']), 1)
                self.assertEqual(host.OPERATION_FDS, [])

    def test_recovery_acceptance_checks_full_manifest_and_owned_smoke_receipts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.environment(root):
                plan = self.prepare(root)
                self.acquire(plan)
                value = host.state(plan)
                value['completed'] = ['backup','upload','stop','install','candidate_start','candidate_verify','formal_start','formal_verify','business_verify']
                value['business'] = {'cleanup_complete': True}
                value['auth_fixtures'] = {mode: {'cleanup_complete': True} for mode in ('candidate','formal')}
                evidence = {key: plan['ci'][key] for key in ('run_attempt','artifact_id','verification_attempt','verification_receipt_sha256')}
                evidence['artifact_sha256'] = plan['artifact_sha256']
                host.BINARY.write_bytes(Path(plan['artifact_path']).read_bytes())
                manifest = host.manifests.create_manifest(component='api', repository='LonelyFellas/tsz-rust',
                    git_sha=plan['source']['git_sha'],git_tree=plan['source']['git_tree'],ci_run_id=plan['ci']['run_id'],
                    ci_run_url=plan['ci']['run_url'],artifact=host.BINARY,artifact_path=str(host.manifests.EXPECTED_ARTIFACT_PATH),
                    output=host.MANIFEST,release_evidence=evidence)
                with patch.object(host, 'ledger', return_value=plan['baseline']['postgresql']['migration_ledger']):
                    self.assertTrue(host.accepted_identity(plan,manifest,value))
                    manifest['ci']['verification_receipt_sha256'] = '0'*64
                    self.assertFalse(host.accepted_identity(plan,manifest,value))

    def test_configuration_is_parsed_never_executed_and_unknown_expansion_blocks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.environment(root):
                host.ROOT.mkdir()
                (host.ROOT / ".env").write_text("COOKIE_SECURE=true\nDATABASE_URL='synthetic value'\n")
                self.assertEqual(host.configuration()["DATABASE_URL"], "synthetic value")
                (host.ROOT / ".env").write_text("COOKIE_SECURE=true\nJWT_SECRET=$(touch should-not-exist)\n")
                with self.assertRaises(host.HostError):host.configuration()
                self.assertFalse((root / "should-not-exist").exists())


if __name__ == "__main__":
    unittest.main()
