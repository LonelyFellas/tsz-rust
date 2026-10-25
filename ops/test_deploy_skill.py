import contextlib
import io
import json
import pathlib
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).parent))

import deploy_test


BASH_FENCE = re.compile(r"^[ \t]*```(?:bash|sh|shell)[ \t]*\n(.*?)^[ \t]*```[ \t]*$", re.M | re.S)


class DeploySkillTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = pathlib.Path(__file__).resolve().parent.parent
        cls.project_entry = cls.root / '.agents/skills/deploy/SKILL.md'
        cls.compatibility_entry = cls.root / '.claude/skills/deploy/SKILL.md'
        cls.runbook_path = cls.project_entry.parent / 'references/runbook.md'
        cls.runbook = cls.runbook_path.read_text(encoding='utf-8')

    def test_skill_entries_resolve_to_the_controller_runbook(self):
        for entry, expected in ((self.compatibility_entry, self.project_entry),
                                (self.project_entry, self.runbook_path),
                                (self.root / 'docs/deployment.md', self.runbook_path)):
            links = re.findall(r'\]\(([^)]+)\)', entry.read_text())
            targets = {(entry.parent / link).resolve() for link in links}
            self.assertIn(expected.resolve(), targets)
        for path in (self.project_entry, self.compatibility_entry, self.root / 'docs/deployment.md'):
            self.assertIn('ops/deploy_test.py', path.read_text())

    def test_commands_use_only_controller_for_deployment_and_recovery(self):
        examples = []
        for block in BASH_FENCE.findall(self.runbook):
            for line in block.splitlines():
                if line.startswith('python3 /absolute/session/source/ops/deploy_test.py '):
                    examples.append(shlex.split(line))
            self.assertNotRegex(block, r'(?m)^\s*(?:ssh|rsync|scp|systemctl)\s')
            self.assertNotIn('backend-deployment-manifest.py', block)
            self.assertNotIn('source ~/.config/', block)
        commands = [args[args.index('--state-dir') + 2] for args in examples]
        self.assertEqual(commands, ['plan', 'apply', 'status', 'resume'])
        # Exercise the real CLI parser and dispatch with offline adapters. A doc argument
        # drift must fail here, without calling GitHub/SSH or touching any real service.
        with tempfile.TemporaryDirectory() as directory:
            folder = pathlib.Path(directory)
            plan_file = folder / 'plan.json'
            plan_file.write_text('{}')
            identifier = '11111111-1111-4111-8111-111111111111'
            result = {'deployment_id': identifier, 'phase': 'accepted', 'status': 'accepted', 'pending': None}
            for args, name in zip(examples, commands):
                argv = [value.replace('/absolute/session/source', str(self.root))
                        .replace('/absolute/controller-state', str(folder / 'state'))
                        .replace('/absolute/session/bundle', str(folder / 'bundle'))
                        .replace('/absolute/session/plan.json', str(plan_file))
                        .replace('<deployment-uuid>', identifier) for value in args[1:]]
                if name == 'plan':
                    plan_file.unlink(missing_ok=True)
                else:
                    plan_file.write_text('{}')
                with self.subTest(command=name), patch.object(sys, 'argv', argv), \
                     patch('deployment_transport.GithubGate'), patch('deployment_transport.SshHost'), \
                     patch.object(deploy_test, 'Controller') as controller, \
                     patch.object(deploy_test, 'build_plan', return_value={'deployment_id': identifier}) as build, \
                     patch.object(deploy_test, 'atomic_json') as save, \
                     patch.object(signal, 'signal'), \
                     contextlib.redirect_stdout(io.StringIO()):
                    instance = controller.return_value
                    for method in ('apply', 'status', 'resume'):
                        getattr(instance, method).return_value = result
                    self.assertEqual(deploy_test.main(), 0)
                    if name == 'plan':
                        build.assert_called_once()
                        save.assert_called_once()
                    elif name == 'apply':
                        instance.apply.assert_called_once_with({})
                    else:
                        getattr(instance, name).assert_called_once_with(identifier)

    def test_every_bash_block_is_fail_fast_and_valid_shell(self):
        blocks = BASH_FENCE.findall(self.runbook)
        self.assertTrue(blocks)
        for block in blocks:
            with self.subTest(block=block.splitlines()[1:3]):
                self.assertEqual(block.splitlines()[0].strip(), 'set -euo pipefail')
                syntax = subprocess.run(['bash', '-n'], input=textwrap.dedent(block),
                                        text=True, capture_output=True, check=False)
                self.assertEqual(syntax.returncode, 0, syntax.stderr)

    def test_runner_routing_and_authorization_remain_host_specific(self):
        agents = (self.root / 'AGENTS.md').read_text()
        skill = self.project_entry.read_text()
        self.assertIn('Codex', agents)
        self.assertIn('Claude', agents)
        self.assertIn('backend_deploy_runner', skill)
        self.assertIn('Claude 会话', skill)
        self.assertIn('apply', skill)
        self.assertIn('resume', skill)
        paired = (self.root / '.agents/skills/contract-sync/references/paired-release.md').read_text()
        self.assertIn('ops/deploy_test.py', paired)
        self.assertNotIn('必须使用项目指定的 `backend_deploy_runner`', paired)

    def test_gate_ownership_recovery_and_cleanup_contract_is_documented(self):
        for term in ('artifact_id', 'ci.run_attempt', 'verification attempt',
                     'migration-receipt.json', 'deploy-info.json', '恢复点',
                     'COOKIE_SECURE=true', 'LOGIN_REWARD_ENABLED', 'manual_required',
                     'verified_down', 'state.json', 'owner', 'Redis', 'PITR'):
            with self.subTest(term=term):
                self.assertIn(term, self.runbook)
        self.assertIn('生产 attempt', self.runbook)
        self.assertIn('不会自动', self.runbook)
        self.assertIn('server build=0', self.runbook)
        self.assertNotIn('server_lock_result=', self.runbook)
        self.assertNotIn('deploy_backup_dir=$(ssh', self.runbook)
        self.assertNotIn('source ~/.config/tsz-rust/deploy-smoke.env', self.runbook)

    def test_real_normal_evidence_does_not_claim_real_rollback(self):
        path = self.root / 'docs/ci/evidence/2026-10-25-stage-d-manual-deployment.json'
        evidence = json.loads(path.read_text())
        self.assertEqual(evidence['status'], 'passed')
        self.assertEqual(evidence['controller_status'], 'accepted')
        self.assertFalse(evidence['failure_recovery']['triggered'])
        self.assertFalse(evidence['failure_recovery']['real_rollback_rehearsed'])
        for doc in (self.project_entry, self.runbook_path, self.root / 'docs/ci/deployment-controller.md'):
            self.assertIn('真实回退尚未演练', doc.read_text())
        self.assertFalse(evidence['automatic_cd_enabled'])


if __name__ == '__main__':
    unittest.main()
