import os
import pathlib
import subprocess
import tempfile
import unittest


class PrePushHookTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.log = self.root / 'cargo.log'
        cargo = self.root / 'cargo'
        cargo.write_text(
            '#!/bin/sh\n'
            'printf "%s\\n" "$*" >> "$CARGO_LOG"\n'
            'case "$1" in\n'
            '  clippy) exit "${CLIPPY_EXIT:-0}" ;;\n'
            '  test) exit "${TEST_EXIT:-0}" ;;\n'
            'esac\n'
        )
        cargo.chmod(0o755)
        self.env = dict(os.environ, PATH=f'{self.root}:{os.environ["PATH"]}',
                        CARGO_LOG=str(self.log))
        for name in ('DATABASE_URL', 'TEST_REDIS_URL', 'REDIS_URL', 'SQLX_OFFLINE'):
            self.env.pop(name, None)
        self.env['DATABASE_URL'] = 'postgres://test:secret@localhost:15432/test'
        self.hook = pathlib.Path(__file__).resolve().parent.parent / '.githooks/pre-push'

    def run_hook(self, cwd=None):
        return subprocess.run(['sh', str(self.hook)], cwd=cwd or self.root, env=self.env,
                              capture_output=True, text=True)

    def test_missing_postgres_fails_before_cargo(self):
        self.env['TEST_REDIS_URL'] = 'redis://localhost:16379/0'
        for value in (None, ''):
            with self.subTest(value=value):
                self.env.pop('DATABASE_URL', None)
                if value is not None:
                    self.env['DATABASE_URL'] = value
                result = self.run_hook()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('DATABASE_URL', result.stderr)
                self.assertIn('CREATEDB', result.stderr)
                self.assertFalse(self.log.exists())

    def test_all_missing_reports_both_dependencies(self):
        self.env.pop('DATABASE_URL')
        result = self.run_hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('DATABASE_URL', result.stderr)
        self.assertIn('TEST_REDIS_URL', result.stderr)
        self.assertFalse(self.log.exists())

    def test_dotenv_candidate_defers_validation_without_executing_it(self):
        self.env.pop('DATABASE_URL')
        (self.root / '.env').write_text(
            'DATABASE_URL=postgres://test:secret@localhost:15432/test\n'
            'REDIS_URL=redis://localhost:16379/0\n'
            'SHELL_COMMAND=$(touch should_not_exist)\n'
        )
        nested = self.root / 'nested'
        nested.mkdir()
        for cwd in (self.root, nested):
            with self.subTest(cwd=cwd):
                self.log.unlink(missing_ok=True)
                result = self.run_hook(cwd)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.log.read_text().splitlines(), [
                    'clippy --all-targets --all-features -- -D warnings',
                    'test --locked --lib --bins',
                ])
                self.assertFalse((cwd / 'should_not_exist').exists())
                self.assertNotIn('secret', result.stdout + result.stderr)

    def test_missing_redis_fails_before_cargo_even_when_sqlx_offline(self):
        self.env['SQLX_OFFLINE'] = 'true'
        result = self.run_hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('TEST_REDIS_URL', result.stderr)
        self.assertIn('REDIS_URL', result.stderr)
        self.assertNotIn(self.env['DATABASE_URL'], result.stdout + result.stderr)
        self.assertFalse(self.log.exists())

    def test_configured_redis_preserves_both_commands(self):
        for config in ({'TEST_REDIS_URL': 'redis://localhost:16379/0'},
                       {'REDIS_URL': 'redis://localhost:16379/0'},
                       {'TEST_REDIS_URL': 'redis://localhost:16379/0', 'REDIS_URL': ''}):
            with self.subTest(config=config):
                self.log.unlink(missing_ok=True)
                self.env.pop('TEST_REDIS_URL', None)
                self.env.pop('REDIS_URL', None)
                self.env.update(config)
                result = self.run_hook()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.log.read_text().splitlines(), [
                    'clippy --all-targets --all-features -- -D warnings',
                    'test --locked --lib --bins',
                ])
                for value in config.values():
                    if value:
                        self.assertNotIn(value, result.stdout + result.stderr)

    def test_empty_selected_redis_does_not_fall_back(self):
        for config in ({'REDIS_URL': ''},
                       {'TEST_REDIS_URL': '', 'REDIS_URL': 'redis://localhost:16379/0'}):
            with self.subTest(config=config):
                self.env.pop('TEST_REDIS_URL', None)
                self.env.pop('REDIS_URL', None)
                self.env.update(config)
                result = self.run_hook()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('TEST_REDIS_URL', result.stderr)
                self.assertFalse(self.log.exists())

    def test_cargo_failures_remain_blocking(self):
        self.env['TEST_REDIS_URL'] = 'redis://localhost:16379/0'
        for variable, count in (('CLIPPY_EXIT', 1), ('TEST_EXIT', 2)):
            with self.subTest(variable=variable):
                self.log.unlink(missing_ok=True)
                self.env.pop('CLIPPY_EXIT', None)
                self.env.pop('TEST_EXIT', None)
                self.env[variable] = '9'
                result = self.run_hook()
                self.assertEqual(result.returncode, 9)
                self.assertEqual(len(self.log.read_text().splitlines()), count)


if __name__ == '__main__':
    unittest.main()
