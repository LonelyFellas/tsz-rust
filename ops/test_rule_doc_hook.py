import os
import pathlib
import subprocess
import tempfile
import unittest


class RuleDocHookTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.git('init', '-q')
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.log = self.root / 'cargo.log'
        cargo = self.bin / 'cargo'
        cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CARGO_LOG"\nexit "${CARGO_EXIT:-0}"\n')
        cargo.chmod(0o755)
        self.env = dict(os.environ, PATH=f'{self.bin}:{os.environ["PATH"]}', CARGO_LOG=str(self.log))
        self.hook = pathlib.Path(__file__).resolve().parent.parent / '.githooks/pre-commit'
        # The normal hook stages SQLx output. Keep a tracked cache fixture available.
        self.stage('.sqlx/query.json')
        tree = self.git('write-tree')
        commit = subprocess.check_output(
            ['git', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
             'commit-tree', tree, '-m', 'baseline'], cwd=self.repo, text=True,
        ).strip()
        self.git('update-ref', 'HEAD', commit)

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.repo, text=True).strip()

    def stage(self, path):
        file = self.repo / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(file.read_text() + 'changed\n' if file.exists() else 'fixture\n')
        self.git('add', '--', path)

    def run_hook(self):
        return subprocess.run(['sh', str(self.hook)], cwd=self.repo, env=self.env,
                              capture_output=True, text=True)

    def test_rules_only_do_not_need_database_or_cargo(self):
        for path in ['AGENTS.md', '.agents/skills/build/SKILL.md',
                     '.claude/skills/deploy/references/runbook.md']:
            self.stage(path)
        self.env['CARGO_EXIT'] = '9'
        self.assertEqual(self.run_hook().returncode, 0)
        self.assertFalse(self.log.exists())

    def test_mixed_source_retains_full_gate_and_failure(self):
        self.stage('AGENTS.md')
        self.stage('src/lib.rs')
        self.env['CARGO_EXIT'] = '9'
        self.assertEqual(self.run_hook().returncode, 9)
        self.assertIn('sqlx prepare', self.log.read_text())

    def test_configs_scripts_and_business_docs_do_not_qualify(self):
        for path in ['Cargo.toml', '.agents/skills/deploy/scripts/check.sh',
                     'docs/openapi.json', 'docs/template.md', 'migrations/001.up.sql', '.sqlx/query.json',
                     '.agents/skills/x/looks.md\nsrc.rs']:
            with self.subTest(path=path):
                self.git('reset', '-q', 'HEAD')
                self.stage(path)
                self.log.unlink(missing_ok=True)
                self.assertEqual(self.run_hook().returncode, 0)
                calls = self.log.read_text()
                if path.endswith(('.rs', '.sql')) or path == 'Cargo.toml':
                    self.assertIn('sqlx prepare', calls)
                else:
                    self.assertNotIn('sqlx prepare', calls)
                self.assertIn('clippy', calls)

    def test_rename_from_runtime_path_cannot_hide_behind_rule_name(self):
        self.git('mv', '.sqlx/query.json', 'AGENTS.md')
        self.env['CARGO_EXIT'] = '9'
        self.assertEqual(self.run_hook().returncode, 9)


if __name__ == '__main__':
    unittest.main()
