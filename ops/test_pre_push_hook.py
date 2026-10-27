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
            '  fmt) exit "${FMT_EXIT:-0}" ;;\n'
            '  *) exit 99 ;;\n'
            'esac\n'
        )
        cargo.chmod(0o755)
        self.env = dict(os.environ, PATH=f'{self.root}:{os.environ["PATH"]}',
                        CARGO_LOG=str(self.log))
        for name in ('DATABASE_URL', 'TEST_REDIS_URL', 'REDIS_URL', 'SQLX_OFFLINE', 'FMT_EXIT'):
            self.env.pop(name, None)
        self.hook = pathlib.Path(__file__).resolve().parent.parent / '.githooks/pre-push'

    def run_hook(self):
        return subprocess.run(['sh', str(self.hook)], cwd=self.root, env=self.env,
                              capture_output=True, text=True)

    def test_push_checks_format_without_runtime_dependencies_or_compilation(self):
        result = self.run_hook()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.log.read_text().splitlines(), ['fmt --all -- --check'])

    def test_format_failure_remains_blocking(self):
        self.env['FMT_EXIT'] = '9'
        result = self.run_hook()
        self.assertEqual(result.returncode, 9)
        self.assertEqual(self.log.read_text().splitlines(), ['fmt --all -- --check'])


if __name__ == '__main__':
    unittest.main()
